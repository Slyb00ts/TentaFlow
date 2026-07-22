// ===== File: nvdec_downstream_probe.rs — does the REAL downstream negotiate? =====
//
// The pad probes proved that on the LIVE camera, depay+parse deliver valid
// byte-stream H.264 to nvh264dec and the decoder emits ZERO frames with no error.
// `nvdec_branch_probe` decoded 746 frames via mediamtx, but with a plain (AVC)
// parse and an RTSP front — so it cannot separate "downstream negotiation" from
// "live Axis RTP". This probe removes both variables: it feeds a RECORDED Axis
// clip (raw pre-decode H.264, byte-stream forced exactly like production) through
// the EXACT production tail — nvh264dec → cudadownload → sink — and counts frames
// at the decoder src AND at the download output.
//
//   CUDA_VISIBLE_DEVICES=7 cargo run --release --features camera \
//       --example nvdec_downstream_probe -- <clip.mp4>
//
// Zero frames past nvh264dec ⇒ the decoder/download negotiation is the fault and
// it is reproducible off-camera. Frames flowing ⇒ the tail is fine and the fault
// lives purely in the live Axis RTP feed.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;

const RUN_FOR: Duration = Duration::from_secs(20);

fn main() {
    let clip = std::env::args()
        .nth(1)
        .expect("usage: nvdec_downstream_probe <clip.mp4>");
    gst::init().expect("gst init");

    println!("== element registry ==");
    for n in ["nvh264dec", "cudadownload", "cudaconvert", "cudascale"] {
        let ok = gst::ElementFactory::find(n).is_some();
        println!("  {n:<14} {}", if ok { "present" } else { "MISSING" });
    }

    match probe(&clip) {
        Ok(()) => {}
        Err(e) => println!("\npipeline error: {e}"),
    }
}

fn probe(clip: &str) -> Result<(), String> {
    let pipeline = gst::Pipeline::new();
    let src = mk("filesrc")?;
    src.set_property("location", clip);
    let demux = mk("qtdemux")?;
    // Byte-stream + config-interval=-1, exactly like the production NVDEC branch.
    let parse = gst::ElementFactory::make("h264parse")
        .property_from_str("config-interval", "-1")
        .build()
        .map_err(|_| "h264parse".to_string())?;
    let dec = mk("nvh264dec")?;
    // Production NvdecNv12 tail: nvh264dec → cudadownload → (NV12 host) → sink.
    let download = mk("cudadownload")?;
    let sink = mk("fakesink")?;
    sink.set_property("sync", false);

    for e in [&src, &demux, &parse, &dec, &download, &sink] {
        pipeline.add(e).map_err(|e| e.to_string())?;
    }
    src.link(&demux).map_err(|e| e.to_string())?;

    // Force Annex-B byte-stream from parse into the decoder, as production does.
    let byte_stream = gst::Caps::builder("video/x-h264")
        .field("stream-format", "byte-stream")
        .field("alignment", "au")
        .build();
    parse
        .link_filtered(&dec, &byte_stream)
        .map_err(|e| format!("parse->dec byte-stream: {e}"))?;
    dec.link(&download).map_err(|e| e.to_string())?;
    download.link(&sink).map_err(|e| e.to_string())?;

    let parse_weak = parse.downgrade();
    demux.connect_pad_added(move |_, pad| {
        if let Some(parse) = parse_weak.upgrade() {
            if let Some(t) = parse.static_pad("sink") {
                if !t.is_linked() {
                    let _ = pad.link(&t);
                }
            }
        }
    });

    // Count buffers AND capture caps at the decoder src and the download src, so
    // we see whether nvh264dec emits and what NV12 memory type it produces.
    let started = Instant::now();
    let dec_n = count_pad(&dec, "nvdec", &started);
    let dl_n = count_pad(&download, "cudadownload", &started);

    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| e.to_string())?;

    let bus = pipeline.bus().ok_or("no bus")?;
    let deadline = Instant::now() + RUN_FOR;
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        if let Some(msg) =
            bus.timed_pop(gst::ClockTime::from_mseconds(left.as_millis().min(500) as u64))
        {
            match msg.view() {
                gst::MessageView::Eos(_) => break,
                gst::MessageView::Error(e) => {
                    println!("  bus ERROR: {} ({:?})", e.error(), e.debug());
                    break;
                }
                gst::MessageView::Warning(w) => {
                    let src = w.src().map(|s| s.name().to_string()).unwrap_or_default();
                    println!("  bus WARNING from {src}: {} ({:?})", w.error(), w.debug());
                }
                _ => {}
            }
        }
    }
    let _ = pipeline.set_state(gst::State::Null);

    let d = dec_n.load(Ordering::Relaxed);
    let dl = dl_n.load(Ordering::Relaxed);
    println!("\n== result ==");
    println!("  nvh264dec src frames = {d}{}", if d == 0 { "  <-- DECODER SILENT" } else { "" });
    println!("  cudadownload src frames = {dl}{}", if dl == 0 { "  <-- DOWNLOAD SILENT" } else { "" });
    Ok(())
}

fn count_pad(el: &gst::Element, label: &str, started: &Instant) -> Arc<AtomicU64> {
    let n = Arc::new(AtomicU64::new(0));
    let logged = Arc::new(Mutex::new(false));
    if let Some(pad) = el.static_pad("src") {
        let (nc, lc, st, lab) = (n.clone(), logged.clone(), *started, label.to_string());
        pad.add_probe(gst::PadProbeType::BUFFER, move |p, _| {
            if nc.fetch_add(1, Ordering::Relaxed) == 0 {
                let caps = p.current_caps().map(|c| c.to_string()).unwrap_or_default();
                println!(
                    "  [{lab}] first buffer @ {} ms  caps={}",
                    st.elapsed().as_millis(),
                    &caps[..caps.len().min(160)]
                );
                *lc.lock().unwrap() = true;
            }
            gst::PadProbeReturn::Ok
        });
    }
    n
}

fn mk(name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|_| format!("cannot create {name}"))
}
