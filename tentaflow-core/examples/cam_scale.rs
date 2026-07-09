// =============================================================================
// File: examples/cam_scale.rs — Stage 0: real concurrent multi-camera capacity
// =============================================================================
//
// Answers "how many cameras can ONE GPU sustain, RIGHT NOW" — not the serial
// latency-divided guess. Spawns N OS threads (one per simulated camera), each
// driving the FULL per-frame pipeline (detect + 3×state + 1×plate + 1×adr) at a
// target fps, ramps N, and reports whether the fleet keeps up (per-frame p99 <
// frame budget) plus achieved throughput and GPU utilization. The models'
// session pools (env `TENTAFLOW_VISION_{DETECTOR,STAN,PLATE,ADR}_SESSIONS`)
// bound real concurrency — run at pool=1 (today) and pool=8 to see headroom.
//
//   TENTAFLOW_VISION_GPUS=7 cargo run --release \
//     --features inference-vision-gpu,inference-supertonic --example cam_scale -- \
//     --levels 1,5,10,20,40,80,160 --secs 5 --fps 25
//   add --detect-only to measure detect-alone capacity (enrichment is event-driven).
#![cfg(feature = "inference-vision-gpu")]

use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tentaflow_core::vision::adr_ocr;
use tentaflow_core::vision::classifier_stan::StateClassifier;
use tentaflow_core::vision::detector_rfdetr::{RfDetrDetector, RESOLUTION};
// The cross-camera batcher lives on the ort/TRT path (crossbeam + `&self`
// concurrency-safe pools); a Burn-only build has no batcher, so `--batched`
// there falls back to the direct per-camera calls.
#[cfg(feature = "inference-supertonic")]
use tentaflow_core::services::detection_bus::Detection;
use tentaflow_core::vision::inference_batcher::{batch_window, InferenceBatcher, MAX_BATCH};
use tentaflow_core::vision::ocr_plate::PlateOcr;

struct Args {
    levels: Vec<usize>,
    secs: u64,
    fps: f64,
    detect_only: bool,
    dets: usize,
    /// Route state/plate crops through the cross-camera dynamic batcher instead
    /// of calling `classify_batch`/`read_batch` per camera — measures whether
    /// aggregation lifts GPU util and breaks the per-camera batch-1 plateau.
    batched: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        levels: vec![1, 5, 10, 20, 40, 80, 160],
        secs: 5,
        fps: 25.0,
        detect_only: false,
        dets: 3,
        batched: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        match k.as_str() {
            "--levels" => {
                if let Some(v) = it.next() {
                    a.levels = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                }
            }
            "--secs" => a.secs = it.next().and_then(|v| v.parse().ok()).unwrap_or(a.secs),
            "--fps" => a.fps = it.next().and_then(|v| v.parse().ok()).unwrap_or(a.fps),
            "--dets" => a.dets = it.next().and_then(|v| v.parse().ok()).unwrap_or(a.dets),
            "--detect-only" => a.detect_only = true,
            "--batched" => a.batched = true,
            _ => {}
        }
    }
    a
}

fn gpu_index() -> String {
    std::env::var("TENTAFLOW_VISION_GPUS")
        .ok()
        .and_then(|v| v.split([',', ' ']).next().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "0".to_string())
}

fn sample_gpu_util(gpu: &str) -> Option<u32> {
    let out = Command::new("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu",
            "--format=csv,noheader,nounits",
            "-i",
            gpu,
        ])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().ok()
}

