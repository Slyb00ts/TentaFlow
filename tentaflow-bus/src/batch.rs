// ===== File: batch.rs — wire format: batch header, record layout, codec =====
//
// PLAN.md §2.3: the batch is the unit of write/fsync/replication/fetch, so a
// record is never serialized on its own on the hot path. `BatchBuilder`
// accumulates records into one contiguous buffer and hands back a single
// `bytes::Bytes` from `build()`; that exact buffer is what `Segment::append`
// writes, what a replication stream would forward (M2), and what every
// fan-out reader slices from.
//
// Compressed body format (review P2-13): when `flags` selects `Codec::Lz4`,
// the stored body is `lz4_flex::block::compress_prepend_size` output, i.e. a
// 4-byte little-endian *uncompressed* length prefix followed by the raw lz4
// block — not documented in PLAN §2.3's on-wire diagram, written down here
// so a second implementation reading `body_len` bytes after the header knows
// it must also strip/interpret those first 4 body bytes before calling an
// lz4 block decompressor.
//
// CRC (review P2-13, decision #3): `crc32c` is true Castagnoli CRC-32C (the
// `crc32c` crate, hardware-accelerated where available), computed over the
// *stored* (post-compression) body — matching both the field name and PLAN
// §2.3's "crc32c nad body".

use bytes::Bytes;
use smallvec::SmallVec;

use crate::error::{BusError, Result};

/// Batch header size on the wire (PLAN §2.3), little-endian, fixed layout.
pub const BATCH_HEADER_LEN: usize = 40;

/// Fixed-size prefix of a record, including the 4-byte `rec_len` field
/// itself (PLAN §2.3 record layout).
pub const RECORD_FIXED_LEN: usize = 28;

/// Only wire version this crate speaks.
pub const MAGIC_V1: u16 = 0x0001;

const FLAG_CODEC_MASK: u16 = 0b0000_0000_0000_0011;
// Bits 2-3 of the batch-level `flags` field are reserved by PLAN §2.3 for
// "external"/"encrypted" batch markers. Nothing in this crate sets or reads
// them yet (M0 has no `BlobRef`/encryption support), so no named constant or
// accessor is defined for them here — review P3-1: do not carry accessors
// that are dead code just because the wire format sets bits aside for later.

/// Record-level flag: payload is a `BlobRef`, not inline bytes (PLAN §2.4).
/// M0 never sets this — large-payload handling is M1 scope — but the bit is
/// defined now so the wire format does not need to change later.
pub const RECORD_FLAG_EXTERNAL: u16 = 1 << 0;

/// Batches whose *uncompressed* body exceeds this size get lz4-compressed
/// by default (PLAN §5.3.9: "compression on by default for batches > 32
/// KiB"). Below the threshold lz4 framing overhead is not worth the CPU.
/// Only applies to `BatchBuilder`'s default (`Codec`-unset) mode — a
/// producer that pins a codec via `with_codec` bypasses this heuristic
/// entirely (review P2-12, PLAN §7.1 `compression = lz4 | none` per topic).
pub const LZ4_COMPRESS_THRESHOLD: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Codec {
    None = 0,
    Lz4 = 1,
}

impl Codec {
    fn from_flags(flags: u16) -> Result<Self> {
        match flags & FLAG_CODEC_MASK {
            0 => Ok(Codec::None),
            1 => Ok(Codec::Lz4),
            other => Err(BusError::UnknownCodec(other)),
        }
    }
}

/// Decoded 40-byte batch header. Field names/order follow PLAN §2.3
/// verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchHeader {
    pub body_len: u32,
    pub base_offset: u64,
    pub record_count: u32,
    pub last_offset_delta: u32,
    pub base_timestamp_ms: i64,
    pub magic_version: u16,
    pub flags: u16,
    pub producer_epoch: u32,
    pub crc32c: u32,
}

impl BatchHeader {
    pub fn codec(&self) -> Result<Codec> {
        Codec::from_flags(self.flags)
    }

    /// Next absolute offset after this batch — the value a partition's
    /// `log_end_offset` advances to once the batch is durable.
    pub fn next_offset(&self) -> u64 {
        self.base_offset + self.record_count as u64
    }

