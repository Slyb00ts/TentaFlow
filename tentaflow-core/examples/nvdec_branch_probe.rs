// ===== File: nvdec_branch_probe.rs — why does the real NVDEC branch deliver no frames? =====
//
// `nvdec_probe` proved the decoder works (filesrc → h264parse → nvh264dec decodes
// a 4K clip in 346 ms). Production still demotes every NVDEC attempt with "no
// frames within warmup window", so the fault is in the BRANCH TOPOLOGY around the
// decoder, not the decoder.
//
// This rebuilds the production shapes from `rtsp.rs` — the real rtspsrc front
// (rtspsrc → capsfilter → rtp_tee → queue_branch_a) plus each decode tail — and
// counts buffers at each appsink. It runs against a LOCAL RTSP server serving a
// recorded clip, never the production camera.
//
//   cargo run --release --features camera --example nvdec_branch_probe -- <rtsp-url> [topology]
//
// Topologies: cpu | nvdec-cpuconvert | nvdec-nv12 | all (default)

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;

const RUN_FOR: Duration = Duration::from_secs(25);

/// Which decode tail to build behind the shared RTP front.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Topology {
    /// decodebin → videoconvert → RGB (the always-works floor production lands on).
    Cpu,
    /// NVDEC → cudadownload → queue → videoconvert → RGB.
    NvdecCpuConvert,
    /// NVDEC → cudadownload → tee → {crops NV12 appsink} + {detect NV12 appsink}.
    NvdecNv12,
}

impl Topology {
    fn label(self) -> &'static str {
        match self {
            Topology::Cpu => "CPU decode (decodebin → videoconvert → RGB)",
            Topology::NvdecCpuConvert => "NVDEC + CPU convert (nvh264dec → cudadownload → RGB)",
            Topology::NvdecNv12 => "NVDEC + GPU NV12 detect (nvh264dec → cudadownload → tee)",
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let url = args.next().expect(
        "usage: nvdec_branch_probe <rtsp-url> [cpu|nvdec-cpuconvert|nvdec-nv12|all] [tcp|udp]",
    );
    let which = args.next().unwrap_or_else(|| "all".into());
    // Production defaults to TCP interleaved (`[vision] rtsp_protocols`).
    let transport = args.next().unwrap_or_else(|| "tcp".into());

    gst::init().expect("gst init");

    println!("== element registry ==");
    for name in [
        "nvh264dec",
        "cudadownload",
        "cudaupload",
        "cudaconvert",
        "cudascale",
        "rtph264depay",
        "videoconvert",
    ] {
        println!(
            "  {name:<16} {}",
            if gst::ElementFactory::find(name).is_some() {
                "present"
            } else {
                "MISSING"
            }
        );
    }

    let topologies = match which.as_str() {
        "cpu" => vec![Topology::Cpu],
        "nvdec-cpuconvert" => vec![Topology::NvdecCpuConvert],
        "nvdec-nv12" => vec![Topology::NvdecNv12],
        _ => vec![
            Topology::Cpu,
            Topology::NvdecCpuConvert,
            Topology::NvdecNv12,
        ],
    };
    for topo in topologies {
        println!("\n== {} [{transport}] ==", topo.label());
        match probe(&url, topo, &transport) {
            Ok(r) => println!("{r}"),
            Err(e) => println!("  pipeline error: {e}"),
        }
    }
}

/// Buffer counter attached to one appsink, reporting first-arrival latency.
struct Tap {
    name: &'static str,
    count: Arc<AtomicU64>,
    first_ms: Arc<Mutex<Option<u128>>>,
    caps: Arc<Mutex<Option<String>>>,
}

impl Tap {
    fn attach(name: &'static str, sink: &gst::Element, started: Instant) -> Result<Tap, String> {
        let count = Arc::new(AtomicU64::new(0));
        let first_ms = Arc::new(Mutex::new(None));
        let caps = Arc::new(Mutex::new(None));
        let pad = sink.static_pad("sink").ok_or("sink pad missing")?;
        let (c, f, cp) = (count.clone(), first_ms.clone(), caps.clone());
        pad.add_probe(gst::PadProbeType::BUFFER, move |pad, _| {
            if c.fetch_add(1, Ordering::Relaxed) == 0 {
                *f.lock().unwrap() = Some(started.elapsed().as_millis());
                *cp.lock().unwrap() = pad.current_caps().map(|x| x.to_string());
            }
            gst::PadProbeReturn::Ok
        });
        Ok(Tap {
            name,
            count,
            first_ms,
            caps,
        })
    }

