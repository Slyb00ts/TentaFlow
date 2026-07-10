// =============================================================================
// File: protocol/lidar.rs — canonical, vendor-agnostic LiDAR point-cloud frame
// Purpose: a FIXED little-endian binary layout for a single point-cloud frame.
// The bulk (points) is packed f32 — NOT CBOR — so a robot addon can emit tens
// of thousands of points per frame with one preallocated buffer and a single
// WASM->host copy, never touching JSON on the metered service tick. Every robot
// driver addon (Go2 today, others later) decodes its vendor-specific sensor
// format into THIS layout, so Core and the renderer stay vendor-agnostic.
//
// This module is dependency-free (only core slices + fixed arrays) so it also
// compiles into the addon wasm guest, which links this same crate.
// =============================================================================

/// Frame format version. Bump only on an incompatible layout change; a decoder
/// rejects a header whose version it does not understand. v2 grew the fixed header
/// from 36 to 44 bytes (`host_send_us` at offset 20) and added the packed-i16 grid
/// body layout, so a stale v1 decoder fails closed instead of misreading offsets.
pub const LIDAR_FRAME_VERSION: u8 = 2;

/// Layout tag: 3 little-endian f32 per point — interleaved `[x, y, z]` (meters).
pub const LIDAR_LAYOUT_XYZ: u8 = 3;
/// Layout tag: 4 little-endian f32 per point — interleaved `[x, y, z, intensity]`.
pub const LIDAR_LAYOUT_XYZI: u8 = 4;
// Tag 5 is RETIRED: it was a short-lived INTERLEAVED i16 variant `[ix,iy,iz,…]`.
// Planar got its own tag (6) so any leftover interleaved frame fails closed in the
// decoder instead of being misread as three planes. Do not reuse value 5.
//
/// Layout tag: i16 grid indices in PLANAR order — all `ix` (point_count × i16),
/// then all `iy`, then all `iz`. Reconstruct world meters as
/// `coord[k] = origin[k] + idx[k] * resolution`. Half the wire bytes of `XYZ`
/// (6 vs 12 B/point) and LOSSLESS for grid-aligned sources (voxel maps). Planar
/// (not interleaved) so each component plane is a long, low-entropy run — `iy`/`iz`
/// barely change along a scan row — which `LIDAR_FLAG_LZ4_BODY` then compresses far
/// better than interleaved `[ix,iy,iz,…]` (where fast-varying `ix` breaks the runs).
pub const LIDAR_LAYOUT_XYZ_I16_PLANAR: u8 = 6;

/// Flags bit: the body bytes are an LZ4 block (compress the body, NOT the header,
/// so the host can still stamp `host_send_us` and the decoder sizes the inflate
/// buffer from `point_count`+`layout`). Applied as a universal, LOSSLESS wire
/// compression on TOP of any layout (f32 or i16). The uncompressed body length is
/// always `body_len()`; the on-wire body is `bytes[LIDAR_HEADER_LEN..]`.
pub const LIDAR_FLAG_LZ4_BODY: u8 = 0x01;
/// Mask of all flag bits the current decoder understands. A header carrying any
/// bit outside this mask is rejected (fail closed, never misread).
pub const LIDAR_FLAGS_KNOWN: u8 = LIDAR_FLAG_LZ4_BODY;
/// Byte offset of the `flags` field inside the fixed header.
pub const LIDAR_FLAGS_OFFSET: usize = 2;

/// Total size in bytes of the fixed frame header. The body begins at exactly this
/// offset. When `LIDAR_FLAG_LZ4_BODY` is set the on-wire body is an LZ4 block of
/// that many decompressed bytes.
///
/// Byte layout (all multi-byte fields little-endian):
/// ```text
///   offset  size  field
///        0     1  version        u8   (== LIDAR_FRAME_VERSION)
///        1     1  layout         u8   (XYZ=3 | XYZI=4 | XYZ_I16_PLANAR=6; 5 retired)
///        2     1  flags          u8   (LIDAR_FLAG_LZ4_BODY=0x01)
///        3     1  _reserved      u8   (must be 0; keeps point_count 4-aligned)
///        4     4  point_count    u32
///        8     4  frame_seq      u32
///       12     8  timestamp_us   i64  (addon decode wall-clock µs)
///       20     8  host_send_us   i64  (host pump-send wall-clock µs; 0 until stamped)
///       28     4  resolution     f32  (meters per voxel/unit)
///       32     4  origin_x       f32
///       36     4  origin_y       f32
///       40     4  origin_z       f32
///       44          <-- body starts here (uncompressed: point_count * stride * comp_bytes)
/// ```
pub const LIDAR_HEADER_LEN: usize = 44;

