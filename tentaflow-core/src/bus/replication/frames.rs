// =============================================================================
// File: bus/replication/frames.rs — M2 replication wire frames + codec
// =============================================================================
//
// PLAN-M2 §1b: frozen wave-0 contract. Agents RL (`leader.rs`) and RF
// (`follower.rs`) both build against this module read-only starting wave 1
// — nothing here should need to change shape once they land; if it does,
// that is a coordinator-only edit (PLAN-M2 §2).
//
// Wire shape, uniform across every frame kind:
//
//   [u8 kind][u32 cbor_len][cbor bytes][u32 raw_len][raw bytes]
//
// `raw` carries the actual batch bytes for `ReplFrame::Batch` ONLY (the
// follower's `Partition::append_replicated` wants the exact bytes the
// leader's `PartitionReader::fetch_from_offset` returned, zero
// re-serialization — PLAN-M2 §1a); every other kind always writes
// `raw_len = 0` and no raw bytes. Keeping the trailing `[u32 raw_len][raw]`
// present (even if empty) on every frame, rather than only on `Batch`,
// means `read_frame`/`write_frame` never need a kind-dependent branch for
// how many sections to read — only for what to do with them.
//
// CBOR (not the crate's `minicbor`) because this framing is deliberately
// modeled on `sync/baseline_transport.rs`'s len-prefixed `ciborium` frames
// (the existing "bulk stream over a dedicated ALPN" pattern this repo
// already trusts), and `ciborium`'s serde integration matches this
// module's plain `#[derive(Serialize, Deserialize)]` structs without any
// hand-written encode/decode per field.

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tentaflow_protocol::environment::NodeEnvironment;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Upper bound on one frame's total wire size (kind byte + both
/// length-prefixed sections). PLAN-M2 §1b's own number: generous for a
/// control frame (Hello/Ack/Heartbeat/…, all well under 1 KiB even with
/// long node ids), and large enough for one `Batch` frame's raw bytes —
/// `batch_max_bytes` (PLAN §5.3.1 default 1 MiB) plus CBOR/index overhead
/// never comes close.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

const KIND_HELLO: u8 = 0;
const KIND_HELLO_ACK: u8 = 1;
const KIND_BATCH: u8 = 2;
const KIND_ACK: u8 = 3;
const KIND_HEARTBEAT: u8 = 4;
const KIND_TRUNCATE: u8 = 5;
const KIND_LEO_QUERY: u8 = 6;
const KIND_LEO_REPLY: u8 = 7;
const KIND_OFFSETS: u8 = 8;

/// First frame on a new leader->follower replication stream (PLAN-M2 §1b).
/// Carries both fencing checks a stream establishment needs (PLAN-M2 §1b
/// "Fencing środowisk"): `environment` here is the Hello/HelloAck half of
/// the two independent environment gates (the other is the mesh accept-arm
/// check in `mesh/iroh_manager.rs`, PLAN-M2 §1d).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplHello {
    pub org_id: String,
    pub topic: String,
    pub partition: u32,
    pub leader_node_id: String,
    pub leader_epoch: u32,
    pub replicas: Vec<String>,
    pub environment: NodeEnvironment,
}

/// Follower's response to `ReplHello`. `accepted = false` always carries a
/// `reject` explaining why (PLAN-M2 §1b); `accepted = true` carries the
/// follower's own offsets so the leader knows where to resume feeding from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplHelloAck {
    pub accepted: bool,
    pub follower_leo: u64,
    pub follower_hw: u64,
    pub follower_epoch: u32,
    pub environment: NodeEnvironment,
    pub reject: Option<ReplReject>,
}

/// Why a follower refused a `ReplHello` (PLAN-M2 §1b).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplReject {
    EnvironmentMismatch {
        theirs: NodeEnvironment,
        ours: NodeEnvironment,
    },
    StaleEpoch {
        have: u32,
    },
    NotAReplica,
    TopicUnknown,
    Detached,
}