    fn report(&self) -> String {
        let n = self.count.load(Ordering::Relaxed);
        let first = self
            .first_ms
            .lock()
            .unwrap()
            .map(|m| format!("{m} ms"))
            .unwrap_or_else(|| "never".into());
        let caps = self
            .caps
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "-".into());
        format!(
            "  {:<12} frames={n:<6} first={first:<10}{}\n      caps: {caps}",
            self.name,
            if n == 0 { " <-- NO FRAMES" } else { "" }
        )
    }
}

fn probe(url: &str, topo: Topology, transport: &str) -> Result<String, String> {
    let pipeline = gst::Pipeline::new();
    let started = Instant::now();

    // ---- shared RTP front, identical to build_rtspsrc + build_rtp_front ----
    let rtspsrc = gst::ElementFactory::make("rtspsrc")
        .property("location", url)
        .property("latency", 200u32)
        .property("timeout", 30_000_000u64)
        .property("retry", 0u32)
        .property_from_str("protocols", transport)
        .build()
        .map_err(|e| e.to_string())?;
    let rtp_filter = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("application/x-rtp")
                .field("media", "video")
                .build(),
        )
        .build()
        .map_err(|e| e.to_string())?;
    let tee = gst::ElementFactory::make("tee")
        .property("name", "rtp_tee")
        .property("allow-not-linked", true)
        .build()
        .map_err(|e| e.to_string())?;
    let queue_a = gst::ElementFactory::make("queue")
        .property("name", "queue_branch_a")
        .property("max-size-buffers", 100u32)
        .build()
        .map_err(|e| e.to_string())?;

    pipeline
        .add_many([&rtspsrc, &rtp_filter, &tee, &queue_a])
        .map_err(|e| e.to_string())?;
    gst::Element::link(&rtp_filter, &tee).map_err(|e| e.to_string())?;
    let tee_src = tee.request_pad_simple("src_%u").ok_or("tee src req")?;
    tee_src
        .link(&queue_a.static_pad("sink").ok_or("queue_a sink")?)
        .map_err(|e| format!("{e:?}"))?;

    let mut taps = Vec::new();

    match topo {
        Topology::Cpu => {
            // decodebin handles depay+parse+decode itself, linked dynamically on
            // its own pad-added — the production CPU floor.
            let depay = mk("rtph264depay")?;
            let decodebin = mk("decodebin")?;
            let convert = mk("videoconvert")?;
            let rgb = rgb_capsfilter()?;
            let sink = appsink("sink")?;
            pipeline
                .add_many([&depay, &decodebin, &convert, &rgb, &sink])
                .map_err(|e| e.to_string())?;
            gst::Element::link_many([&queue_a, &depay, &decodebin]).map_err(|e| e.to_string())?;
            gst::Element::link_many([&convert, &rgb, &sink]).map_err(|e| e.to_string())?;
            let convert_weak = convert.downgrade();
            decodebin.connect_pad_added(move |_, pad| {
                if let Some(c) = convert_weak.upgrade() {
                    if let Some(t) = c.static_pad("sink") {
                        if !t.is_linked() {
                            let _ = pad.link(&t);
                        }
                    }
                }
            });
            taps.push(Tap::attach("crops", &sink, started)?);
        }
        Topology::NvdecCpuConvert => {
            let cudadownload = mk("cudadownload")?;
            let queue_dec = leaky_queue("queue_decoded_a")?;
            let convert = mk("videoconvert")?;
            let rgb = rgb_capsfilter()?;
            let sink = appsink("sink")?;
            pipeline
                .add_many([&cudadownload, &queue_dec, &convert, &rgb, &sink])
                .map_err(|e| e.to_string())?;
            gst::Element::link_many([&cudadownload, &queue_dec, &convert, &rgb, &sink])
                .map_err(|e| e.to_string())?;
            taps.push(Tap::attach("crops", &sink, started)?);
            arm_dynamic_decoder(&pipeline, &queue_a, &cudadownload)?;
        }
        Topology::NvdecNv12 => {
            // Production NvdecNv12: crops appsink takes RAW NV12 with NO capsfilter
            // in front of it, and the detect branch pins video/x-raw,format=NV12.
            let cudadownload = mk("cudadownload")?;
            let tee_decode = gst::ElementFactory::make("tee")
                .property("name", "tee_decode")
                .property("allow-not-linked", true)
                .build()
                .map_err(|e| e.to_string())?;
            let queue_dec = leaky_queue("queue_decoded_a")?;
            let sink_crops = appsink("sink")?;
            let queue_det = leaky_queue("queue_detect_nv12")?;
            let nv12_caps = gst::ElementFactory::make("capsfilter")
                .property(
                    "caps",
                    gst::Caps::builder("video/x-raw")
                        .field("format", "NV12")
                        .build(),
                )
                .build()
                .map_err(|e| e.to_string())?;
            let sink_det = appsink("sink_detect_nv12")?;
            pipeline
                .add_many([
                    &cudadownload,
                    &tee_decode,
                    &queue_dec,
                    &sink_crops,
                    &queue_det,
                    &nv12_caps,
                    &sink_det,
                ])
                .map_err(|e| e.to_string())?;
            gst::Element::link(&cudadownload, &tee_decode).map_err(|e| e.to_string())?;
            let t_crops = tee_decode.request_pad_simple("src_%u").ok_or("tee crops")?;
            t_crops
                .link(&queue_dec.static_pad("sink").ok_or("queue_dec sink")?)
                .map_err(|e| format!("{e:?}"))?;
            gst::Element::link(&queue_dec, &sink_crops).map_err(|e| e.to_string())?;
            let t_det = tee_decode.request_pad_simple("src_%u").ok_or("tee det")?;
            t_det
                .link(&queue_det.static_pad("sink").ok_or("queue_det sink")?)
                .map_err(|e| format!("{e:?}"))?;
            gst::Element::link_many([&queue_det, &nv12_caps, &sink_det])
                .map_err(|e| e.to_string())?;
            taps.push(Tap::attach("crops", &sink_crops, started)?);
            taps.push(Tap::attach("detect", &sink_det, started)?);
            arm_dynamic_decoder(&pipeline, &queue_a, &cudadownload)?;
        }
    }

    // rtspsrc exposes its video pad only after SETUP.
    let rtp_filter_weak = rtp_filter.downgrade();
    rtspsrc.connect_pad_added(move |_, pad| {
        let Some(f) = rtp_filter_weak.upgrade() else {
            return;
        };
        let Some(t) = f.static_pad("sink") else {
            return;
        };
        if t.is_linked() {
            return;
        }
        let media = pad
            .current_caps()
            .and_then(|c| c.structure(0).and_then(|s| s.get::<String>("media").ok()));
        if media.as_deref() == Some("video") {
            let _ = pad.link(&t);
        }
    });

    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| e.to_string())?;

    let bus = pipeline.bus().ok_or("no bus")?;
    let mut err = None;
    let deadline = Instant::now() + RUN_FOR;
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        if let Some(msg) = bus.timed_pop(gst::ClockTime::from_mseconds(
            left.as_millis().min(500) as u64
        )) {
            match msg.view() {
                gst::MessageView::Eos(_) => break,
                gst::MessageView::Error(e) => {
                    err = Some(format!("{} ({:?})", e.error(), e.debug()));
                    break;
                }
                gst::MessageView::Warning(w) => {
                    println!("  bus warning: {} ({:?})", w.error(), w.debug());
                }
                _ => {}
            }
        }
    }

    let mut out = String::new();
    for t in &taps {
        out.push_str(&t.report());
        out.push('\n');
    }
    if let Some(q) = pipeline.by_name("queue_branch_a") {
        out.push_str(&format!(
            "  queue_branch_a held={} src_linked={}\n",
            q.property::<u32>("current-level-buffers"),
            q.static_pad("src").map(|p| p.is_linked()).unwrap_or(false)
        ));
    }
    let _ = pipeline.set_state(gst::State::Null);
    if let Some(e) = err {
        out.push_str(&format!("  bus error: {e}\n"));
    }
    Ok(out)
}