/// Byte offset of the `host_send_us` i64 field inside the fixed header. The host
/// pump overwrites these 8 LE bytes in place just before broadcasting, so it must
/// not hardcode the offset — reuse this constant.
pub const LIDAR_HOST_SEND_US_OFFSET: usize = 20;

/// Parsed/serializable view of the fixed frame header. `layout` is the f32
/// stride per point (3 or 4); `point_count * layout * 4` body bytes follow.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LidarFrameHeader {
    /// Format version; encode always writes `LIDAR_FRAME_VERSION`.
    pub version: u8,
    /// f32 components per point: `LIDAR_LAYOUT_XYZ` or `LIDAR_LAYOUT_XYZI`.
    pub layout: u8,
    /// Bit flags (`LIDAR_FLAG_LZ4_BODY`). `0` for a plain uncompressed body.
    pub flags: u8,
    /// Number of points packed in the body.
    pub point_count: u32,
    /// Monotonic per-session frame counter (renderer freshness / drop detection).
    pub frame_seq: u32,
    /// Capture wall-clock time in microseconds since the Unix epoch.
    pub timestamp_us: i64,
    /// Host pump send wall-clock time in microseconds since the Unix epoch,
    /// stamped just before the frame is broadcast over the stream rails. `0` until
    /// the host stamps it (the addon writes 0; the renderer treats 0 as "unset").
    /// Lets the browser split end-to-end latency into addon→host vs host→browser.
    pub host_send_us: i64,
    /// Source grid resolution in meters (informational; points are already in meters).
    pub resolution: f32,
    /// Frame origin in meters `[x, y, z]` (informational; points already include it).
    pub origin: [f32; 3],
}

impl LidarFrameHeader {
    /// Scalar components per point for this header's layout (3 or 4). Returns 0 for
    /// an unknown layout tag (the decoder rejects such headers before this).
    #[inline]
    pub const fn stride(&self) -> usize {
        match self.layout {
            LIDAR_LAYOUT_XYZ | LIDAR_LAYOUT_XYZ_I16_PLANAR => 3,
            LIDAR_LAYOUT_XYZI => 4,
            _ => 0,
        }
    }

    /// Bytes per scalar component for this layout: f32 layouts = 4, packed-i16
    /// grid = 2. Returns 0 for an unknown layout tag.
    #[inline]
    pub const fn component_bytes(&self) -> usize {
        match self.layout {
            LIDAR_LAYOUT_XYZ_I16_PLANAR => 2,
            LIDAR_LAYOUT_XYZ | LIDAR_LAYOUT_XYZI => 4,
            _ => 0,
        }
    }

    /// `true` when the on-wire body is an LZ4 block (`body_len()` is the inflated
    /// size; the compressed bytes are everything after the fixed header).
    #[inline]
    pub const fn lz4_body(&self) -> bool {
        self.flags & LIDAR_FLAG_LZ4_BODY != 0
    }

    /// Exact UNCOMPRESSED body length in bytes for this header:
    /// `point_count * stride * component_bytes`. `None` if the layout is unknown
    /// or the product overflows `usize`. With `LIDAR_FLAG_LZ4_BODY` this is the
    /// inflate target size, not the on-wire byte count.
    #[inline]
    pub fn body_len(&self) -> Option<usize> {
        let stride = self.stride();
        let comp = self.component_bytes();
        if stride == 0 || comp == 0 {
            return None;
        }
        (self.point_count as usize)
            .checked_mul(stride)
            .and_then(|n| n.checked_mul(comp))
    }

    /// Total frame size in bytes (`LIDAR_HEADER_LEN + body_len`).
    #[inline]
    pub fn frame_len(&self) -> Option<usize> {
        self.body_len()
            .and_then(|b| b.checked_add(LIDAR_HEADER_LEN))
    }

