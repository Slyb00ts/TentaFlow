// =============================================================================
// Plik: flow_engine/node_adapters/vision_crop.rs
// Opis: Wspólny helper kadrowania detekcji dla węzłów wizyjnych (vision_ocr /
//       vision_classify). Wycina ciasny RGB24 crop wg znormalizowanego bbox
//       detekcji, tak jak robi to hardcoded enrich path — jeden kod, jeden test.
// =============================================================================

/// A tightly-packed RGB24 crop plus its pixel dimensions.
pub struct RgbCrop {
    pub rgb: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Minimum crop edge (px). Sub-8px crops carry no readable plate/placard and
/// would only waste an inference call — skipped (matches the enrich path).
const MIN_CROP_EDGE: u32 = 8;

/// Cuts a crop from a full RGB24 frame for one detection's normalized bbox
/// (`[x, y, w, h]` in `0..=1`). Clamps to frame bounds; returns `None` when the
/// clamped crop is smaller than [`MIN_CROP_EDGE`] on either edge (nothing worth
/// running a model on). `frame` must be exactly `frame_w * frame_h * 3` bytes.
pub fn crop_detection(frame: &[u8], frame_w: u32, frame_h: u32, bbox: [f32; 4]) -> Option<RgbCrop> {
    let fw = frame_w as f32;
    let fh = frame_h as f32;
    let x0 = (bbox[0] * fw).round().clamp(0.0, fw) as u32;
    let y0 = (bbox[1] * fh).round().clamp(0.0, fh) as u32;
    let cw = (bbox[2] * fw).round().max(0.0) as u32;
    let ch = (bbox[3] * fh).round().max(0.0) as u32;
    let cw = cw.min(frame_w.saturating_sub(x0));
    let ch = ch.min(frame_h.saturating_sub(y0));
    if cw < MIN_CROP_EDGE || ch < MIN_CROP_EDGE {
        return None;
    }
    let stride = frame_w as usize * 3;
    let row_bytes = cw as usize * 3;
    let mut rgb = Vec::with_capacity(row_bytes * ch as usize);
    for row in 0..ch as usize {
        let start = (y0 as usize + row) * stride + x0 as usize * 3;
        rgb.extend_from_slice(&frame[start..start + row_bytes]);
    }
    Some(RgbCrop {
        rgb,
        width: cw,
        height: ch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_frame(w: u32, h: u32, val: u8) -> Vec<u8> {
        vec![val; (w * h * 3) as usize]
    }

    #[test]
    fn crops_to_clamped_pixel_box() {
        // 100x100 frame, bbox covering the top-left quarter.
        let frame = solid_frame(100, 100, 7);
        let c = crop_detection(&frame, 100, 100, [0.0, 0.0, 0.5, 0.5]).expect("crop");
        assert_eq!((c.width, c.height), (50, 50));
        assert_eq!(c.rgb.len(), 50 * 50 * 3);
        assert!(c.rgb.iter().all(|&b| b == 7));
    }

    #[test]
    fn clamps_overflowing_box_to_frame() {
        let frame = solid_frame(40, 40, 1);
        // x0=30 (0.75*40), requested w=0.5*40=20 but only 10px remain → cw=10.
        let c = crop_detection(&frame, 40, 40, [0.75, 0.0, 0.5, 0.25]).expect("crop");
        assert_eq!(c.width, 10);
        assert_eq!(c.height, 10);
        assert_eq!(c.rgb.len(), 10 * 10 * 3);
    }

    #[test]
    fn rejects_subminimal_crop() {
        let frame = solid_frame(100, 100, 0);
        // 0.05*100 = 5px < MIN_CROP_EDGE.
        assert!(crop_detection(&frame, 100, 100, [0.0, 0.0, 0.05, 0.05]).is_none());
    }

    #[test]
    fn extracts_correct_region_pixels() {
        // 20x10 frame: R channel = column index, so a crop's first column of R
        // values proves we extracted the right x-offset and stride.
        let (w, h) = (20u32, 10u32);
        let mut frame = vec![0u8; (w * h * 3) as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                frame[(y * w as usize + x) * 3] = x as u8;
            }
        }
        // bbox x=0.5 (col 10), full height, width 0.5 → cols 10..20 (10px wide).
        let c = crop_detection(&frame, w, h, [0.5, 0.0, 0.5, 1.0]).expect("crop");
        assert_eq!((c.width, c.height), (10, 10));
        // First pixel of the crop is column 10 → R == 10.
        assert_eq!(c.rgb[0], 10);
        // Last pixel of the first row is column 19 → R == 19.
        assert_eq!(c.rgb[9 * 3], 19);
    }
}
