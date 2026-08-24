// ===== File: live_stream_probe.rs — analyse the LIVE Axis stream, no tentaflow =====
//
// Every off-camera reproduction (file, mediamtx, with/without DTS) decodes fine,
// so the fault is exclusively the live Axis RTP feed into nvh264dec. This probe
// connects DIRECTLY to the camera in ONE short session — outside the app — and
// feeds the live stream through the SAME hardware path plus a parallel software
// decode as a liveness control. It logs, per buffer entering the decoder, the
// PTS/DTS/flags/size, plus caps and any bus warning the decoder emits, so we see
// exactly what nvh264dec receives live and why it holds frames.
//
//   CUDA_VISIBLE_DEVICES=7 cargo run --release --features camera \
//       --example live_stream_probe -- <rtsp-url> <creds-hex>
//
// creds-hex = hex(credentials_encrypted) from the cameras row; decrypted here
// with the app's own cipher. Credentials are never printed.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app;

use tentaflow_core::services::camera_ingest::credentials::credentials_cipher;

const RUN_FOR: Duration = Duration::from_secs(20);

fn main() {
    let mut args = std::env::args().skip(1);
    let url = args
        .next()
        .expect("usage: live_stream_probe <rtsp-url> <creds-hex>");
    let creds_hex = args.next().expect("missing creds-hex");

    // Surface the decoder's own reasoning for THIS throwaway process only.
    std::env::set_var("GST_DEBUG", "nvdec:5,gstnvdec:5,nvh264dec:5");
    gst::init().expect("gst init");

    let blob = hex_to_bytes(&creds_hex).expect("bad creds hex");
    let creds = credentials_cipher()
        .decrypt(&blob)
        .expect("decrypt credentials (key at .runtime/keys/cameras.key?)");
    // creds = "user:password"; splice into the URL authority. Never printed.
    let auth_url = splice_credentials(&url, &creds);
    println!("connecting to {} (one session)", redact(&url));

    match probe(&auth_url) {
        Ok(()) => {}
        Err(e) => println!("pipeline error: {e}"),
    }
}