/// Mirrors rtsp.rs: the decoder chain is created only once the RTP caps are known,
/// then linked into an already-PLAYING pipeline.
fn arm_dynamic_decoder(
    pipeline: &gst::Pipeline,
    queue_a: &gst::Element,
    downstream: &gst::Element,
) -> Result<(), String> {
    let pipeline_weak = pipeline.downgrade();
    let downstream_weak = downstream.downgrade();
    let queue_a_src = queue_a.static_pad("src").ok_or("queue_a src pad")?;
    let built = Arc::new(AtomicBool::new(false));
    let build = move |caps: &gst::Caps| {
        if built.swap(true, Ordering::SeqCst) {
            return;
        }
        let (Some(pipeline), Some(downstream)) =
            (pipeline_weak.upgrade(), downstream_weak.upgrade())
        else {
            return;
        };
        let Some(queue_a) = pipeline.by_name("queue_branch_a") else {
            return;
        };
        let encoding = caps
            .structure(0)
            .and_then(|s| s.get::<String>("encoding-name").ok())
            .unwrap_or_default();
        println!("  notify::caps encoding={encoding} — building NVDEC chain");
        match link_decoder(&pipeline, &queue_a, &downstream) {
            Ok(()) => println!("  NVDEC chain linked"),
            Err(e) => println!("  NVDEC chain link FAILED: {e}"),
        }
    };
    let build = Arc::new(build);
    if let Some(caps) = queue_a_src.current_caps() {
        build(&caps);
    } else {
        let b = build.clone();
        queue_a_src.connect_notify(Some("caps"), move |pad, _| {
            if let Some(caps) = pad.current_caps() {
                b(&caps);
            }
        });
    }
    Ok(())
}

