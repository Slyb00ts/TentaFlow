// Smoke: full per-frame CV pipeline (RF-DETR detect + state on nalepki + plate OCR on registration).
#[cfg(feature = "inference-vision-gpu")]
fn main() -> anyhow::Result<()> {
    use tentaflow_core::vision::classifier_stan::StateClassifier;
    use tentaflow_core::vision::detector_rfdetr::RfDetrDetector;
    use tentaflow_core::vision::ocr_plate::PlateOcr;
    let path = std::env::args().nth(1).expect("usage: <image>");
    let img = image::open(&path)?.to_rgb8();
    let (w, h) = (img.width(), img.height());
    let rgb = img.as_raw();
    let mut det = RfDetrDetector::load()?;
    let mut stan = StateClassifier::load()?;
    let mut ocr = PlateOcr::load()?;
    let crop = |b: &[f32; 4]| -> Option<(Vec<u8>, u32, u32)> {
        let x0 = (b[0] * w as f32).round().max(0.0) as u32;
        let y0 = (b[1] * h as f32).round().max(0.0) as u32;
        let cw = ((b[2] * w as f32).round() as u32).min(w.saturating_sub(x0));
        let ch = ((b[3] * h as f32).round() as u32).min(h.saturating_sub(y0));
        if cw < 8 || ch < 8 {
            return None;
        }
        let mut c = Vec::with_capacity((cw * ch * 3) as usize);
        for yy in y0..y0 + ch {
            let o = ((yy * w + x0) * 3) as usize;
            c.extend_from_slice(&rgb[o..o + (cw * 3) as usize]);
        }
        Some((c, cw, ch))
    };
    let dets = det.detect(rgb, w, h)?;
    println!(
        "{}  {} detections:",
        path.rsplit('/').next().unwrap(),
        dets.len()
    );
    for d in &dets {
        let mut extra = String::new();
        if d.klasa.starts_with("nalepka")
            || d.klasa == "znak_srodowiskowy"
            || d.klasa == "termometr"
        {
            if let Some((c, cw, ch)) = crop(&d.bbox) {
                extra = format!("stan={:?}", stan.classify(&c, cw, ch)?);
            }
        } else if d.klasa == "tablica_rejestracyjna" {
            if let Some((c, cw, ch)) = crop(&d.bbox) {
                extra = format!("plate={:?}", ocr.read(&c, cw, ch)?);
            }
        }
        println!("  {:22} {:.3}  {}", d.klasa, d.score, extra);
    }
    Ok(())
}
#[cfg(not(feature = "inference-vision-gpu"))]
fn main() {}
