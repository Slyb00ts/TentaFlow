// ===== File: nvdec_probe.rs — does NVDEC actually decode on this GPU? =====
//
// The ingest ladder demotes to software decode with "no frames within warmup
// window" and never recovers, so the question is whether `nvh264dec` can decode
// at all on this device. This probe answers it off the live camera: it decodes a
// recorded clip and counts frames that reach the sink, comparing the hardware
// decoder against the software one on the same file.
//
//   cargo run --release --features camera --example nvdec_probe -- <clip.mp4> [gpu_index]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;

const RUN_FOR: Duration = Duration::from_secs(15);

fn main() {
    let mut args = std::env::args().skip(1);
    let clip = args
        .next()
        .expect("usage: nvdec_probe <clip.mp4> [gpu_index]");
    let gpu: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(7);

    gst::init().expect("gst init");

    println!("== registered nvcodec elements ==");
    for name in [
        "nvh264dec",
        "nvh264sldec",
        "nvautogpuh264dec",
        "nvcudah264dec",
        "cudaconvert",
    ] {
        let found = gst::ElementFactory::find(name).is_some();
        println!("  {name:<18} {}", if found { "present" } else { "MISSING" });
    }

    for decoder in ["nvh264dec", "avdec_h264"] {
        if gst::ElementFactory::find(decoder).is_none() {
            println!("\n== {decoder}: factory missing, skipping ==");
            continue;
        }
        println!("\n== {decoder} on GPU {gpu} ==");
        match probe(&clip, decoder, gpu) {
            Ok((frames, first_ms)) => {
                let verdict = if frames == 0 {
                    "  <-- DECODES NOTHING"
                } else {
                    ""
                };
                println!(
                    "  frames={frames} first_frame_after={}{}",
                    first_ms
                        .map(|m| format!("{m} ms"))
                        .unwrap_or_else(|| "never".into()),
                    verdict
                );
            }
            Err(e) => println!("  pipeline error: {e}"),
        }
    }
}

/// Returns (frames reaching the sink, milliseconds until the first one).
fn probe(clip: &str, decoder: &str, gpu: u32) -> Result<(u64, Option<u128>), String> {
    let pipeline = gst::Pipeline::new();
    let src = mk("filesrc")?;
    src.set_property("location", clip);
    let demux = mk("qtdemux")?;
    let parse = mk("h264parse")?;
    let dec = mk(decoder)?;
    // `cuda-device-id` is READ-ONLY on this GStreamer build, so the decoder cannot
    // be steered at the element level — device selection has to happen outside the
    // process. Left here as a probe of that fact rather than a working knob.
    if decoder.starts_with("nv") {
        let writable = dec
            .find_property("cuda-device-id")
            .map(|p| p.flags().contains(gst::glib::ParamFlags::WRITABLE))
            .unwrap_or(false);
        println!(
            "  cuda-device-id writable: {} (requested gpu {gpu})",
            if writable {
                "yes"
            } else {
                "NO — decode lands on the process default device"
            }
        );
        if writable {
            dec.set_property("cuda-device-id", gpu as i32);
        }
    }
    let sink = mk("fakesink")?;
    sink.set_property("sync", false);

    let elements = [&src, &demux, &parse, &dec, &sink];
    for e in elements {
        pipeline.add(e).map_err(|e| e.to_string())?;
    }
    src.link(&demux).map_err(|e| e.to_string())?;
    parse.link(&dec).map_err(|e| e.to_string())?;
    dec.link(&sink).map_err(|e| e.to_string())?;

    // qtdemux pads appear only once the container is parsed.
    let parse_weak = parse.downgrade();
    demux.connect_pad_added(move |_, pad| {
        if let Some(parse) = parse_weak.upgrade() {
            if let Some(target) = parse.static_pad("sink") {
                if !target.is_linked() {
                    let _ = pad.link(&target);
                }
            }
        }
    });

    let frames = Arc::new(AtomicU64::new(0));
    let started = Instant::now();
    let first_at: Arc<std::sync::Mutex<Option<u128>>> = Arc::new(std::sync::Mutex::new(None));
    {
        let frames = frames.clone();
        let first_at = first_at.clone();
        let pad = sink.static_pad("sink").ok_or("fakesink has no sink pad")?;
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            if frames.fetch_add(1, Ordering::Relaxed) == 0 {
                *first_at.lock().unwrap() = Some(started.elapsed().as_millis());
            }
            gst::PadProbeReturn::Ok
        });
    }

    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| e.to_string())?;

    let bus = pipeline.bus().ok_or("no bus")?;
    let mut err = None;
    let deadline = Instant::now() + RUN_FOR;
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match bus.timed_pop(gst::ClockTime::from_mseconds(
            left.as_millis().min(500) as u64
        )) {
            Some(msg) => match msg.view() {
                gst::MessageView::Eos(_) => break,
                gst::MessageView::Error(e) => {
                    err = Some(format!("{} ({:?})", e.error(), e.debug()));
                    break;
                }
                _ => {}
            },
            None => {}
        }
    }
    let _ = pipeline.set_state(gst::State::Null);

    let count = frames.load(Ordering::Relaxed);
    // An error that still produced frames is reported as success with a note:
    // the question is whether pixels come out, not whether teardown was clean.
    if let Some(e) = err {
        if count == 0 {
            return Err(e);
        }
        println!("  (bus error after {count} frames: {e})");
    }
    let first = *first_at.lock().unwrap();
    Ok((count, first))
}

fn mk(name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|_| format!("cannot create element {name}"))
}