    pub fn encode(&self, out: &mut [u8; BATCH_HEADER_LEN]) {
        out[0..4].copy_from_slice(&self.body_len.to_le_bytes());
        out[4..12].copy_from_slice(&self.base_offset.to_le_bytes());
        out[12..16].copy_from_slice(&self.record_count.to_le_bytes());
        out[16..20].copy_from_slice(&self.last_offset_delta.to_le_bytes());
        out[20..28].copy_from_slice(&self.base_timestamp_ms.to_le_bytes());
        out[28..30].copy_from_slice(&self.magic_version.to_le_bytes());
        out[30..32].copy_from_slice(&self.flags.to_le_bytes());
        out[32..36].copy_from_slice(&self.producer_epoch.to_le_bytes());
        out[36..40].copy_from_slice(&self.crc32c.to_le_bytes());
    }

    /// Decodes the first `BATCH_HEADER_LEN` bytes of `buf`. `buf` may be
    /// longer than the header (the body follows) or, safely, shorter than
    /// it — the length check happens *before* any indexing, so a caller
    /// never needs to pre-slice `buf` to exactly 40 bytes itself (review
    /// P1-2: `&batch[..BATCH_HEADER_LEN]` at a call site panics on a short
    /// buffer before this function's own bounds check ever runs).
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < BATCH_HEADER_LEN {
            return Err(BusError::TruncatedBatch {
                needed: BATCH_HEADER_LEN,
                available: buf.len(),
            });
        }
        let header = BatchHeader {
            body_len: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            base_offset: u64::from_le_bytes(buf[4..12].try_into().unwrap()),
            record_count: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
            last_offset_delta: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
            base_timestamp_ms: i64::from_le_bytes(buf[20..28].try_into().unwrap()),
            magic_version: u16::from_le_bytes(buf[28..30].try_into().unwrap()),
            flags: u16::from_le_bytes(buf[30..32].try_into().unwrap()),
            producer_epoch: u32::from_le_bytes(buf[32..36].try_into().unwrap()),
            crc32c: u32::from_le_bytes(buf[36..40].try_into().unwrap()),
        };
        if header.magic_version != MAGIC_V1 {
            return Err(BusError::BadMagic(header.magic_version));
        }
        Ok(header)
    }
}

/// One (key, value) header pair, both raw bytes on the wire — string
/// decoding is the caller's job, this crate never touches header content.
pub type HeaderPair = (Bytes, Bytes);

/// Producer-facing input record. Consumed by `BatchBuilder::push`.
#[derive(Debug, Clone)]
pub struct RecordInput {
    pub key: Option<Bytes>,
    pub headers: SmallVec<[HeaderPair; 4]>,
    pub payload: Bytes,
    pub schema_id: u32,
    pub external: bool,
    pub timestamp_ms: i64,
}

impl RecordInput {
    pub fn new(payload: Bytes, timestamp_ms: i64) -> Self {
        Self {
            key: None,
            headers: SmallVec::new(),
            payload,
            schema_id: 0,
            external: false,
            timestamp_ms,
        }
    }

    pub fn with_key(mut self, key: Bytes) -> Self {
        self.key = Some(key);
        self
    }

    pub fn with_header(mut self, key: impl Into<Bytes>, value: impl Into<Bytes>) -> Self {
        self.headers.push((key.into(), value.into()));
        self
    }

    pub fn with_schema_id(mut self, schema_id: u32) -> Self {
        self.schema_id = schema_id;
        self
    }
}

/// Accumulates records into one batch buffer and finalizes it into the
/// on-wire `Bytes` (header + optionally-compressed body). One builder is
/// used for exactly one batch; offsets are assigned as 0-based deltas in
/// push order, matching `base_offset + offset_delta` on decode.
///
/// `raw_body` reserves its first `BATCH_HEADER_LEN` bytes up front and
/// records are appended after them, so the common (uncompressed) `build()`
/// path patches the header in place and hands the same buffer to `Bytes`
/// with zero extra copies of the body (review P2-6 — the previous version
/// built the body into `raw_body` at offset 0, then `Vec::extend_from_slice`d
/// the whole thing into a second, header-prefixed buffer on every batch).
pub struct BatchBuilder {
    base_offset: u64,
    producer_epoch: u32,
    base_timestamp_ms: Option<i64>,
    record_count: u32,
    last_offset_delta: u32,
    raw_body: Vec<u8>,
    /// `None` = auto (default heuristic: compress when the body clears
    /// `LZ4_COMPRESS_THRESHOLD` *and* compression actually shrinks it).
    /// `Some(codec)` pins the codec unconditionally (review P2-12, PLAN
    /// §7.1 per-topic `compression = lz4 | none`).
    codec: Option<Codec>,
}