/// CBOR-encoded metadata half of a `ReplFrame::Batch` — the raw batch
/// bytes ride alongside in the frame's `raw` section, untouched (PLAN-M2
/// §1a: `append_replicated` does not re-serialize).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplBatchHeader {
    pub leader_epoch: u32,
    pub base_offset: u64,
    pub hw: u64,
    pub batch_len: u32,
    /// K-M2-6 (A6, PLAN-M2 §4.1): producer idempotency state rides with
    /// the batch, one mark per batch rather than per record, since the
    /// engine's on-disk batch header carries no `producer_id` field and
    /// changing that would break M1 on-disk compatibility.
    pub producer: Option<ReplProducerMark>,
    /// Reserved for layer-2 dedup (mmap `idempotency_key`, PLAN-M2 §4.1
    /// A7) — always empty until CEL is wired in M3a. Kept in the frame
    /// shape now so wiring it later does not change this wave's frozen
    /// contract.
    pub dedup_keys: Vec<[u8; 16]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplProducerMark {
    pub producer_id: String,
    pub epoch: u32,
    pub base_offset: u64,
    /// M2 wave 2 (agent G): the producer's own idempotency sequence counter
    /// at the start of this batch — distinct from `base_offset` (the
    /// PARTITION offset the leader assigned this batch). Fixes the
    /// documented gap `follower.rs`'s `Batch` handling used to carry (this
    /// module's own doc, wave 1): before this field existed, a follower had
    /// no choice but to key `ProducerIdentity::base_seq` off `base_offset`,
    /// which only happened to be correct for a producer's very first batch
    /// after a fresh partition. `#[serde(default)]` (= `0`) so a peer still
    /// running a wave-1 build (no `base_seq` on the wire at all) decodes
    /// cleanly instead of failing the frame outright — `0` is the same
    /// imprecise-but-safe value `base_offset`-keying used to produce for a
    /// producer's very first batch, so this is a graceful downgrade, not a
    /// new failure mode.
    #[serde(default)]
    pub base_seq: u64,
}

/// Follower -> leader acknowledgement (PLAN-M2 §1b), sent on a cadence
/// (every N batches or 500 ms) rather than per batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplAck {
    pub leader_epoch: u32,
    pub follower_leo: u64,
    pub follower_hw: u64,
}

/// Leader -> follower keep-alive in the absence of real traffic (PLAN-M2
/// §1b, every 500 ms) — also the follower's `leader_lease_ms` watchdog
/// input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplHeartbeat {
    pub leader_epoch: u32,
    pub hw: u64,
    pub leader_leo: u64,
}

/// Leader -> follower tail truncation (PLAN-M2 §1a `Partition::
/// truncate_to_offset`, K-M2-1): sent to a replica whose `leo` is ahead of
/// the new leader's chain (in practice, a former leader rejoining after a
/// failover).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplTruncate {
    pub leader_epoch: u32,
    pub to_offset: u64,
}

/// K-M2-3: election candidate's query to another replica for its `leo`,
/// since a follower otherwise never learns another follower's offset
/// (it only ever talks to the leader).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplLeoQuery {
    pub org_id: String,
    pub topic: String,
    pub partition: u32,
    pub known_epoch: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplLeoReply {
    pub leo: u64,
    pub hw: u64,
    pub leader_epoch: u32,
    pub in_isr: bool,
}

/// K-M2-5: consumer-group offset/attempts/discard state, replicated
/// out-of-band from the hot path (coalesced every 500 ms) so a promoted
/// follower has a bounded (<=500 ms), never-early view of group progress
/// instead of resetting it on failover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplOffsets {
    pub leader_epoch: u32,
    /// `(group, partition, offset, attempts)`.
    pub commits: Vec<(String, u32, u64, u32)>,
    /// `(partition, offset)` — `dlq_discarded` entries (PLAN-M2 §0 K-M2-5).
    pub discarded: Vec<(u32, u64)>,
}

