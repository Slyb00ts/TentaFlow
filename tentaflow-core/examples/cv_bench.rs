// =============================================================================
// File: examples/cv_bench.rs — camera-CV pipeline latency + fleet-capacity bench
// =============================================================================
//
// Answers two operational questions for the ADR PoC:
//   1. How long does one frame's analysis really take? (detector + state + OCR)
//   2. How many cameras can one GPU sustain at a target FPS?
//
// The three ONNX graphs are fixed-shape, so timing is independent of pixel
// content — synthetic buffers give the same latency as real frames. The only
// content-dependent variable is "how many objects per frame", which drives the
// downstream state/OCR cost; it is a CLI parameter (`--dets`, `--plates`).
//
// Run (CPU EP):  cargo run --release --features inference-vision-gpu --example cv_bench
// Run (CUDA EP): build ort with the `cuda` feature, then same command.
//
// Flags:
//   --frame WxH     synthetic source frame size (default 1920x1080)
//   --iters N       timed iterations per measurement (default 50)
//   --warmup N      warmup iterations (default 10)
//   --dets D        assumed objects/frame for the fleet model (default 3)
//   --plates P      assumed plates/frame (subset of D) for the fleet model (default 1)
//   --fps a,b,c     target per-camera FPS values for the capacity table

#![cfg(feature = "inference-vision-gpu")]

use std::time::Instant;

use tentaflow_core::vision::classifier_stan::StateClassifier;
use tentaflow_core::vision::detector_rfdetr::RfDetrDetector;
use tentaflow_core::vision::ocr_plate::PlateOcr;

struct Args {
    fw: u32,
    fh: u32,
    iters: u32,
    warmup: u32,
    dets: u32,
    plates: u32,
    fps: Vec<f64>,
    batches: Vec<usize>,
}

fn parse_args() -> Args {
    let mut a = Args {
        fw: 1920,
        fh: 1080,
        iters: 50,
        warmup: 10,
        dets: 3,
        plates: 1,
        fps: vec![1.0, 5.0, 10.0, 15.0, 25.0, 30.0],
        batches: vec![1, 2, 4, 8, 16, 32],
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--frame" => {
                if let Some(v) = it.next() {
                    if let Some((w, h)) = v.split_once('x') {
                        a.fw = w.parse().unwrap_or(a.fw);
                        a.fh = h.parse().unwrap_or(a.fh);
                    }
                }
            }
            "--iters" => a.iters = it.next().and_then(|v| v.parse().ok()).unwrap_or(a.iters),
            "--warmup" => a.warmup = it.next().and_then(|v| v.parse().ok()).unwrap_or(a.warmup),
            "--dets" => a.dets = it.next().and_then(|v| v.parse().ok()).unwrap_or(a.dets),
            "--plates" => a.plates = it.next().and_then(|v| v.parse().ok()).unwrap_or(a.plates),
            "--fps" => {
                if let Some(v) = it.next() {
                    a.fps = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                }
            }
            "--batches" => {
                if let Some(v) = it.next() {
                    a.batches = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                }
            }
            other => eprintln!("ignoring unknown flag: {other}"),
        }
    }
    a
}

