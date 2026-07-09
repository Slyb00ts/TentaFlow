// A/B harness: run a folder of plate crops through the plate OCR model with the
// current bilinear-stretch preprocessing vs. the new perspective-deskew path, on
// the SAME loaded model, and print the reads side by side so the accuracy delta
// is MEASURED, not assumed.
//
//   cargo run --example ocr_deskew_ab --features inference-vision-gpu,inference-supertonic -- <dir-of-crops>
//
// Feed it the raw crops dumped by `[vision] ocr_dump_dir` (the `*_raw.png`
// files are exactly the crop the model received) and/or any synthetic skewed
// plates. Only `*_raw*.png` / non-tensor PNGs are read (tensor/deskew dumps are
// skipped so a dump folder can be pointed at directly).
#[cfg(feature = "inference-vision-gpu")]
fn main() -> anyhow::Result<()> {
    use tentaflow_core::vision::ocr_plate::PlateOcr;

    let dir = std::env::args()
        .nth(1)
        .expect("usage: ocr_deskew_ab <dir-of-crop-pngs>");

    // Default vision settings: deskew ON, no dump dir — the harness drives both
    // arms explicitly via `read_ab`, so nothing else needs tuning.
    tentaflow_core::vision::settings::init(Default::default())?;

    let ocr = PlateOcr::load()?;

    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            matches!(ext, "png" | "jpg" | "jpeg")
                && !name.contains("_tensor")
                && !name.contains("_deskew")
        })
        .collect();
    files.sort();

    if files.is_empty() {
        println!("no crop images found in {dir}");
        return Ok(());
    }

    println!(
        "{:<34} {:>12} {:>6}   {:>12} {:>6}   {}",
        "file", "stretch", "conf", "deskew", "conf", "delta"
    );
    let (mut changed, mut gained, mut lost, mut same) = (0u32, 0u32, 0u32, 0u32);
    for path in &files {
        let img = match image::open(path) {
            Ok(i) => i.to_rgb8(),
            Err(e) => {
                println!("{}: decode error: {e}", path.display());
                continue;
            }
        };
        let (w, h) = (img.width(), img.height());
        let ((off, off_s), (on, on_s)) = ocr.read_ab(img.as_raw(), w, h)?;

        let off_str = off.clone().unwrap_or_else(|| "-".into());
        let on_str = on.clone().unwrap_or_else(|| "-".into());
        let delta = match (&off, &on) {
            (a, b) if a == b => {
                same += 1;
                ""
            }
            (None, Some(_)) => {
                gained += 1;
                changed += 1;
                "GAINED"
            }
            (Some(_), None) => {
                lost += 1;
                changed += 1;
                "LOST"
            }
            _ => {
                changed += 1;
                "CHANGED"
            }
        };
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        println!(
            "{:<34} {:>12} {:>6.2}   {:>12} {:>6.2}   {}",
            trunc(name, 34),
            off_str,
            off_s,
            on_str,
            on_s,
            delta
        );
    }

    println!(
        "\n{} files | same={same} changed={changed} (gained={gained} lost={lost})",
        files.len()
    );
    println!(
        "NOTE: 'gained' = deskew produced a valid read where stretch read nothing; \
         'lost' = the reverse (investigate — deskew must not regress frontal plates). \
         Ground-truth accuracy needs labelled captures from the live camera."
    );
    Ok(())
}

#[cfg(feature = "inference-vision-gpu")]
fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).collect::<String>() + "…"
    }
}

#[cfg(not(feature = "inference-vision-gpu"))]
fn main() {
    eprintln!("ocr_deskew_ab requires --features inference-vision-gpu");
}