fn probe(url: &str) -> Result<(), String> {
    let pipeline = gst::Pipeline::new();
    // Match the app's rtspsrc config.
    let src = gst::ElementFactory::make("rtspsrc")
        .property("location", url)
        .property("latency", 200u32)
        .property("timeout", 30_000_000u64)
        .property("retry", 0u32)
        .property_from_str("protocols", "udp+udp-mcast+tcp")
        .build()
        .map_err(|e| e.to_string())?;
    let depay = mk("rtph264depay")?;
    let parse = gst::ElementFactory::make("h264parse")
        .property_from_str("config-interval", "-1")
        .build()
        .map_err(|_| "h264parse".to_string())?;
    let tee = gst::ElementFactory::make("tee")
        .property("allow-not-linked", true)
        .build()
        .map_err(|e| e.to_string())?;

    // Hardware branch — byte-stream into nvh264dec, exactly like production.
    // With `full-downstream` the decoder feeds the app's real tail
    // (cudadownload → tee → queue → appsink) instead of a fakesink, to test
    // whether that tail — not the decoder — is what stalls nvdec in the app.
    let full_downstream = std::env::args().any(|a| a == "full-downstream");
    let hq = mk("queue")?;
    let hdec = mk("nvh264dec")?;
    let hsink = mk("fakesink")?;
    hsink.set_property("sync", false);
    // Software control branch — proves the live stream itself is decodable.
    let sq = mk("queue")?;
    let sdec = mk("avdec_h264")?;
    let ssink = mk("fakesink")?;
    ssink.set_property("sync", false);

    pipeline
        .add_many([
            &src, &depay, &parse, &tee, &hq, &hdec, &hsink, &sq, &sdec, &ssink,
        ])
        .map_err(|e| e.to_string())?;
    gst::Element::link_many([&depay, &parse, &tee]).map_err(|e| e.to_string())?;

    let bs = gst::Caps::builder("video/x-h264")
        .field("stream-format", "byte-stream")
        .field("alignment", "au")
        .build();
    gst::Element::link(&tee, &hq).map_err(|e| e.to_string())?;
    hq.link_filtered(&hdec, &bs)
        .map_err(|e| format!("hq->hdec: {e}"))?;
    if full_downstream {
        // The app's real NvdecNv12 tail: nvh264dec → cudadownload → tee →
        // queue → appsink that actually pulls (mimicking analysis consumption).
        // `fixed-tail`: insert a leaky=downstream queue BETWEEN the decoder and
        // cudadownload — the proposed fix. It lets a slow 4K download drop decoded
        // frames instead of backpressuring nvh264dec into a stall.
        let fixed_tail = std::env::args().any(|a| a == "fixed-tail");
        let dl = mk("cudadownload")?;
        let dtee = gst::ElementFactory::make("tee")
            .property("allow-not-linked", true)
            .build()
            .map_err(|e| e.to_string())?;
        // Match the app EXACTLY: leaky=downstream queue + appsink max-buffers=1
        // drop=true. The app is built to DROP, never backpressure, so if the
        // decoder still stalls with this config the downstream is genuinely at
        // fault; if it flows, an earlier probe artifact (drop=false) misled me.
        let qd = mk("queue")?;
        qd.set_property_from_str("leaky", "downstream");
        qd.set_property("max-size-buffers", 1u32);
        let asink = gst::ElementFactory::make("appsink")
            .property("emit-signals", false)
            .property("max-buffers", 1u32)
            .property("drop", true)
            .property("sync", false)
            .build()
            .map_err(|e| e.to_string())?;
        pipeline
            .add_many([&dl, &dtee, &qd, &asink])
            .map_err(|e| e.to_string())?;
        if fixed_tail {
            let protect = mk("queue")?;
            protect.set_property_from_str("leaky", "downstream");
            protect.set_property("max-size-buffers", 2u32);
            protect.set_property("max-size-bytes", 0u32);
            protect.set_property("max-size-time", 0u64);
            pipeline.add(&protect).map_err(|e| e.to_string())?;
            gst::Element::link_many([&hdec, &protect, &dl, &dtee, &qd, &asink])
                .map_err(|e| format!("fixed tail: {e}"))?;
        } else {
            gst::Element::link_many([&hdec, &dl, &dtee, &qd, &asink])
                .map_err(|e| format!("full tail: {e}"))?;
        }
        // Pull samples like the analysis appsink does, so backpressure is realistic.
        let appsink = asink.dynamic_cast::<gstreamer_app::AppSink>().unwrap();
        appsink.set_callbacks(
            gstreamer_app::AppSinkCallbacks::builder()
                .new_sample(|s| {
                    let _ = s.pull_sample();
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
    } else {
        gst::Element::link(&hdec, &hsink).map_err(|e| e.to_string())?;
    }
    gst::Element::link(&tee, &sq).map_err(|e| e.to_string())?;
    gst::Element::link_many([&sq, &sdec, &ssink]).map_err(|e| e.to_string())?;

    // rtspsrc pads appear dynamically; link the first video depay.
    let depay_weak = depay.downgrade();
    src.connect_pad_added(move |_, pad| {
        if let Some(depay) = depay_weak.upgrade() {
            if let Some(sink) = depay.static_pad("sink") {
                if !sink.is_linked() {
                    let _ = pad.link(&sink);
                }
            }
        }
    });

    // Log the first buffers ENTERING nvh264dec: pts/dts/flags/size. A live RTP
    // stream that differs from a file shows it here (missing dts, delta-only start,
    // huge/zero durations, non-monotonic pts).
    let logged = Arc::new(AtomicU64::new(0));
    if let Some(sink_pad) = hdec.static_pad("sink") {
        let lg = logged.clone();
        sink_pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if let Some(gst::PadProbeData::Buffer(ref buf)) = info.data {
                let n = lg.fetch_add(1, Ordering::Relaxed);
                if n < 12 {
                    let f = buf.flags();
                    println!(
                        "  nvdec<-in #{n}: pts={:?} dts={:?} dur={:?} bytes={} delta={} header={}",
                        buf.pts().map(|t| t.mseconds()),
                        buf.dts().map(|t| t.mseconds()),
                        buf.duration().map(|t| t.mseconds()),
                        buf.size(),
                        f.contains(gst::BufferFlags::DELTA_UNIT),
                        f.contains(gst::BufferFlags::HEADER),
                    );
                }
            }
            gst::PadProbeReturn::Ok
        });
    }

    let started = Instant::now();
    let hw = count_src(&hdec, "nvh264dec", &started);
    let sw = count_src(&sdec, "avdec_h264", &started);

    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| e.to_string())?;
    let bus = pipeline.bus().ok_or("no bus")?;
    let deadline = Instant::now() + RUN_FOR;
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        if let Some(msg) = bus.timed_pop(gst::ClockTime::from_mseconds(
            left.as_millis().min(500) as u64
        )) {
            match msg.view() {
                gst::MessageView::Eos(_) => break,
                gst::MessageView::Error(e) => {
                    println!(
                        "  bus ERROR from {:?}: {} ({:?})",
                        e.src().map(|s| s.name().to_string()),
                        e.error(),
                        e.debug()
                    );
                    break;
                }
                gst::MessageView::Warning(w) => {
                    println!(
                        "  bus WARNING from {:?}: {} ({:?})",
                        w.src().map(|s| s.name().to_string()),
                        w.error(),
                        w.debug()
                    );
                }
                _ => {}
            }
        }
    }
    let _ = pipeline.set_state(gst::State::Null);

    let h = hw.load(Ordering::Relaxed);
    let s = sw.load(Ordering::Relaxed);
    let secs = started.elapsed().as_secs_f64().max(0.001);
    println!("\n== result over {:.1}s ==", secs);
    println!(
        "  nvh264dec (HARDWARE) frames = {h}  ({:.1} fps){}",
        h as f64 / secs,
        if h == 0 { "  <-- HARDWARE SILENT" } else { "" }
    );
    println!(
        "  avdec_h264 (software)  frames = {s}  ({:.1} fps){}",
        s as f64 / secs,
        if s == 0 {
            "  <-- stream itself dead"
        } else {
            ""
        }
    );
    Ok(())
}