impl BatchBuilder {
    pub fn new(base_offset: u64, producer_epoch: u32) -> Self {
        Self::with_capacity(base_offset, producer_epoch, 4096)
    }

    pub fn with_capacity(base_offset: u64, producer_epoch: u32, capacity: usize) -> Self {
        let mut raw_body = Vec::with_capacity(BATCH_HEADER_LEN + capacity);
        raw_body.resize(BATCH_HEADER_LEN, 0);
        Self {
            base_offset,
            producer_epoch,
            base_timestamp_ms: None,
            record_count: 0,
            last_offset_delta: 0,
            raw_body,
            codec: None,
        }
    }

    /// Pins the codec `build()` uses, overriding the size-threshold
    /// heuristic entirely (review P2-12). `Codec::None` never compresses;
    /// `Codec::Lz4` always compresses (even below `LZ4_COMPRESS_THRESHOLD`),
    /// still falling back to storing the raw body if compression would not
    /// shrink it.
    pub fn with_codec(mut self, codec: Codec) -> Self {
        self.codec = Some(codec);
        self
    }

    pub fn base_offset(&self) -> u64 {
        self.base_offset
    }

    pub fn record_count(&self) -> u32 {
        self.record_count
    }

    /// Raw (pre-compression) body size accumulated so far — what a producer
    /// compares against `batch_max_bytes` while deciding whether to flush.
    /// Excludes the reserved 40-byte header slot.
    pub fn raw_body_len(&self) -> usize {
        self.raw_body.len() - BATCH_HEADER_LEN
    }

    pub fn push(&mut self, record: RecordInput) -> Result<()> {
        let offset_delta = self.record_count;
        let base_ts = *self.base_timestamp_ms.get_or_insert(record.timestamp_ms);
        let ts_delta = record.timestamp_ms - base_ts;
        let ts_delta_ms: i32 = ts_delta
            .try_into()
            .map_err(|_| BusError::RecordFieldTooLarge {
                field: "ts_delta_ms",
                len: ts_delta.unsigned_abs() as usize,
            })?;

        let key_len = record.key.as_ref().map_or(0, |k| k.len());
        if key_len > u32::MAX as usize {
            return Err(BusError::RecordFieldTooLarge {
                field: "key_len",
                len: key_len,
            });
        }
        let payload_len = record.payload.len();
        if payload_len > u32::MAX as usize {
            return Err(BusError::RecordFieldTooLarge {
                field: "payload_len",
                len: payload_len,
            });
        }
        if record.headers.len() > u16::MAX as usize {
            return Err(BusError::TooManyHeaders {
                count: record.headers.len(),
            });
        }

        let mut headers_len = 0usize;
        for (k, v) in &record.headers {
            if k.len() > u16::MAX as usize {
                return Err(BusError::RecordFieldTooLarge {
                    field: "header_key_len",
                    len: k.len(),
                });
            }
            if v.len() > u32::MAX as usize {
                return Err(BusError::RecordFieldTooLarge {
                    field: "header_val_len",
                    len: v.len(),
                });
            }
            headers_len += 2 + k.len() + 4 + v.len();
        }

        let flags: u16 = if record.external {
            RECORD_FLAG_EXTERNAL
        } else {
            0
        };
        let variable_len = key_len + headers_len + payload_len;
        // rec_len covers everything *after* the rec_len field itself, so a
        // reader that has just consumed those 4 bytes knows exactly how far
        // to skip without decoding the rest.
        let rec_len = (RECORD_FIXED_LEN - 4 + variable_len) as u32;

        self.raw_body.reserve(4 + rec_len as usize);
        self.raw_body.extend_from_slice(&rec_len.to_le_bytes());
        self.raw_body.extend_from_slice(&offset_delta.to_le_bytes());
        self.raw_body.extend_from_slice(&ts_delta_ms.to_le_bytes());
        self.raw_body
            .extend_from_slice(&record.schema_id.to_le_bytes());
        self.raw_body
            .extend_from_slice(&(record.headers.len() as u16).to_le_bytes());
        self.raw_body.extend_from_slice(&flags.to_le_bytes());
        self.raw_body
            .extend_from_slice(&(key_len as u32).to_le_bytes());
        self.raw_body
            .extend_from_slice(&(payload_len as u32).to_le_bytes());
        if let Some(k) = &record.key {
            self.raw_body.extend_from_slice(k);
        }
        for (k, v) in &record.headers {
            self.raw_body
                .extend_from_slice(&(k.len() as u16).to_le_bytes());
            self.raw_body.extend_from_slice(k);
            self.raw_body
                .extend_from_slice(&(v.len() as u32).to_le_bytes());
            self.raw_body.extend_from_slice(v);
        }
        self.raw_body.extend_from_slice(&record.payload);

        self.last_offset_delta = offset_delta;
        self.record_count += 1;
        Ok(())
    }

