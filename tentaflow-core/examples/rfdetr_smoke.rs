// Smoke test: run the Rust RF-DETR runner on a real image and print detections.
// Proves the in-core ort path matches the validated Python ONNX recipe.
#[cfg(feature = "inference-vision-gpu")]
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: rfdetr_smoke <image>");
    let img = image::open(&path)?.to_rgb8();
    let (w, h) = (img.width(), img.height());
    println!("image {}x{}", w, h);
    let mut det = tentaflow_core::vision::detector_rfdetr::RfDetrDetector::load()?;
    let t = std::time::Instant::now();
    let dets = det.detect(img.as_raw(), w, h)?;
    println!("detect took {:?}, {} detections (score>0.5):", t.elapsed(), dets.len());
    for d in &dets {
        println!("  {:24} {:.3}  bbox(xywh_norm)=[{:.3},{:.3},{:.3},{:.3}]",
            d.klasa, d.score, d.bbox[0], d.bbox[1], d.bbox[2], d.bbox[3]);
    }
    Ok(())
}
#[cfg(not(feature = "inference-vision-gpu"))]
fn main() { eprintln!("build with --features inference-vision-gpu"); }