/// One frame on a replication stream (PLAN-M2 §1b). `Batch` is the only
/// variant whose payload spans both wire sections: `header` is CBOR, the
/// batch bytes are `raw`. No `Serialize`/`Deserialize` here — `ReplFrame`
/// itself is never CBOR-encoded as a whole (`encode_parts`/`read_frame`
/// dispatch on `kind` and en/decode each variant's own payload type), so
/// deriving them would be dead code, and `Bytes` (the `Batch` field) is not
/// `Deserialize` without the `bytes` crate's optional `serde` feature,
/// which this crate does not enable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplFrame {
    Hello(ReplHello),
    HelloAck(ReplHelloAck),
    Batch {
        header: ReplBatchHeader,
        bytes: Bytes,
    },
    Ack(ReplAck),
    Heartbeat(ReplHeartbeat),
    Truncate(ReplTruncate),
    LeoQuery(ReplLeoQuery),
    LeoReply(ReplLeoReply),
    Offsets(ReplOffsets),
}

/// Errors from `read_frame`/`write_frame`. Distinct from
/// `tentaflow_bus::BusError`/`BusServiceError` — this is a pure transport
/// codec with no engine or service state in scope.
#[derive(Debug, thiserror::Error)]
pub enum ReplCodecError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("CBOR decode error: {0}")]
    Decode(String),
    #[error("CBOR encode error: {0}")]
    Encode(String),
    #[error("frame of {len} bytes exceeds MAX_FRAME_BYTES ({MAX_FRAME_BYTES})")]
    FrameTooLarge { len: usize },
    #[error("unknown replication frame kind {0:#04x}")]
    UnknownKind(u8),
}

fn encode_cbor<T: Serialize>(value: &T) -> Result<Vec<u8>, ReplCodecError> {
    let mut body = Vec::new();
    ciborium::ser::into_writer(value, &mut body)
        .map_err(|e| ReplCodecError::Encode(e.to_string()))?;
    Ok(body)
}

fn decode_cbor<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, ReplCodecError> {
    ciborium::de::from_reader(std::io::Cursor::new(body))
        .map_err(|e| ReplCodecError::Decode(e.to_string()))
}

/// `(kind, cbor bytes, raw bytes)` for one frame — the shared shape both
/// `write_frame` and the size-checking logic operate on.
fn encode_parts(frame: &ReplFrame) -> Result<(u8, Vec<u8>, &[u8]), ReplCodecError> {
    Ok(match frame {
        ReplFrame::Hello(v) => (KIND_HELLO, encode_cbor(v)?, &[][..]),
        ReplFrame::HelloAck(v) => (KIND_HELLO_ACK, encode_cbor(v)?, &[][..]),
        ReplFrame::Batch { header, bytes } => (KIND_BATCH, encode_cbor(header)?, bytes.as_ref()),
        ReplFrame::Ack(v) => (KIND_ACK, encode_cbor(v)?, &[][..]),
        ReplFrame::Heartbeat(v) => (KIND_HEARTBEAT, encode_cbor(v)?, &[][..]),
        ReplFrame::Truncate(v) => (KIND_TRUNCATE, encode_cbor(v)?, &[][..]),
        ReplFrame::LeoQuery(v) => (KIND_LEO_QUERY, encode_cbor(v)?, &[][..]),
        ReplFrame::LeoReply(v) => (KIND_LEO_REPLY, encode_cbor(v)?, &[][..]),
        ReplFrame::Offsets(v) => (KIND_OFFSETS, encode_cbor(v)?, &[][..]),
    })
}