    /// Finalizes the batch: compresses the body per the configured codec
    /// policy, computes the CRC over the stored (possibly compressed)
    /// bytes, and writes the 40-byte header into the buffer's reserved
    /// prefix. The returned `Bytes` is the exact buffer that goes to disk.
    pub fn build(self) -> Result<Bytes> {
        if self.record_count == 0 {
            return Err(BusError::EmptyBatch);
        }

        let raw_len = self.raw_body.len() - BATCH_HEADER_LEN;
        let want_compress = match self.codec {
            Some(Codec::None) => false,
            Some(Codec::Lz4) => true,
            None => raw_len > LZ4_COMPRESS_THRESHOLD,
        };

        let (codec, mut out): (Codec, Vec<u8>) = if want_compress {
            let compressed =
                lz4_flex::block::compress_prepend_size(&self.raw_body[BATCH_HEADER_LEN..]);
            if compressed.len() < raw_len {
                let mut out = Vec::with_capacity(BATCH_HEADER_LEN + compressed.len());
                out.resize(BATCH_HEADER_LEN, 0);
                out.extend_from_slice(&compressed);
                (Codec::Lz4, out)
            } else {
                (Codec::None, self.raw_body)
            }
        } else {
            (Codec::None, self.raw_body)
        };

        let body_len = out.len() - BATCH_HEADER_LEN;
        if body_len > u32::MAX as usize {
            return Err(BusError::BatchTooLarge { len: body_len });
        }

        let crc32c = crc32c::crc32c(&out[BATCH_HEADER_LEN..]);
        let header = BatchHeader {
            body_len: body_len as u32,
            base_offset: self.base_offset,
            record_count: self.record_count,
            last_offset_delta: self.last_offset_delta,
            base_timestamp_ms: self.base_timestamp_ms.unwrap_or(0),
            magic_version: MAGIC_V1,
            flags: codec as u16,
            producer_epoch: self.producer_epoch,
            crc32c,
        };
        let header_buf: &mut [u8; BATCH_HEADER_LEN] =
            (&mut out[..BATCH_HEADER_LEN]).try_into().unwrap();
        header.encode(header_buf);
        Ok(Bytes::from(out))
    }
}

/// A parsed, CRC-validated, decompressed batch ready for record iteration.
/// When the batch is stored uncompressed, `body` is a zero-copy slice of the
/// input `Bytes`; lz4-compressed batches pay one decompression copy here.
#[derive(Debug, Clone)]
pub struct BatchView {
    header: BatchHeader,
    body: Bytes,
}