    /// Serialize the header to its fixed little-endian byte layout.
    pub fn encode_header(&self) -> [u8; LIDAR_HEADER_LEN] {
        let mut buf = [0u8; LIDAR_HEADER_LEN];
        buf[0] = self.version;
        buf[1] = self.layout;
        buf[LIDAR_FLAGS_OFFSET] = self.flags;
        // byte 3 reserved, already zero.
        buf[4..8].copy_from_slice(&self.point_count.to_le_bytes());
        buf[8..12].copy_from_slice(&self.frame_seq.to_le_bytes());
        buf[12..20].copy_from_slice(&self.timestamp_us.to_le_bytes());
        buf[20..28].copy_from_slice(&self.host_send_us.to_le_bytes());
        buf[28..32].copy_from_slice(&self.resolution.to_le_bytes());
        buf[32..36].copy_from_slice(&self.origin[0].to_le_bytes());
        buf[36..40].copy_from_slice(&self.origin[1].to_le_bytes());
        buf[40..44].copy_from_slice(&self.origin[2].to_le_bytes());
        buf
    }

    /// Parse a header from the first `LIDAR_HEADER_LEN` bytes of `bytes`.
    /// Returns `None` if the slice is too short, the version is unknown, the
    /// reserved field is non-zero, or the layout tag is not recognized — a
    /// malformed header is rejected rather than interpreted loosely.
    pub fn decode_header(bytes: &[u8]) -> Option<LidarFrameHeader> {
        if bytes.len() < LIDAR_HEADER_LEN {
            return None;
        }
        let version = bytes[0];
        if version != LIDAR_FRAME_VERSION {
            return None;
        }
        let layout = bytes[1];
        if layout != LIDAR_LAYOUT_XYZ
            && layout != LIDAR_LAYOUT_XYZI
            && layout != LIDAR_LAYOUT_XYZ_I16_PLANAR
        {
            return None;
        }
        let flags = bytes[LIDAR_FLAGS_OFFSET];
        // Unknown flag bits or a non-zero reserved byte → reject (fail closed).
        if flags & !LIDAR_FLAGS_KNOWN != 0 || bytes[3] != 0 {
            return None;
        }
        let point_count = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let frame_seq = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let timestamp_us = i64::from_le_bytes([
            bytes[12], bytes[13], bytes[14], bytes[15], bytes[16], bytes[17], bytes[18], bytes[19],
        ]);
        let host_send_us = i64::from_le_bytes([
            bytes[20], bytes[21], bytes[22], bytes[23], bytes[24], bytes[25], bytes[26], bytes[27],
        ]);
        let resolution = f32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]);
        let origin = [
            f32::from_le_bytes([bytes[32], bytes[33], bytes[34], bytes[35]]),
            f32::from_le_bytes([bytes[36], bytes[37], bytes[38], bytes[39]]),
            f32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]),
        ];
        Some(LidarFrameHeader {
            version,
            layout,
            flags,
            point_count,
            frame_seq,
            timestamp_us,
            host_send_us,
            resolution,
            origin,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header(point_count: u32) -> LidarFrameHeader {
        LidarFrameHeader {
            version: LIDAR_FRAME_VERSION,
            layout: LIDAR_LAYOUT_XYZ,
            flags: 0,
            point_count,
            frame_seq: 7,
            timestamp_us: 1_700_000_000_000_123,
            host_send_us: 1_700_000_000_000_456,
            resolution: 0.05,
            origin: [-1.5, 0.0, 2.25],
        }
    }

    #[test]
    fn lidar_header_len_is_fixed() {
        assert_eq!(LIDAR_HEADER_LEN, 44);
        assert_eq!(LIDAR_HOST_SEND_US_OFFSET, 20);
        assert_eq!(sample_header(0).encode_header().len(), LIDAR_HEADER_LEN);
    }

    #[test]
    fn lidar_header_round_trip() {
        let h = sample_header(42_000);
        let bytes = h.encode_header();
        let back = LidarFrameHeader::decode_header(&bytes).expect("decode");
        assert_eq!(back, h);
        assert_eq!(back.host_send_us, 1_700_000_000_000_456);
        // The host_send_us i64 sits at its documented offset, little-endian.
        assert_eq!(
            i64::from_le_bytes(
                bytes[LIDAR_HOST_SEND_US_OFFSET..LIDAR_HOST_SEND_US_OFFSET + 8]
                    .try_into()
                    .unwrap()
            ),
            h.host_send_us,
        );
        assert_eq!(back.stride(), 3);
        assert_eq!(back.component_bytes(), 4);
        assert_eq!(back.body_len(), Some(42_000 * 3 * 4));
        assert_eq!(back.frame_len(), Some(LIDAR_HEADER_LEN + 42_000 * 3 * 4));
    }

    #[test]
    fn lidar_i16_grid_layout_sizing() {
        // Packed-i16 grid body is 6 bytes/point (3 components * 2 bytes), half the
        // f32 XYZ wire size, and the layout decodes back round-trip.
        let mut h = sample_header(10_000);
        h.layout = LIDAR_LAYOUT_XYZ_I16_PLANAR;
        let bytes = h.encode_header();
        let back = LidarFrameHeader::decode_header(&bytes).expect("decode i16");
        assert_eq!(back.layout, LIDAR_LAYOUT_XYZ_I16_PLANAR);
        assert_eq!(back.stride(), 3);
        assert_eq!(back.component_bytes(), 2);
        assert_eq!(back.body_len(), Some(10_000 * 3 * 2));
        assert_eq!(back.frame_len(), Some(LIDAR_HEADER_LEN + 10_000 * 3 * 2));
    }

    #[test]
    fn lidar_header_rejects_bad_version_layout_reserved_short() {
        let h = sample_header(1);
        // Short buffer.
        assert!(LidarFrameHeader::decode_header(&[0u8; 10]).is_none());
        // Bad version (unknown).
        let mut b = h.encode_header();
        b[0] = 99;
        assert!(LidarFrameHeader::decode_header(&b).is_none());
        // Bad layout.
        let mut b = h.encode_header();
        b[1] = 9;
        assert!(LidarFrameHeader::decode_header(&b).is_none());
        // Unknown flag bit (outside LIDAR_FLAGS_KNOWN).
        let mut b = h.encode_header();
        b[LIDAR_FLAGS_OFFSET] = 0x80;
        assert!(LidarFrameHeader::decode_header(&b).is_none());
        // A KNOWN flag (LZ4) is accepted and round-trips.
        let mut b = h.encode_header();
        b[LIDAR_FLAGS_OFFSET] = LIDAR_FLAG_LZ4_BODY;
        let back = LidarFrameHeader::decode_header(&b).expect("lz4 flag valid");
        assert!(back.lz4_body());
        // Non-zero reserved byte.
        let mut b = h.encode_header();
        b[3] = 1;
        assert!(LidarFrameHeader::decode_header(&b).is_none());
    }

    #[test]
    fn lidar_full_frame_builds_and_parses_back_xyz() {
        // Build a full canonical frame: header + N interleaved [x,y,z] f32 points.
        let points: [[f32; 3]; 3] = [[0.0, 1.0, 2.0], [-3.5, 4.25, 5.0], [10.0, -20.0, 30.5]];
        let h = sample_header(points.len() as u32);
        let body_len = h.body_len().unwrap();
        let mut frame = Vec::with_capacity(LIDAR_HEADER_LEN + body_len);
        frame.extend_from_slice(&h.encode_header());
        for p in &points {
            for c in p {
                frame.extend_from_slice(&c.to_le_bytes());
            }
        }
        assert_eq!(frame.len(), h.frame_len().unwrap());

        // Parse it back.
        let back = LidarFrameHeader::decode_header(&frame).expect("decode");
        assert_eq!(back.point_count as usize, points.len());
        let stride = back.stride();
        let body = &frame[LIDAR_HEADER_LEN..];
        assert_eq!(body.len(), back.body_len().unwrap());
        for (i, p) in points.iter().enumerate() {
            for (c, expect) in p.iter().enumerate() {
                let off = (i * stride + c) * 4;
                let got =
                    f32::from_le_bytes([body[off], body[off + 1], body[off + 2], body[off + 3]]);
                assert_eq!(got, *expect, "point {i} comp {c}");
            }
        }
    }

    #[test]
    fn lidar_body_len_xyzi_layout() {
        let mut h = sample_header(100);
        h.layout = LIDAR_LAYOUT_XYZI;
        assert_eq!(h.stride(), 4);
        assert_eq!(h.body_len(), Some(100 * 4 * 4));
    }
}