fn count_src(el: &gst::Element, label: &str, started: &Instant) -> Arc<AtomicU64> {
    let n = Arc::new(AtomicU64::new(0));
    if let Some(pad) = el.static_pad("src") {
        let (nc, st, lab) = (n.clone(), *started, label.to_string());
        pad.add_probe(gst::PadProbeType::BUFFER, move |p, _| {
            if nc.fetch_add(1, Ordering::Relaxed) == 0 {
                let caps = p.current_caps().map(|c| c.to_string()).unwrap_or_default();
                println!(
                    "  [{lab}] first frame @ {} ms  caps={}",
                    st.elapsed().as_millis(),
                    &caps[..caps.len().min(150)]
                );
            }
            gst::PadProbeReturn::Ok
        });
    }
    n
}

fn splice_credentials(url: &str, creds: &str) -> String {
    match url.strip_prefix("rtsp://") {
        Some(rest) => format!("rtsp://{creds}@{rest}"),
        None => url.to_string(),
    }
}

fn redact(url: &str) -> String {
    // host:port/path only.
    url.strip_prefix("rtsp://")
        .map(|r| format!("rtsp://{r}"))
        .unwrap_or_else(|| url.into())
}

fn hex_to_bytes(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn mk(name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|_| format!("cannot create {name}"))
}