/// Median of timed closure over `iters` runs, in milliseconds.
fn time_ms<F: FnMut()>(warmup: u32, iters: u32, mut f: F) -> (f64, f64, f64) {
    for _ in 0..warmup {
        f();
    }
    let mut samples = Vec::with_capacity(iters as usize);
    for _ in 0..iters {
        let t = Instant::now();
        f();
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = samples[samples.len() / 2];
    let min = samples[0];
    let max = samples[samples.len() - 1];
    (median, min, max)
}

fn main() -> anyhow::Result<()> {
    let args = parse_args();

    println!("== camera-CV bench ==");
    println!(
        "frame {}x{}  iters {}  warmup {}  assumed dets/frame {}  plates/frame {}",
        args.fw, args.fh, args.iters, args.warmup, args.dets, args.plates
    );

    let detector = RfDetrDetector::load()?;
    let classifier = StateClassifier::load()?;
    let ocr = PlateOcr::load()?;
    println!("models loaded.\n");

    // Synthetic source frame + crops (content-independent timing).
    let frame = vec![128u8; (args.fw * args.fh * 3) as usize];
    let state_crop = vec![128u8; (96 * 96 * 3) as usize];
    let plate_crop = vec![128u8; (48 * 120 * 3) as usize];

    // --- Detector: single-image and batch scaling ---
    println!("-- detector (RF-DETR 560x560) --");
    println!(
        "{:>6}  {:>10}  {:>10}  {:>12}",
        "batch", "ms/batch", "ms/img", "img/s"
    );
    let mut detect_img_per_s_by_batch: Vec<(usize, f64)> = Vec::new();
    for &n in &args.batches {
        let frames: Vec<(&[u8], u32, u32)> =
            (0..n).map(|_| (frame.as_slice(), args.fw, args.fh)).collect();
        // One probe call: if it fails (e.g. CUDA OOM at large batch), skip this
        // batch size and stop growing rather than aborting the whole bench.
        if let Err(e) = detector.detect_batch(&frames, None) {
            println!("{:>6}  (skipped: {})", n, e);
            break;
        }
        let iters = if n >= 16 { args.iters / 2 + 1 } else { args.iters };
        let (med, _min, _max) = time_ms(args.warmup.max(2), iters.max(3), || {
            detector.detect_batch(&frames, None).expect("detect_batch");
        });
        let ms_img = med / n as f64;
        let img_s = 1000.0 / ms_img;
        detect_img_per_s_by_batch.push((n, img_s));
        println!("{:>6}  {:>10.2}  {:>10.2}  {:>12.1}", n, med, ms_img, img_s);
    }

    // --- State classifier (per crop) ---
    println!("\n-- state classifier (MobileNetV4 160px, per crop) --");
    let (st_med, st_min, st_max) = time_ms(args.warmup, args.iters, || {
        classifier.classify(&state_crop, 96, 96).expect("classify");
    });
    println!(
        "ms/crop  median {:.3}  min {:.3}  max {:.3}  ({:.0} crops/s)",
        st_med,
        st_min,
        st_max,
        1000.0 / st_med
    );

    // --- Plate OCR (per crop) ---
    println!("\n-- plate OCR (fast-plate 70x140, per crop) --");
    let (ocr_med, ocr_min, ocr_max) = time_ms(args.warmup, args.iters, || {
        ocr.read(&plate_crop, 120, 48).expect("read");
    });
    println!(
        "ms/crop  median {:.3}  min {:.3}  max {:.3}  ({:.0} crops/s)",
        ocr_med,
        ocr_min,
        ocr_max,
        1000.0 / ocr_med
    );

    // --- Fleet capacity model ---
    // Per-frame serial-equivalent cost = amortized detect (at best batch) +
    // dets * state + plates * ocr. Sustained capacity = 1000 / cost_ms frames/s,
    // shared across cameras → cameras = capacity / fps.
    let best_detect = detect_img_per_s_by_batch
        .iter()
        .cloned()
        .fold((1usize, 0.0f64), |acc, x| if x.1 > acc.1 { x } else { acc });
    let detect_ms_img = 1000.0 / best_detect.1;
    let downstream_ms = args.dets as f64 * st_med + args.plates as f64 * ocr_med;
    let per_frame_ms = detect_ms_img + downstream_ms;
    let capacity_fps = 1000.0 / per_frame_ms;

    println!("\n== fleet capacity (one engine, this EP) ==");
    println!(
        "best detect batch = {} ({:.1} img/s, {:.2} ms/img)",
        best_detect.0, best_detect.1, detect_ms_img
    );
    println!(
        "per-frame cost = detect {:.2} + {}*state {:.2} + {}*ocr {:.2} = {:.2} ms",
        detect_ms_img, args.dets, args.dets as f64 * st_med, args.plates, args.plates as f64 * ocr_med, per_frame_ms
    );
    println!("sustained capacity = {:.0} analyzed frames/s\n", capacity_fps);
    println!("{:>10}  {:>14}", "per-cam fps", "max cameras");
    for &f in &args.fps {
        if f <= 0.0 {
            continue;
        }
        println!("{:>10.0}  {:>14.1}", f, capacity_fps / f);
    }
    println!(
        "\nnote: detect-only (no downstream) at {} fps → {:.1} cameras",
        25,
        best_detect.1 / 25.0
    );

    Ok(())
}
