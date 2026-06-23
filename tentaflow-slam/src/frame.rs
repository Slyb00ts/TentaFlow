// =============================================================================
// File: frame.rs — decode the canonical LiDAR frame into SLAM points (chunk 0e).
// Purpose: the bridge between the wire (tentaflow_sdk_spec::LidarFrameHeader, the
// EXACT bytes the Go2 addon publishes) and the SLAM front-end's `Point3<f32>`
// input. Mirrors the browser wasm decoder: reconstructs world XYZ from whichever
// body layout the header declares (f32 XYZ/XYZI or planar-i16 grid), inflating an
// LZ4 body if flagged. Keeps the engine vendor-agnostic — every source decodes to
// the same point list here.
// =============================================================================

use nalgebra::Point3;
use tentaflow_sdk_spec::{
    LidarFrameHeader, LIDAR_HEADER_LEN, LIDAR_LAYOUT_XYZ, LIDAR_LAYOUT_XYZI,
    LIDAR_LAYOUT_XYZ_I16_PLANAR,
};

/// Largest inflated body we will allocate from an (untrusted) header — bounds an
/// LZ4 amplification / bogus point_count. 64 MiB ≫ any real frame.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Decoded canonical frame: the point cloud + the capture timestamp.
#[derive(Debug, Clone)]
pub struct DecodedLidar {
    pub points: Vec<Point3<f32>>,
    pub timestamp_us: i64,
}

/// Decode canonical LiDAR frame bytes into world-space points. Returns `None` for a
/// malformed/short/over-large frame or a failed inflate (never a partial cloud).
pub fn decode_lidar_frame(bytes: &[u8]) -> Option<DecodedLidar> {
    let header = LidarFrameHeader::decode_header(bytes)?;
    let body_len = header.body_len()?;
    if body_len > MAX_BODY_BYTES {
        return None;
    }
    // Reject a header whose reconstruction scalars are non-finite — for the planar
    // layout these multiply into every point, so a NaN/Inf here would poison the map.
    if !header.resolution.is_finite() || header.origin.iter().any(|o| !o.is_finite()) {
        return None;
    }

    // Obtain the uncompressed body (inflate the LZ4 block if flagged).
    let inflated;
    let body: &[u8] = if header.lz4_body() {
        match lz4_flex::block::decompress(bytes.get(LIDAR_HEADER_LEN..)?, body_len) {
            Ok(d) if d.len() == body_len => {
                inflated = d;
                &inflated
            }
            _ => return None,
        }
    } else {
        bytes.get(LIDAR_HEADER_LEN..LIDAR_HEADER_LEN + body_len)?
    };

    let n = header.point_count as usize;
    let res = header.resolution;
    let [ox, oy, oz] = header.origin;
    let mut points = Vec::with_capacity(n);

    if header.layout == LIDAR_LAYOUT_XYZ_I16_PLANAR {
        // Planar grid indices: all ix, then iy, then iz. world = idx*res + origin.
        let iy_base = n * 2;
        let iz_base = n * 4;
        let rd = |o: usize| i16::from_le_bytes([body[o], body[o + 1]]) as f32;
        for p in 0..n {
            points.push(Point3::new(
                rd(p * 2) * res + ox,
                rd(iy_base + p * 2) * res + oy,
                rd(iz_base + p * 2) * res + oz,
            ));
        }
    } else if header.layout == LIDAR_LAYOUT_XYZ || header.layout == LIDAR_LAYOUT_XYZI {
        // f32 scalars, `stride` per point (3 or 4); take XYZ.
        let stride = header.stride();
        let rd = |o: usize| f32::from_le_bytes([body[o], body[o + 1], body[o + 2], body[o + 3]]);
        for p in 0..n {
            let off = p * stride * 4;
            points.push(Point3::new(rd(off), rd(off + 4), rd(off + 8)));
        }
    } else {
        return None;
    }

    // Fail closed on any non-finite point (malformed/hostile frame) rather than
    // folding NaN/Inf into the map.
    if points.iter().any(|p| !p.coords.iter().all(|c| c.is_finite())) {
        return None;
    }

    Some(DecodedLidar { points, timestamp_us: header.timestamp_us })
}