impl BatchView {
    /// Parses one batch (header + stored body) out of `raw`. `raw` may carry
    /// trailing bytes belonging to the next batch — only `body_len` bytes
    /// after the header are consumed and CRC-checked.
    pub fn parse(raw: Bytes) -> Result<Self> {
        if raw.len() < BATCH_HEADER_LEN {
            return Err(BusError::TruncatedBatch {
                needed: BATCH_HEADER_LEN,
                available: raw.len(),
            });
        }
        let header = BatchHeader::decode(&raw[..BATCH_HEADER_LEN])?;
        // `body_len` is attacker/corruption-controlled input (it comes
        // straight off the wire); a 32-bit target could see this overflow
        // `usize` (review P3-6).
        let needed = (header.body_len as usize)
            .checked_add(BATCH_HEADER_LEN)
            .ok_or(BusError::BatchTooLarge {
                len: header.body_len as usize,
            })?;
        if raw.len() < needed {
            return Err(BusError::TruncatedBatch {
                needed,
                available: raw.len(),
            });
        }
        let stored_body = raw.slice(BATCH_HEADER_LEN..needed);

        let computed = crc32c::crc32c(&stored_body);
        if computed != header.crc32c {
            return Err(BusError::CrcMismatch {
                expected: header.crc32c,
                computed,
            });
        }

        let body = match header.codec()? {
            Codec::None => stored_body,
            Codec::Lz4 => {
                let decompressed = lz4_flex::block::decompress_size_prepended(&stored_body)
                    .map_err(|e| BusError::Compression(e.to_string()))?;
                Bytes::from(decompressed)
            }
        };

        Ok(BatchView { header, body })
    }

    /// Total on-wire length of this batch (header + stored body), i.e. what
    /// `parse` consumed from the front of its input.
    pub fn wire_len(&self) -> usize {
        BATCH_HEADER_LEN + self.header.body_len as usize
    }

    pub fn header(&self) -> &BatchHeader {
        &self.header
    }

    pub fn records(&self) -> RecordIter<'_> {
        RecordIter {
            body: &self.body,
            pos: 0,
            remaining: self.header.record_count,
        }
    }

    /// Iterates only the records whose absolute offset
    /// (`header.base_offset + offset_delta`) is `>= from_offset`.
    ///
    /// `PartitionReader::fetch_from_offset` returns whole batches (review
    /// P3-13/P3-12): its sparse index only gives a *floor* position, so the
    /// first returned batch may start before the requested offset, same as
    /// Kafka. Callers that need to resume reading from an exact offset
    /// should drive iteration through this helper on the first batch of a
    /// fetch result rather than `records()` directly, instead of
    /// re-deriving the filter themselves.
    pub fn records_from(&self, from_offset: u64) -> impl Iterator<Item = Result<RecordView>> + '_ {
        let base = self.header.base_offset;
        self.records().filter(move |r| match r {
            Ok(rv) => base + rv.offset_delta as u64 >= from_offset,
            Err(_) => true,
        })
    }
}

/// A decoded record, borrowed zero-copy (via `Bytes::slice` refcount bumps,
/// no memcpy) from the batch's decompressed body.
#[derive(Debug, Clone)]
pub struct RecordView {
    pub offset_delta: u32,
    pub ts_delta_ms: i32,
    pub schema_id: u32,
    pub external: bool,
    pub key: Option<Bytes>,
    pub headers: SmallVec<[HeaderPair; 4]>,
    pub payload: Bytes,
}

pub struct RecordIter<'a> {
    body: &'a Bytes,
    pos: usize,
    remaining: u32,
}

impl<'a> Iterator for RecordIter<'a> {
    type Item = Result<RecordView>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let item = self.decode_one();
        if item.is_err() {
            // A malformed record poisons the rest of the iteration: `pos`
            // cannot be trusted to point at the next record boundary, so
            // stop instead of decoding garbage as if it were a fresh
            // record header.
            self.remaining = 0;
        }
        Some(item)
    }
}

impl<'a> RecordIter<'a> {
    /// Reads a `u32`/`u16` field at `p..p+width` if it fits within `buf`;
    /// otherwise reports a `TruncatedBatch` naming the field.
    fn checked_range(buf_len: usize, p: usize, len: usize, field: &'static str) -> Result<()> {
        let end = p.checked_add(len).ok_or(BusError::RecordFieldOutOfBounds {
            field,
            pos: p,
            len,
            record_end: buf_len,
        })?;
        if end > buf_len {
            return Err(BusError::RecordFieldOutOfBounds {
                field,
                pos: p,
                len,
                record_end: buf_len,
            });
        }
        Ok(())
    }