fn pctl(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn main() -> anyhow::Result<()> {
    let args = parse_args();
    let gpu = gpu_index();
    let budget_ms = 1000.0 / args.fps;

    println!("loading models (GPU {gpu})...");
    let detector = Arc::new(RfDetrDetector::load()?);
    let classifier = Arc::new(StateClassifier::load()?);
    let ocr = Arc::new(PlateOcr::load()?);
    let adr = adr_ocr::get();

    // Cross-camera dynamic batchers over the SAME loaded models — in `--batched`
    // mode every simulated camera submits its crops here (from its own thread),
    // so crops from all cameras coalesce into one big forward per model.
    #[cfg(feature = "inference-supertonic")]
    let state_batcher = Arc::new(InferenceBatcher::<Vec<String>>::new(MAX_BATCH, batch_window(), {
        let c = classifier.clone();
        // GPU-resident state preprocess (crop+resize+normalize on GPU, device-tensor
        // input, no H2D) — measures whether removing the CPU preprocess for the
        // hottest stage lifts end-to-end throughput + GPU util past the CPU-bound plateau.
        Arc::new(move |crops: &[(Arc<[u8]>, u32, u32)]| {
            let refs: Vec<(&[u8], u32, u32)> =
                crops.iter().map(|(cr, w, h)| (cr.as_ref(), *w, *h)).collect();
            c.classify_batch_gpu(&refs)
        })
    }));
    #[cfg(feature = "inference-supertonic")]
    let plate_batcher = Arc::new(InferenceBatcher::<Option<String>>::new(MAX_BATCH, batch_window(), {
        let o = ocr.clone();
        Arc::new(move |crops: &[(Arc<[u8]>, u32, u32)]| o.read_batch(crops))
    }));
    // Detect batcher: aggregates FRAMES from many cameras into one detect_batch
    // forward. Without it every camera thread calls detect_batch(1) directly and 60
    // threads contend on the detector session pool (perf: 31% CPU in futex/kernel) +
    // fire tiny per-camera launches — this coalesces them into big cross-camera batches.
    #[cfg(feature = "inference-supertonic")]
    let detect_batcher = Arc::new(InferenceBatcher::<Vec<Detection>>::new(MAX_BATCH, batch_window(), {
        let d = detector.clone();
        Arc::new(move |frames: &[(Arc<[u8]>, u32, u32)]| {
            let refs: Vec<(&[u8], u32, u32)> =
                frames.iter().map(|(f, w, h)| (f.as_ref(), *w, *h)).collect();
            d.detect_batch(&refs, None)
        })
    }));
    // ADR batcher: a REAL cross-placard forward — `read_adr_batch` packs the
    // rows of every submitted placard into ONE tensor. (An earlier attempt that
    // looped `read_adr` per crop on the batcher worker only serialized the
    // placards and measured slower; the shared forward is the win, not routing.)
    #[cfg(feature = "inference-supertonic")]
    let adr_batcher = adr.clone().map(|a| {
        Arc::new(InferenceBatcher::<Option<(String, String)>>::new(
            MAX_BATCH,
            batch_window(),
            Arc::new(move |crops: &[(Arc<[u8]>, u32, u32)]| Ok(a.read_adr_batch(crops))),
        ))
    });
    // Frame as Arc for the detect batcher submit path (560×560×3 synthetic).
    let det_frame_arc: Arc<[u8]> =
        Arc::from(vec![128u8; (RESOLUTION * RESOLUTION * 3) as usize].into_boxed_slice());

    #[cfg(feature = "inference-supertonic")]
    let batched_note = if args.batched {
        format!(
            " [batched: state/plate/adr via cross-camera batcher, window {}µs, max {}]",
            batch_window().as_micros(),
            MAX_BATCH
        )
    } else {
        String::new()
    };
    #[cfg(not(feature = "inference-supertonic"))]
    let batched_note = if args.batched {
        " [--batched requested but built without inference-supertonic; using direct calls]"
            .to_string()
    } else {
        String::new()
    };

    println!(
        "loaded. per-frame work = detect(1) {} | fps target {} | budget {:.1} ms/frame | {} s/level\n",
        if args.detect_only {
            "[detect-only]".into()
        } else {
            format!("+ {}×state + 1×plate + 1×adr{}", args.dets, batched_note)
        },
        args.fps,
        budget_ms,
        args.secs
    );

    // Synthetic inputs. A 560×560 detect frame hits fill_frame's copy fast-path
    // (GPU-scaled-equivalent), so this measures GPU detect, not CPU resize.
    let det_frame = vec![128u8; (RESOLUTION * RESOLUTION * 3) as usize];
    let state_crops: Vec<(Arc<[u8]>, u32, u32)> = (0..args.dets)
        .map(|_| (Arc::from(vec![128u8; 96 * 96 * 3].into_boxed_slice()), 96u32, 96u32))
        .collect();
    let plate_crops: Vec<(Arc<[u8]>, u32, u32)> =
        vec![(Arc::from(vec![128u8; 48 * 120 * 3].into_boxed_slice()), 120u32, 48u32)];
    let adr_crop = Arc::<[u8]>::from(vec![128u8; 128 * 160 * 3].into_boxed_slice());

    // Warmup: builds TRT engines for each batch size actually used, so the first
    // ramped level is not paying engine-build cost.
    println!("warmup...");
    let _ = detector.detect_batch(&[(det_frame.as_slice(), RESOLUTION, RESOLUTION)], None);
    let _ = classifier.classify_batch(&state_crops);
    let _ = ocr.read_batch(&plate_crops);
    if let Some(a) = &adr {
        let _ = a.read_adr(&adr_crop, 128, 160);
    }
    println!("warmup done.\n");

    println!(
        "{:>5} {:>10} {:>10} {:>8} {:>8} {:>8} {:>7}  keeps-up",
        "cams", "target/s", "actual/s", "p50 ms", "p99 ms", "max ms", "gpu%"
    );

    for &n in &args.levels {
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicU64::new(0));
        let lat_bins = Arc::new(Mutex::new(Vec::<f64>::new()));

        // GPU util sampler.
        let util_samples = Arc::new(Mutex::new(Vec::<u32>::new()));
        let sampler = {
            let stop = stop.clone();
            let util_samples = util_samples.clone();
            let gpu = gpu.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    if let Some(u) = sample_gpu_util(&gpu) {
                        util_samples.lock().unwrap().push(u);
                    }
                    std::thread::sleep(Duration::from_millis(150));
                }
            })
        };

        let period = Duration::from_secs_f64(1.0 / args.fps);
        let mut workers = Vec::with_capacity(n);
        for _ in 0..n {
            let stop = stop.clone();
            let done = done.clone();
            let lat_bins = lat_bins.clone();
            let detector = detector.clone();
            let classifier = classifier.clone();
            let ocr = ocr.clone();
            let adr = adr.clone();
            #[cfg(feature = "inference-supertonic")]
            let state_batcher = state_batcher.clone();
            #[cfg(feature = "inference-supertonic")]
            let plate_batcher = plate_batcher.clone();
            #[cfg(feature = "inference-supertonic")]
            let detect_batcher = detect_batcher.clone();
            #[cfg(feature = "inference-supertonic")]
            let adr_batcher = adr_batcher.clone();
            #[cfg(feature = "inference-supertonic")]
            let det_frame_arc = det_frame_arc.clone();
            let det_frame = det_frame.clone();
            let state_crops = state_crops.clone();
            let plate_crops = plate_crops.clone();
            let adr_crop = adr_crop.clone();
            let detect_only = args.detect_only;
            #[cfg(feature = "inference-supertonic")]
            let batched = args.batched;
            workers.push(std::thread::spawn(move || {
                let mut local: Vec<f64> = Vec::new();
                let mut next = Instant::now();
                while !stop.load(Ordering::Relaxed) {
                    // Throttle to target fps: a real camera delivers one frame per
                    // period; if the work overruns, `next` falls behind and we stop
                    // sleeping — that surfaces as p99 > budget (can't keep up).
                    let now = Instant::now();
                    if now < next {
                        std::thread::sleep(next - now);
                    }
                    next += period;
                    let t0 = Instant::now();
                    // Detect: in --batched, submit the frame to the cross-camera
                    // detect batcher (coalesces many cameras' frames into one forward,
                    // kills the 60-thread pool contention); else direct per-camera.
                    #[cfg(feature = "inference-supertonic")]
                    let did_detect_batched = if batched {
                        let _ = detect_batcher.submit(det_frame_arc.clone(), RESOLUTION, RESOLUTION);
                        true
                    } else {
                        false
                    };
                    #[cfg(not(feature = "inference-supertonic"))]
                    let did_detect_batched = false;
                    if !did_detect_batched {
                        let _ =
                            detector.detect_batch(&[(det_frame.as_slice(), RESOLUTION, RESOLUTION)], None);
                    }
                    if !detect_only {
                        // On the ort/TRT build, `--batched` submits this camera's
                        // crops to the shared batchers; they coalesce with crops
                        // from every other camera thread into one big forward per
                        // model. A Burn-only build has no batcher, so it always
                        // takes the direct per-camera calls.
                        #[cfg(feature = "inference-supertonic")]
                        let did_batched = if batched {
                            let _ = state_batcher.submit_all(&state_crops);
                            let _ = plate_batcher.submit_all(&plate_crops);
                            true
                        } else {
                            false
                        };
                        #[cfg(not(feature = "inference-supertonic"))]
                        let did_batched = false;
                        if !did_batched {
                            let _ = classifier.classify_batch(&state_crops);
                            let _ = ocr.read_batch(&plate_crops);
                        }
                        // ADR in --batched goes through the cross-placard batcher:
                        // one `read_adr_batch` forward covers the rows of every
                        // camera's placard collected in the window.
                        #[cfg(feature = "inference-supertonic")]
                        let did_adr_batched = if batched {
                            if let Some(b) = &adr_batcher {
                                let _ = b.submit(adr_crop.clone(), 128, 160);
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        #[cfg(not(feature = "inference-supertonic"))]
                        let did_adr_batched = false;
                        if !did_adr_batched {
                            if let Some(a) = &adr {
                                let _ = a.read_adr(&adr_crop, 128, 160);
                            }
                        }
                    }
                    local.push(t0.elapsed().as_secs_f64() * 1000.0);
                    done.fetch_add(1, Ordering::Relaxed);
                }
                lat_bins.lock().unwrap().extend(local);
            }));
        }

        std::thread::sleep(Duration::from_secs(args.secs));
        stop.store(true, Ordering::Relaxed);
        for w in workers {
            let _ = w.join();
        }
        let _ = sampler.join();

        let total = done.load(Ordering::Relaxed);
        let actual = total as f64 / args.secs as f64;
        let target = n as f64 * args.fps;
        let mut lats = Arc::try_unwrap(lat_bins).unwrap().into_inner().unwrap();
        lats.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p50 = pctl(&lats, 0.50);
        let p99 = pctl(&lats, 0.99);
        let mx = lats.last().copied().unwrap_or(0.0);
        let utils = util_samples.lock().unwrap();
        let avg_util =
            if utils.is_empty() { 0 } else { utils.iter().sum::<u32>() / utils.len() as u32 };
        // "keeps up" = delivered ≥ 97% of the offered frames AND per-frame p99 fits
        // the budget (no camera silently dropping frames to appear on-time).
        let keeps = actual >= 0.97 * target && p99 <= budget_ms;
        println!(
            "{:>5} {:>10.0} {:>10.0} {:>8.1} {:>8.1} {:>8.1} {:>6}%  {}",
            n,
            target,
            actual,
            p50,
            p99,
            mx,
            avg_util,
            if keeps { "YES" } else { "NO (saturated)" }
        );
    }

    println!(
        "\nreal cameras/GPU (this pool config) = the LAST 'YES' row. Raise pools via\n\
         TENTAFLOW_VISION_DETECTOR_SESSIONS / _STAN_ / _PLATE_ / _ADR_SESSIONS to see headroom.\n\
         Enrichment is event-driven in production, so --detect-only is the more realistic ceiling."
    );
    Ok(())
}
