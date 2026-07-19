// =============================================================================
// Plik: examples/rfdetr_diag.rs
// Opis: Diagnostyka wspolrzednych detekcji RUNTIME (.bpk + detector_rfdetr.rs).
//       Uruchamia detektor na statycznej klatce i wypisuje dla kazdej detekcji
//       bbox znormalizowany [x,y,w,h] oraz przeliczony na PIKSELE xyxy dla
//       oryginalnej rozdzielczosci klatki. Sluzy do porownania z ground-truth
//       PyTorch (model.predict).
// =============================================================================

#[cfg(feature = "inference-vision-gpu")]
fn main() -> anyhow::Result<()> {
    use tentaflow_core::vision::detector_rfdetr::RfDetrDetector;

    let path = std::env::args().nth(1).expect("usage: <image>");
    let img = image::open(&path)?.to_rgb8();
    let (w, h) = (img.width(), img.height());
    let rgb = img.as_raw();

    println!("== KLATKA ==");
    println!("plik: {path}");
    println!("rozmiar: {w}x{h}");
    println!();

    let mut det = RfDetrDetector::load()?;
    let dets = det.detect(rgb, w, h)?;

    println!("== RUNTIME DETEKCJE ({}) ==", dets.len());
    for d in &dets {
        // bbox znormalizowany [x, y, w, h] (x,y = lewy-gorny rog, znormalizowane 0..1)
        let [nx, ny, nw, nh] = d.bbox;

        // Przeliczenie na PIKSELE xyxy dla oryginalnej rozdzielczosci klatki.
        let x1 = nx * w as f32;
        let y1 = ny * h as f32;
        let x2 = (nx + nw) * w as f32;
        let y2 = (ny + nh) * h as f32;

        println!(
            "{:22} score={:.3}",
            d.klasa, d.score
        );
        println!(
            "   norm [x,y,w,h] = [{:.4}, {:.4}, {:.4}, {:.4}]",
            nx, ny, nw, nh
        );
        println!(
            "   px   xyxy      = [{:.1}, {:.1}, {:.1}, {:.1}]",
            x1, y1, x2, y2
        );
    }

    Ok(())
}

#[cfg(not(feature = "inference-vision-gpu"))]
fn main() {
    eprintln!("wymaga feature inference-vision-gpu");
}