    fn decode_one(&mut self) -> Result<RecordView> {
        let buf = self.body;
        if buf.len() < self.pos + 4 {
            return Err(BusError::TruncatedBatch {
                needed: self.pos + 4,
                available: buf.len(),
            });
        }
        let rec_len = u32::from_le_bytes(buf[self.pos..self.pos + 4].try_into().unwrap()) as usize;
        if rec_len < RECORD_FIXED_LEN - 4 {
            return Err(BusError::TruncatedBatch {
                needed: RECORD_FIXED_LEN,
                available: rec_len + 4,
            });
        }
        let record_end = self
            .pos
            .checked_add(4)
            .and_then(|p| p.checked_add(rec_len))
            .ok_or(BusError::RecordFieldOutOfBounds {
                field: "rec_len",
                pos: self.pos,
                len: rec_len,
                record_end: buf.len(),
            })?;
        if buf.len() < record_end {
            return Err(BusError::TruncatedBatch {
                needed: record_end,
                available: buf.len(),
            });
        }

        let mut p = self.pos + 4;
        let offset_delta = u32::from_le_bytes(buf[p..p + 4].try_into().unwrap());
        p += 4;
        let ts_delta_ms = i32::from_le_bytes(buf[p..p + 4].try_into().unwrap());
        p += 4;
        let schema_id = u32::from_le_bytes(buf[p..p + 4].try_into().unwrap());
        p += 4;
        let header_count = u16::from_le_bytes(buf[p..p + 2].try_into().unwrap());
        p += 2;
        let flags = u16::from_le_bytes(buf[p..p + 2].try_into().unwrap());
        p += 2;
        let key_len = u32::from_le_bytes(buf[p..p + 4].try_into().unwrap()) as usize;
        p += 4;
        let payload_len = u32::from_le_bytes(buf[p..p + 4].try_into().unwrap()) as usize;
        p += 4;

        // Every variable-length field below is validated to land within
        // `record_end` — the record's *own* declared boundary, not merely
        // the whole buffer — before it is ever sliced. `Bytes::slice`
        // panics on an out-of-range index; a batch with a maliciously (or
        // corruption-induced) large `key_len`/`payload_len` that would
        // otherwise still fall inside the buffer but past this record's
        // end must be rejected, not silently handed back as if it were
        // this record's data (review P1-1).
        Self::checked_range(record_end, p, key_len, "key_len")?;
        let key = if key_len > 0 {
            let k = self.body.slice(p..p + key_len);
            p += key_len;
            Some(k)
        } else {
            None
        };

        let mut headers = SmallVec::new();
        for _ in 0..header_count {
            Self::checked_range(record_end, p, 2, "header_key_len")?;
            let klen = u16::from_le_bytes(buf[p..p + 2].try_into().unwrap()) as usize;
            p += 2;
            Self::checked_range(record_end, p, klen, "header_key")?;
            let hk = self.body.slice(p..p + klen);
            p += klen;
            Self::checked_range(record_end, p, 4, "header_val_len")?;
            let vlen = u32::from_le_bytes(buf[p..p + 4].try_into().unwrap()) as usize;
            p += 4;
            Self::checked_range(record_end, p, vlen, "header_val")?;
            let hv = self.body.slice(p..p + vlen);
            p += vlen;
            headers.push((hk, hv));
        }

        Self::checked_range(record_end, p, payload_len, "payload_len")?;
        let payload = self.body.slice(p..p + payload_len);
        p += payload_len;
        if p != record_end {
            return Err(BusError::RecordFieldOutOfBounds {
                field: "record_trailer",
                pos: p,
                len: 0,
                record_end,
            });
        }

        self.pos = record_end;
        self.remaining -= 1;

        Ok(RecordView {
            offset_delta,
            ts_delta_ms,
            schema_id,
            external: flags & RECORD_FLAG_EXTERNAL != 0,
            key,
            headers,
            payload,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(n: usize) -> Bytes {
        Bytes::from(vec![0xAB; n])
    }

    #[test]
    fn roundtrip_single_record() {
        let mut b = BatchBuilder::new(100, 7);
        b.push(RecordInput::new(payload(16), 1_000)).unwrap();
        let raw = b.build().unwrap();

        let view = BatchView::parse(raw).unwrap();
        assert_eq!(view.header().base_offset, 100);
        assert_eq!(view.header().record_count, 1);
        assert_eq!(view.header().last_offset_delta, 0);
        assert_eq!(view.header().base_timestamp_ms, 1_000);
        assert!(matches!(view.header().codec().unwrap(), Codec::None));

        let records: Vec<_> = view.records().collect::<Result<_>>().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].offset_delta, 0);
        assert_eq!(records[0].payload.as_ref(), payload(16).as_ref());
        assert!(records[0].key.is_none());
        assert!(records[0].headers.is_empty());
    }

    #[test]
    fn roundtrip_multiple_records_with_key_and_headers() {
        let mut b = BatchBuilder::new(0, 1);
        for i in 0..5u32 {
            let rec = RecordInput::new(payload(32), 2_000 + i as i64 * 10)
                .with_key(Bytes::from(format!("key-{i}")))
                .with_header("tf.actor", "unit-test")
                .with_header("tf.org", "org-1")
                .with_schema_id(42);
            b.push(rec).unwrap();
        }
        let raw = b.build().unwrap();
        let view = BatchView::parse(raw).unwrap();
        assert_eq!(view.header().record_count, 5);
        assert_eq!(view.header().last_offset_delta, 4);

        let records: Vec<_> = view.records().collect::<Result<_>>().unwrap();
        assert_eq!(records.len(), 5);
        for (i, rec) in records.iter().enumerate() {
            assert_eq!(rec.offset_delta, i as u32);
            assert_eq!(rec.ts_delta_ms, i as i32 * 10);
            assert_eq!(rec.schema_id, 42);
            assert_eq!(rec.key.as_deref(), Some(format!("key-{i}").as_bytes()));
            assert_eq!(rec.headers.len(), 2);
            assert_eq!(rec.headers[0].1.as_ref(), b"unit-test");
        }
    }

    #[test]
    fn records_from_filters_records_before_the_requested_offset() {
        let mut b = BatchBuilder::new(10, 1); // base_offset = 10
        for i in 0..5u32 {
            b.push(RecordInput::new(payload(8), i as i64)).unwrap();
        }
        let raw = b.build().unwrap();
        let view = BatchView::parse(raw).unwrap();

        // Absolute offsets present: 10..15. Asking for records_from(12)
        // must skip the first two (offsets 10, 11).
        let kept: Vec<_> = view.records_from(12).collect::<Result<Vec<_>>>().unwrap();
        assert_eq!(kept.len(), 3);
        assert_eq!(kept[0].offset_delta, 2); // absolute offset 12
        assert_eq!(kept[1].offset_delta, 3);
        assert_eq!(kept[2].offset_delta, 4);

        // Below the batch's base offset: every record is kept.
        let all: Vec<_> = view.records_from(0).collect::<Result<Vec<_>>>().unwrap();
        assert_eq!(all.len(), 5);

        // Past the batch's last offset: nothing is kept.
        let none: Vec<_> = view.records_from(100).collect::<Result<Vec<_>>>().unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn compression_roundtrip_large_batch() {
        let mut b = BatchBuilder::new(0, 1);
        // Highly compressible payload well above the 32 KiB threshold.
        for _ in 0..200 {
            b.push(RecordInput::new(payload(512), 0)).unwrap();
        }
        assert!(b.raw_body_len() > LZ4_COMPRESS_THRESHOLD);
        let raw = b.build().unwrap();

        let view = BatchView::parse(raw.clone()).unwrap();
        assert!(matches!(view.header().codec().unwrap(), Codec::Lz4));
        // Compressed wire form must be smaller than an uncompressed batch
        // would have been (header + raw body), proving compression fired.
        assert!(view.wire_len() < BATCH_HEADER_LEN + 200 * (RECORD_FIXED_LEN - 4 + 512));

        let records: Vec<_> = view.records().collect::<Result<_>>().unwrap();
        assert_eq!(records.len(), 200);
        assert_eq!(records[0].payload.as_ref(), payload(512).as_ref());
    }

    #[test]
    fn codec_none_forces_uncompressed_even_above_threshold() {
        let mut b = BatchBuilder::new(0, 1).with_codec(Codec::None);
        for _ in 0..200 {
            b.push(RecordInput::new(payload(512), 0)).unwrap();
        }
        assert!(b.raw_body_len() > LZ4_COMPRESS_THRESHOLD);
        let raw = b.build().unwrap();
        let view = BatchView::parse(raw).unwrap();
        assert!(matches!(view.header().codec().unwrap(), Codec::None));
    }

    #[test]
    fn codec_lz4_forces_compression_below_threshold() {
        // Small, but highly repetitive payload — auto mode would skip
        // compression (below LZ4_COMPRESS_THRESHOLD); forcing Lz4 must
        // still compress it.
        let mut b = BatchBuilder::new(0, 1).with_codec(Codec::Lz4);
        b.push(RecordInput::new(Bytes::from(vec![0x00; 4096]), 0))
            .unwrap();
        assert!(b.raw_body_len() < LZ4_COMPRESS_THRESHOLD);
        let raw = b.build().unwrap();
        let view = BatchView::parse(raw).unwrap();
        assert!(matches!(view.header().codec().unwrap(), Codec::Lz4));
    }

    #[test]
    fn crc_mismatch_is_detected() {
        let mut b = BatchBuilder::new(0, 1);
        b.push(RecordInput::new(payload(8), 0)).unwrap();
        let raw = b.build().unwrap();
        let mut corrupted = raw.to_vec();
        let last = corrupted.len() - 1;
        corrupted[last] ^= 0xFF; // flip a payload byte, header's CRC now stale
        let err = BatchView::parse(Bytes::from(corrupted)).unwrap_err();
        assert!(matches!(err, BusError::CrcMismatch { .. }));
    }

    #[test]
    fn empty_batch_is_rejected() {
        let b = BatchBuilder::new(0, 1);
        assert!(matches!(b.build().unwrap_err(), BusError::EmptyBatch));
    }

    #[test]
    fn truncated_batch_is_detected() {
        let mut b = BatchBuilder::new(0, 1);
        b.push(RecordInput::new(payload(64), 0)).unwrap();
        let raw = b.build().unwrap();
        let short = raw.slice(0..raw.len() - 10);
        let err = BatchView::parse(short).unwrap_err();
        assert!(matches!(err, BusError::TruncatedBatch { .. }));
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut b = BatchBuilder::new(0, 1);
        b.push(RecordInput::new(payload(8), 0)).unwrap();
        let raw = b.build().unwrap();
        let mut corrupted = raw.to_vec();
        corrupted[28] = 0xEE; // magic_version low byte
        corrupted[29] = 0xEE;
        let err = BatchView::parse(Bytes::from(corrupted)).unwrap_err();
        assert!(matches!(err, BusError::BadMagic(_)));
    }

    /// Review P1-1 / test gap #7: a record whose `payload_len` claims far
    /// more bytes than remain in the record (and would even reach past the
    /// whole batch buffer) must be rejected with a clean error, never a
    /// `Bytes::slice` panic.
    #[test]
    fn malicious_field_lengths_are_rejected_not_panicking() {
        let mut b = BatchBuilder::new(0, 1);
        b.push(RecordInput::new(payload(8), 0)).unwrap();
        let raw = b.build().unwrap();
        let mut corrupted = raw.to_vec();

        // payload_len lives at header(40) + rec_len(4) + offset_delta(4)
        // + ts_delta(4) + schema_id(4) + header_count(2) + flags(2)
        // + key_len(4) = byte 64, a u32.
        let payload_len_pos = BATCH_HEADER_LEN + 4 + 4 + 4 + 4 + 2 + 2 + 4;
        corrupted[payload_len_pos..payload_len_pos + 4]
            .copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());

        // CRC no longer matches the mutated body, so recompute it — this
        // test targets the *field-bounds* check inside record decoding,
        // not CRC validation, and must reach that code path.
        let stored_body = &corrupted[BATCH_HEADER_LEN..];
        let crc = crc32c::crc32c(stored_body);
        corrupted[36..40].copy_from_slice(&crc.to_le_bytes());

        let view = BatchView::parse(Bytes::from(corrupted)).unwrap();
        let err = view.records().collect::<Result<Vec<_>>>().unwrap_err();
        assert!(matches!(err, BusError::RecordFieldOutOfBounds { .. }));
    }
}