fn link_decoder(
    pipeline: &gst::Pipeline,
    queue_a: &gst::Element,
    downstream: &gst::Element,
) -> Result<(), String> {
    let depay = mk("rtph264depay")?;
    let parse = mk("h264parse")?;
    let dec = mk("nvh264dec")?;
    pipeline
        .add_many([&depay, &parse, &dec])
        .map_err(|e| e.to_string())?;
    gst::Element::link_many([queue_a, &depay, &parse, &dec, downstream])
        .map_err(|e| e.to_string())?;
    for el in [&depay, &parse, &dec] {
        el.sync_state_with_parent().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn appsink(name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make("appsink")
        .property("name", name)
        .property("emit-signals", false)
        .property("sync", false)
        .property("max-buffers", 1u32)
        .property("drop", true)
        .build()
        .map_err(|e| e.to_string())
}

fn rgb_capsfilter() -> Result<gst::Element, String> {
    gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("format", "RGB")
                .build(),
        )
        .build()
        .map_err(|e| e.to_string())
}

fn leaky_queue(name: &str) -> Result<gst::Element, String> {
    let q = gst::ElementFactory::make("queue")
        .property("name", name)
        .property("max-size-buffers", 2u32)
        .property("max-size-bytes", 0u32)
        .property("max-size-time", 0u64)
        .build()
        .map_err(|e| e.to_string())?;
    q.set_property_from_str("leaky", "downstream");
    Ok(q)
}

fn mk(name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|_| format!("cannot create element {name}"))
}
