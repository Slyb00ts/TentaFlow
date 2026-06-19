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
/// rejects a header whose version it does not understand.
pub const LIDAR_FRAME_VERSION: u8 = 1;

/// Layout tag: 3 little-endian f32 per point — interleaved `[x, y, z]` (meters).
pub const LIDAR_LAYOUT_XYZ: u8 = 3;
/// Layout tag: 4 little-endian f32 per point — interleaved `[x, y, z, intensity]`.
pub const LIDAR_LAYOUT_XYZI: u8 = 4;

/// Total size in bytes of the fixed frame header. The body (packed f32 points)
/// begins at exactly this offset.
///
/// Byte layout (all multi-byte fields little-endian):
/// ```text
///   offset  size  field
///        0     1  version        u8   (== LIDAR_FRAME_VERSION)
///        1     1  layout         u8   (LIDAR_LAYOUT_XYZ=3 | LIDAR_LAYOUT_XYZI=4)
///        2     2  _reserved      u16  (must be 0; keeps point_count 4-aligned)
///        4     4  point_count    u32
///        8     4  frame_seq      u32
///       12     8  timestamp_us   i64
///       20     4  resolution     f32  (meters per voxel/unit; informational)
///       24     4  origin_x       f32
///       28     4  origin_y       f32
///       32     4  origin_z       f32
///       36          <-- body starts here (point_count * layout f32, interleaved)
/// ```
pub const LIDAR_HEADER_LEN: usize = 36;

/// Parsed/serializable view of the fixed frame header. `layout` is the f32
/// stride per point (3 or 4); `point_count * layout * 4` body bytes follow.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LidarFrameHeader {
    /// Format version; encode always writes `LIDAR_FRAME_VERSION`.
    pub version: u8,
    /// f32 components per point: `LIDAR_LAYOUT_XYZ` or `LIDAR_LAYOUT_XYZI`.
    pub layout: u8,
    /// Number of points packed in the body.
    pub point_count: u32,
    /// Monotonic per-session frame counter (renderer freshness / drop detection).
    pub frame_seq: u32,
    /// Capture wall-clock time in microseconds since the Unix epoch.
    pub timestamp_us: i64,
    /// Source grid resolution in meters (informational; points are already in meters).
    pub resolution: f32,
    /// Frame origin in meters `[x, y, z]` (informational; points already include it).
    pub origin: [f32; 3],
}

impl LidarFrameHeader {
    /// f32 components per point for this header's layout (3 or 4). Returns 0 for
    /// an unknown layout tag (the decoder rejects such headers before this).
    #[inline]
    pub const fn stride(&self) -> usize {
        match self.layout {
            LIDAR_LAYOUT_XYZ => 3,
            LIDAR_LAYOUT_XYZI => 4,
            _ => 0,
        }
    }

    /// Exact body length in bytes for this header: `point_count * stride * 4`.
    /// `None` if the layout is unknown or the product overflows `usize`.
    #[inline]
    pub fn body_len(&self) -> Option<usize> {
        let stride = self.stride();
        if stride == 0 {
            return None;
        }
        (self.point_count as usize)
            .checked_mul(stride)
            .and_then(|n| n.checked_mul(4))
    }

    /// Total frame size in bytes (`LIDAR_HEADER_LEN + body_len`).
    #[inline]
    pub fn frame_len(&self) -> Option<usize> {
        self.body_len().and_then(|b| b.checked_add(LIDAR_HEADER_LEN))
    }

    /// Serialize the header to its fixed little-endian byte layout.
    pub fn encode_header(&self) -> [u8; LIDAR_HEADER_LEN] {
        let mut buf = [0u8; LIDAR_HEADER_LEN];
        buf[0] = self.version;
        buf[1] = self.layout;
        // bytes 2..4 reserved, already zero.
        buf[4..8].copy_from_slice(&self.point_count.to_le_bytes());
        buf[8..12].copy_from_slice(&self.frame_seq.to_le_bytes());
        buf[12..20].copy_from_slice(&self.timestamp_us.to_le_bytes());
        buf[20..24].copy_from_slice(&self.resolution.to_le_bytes());
        buf[24..28].copy_from_slice(&self.origin[0].to_le_bytes());
        buf[28..32].copy_from_slice(&self.origin[1].to_le_bytes());
        buf[32..36].copy_from_slice(&self.origin[2].to_le_bytes());
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
        if layout != LIDAR_LAYOUT_XYZ && layout != LIDAR_LAYOUT_XYZI {
            return None;
        }
        if bytes[2] != 0 || bytes[3] != 0 {
            return None;
        }
        let point_count = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let frame_seq = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let timestamp_us = i64::from_le_bytes([
            bytes[12], bytes[13], bytes[14], bytes[15], bytes[16], bytes[17], bytes[18], bytes[19],
        ]);
        let resolution = f32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        let origin = [
            f32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
            f32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]),
            f32::from_le_bytes([bytes[32], bytes[33], bytes[34], bytes[35]]),
        ];
        Some(LidarFrameHeader {
            version,
            layout,
            point_count,
            frame_seq,
            timestamp_us,
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
            point_count,
            frame_seq: 7,
            timestamp_us: 1_700_000_000_000_123,
            resolution: 0.05,
            origin: [-1.5, 0.0, 2.25],
        }
    }

    #[test]
    fn lidar_header_len_is_fixed() {
        assert_eq!(LIDAR_HEADER_LEN, 36);
        assert_eq!(sample_header(0).encode_header().len(), LIDAR_HEADER_LEN);
    }

    #[test]
    fn lidar_header_round_trip() {
        let h = sample_header(42_000);
        let bytes = h.encode_header();
        let back = LidarFrameHeader::decode_header(&bytes).expect("decode");
        assert_eq!(back, h);
        assert_eq!(back.stride(), 3);
        assert_eq!(back.body_len(), Some(42_000 * 3 * 4));
        assert_eq!(back.frame_len(), Some(LIDAR_HEADER_LEN + 42_000 * 3 * 4));
    }

    #[test]
    fn lidar_header_rejects_bad_version_layout_reserved_short() {
        let h = sample_header(1);
        // Short buffer.
        assert!(LidarFrameHeader::decode_header(&[0u8; 10]).is_none());
        // Bad version.
        let mut b = h.encode_header();
        b[0] = 2;
        assert!(LidarFrameHeader::decode_header(&b).is_none());
        // Bad layout.
        let mut b = h.encode_header();
        b[1] = 9;
        assert!(LidarFrameHeader::decode_header(&b).is_none());
        // Non-zero reserved.
        let mut b = h.encode_header();
        b[2] = 1;
        assert!(LidarFrameHeader::decode_header(&b).is_none());
    }

    #[test]
    fn lidar_full_frame_builds_and_parses_back_xyz() {
        // Build a full canonical frame: header + N interleaved [x,y,z] f32 points.
        let points: [[f32; 3]; 3] = [
            [0.0, 1.0, 2.0],
            [-3.5, 4.25, 5.0],
            [10.0, -20.0, 30.5],
        ];
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
                let got = f32::from_le_bytes([
                    body[off],
                    body[off + 1],
                    body[off + 2],
                    body[off + 3],
                ]);
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
