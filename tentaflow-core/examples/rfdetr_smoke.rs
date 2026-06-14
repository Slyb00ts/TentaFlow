// Smoke test: full per-frame CV pipeline (RF-DETR detect + state classifier on crops).
#[cfg(feature = "inference-vision-gpu")]
fn main() -> anyhow::Result<()> {
    use tentaflow_core::vision::detector_rfdetr::RfDetrDetector;
    use tentaflow_core::vision::classifier_stan::StateClassifier;
    let path = std::env::args().nth(1).expect("usage: rfdetr_smoke <image>");
    let img = image::open(&path)?.to_rgb8();
    let (w, h) = (img.width(), img.height());
    let rgb = img.as_raw();
    let mut det = RfDetrDetector::load()?;
    let mut stan = StateClassifier::load()?;
    let dets = det.detect(rgb, w, h)?;
    println!("{}  {}x{}  {} detections:", path, w, h, dets.len());
    for d in &dets {
        let wants = d.klasa.starts_with("nalepka") || d.klasa=="znak_srodowiskowy" || d.klasa=="termometr";
        let mut stany = vec![];
        if wants {
            let x0=(d.bbox[0]*w as f32).round().max(0.0) as u32;
            let y0=(d.bbox[1]*h as f32).round().max(0.0) as u32;
            let cw=((d.bbox[2]*w as f32).round() as u32).min(w.saturating_sub(x0));
            let ch=((d.bbox[3]*h as f32).round() as u32).min(h.saturating_sub(y0));
            if cw>=8 && ch>=8 {
                let mut crop=Vec::with_capacity((cw*ch*3) as usize);
                for yy in y0..y0+ch { let off=((yy*w+x0)*3) as usize; crop.extend_from_slice(&rgb[off..off+(cw*3) as usize]); }
                stany = stan.classify(&crop, cw, ch)?;
            }
        }
        println!("  {:22} {:.3}  stan={:?}", d.klasa, d.score, stany);
    }
    Ok(())
}
#[cfg(not(feature = "inference-vision-gpu"))]
fn main() {}