/// Writes one frame: `[u8 kind][u32 cbor_len][cbor][u32 raw_len][raw]`
/// (PLAN-M2 §1b). Rejects a frame whose total wire size would exceed
/// `MAX_FRAME_BYTES` before writing anything, so a caller never observes a
/// half-written oversized frame on the stream.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    frame: &ReplFrame,
) -> Result<(), ReplCodecError> {
    let (kind, cbor, raw) = encode_parts(frame)?;
    let total = 1 + 4 + cbor.len() + 4 + raw.len();
    if total > MAX_FRAME_BYTES {
        return Err(ReplCodecError::FrameTooLarge { len: total });
    }
    w.write_u8(kind).await?;
    w.write_u32(cbor.len() as u32).await?;
    w.write_all(&cbor).await?;
    w.write_u32(raw.len() as u32).await?;
    if !raw.is_empty() {
        w.write_all(raw).await?;
    }
    Ok(())
}

/// Reads one frame written by `write_frame`. Both length prefixes are
/// checked against `MAX_FRAME_BYTES` BEFORE the corresponding buffer is
/// allocated, so a malicious/corrupt length prefix can never itself be
/// used to force a large allocation — the read simply fails fast with
/// `FrameTooLarge` instead of `read_exact`-ing (or trying to) an
/// attacker-controlled number of bytes.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<ReplFrame, ReplCodecError> {
    let kind = r.read_u8().await?;
    let cbor_len = r.read_u32().await? as usize;
    if cbor_len > MAX_FRAME_BYTES {
        return Err(ReplCodecError::FrameTooLarge { len: cbor_len });
    }
    let mut cbor_buf = vec![0u8; cbor_len];
    r.read_exact(&mut cbor_buf).await?;

    let raw_len = r.read_u32().await? as usize;
    if raw_len > MAX_FRAME_BYTES || cbor_len.saturating_add(raw_len) > MAX_FRAME_BYTES {
        return Err(ReplCodecError::FrameTooLarge {
            len: cbor_len + raw_len,
        });
    }
    let mut raw_buf = vec![0u8; raw_len];
    if raw_len > 0 {
        r.read_exact(&mut raw_buf).await?;
    }

    Ok(match kind {
        KIND_HELLO => ReplFrame::Hello(decode_cbor(&cbor_buf)?),
        KIND_HELLO_ACK => ReplFrame::HelloAck(decode_cbor(&cbor_buf)?),
        KIND_BATCH => ReplFrame::Batch {
            header: decode_cbor(&cbor_buf)?,
            bytes: Bytes::from(raw_buf),
        },
        KIND_ACK => ReplFrame::Ack(decode_cbor(&cbor_buf)?),
        KIND_HEARTBEAT => ReplFrame::Heartbeat(decode_cbor(&cbor_buf)?),
        KIND_TRUNCATE => ReplFrame::Truncate(decode_cbor(&cbor_buf)?),
        KIND_LEO_QUERY => ReplFrame::LeoQuery(decode_cbor(&cbor_buf)?),
        KIND_LEO_REPLY => ReplFrame::LeoReply(decode_cbor(&cbor_buf)?),
        KIND_OFFSETS => ReplFrame::Offsets(decode_cbor(&cbor_buf)?),
        other => return Err(ReplCodecError::UnknownKind(other)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> ReplHello {
        ReplHello {
            org_id: "org-1".into(),
            topic: "orders".into(),
            partition: 3,
            leader_node_id: "node-a".into(),
            leader_epoch: 7,
            replicas: vec!["node-a".into(), "node-b".into(), "node-c".into()],
            environment: NodeEnvironment::Prod,
        }
    }

    fn hello_ack(reject: Option<ReplReject>) -> ReplHelloAck {
        ReplHelloAck {
            accepted: reject.is_none(),
            follower_leo: 100,
            follower_hw: 90,
            follower_epoch: 7,
            environment: NodeEnvironment::Prod,
            reject,
        }
    }

    fn batch_frame() -> ReplFrame {
        ReplFrame::Batch {
            header: ReplBatchHeader {
                leader_epoch: 7,
                base_offset: 42,
                hw: 40,
                batch_len: 5,
                producer: Some(ReplProducerMark {
                    producer_id: "p-1".into(),
                    epoch: 2,
                    base_offset: 42,
                    base_seq: 100,
                }),
                dedup_keys: vec![],
            },
            bytes: Bytes::from_static(b"hello"),
        }
    }

    async fn roundtrip(frame: ReplFrame) -> ReplFrame {
        let (mut client, mut server) = tokio::io::duplex(64 * 1024);
        write_frame(&mut client, &frame).await.expect("write");
        drop(client); // half-close is irrelevant here; read_frame only needs its own bytes
        read_frame(&mut server).await.expect("read")
    }

    #[tokio::test]
    async fn round_trips_every_frame_kind() {
        assert_eq!(
            roundtrip(ReplFrame::Hello(hello())).await,
            ReplFrame::Hello(hello())
        );
        assert_eq!(
            roundtrip(ReplFrame::HelloAck(hello_ack(None))).await,
            ReplFrame::HelloAck(hello_ack(None))
        );
        assert_eq!(
            roundtrip(ReplFrame::HelloAck(hello_ack(Some(
                ReplReject::StaleEpoch { have: 9 }
            ))))
            .await,
            ReplFrame::HelloAck(hello_ack(Some(ReplReject::StaleEpoch { have: 9 })))
        );
        assert_eq!(
            roundtrip(ReplFrame::HelloAck(hello_ack(Some(
                ReplReject::EnvironmentMismatch {
                    theirs: NodeEnvironment::Test,
                    ours: NodeEnvironment::Prod,
                }
            ))))
            .await,
            ReplFrame::HelloAck(hello_ack(Some(ReplReject::EnvironmentMismatch {
                theirs: NodeEnvironment::Test,
                ours: NodeEnvironment::Prod,
            })))
        );
        assert_eq!(
            roundtrip(ReplFrame::HelloAck(hello_ack(Some(
                ReplReject::NotAReplica
            ))))
            .await,
            ReplFrame::HelloAck(hello_ack(Some(ReplReject::NotAReplica)))
        );
        assert_eq!(
            roundtrip(ReplFrame::HelloAck(hello_ack(Some(
                ReplReject::TopicUnknown
            ))))
            .await,
            ReplFrame::HelloAck(hello_ack(Some(ReplReject::TopicUnknown)))
        );
        assert_eq!(
            roundtrip(ReplFrame::HelloAck(hello_ack(Some(ReplReject::Detached)))).await,
            ReplFrame::HelloAck(hello_ack(Some(ReplReject::Detached)))
        );

        let bf = batch_frame();
        let got = roundtrip(bf.clone()).await;
        assert_eq!(got, bf);
        // Raw batch bytes must survive byte-for-byte, not just Eq (a
        // future PartialEq bug that ignored `bytes` would make the
        // assertion above pass without this).
        match got {
            ReplFrame::Batch { bytes, .. } => assert_eq!(bytes.as_ref(), b"hello"),
            _ => panic!("expected Batch"),
        }

        let ack = ReplFrame::Ack(ReplAck {
            leader_epoch: 7,
            follower_leo: 100,
            follower_hw: 90,
        });
        assert_eq!(roundtrip(ack.clone()).await, ack);

        let hb = ReplFrame::Heartbeat(ReplHeartbeat {
            leader_epoch: 7,
            hw: 90,
            leader_leo: 100,
        });
        assert_eq!(roundtrip(hb.clone()).await, hb);

        let tr = ReplFrame::Truncate(ReplTruncate {
            leader_epoch: 7,
            to_offset: 80,
        });
        assert_eq!(roundtrip(tr.clone()).await, tr);

        let lq = ReplFrame::LeoQuery(ReplLeoQuery {
            org_id: "org-1".into(),
            topic: "orders".into(),
            partition: 3,
            known_epoch: 6,
        });
        assert_eq!(roundtrip(lq.clone()).await, lq);

        let lr = ReplFrame::LeoReply(ReplLeoReply {
            leo: 100,
            hw: 90,
            leader_epoch: 7,
            in_isr: true,
        });
        assert_eq!(roundtrip(lr.clone()).await, lr);

        let off = ReplFrame::Offsets(ReplOffsets {
            leader_epoch: 7,
            commits: vec![("grp-a".into(), 3, 55, 1)],
            discarded: vec![(3, 12)],
        });
        assert_eq!(roundtrip(off.clone()).await, off);
    }

    #[tokio::test]
    async fn truncated_stream_is_a_read_error_not_a_hang() {
        let (mut client, mut server) = tokio::io::duplex(4 * 1024);
        write_frame(
            &mut client,
            &ReplFrame::Ack(ReplAck {
                leader_epoch: 1,
                follower_leo: 1,
                follower_hw: 1,
            }),
        )
        .await
        .unwrap();
        drop(client);

        // Consume only the kind byte, then close — read_frame must fail,
        // not hang, on the truncated cbor_len prefix.
        let mut one_byte = [0u8; 1];
        server.read_exact(&mut one_byte).await.unwrap();
        let err = read_frame(&mut server).await.unwrap_err();
        assert!(matches!(err, ReplCodecError::Io(_)));
    }

    #[tokio::test]
    async fn oversize_frame_is_rejected_by_the_writer() {
        let (mut client, _server) = tokio::io::duplex(64 * 1024);
        let huge = Bytes::from(vec![0u8; MAX_FRAME_BYTES + 1]);
        let frame = ReplFrame::Batch {
            header: ReplBatchHeader {
                leader_epoch: 1,
                base_offset: 0,
                hw: 0,
                batch_len: huge.len() as u32,
                producer: None,
                dedup_keys: vec![],
            },
            bytes: huge,
        };
        let err = write_frame(&mut client, &frame).await.unwrap_err();
        assert!(matches!(err, ReplCodecError::FrameTooLarge { .. }));
    }

    #[tokio::test]
    async fn oversize_length_prefix_is_rejected_by_the_reader_without_allocating() {
        let (mut client, mut server) = tokio::io::duplex(4 * 1024);
        client.write_u8(KIND_ACK).await.unwrap();
        // Declares a cbor_len far beyond MAX_FRAME_BYTES; a correct reader
        // must reject this from the length prefix alone, never attempt to
        // `read_exact` that many bytes (which would hang forever on this
        // duplex — nothing more is ever written).
        client
            .write_u32((MAX_FRAME_BYTES as u32) + 1)
            .await
            .unwrap();
        drop(client);
        let err = read_frame(&mut server).await.unwrap_err();
        assert!(matches!(err, ReplCodecError::FrameTooLarge { .. }));
    }

    #[tokio::test]
    async fn unknown_kind_is_rejected() {
        let (mut client, mut server) = tokio::io::duplex(4 * 1024);
        client.write_u8(0xFF).await.unwrap();
        client.write_u32(0).await.unwrap();
        client.write_u32(0).await.unwrap();
        drop(client);
        let err = read_frame(&mut server).await.unwrap_err();
        assert!(matches!(err, ReplCodecError::UnknownKind(0xFF)));
    }

    #[tokio::test]
    async fn bad_raw_length_is_rejected() {
        let (mut client, mut server) = tokio::io::duplex(4 * 1024);
        client.write_u8(KIND_ACK).await.unwrap();
        let cbor = encode_cbor(&ReplAck {
            leader_epoch: 1,
            follower_leo: 1,
            follower_hw: 1,
        })
        .unwrap();
        client.write_u32(cbor.len() as u32).await.unwrap();
        client.write_all(&cbor).await.unwrap();
        // raw_len larger than MAX_FRAME_BYTES on its own.
        client
            .write_u32((MAX_FRAME_BYTES as u32) + 1)
            .await
            .unwrap();
        drop(client);
        let err = read_frame(&mut server).await.unwrap_err();
        assert!(matches!(err, ReplCodecError::FrameTooLarge { .. }));
    }
}
