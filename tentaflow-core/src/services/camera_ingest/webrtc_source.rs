// =============================================================================
// File: services/camera_ingest/webrtc_source.rs
// Purpose: Camera source backed by a WebRTC video track. An H.264 Annex-B byte
//          stream (depacketized in tentaflow-hardware, delivered over an mpsc)
//          is pushed into a GStreamer appsrc, parsed once, then fanned out by a
//          tee into Branch A (decode → RGB → appsink, the always-on frame path
//          shared with every other camera) and an on-demand Branch B
//          (mp4mux → appsink) that feeds the fMP4 publisher for smooth MSE
//          playback. This is the only appsrc pipeline in the repo.
//
//          A camera with privacy options active swaps Branch A for the GPU
//          privacy path (NVDEC → privacy probe → CUDA tee) and its Branch B
//          re-encodes the anonymized frames with NVENC instead of passing the
//          robot's H.264 through — no branch ever carries unprocessed pixels.
// =============================================================================

use std::sync::Arc;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use tokio::sync::mpsc;

use super::error::{CameraIngestError, Result};
use super::fakefile::{
    ensure_gst_initialized, install_frame_callback, FakeFilePipeline, FrameCounters, FrameMailbox,
};
use super::rtsp::{detach_mp4_branch, wire_mp4_appsink, Mp4BranchState};
use super::session::CameraConfig;
use super::stream_publisher::Mp4StreamPublisher;

/// Build the appsrc → tee pipeline. Branch A (`tee → queue → decodebin →
/// videoconvert → RGB → appsink`) is always present and drives the existing
/// frame_storage / streaming_bus path. The returned `tee` lets the session
/// attach an on-demand fMP4 mux branch (Branch B) without rebuilding. We also
/// return the appsrc so the pump can feed it.
///
/// We tee the RAW Annex-B byte stream (no parse before the tee) and give Branch
/// B its OWN h264parse. A shared parse before the tee re-frames the stream into
/// access units that split the parameter sets away from the IDR (the front parse
/// emitted SPS as its own AU separate from PPS+IDR), so Branch B's AVC parse
/// could not build avcC codec_data (num_pps=0) and posted a FATAL
/// `No caps set ... GstH264Parse` bus error that tore down the WHOLE shared
/// pipeline (Branch A included). The upstream H264Gate coalesces SPS+PPS+IDR
/// into a single keyframe access unit, so the raw tee delivers Branch B a clean,
/// self-contained keyframe from which codec_data is always constructible.
///
/// Branch B's h264parse uses config-interval=-1 (in-band SPS/PPS) and an
/// SPS-sync pad probe drops mid-GOP slices until the next keyframe AU so the
/// parse never pushes before negotiating caps. Branch A's decodebin carries its
/// own parser and consumes the raw byte stream directly.
///
/// Pipeline graph:
///
///   appsrc(byte-stream,au) → tee(allow-not-linked)
///       ├─ src_0 → queue → decodebin → videoconvert → RGB → appsink                       (Branch A, always on)
///       └─ src_N → queue → [SPS-gate] → h264parse(AVC) → mp4mux(fMP4) → appsink           (Branch B, on demand)
///
/// With privacy options active, Branch A is replaced and Branch B moves behind
/// the probe (see `link_privacy_branch` / `attach_mp4_branch_webrtc_encoded`):
///
///   appsrc → tee → queue → h264parse → h264timestamper → nvh264dec(CUDA NV12)
///       → queue(leaky) → [privacy probe, in place] → tee_cuda
///           ├─ queue(leaky) → [≤ HOST_FRAME_FPS] → cudadownload → RGB → appsink    (always on)
///           └─ queue → nvcudah264enc → h264parse → mp4mux(fMP4) → appsink          (Branch B, on demand)
pub fn build_webrtc_pipeline(
    config: &CameraConfig,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<(FakeFilePipeline, gst_app::AppSrc, WebRtcFanout)> {
    ensure_gst_initialized()?;

    let pipeline = gst::Pipeline::new();

    let appsrc = gst::ElementFactory::make("appsrc")
        .property("name", "src")
        .property("is-live", true)
        .property("do-timestamp", true)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("appsrc: {e}")))?;
    appsrc.set_property_from_str("format", "time");

    // `allow-not-linked=true` lets the pipeline run with Branch B absent (before
    // first attach and after detach) without tripping `not-linked (-1)` when the
    // tee pushes to a released request pad.
    let tee = gst::ElementFactory::make("tee")
        .property("name", "h264_tee")
        .property("allow-not-linked", true)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("tee: {e}")))?;

    pipeline
        .add_many([&appsrc, &tee])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("add_many: {e}")))?;
    gst::Element::link(&appsrc, &tee)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("appsrc → tee: {e}")))?;

    let (appsink_app, fanout) = if config.privacy.active() {
        let (appsink, tee_cuda) =
            link_privacy_branch(&pipeline, &tee, config, mailbox, counters)?;
        (appsink, WebRtcFanout::Anonymized(tee_cuda))
    } else {
        let appsink = link_cpu_decode_branch(&pipeline, &tee, config, mailbox, counters)?;
        (appsink, WebRtcFanout::Passthrough(tee))
    };

    let appsrc_app = appsrc
        .downcast::<gst_app::AppSrc>()
        .map_err(|_| CameraIngestError::PipelineBuild("'src' is not AppSrc".into()))?;

    // The robot delivers H.264 as Annex-B access units (start-code prefixed),
    // unlike RTSP where rtph264depay yields RTP-framed NALUs. Pin the appsrc
    // caps so h264parse negotiates without ambiguity on the first push.
    let in_caps = gst::Caps::builder("video/x-h264")
        .field("stream-format", "byte-stream")
        .field("alignment", "au")
        .build();
    appsrc_app.set_caps(Some(&in_caps));
    // Bounded internal buffering — the gate already drops/re-primes upstream, so
    // back-pressure here just bounds memory, not correctness.
    appsrc_app.set_max_bytes(4 * 1024 * 1024);

    Ok((
        FakeFilePipeline {
            pipeline,
            appsink: appsink_app,
        },
        appsrc_app,
        fanout,
    ))
}

/// Branch A of a camera without privacy processing: `tee → queue → decodebin →
/// videoconvert → RGB → appsink`, the always-on frame path shared with every
/// other camera source.
fn link_cpu_decode_branch(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    config: &CameraConfig,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<gst_app::AppSink> {
    // Branch A queue — decouples decode latency from the tee fan-out so a slow
    // appsink consumer cannot back-pressure Branch B (and vice versa).
    let queue_a = gst::ElementFactory::make("queue")
        .property("name", "queue_branch_a")
        .property("max-size-buffers", 30u32)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("queue_a: {e}")))?;
    queue_a.set_property_from_str("leaky", "downstream");

    let decodebin = gst::ElementFactory::make("decodebin")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("decodebin: {e}")))?;
    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("videoconvert: {e}")))?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGB")
        .build();
    let capsfilter = gst::ElementFactory::make("capsfilter")
        .property("caps", &caps)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("capsfilter: {e}")))?;
    let appsink = gst::ElementFactory::make("appsink")
        .property("name", "sink")
        .property("emit-signals", false)
        .property("sync", false)
        .property("max-buffers", 2u32)
        .property("drop", true)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("appsink: {e}")))?;

    pipeline
        .add_many([
            &queue_a,
            &decodebin,
            &convert,
            &capsfilter,
            &appsink,
        ])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("add_many: {e}")))?;

    // Static segment convert → capsfilter → appsink. tee.src_0 → queue_a →
    // decodebin and decodebin → convert are wired by request pad / dynamic pad
    // below.
    let tee_src_a = tee
        .request_pad_simple("src_%u")
        .ok_or_else(|| CameraIngestError::PipelineBuild("tee src_%u request failed".into()))?;
    let queue_a_sink = queue_a
        .static_pad("sink")
        .ok_or_else(|| CameraIngestError::PipelineBuild("queue_a sink pad missing".into()))?;
    tee_src_a
        .link(&queue_a_sink)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("tee → queue_a: {e:?}")))?;
    gst::Element::link(&queue_a, &decodebin)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("queue_a → decodebin: {e}")))?;
    gst::Element::link_many([&convert, &capsfilter, &appsink])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("link_many tail: {e}")))?;

    // decodebin's video output pad appears dynamically once it identifies the
    // codec — wire it into videoconvert. This source is H.264-only and decodes
    // to a single raw video pad, so we link it robustly: some decodebin impls
    // emit `pad-added` BEFORE caps are negotiated (current_caps() == None at
    // that instant). Bailing on missing caps would leave Branch A unlinked and
    // stop ALL frames (frame_storage / JPEG / inference). So when caps are not
    // yet known we attempt the link unconditionally for the new pad; when caps
    // ARE present we still gate on `video/` to skip any stray non-video pad.
    let convert_weak = convert.downgrade();
    decodebin.connect_pad_added(move |_dec, src_pad| {
        let Some(convert) = convert_weak.upgrade() else {
            return;
        };
        let Some(sink_pad) = convert.static_pad("sink") else {
            return;
        };
        if sink_pad.is_linked() {
            return;
        }
        if let Some(structure) = src_pad
            .current_caps()
            .and_then(|c| c.structure(0).map(|s| s.name().to_string()))
        {
            if !structure.starts_with("video/") {
                return;
            }
        }
        // Caps unknown OR confirmed video: attempt the link. If decodebin is
        // not ready to negotiate yet the link errors harmlessly and the same
        // raw video pad will not re-trigger pad-added — but for this single-pad
        // H.264 source the pad is the decoded video, so the link succeeds once
        // decodebin pushes caps downstream.
        if let Err(e) = src_pad.link(&sink_pad) {
            tracing::warn!("webrtc: decodebin → videoconvert link failed: {e:?}");
        }
    });

    let appsink_app = appsink
        .clone()
        .downcast::<gst_app::AppSink>()
        .map_err(|_| CameraIngestError::PipelineBuild("'sink' is not AppSink".into()))?;
    install_frame_callback(&appsink_app, config.camera_id.clone(), mailbox, Some(counters));
    Ok(appsink_app)

}

/// Rate of anonymized frames downloaded to host memory for the mailbox,
/// snapshots and depth mapping. Their consumers run at a few fps (analysis 5,
/// depth ≤ 2), so downloading every frame would only move ~35 MB/s of pixels to
/// the CPU for nothing.
#[cfg(all(
    any(target_os = "linux", target_os = "windows"),
    feature = "inference-vision-gpu",
    feature = "vision-ort",
    feature = "vision-cuda-preprocess"
))]
const HOST_FRAME_FPS: u64 = 5;

/// Branch A of a privacy-processed camera. The H.264 is decoded by NVDEC into
/// device NV12; the privacy probe anonymizes each frame IN PLACE before the
/// CUDA tee, so every consumer behind it — the host download for the mailbox and
/// Branch B's encoder — only ever sees processed pixels. Returns the host
/// appsink and the CUDA tee Branch B attaches to.
///
/// `h264parse` + `h264timestamper` sit INSIDE this branch, never before the raw
/// tee: a shared parse split SPS from PPS+IDR (see the top of this file).
/// `num-output-surfaces=1` makes the decoder hand out a copy of each picture
/// rather than a surface it still references for prediction, which is what
/// makes the in-place write safe; it is asserted, not assumed.
#[cfg(all(
    any(target_os = "linux", target_os = "windows"),
    feature = "inference-vision-gpu",
    feature = "vision-ort",
    feature = "vision-cuda-preprocess"
))]
fn link_privacy_branch(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    config: &CameraConfig,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<(gst_app::AppSink, gst::Element)> {
    let make = |factory: &str, name: &str| {
        gst::ElementFactory::make(factory)
            .property("name", name)
            .build()
            .map_err(|e| {
                CameraIngestError::PipelineBuild(format!(
                    "privacy path needs GStreamer element '{factory}' (NVIDIA nvcodec): {e}"
                ))
            })
    };
    let queue_in = make("queue", "queue_privacy_in")?;
    queue_in.set_property("max-size-buffers", 30u32);
    queue_in.set_property_from_str("leaky", "downstream");
    let parse = make("h264parse", "parse_privacy")?;
    parse.set_property_from_str("config-interval", "-1");
    let timestamper = make("h264timestamper", "timestamper_privacy")?;
    let decoder = make("nvh264dec", "nvdec_privacy")?;
    decoder.set_property("num-output-surfaces", 1u32);
    if decoder.property::<u32>("num-output-surfaces") != 1 {
        return Err(CameraIngestError::PipelineBuild(
            "nvh264dec refused num-output-surfaces=1; in-place anonymization would corrupt its reference pictures".into(),
        ));
    }
    let cuda_caps = gst::Caps::builder("video/x-raw")
        .features(["memory:CUDAMemory"])
        .field("format", "NV12")
        .build();
    let cuda_filter = make("capsfilter", "caps_privacy_cuda")?;
    cuda_filter.set_property("caps", &cuda_caps);
    // Leaky + tiny: when the probe falls behind, whole frames are dropped here,
    // before they could reach any consumer unprocessed.
    let queue_probe = make("queue", "queue_privacy_probe")?;
    queue_probe.set_property("max-size-buffers", 2u32);
    queue_probe.set_property("max-size-bytes", 0u32);
    queue_probe.set_property("max-size-time", 0u64);
    queue_probe.set_property_from_str("leaky", "downstream");
    let tee_cuda = make("tee", "tee_privacy_cuda")?;
    tee_cuda.set_property("allow-not-linked", true);

    let queue_host = make("queue", "queue_privacy_host")?;
    queue_host.set_property("max-size-buffers", 1u32);
    queue_host.set_property_from_str("leaky", "downstream");
    let download = make("cudadownload", "download_privacy")?;
    let convert = make("videoconvert", "convert_privacy")?;
    let rgb_filter = make("capsfilter", "caps_privacy_rgb")?;
    rgb_filter.set_property(
        "caps",
        gst::Caps::builder("video/x-raw").field("format", "RGB").build(),
    );
    let appsink = gst::ElementFactory::make("appsink")
        .property("name", "sink")
        .property("emit-signals", false)
        .property("sync", false)
        .property("max-buffers", 2u32)
        .property("drop", true)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("appsink: {e}")))?;

    let chain = [
        &queue_in,
        &parse,
        &timestamper,
        &decoder,
        &cuda_filter,
        &queue_probe,
        &tee_cuda,
    ];
    let host = [&queue_host, &download, &convert, &rgb_filter, &appsink];
    pipeline
        .add_many(chain.iter().copied().chain(host.iter().copied()))
        .map_err(|e| CameraIngestError::PipelineBuild(format!("add privacy branch: {e}")))?;
    gst::Element::link_many(chain)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("link privacy chain: {e}")))?;
    gst::Element::link_many(host)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("link privacy host branch: {e}")))?;

    let tee_src = tee
        .request_pad_simple("src_%u")
        .ok_or_else(|| CameraIngestError::PipelineBuild("tee src_%u request failed".into()))?;
    let queue_in_sink = queue_in
        .static_pad("sink")
        .ok_or_else(|| CameraIngestError::PipelineBuild("queue_privacy_in sink pad".into()))?;
    tee_src
        .link(&queue_in_sink)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("tee → privacy branch: {e:?}")))?;
    let cuda_host_src = tee_cuda
        .request_pad_simple("src_%u")
        .ok_or_else(|| CameraIngestError::PipelineBuild("tee_cuda src_%u request failed".into()))?;
    let queue_host_sink = queue_host
        .static_pad("sink")
        .ok_or_else(|| CameraIngestError::PipelineBuild("queue_privacy_host sink pad".into()))?;
    cuda_host_src
        .link(&queue_host_sink)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("tee_cuda → host branch: {e:?}")))?;

    let probe_pad = queue_probe
        .static_pad("src")
        .ok_or_else(|| CameraIngestError::PipelineBuild("queue_privacy_probe src pad".into()))?;
    super::privacy::install_privacy_probe(
        &probe_pad,
        &config.camera_id,
        config.privacy,
        config.target_fps,
        counters,
    )
    .map_err(|e| CameraIngestError::PipelineBuild(format!("privacy probe: {e:#}")))?;
    install_host_decimator(&queue_host_sink, HOST_FRAME_FPS);

    let appsink = appsink
        .downcast::<gst_app::AppSink>()
        .map_err(|_| CameraIngestError::PipelineBuild("'sink' is not AppSink".into()))?;
    // Frames are counted by the probe at the source rate; this sink only sees
    // the decimated subset.
    install_frame_callback(&appsink, config.camera_id.clone(), mailbox, None);
    Ok((appsink, tee_cuda))
}

/// Hosts without the GPU privacy path refuse a camera that asks for privacy:
/// showing it unprocessed is exactly what the option forbids.
// TODO(inference-engine): run the privacy path on TentaFlow's own inference
// engine once it serves every backend (AMD/Intel/Apple), instead of refusing.
#[cfg(not(all(
    any(target_os = "linux", target_os = "windows"),
    feature = "inference-vision-gpu",
    feature = "vision-ort",
    feature = "vision-cuda-preprocess"
)))]
fn link_privacy_branch(
    _pipeline: &gst::Pipeline,
    _tee: &gst::Element,
    _config: &CameraConfig,
    _mailbox: Arc<FrameMailbox>,
    _counters: Arc<FrameCounters>,
) -> Result<(gst_app::AppSink, gst::Element)> {
    Err(CameraIngestError::PipelineBuild(
        "privacy processing (face anonymization / person detection) is not available on this node: it needs the NVIDIA GPU path".into(),
    ))
}

/// Pass at most `fps` buffers per second (by PTS) into the host download: the
/// mailbox consumers need a recent frame, not every frame.
#[cfg(all(
    any(target_os = "linux", target_os = "windows"),
    feature = "inference-vision-gpu",
    feature = "vision-ort",
    feature = "vision-cuda-preprocess"
))]
fn install_host_decimator(pad: &gst::Pad, fps: u64) {
    let interval_ns = 1_000_000_000 / fps.max(1);
    let last = std::sync::atomic::AtomicU64::new(u64::MAX);
    pad.add_probe(gst::PadProbeType::BUFFER, move |_pad, info| {
        let Some(pts) = info.buffer().and_then(|b| b.pts()).map(|t| t.nseconds()) else {
            return gst::PadProbeReturn::Drop;
        };
        let prev = last.load(std::sync::atomic::Ordering::Relaxed);
        if prev != u64::MAX && pts < prev.saturating_add(interval_ns) && pts >= prev {
            return gst::PadProbeReturn::Drop;
        }
        last.store(pts, std::sync::atomic::Ordering::Relaxed);
        gst::PadProbeReturn::Ok
    });
}

/// Where Branch B (the fMP4 publisher branch) attaches, and in which form.
pub enum WebRtcFanout {
    /// The raw Annex-B tee: Branch B passes the robot's H.264 through.
    Passthrough(gst::Element),
    /// The CUDA tee after the privacy probe: Branch B re-encodes anonymized
    /// frames with NVENC. Passing the source H.264 through here would publish
    /// exactly the pixels the probe exists to remove.
    Anonymized(gst::Element),
}

impl WebRtcFanout {
    pub fn tee(&self) -> &gst::Element {
        match self {
            Self::Passthrough(tee) | Self::Anonymized(tee) => tee,
        }
    }
}

/// Attach Branch B (`tee → queue → h264parse(AVC) → mp4mux → appsink`) to the
/// running webrtc pipeline and route the mux output into `publisher`. The tee
/// carries the raw Annex-B byte stream; this branch's h264parse frames it and
/// converts to AVC sample format (codec_data) which mp4mux requires. The
/// upstream gate coalesces SPS+PPS+IDR into one keyframe access unit so the
/// parse can always build avcC codec_data. An SPS-sync pad probe on the parse
/// sink drops mid-GOP buffers until the next keyframe AU so the parse never
/// pushes before negotiating caps — that pre-sync push is what posted the fatal
/// "No caps set" bus error and tore down the whole pipeline. mp4mux properties
/// mirror RTSP exactly so the browser MSE init segment + fragment layout is
/// identical.
pub(super) fn attach_mp4_branch_webrtc(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    publisher: &Arc<Mp4StreamPublisher>,
) -> std::result::Result<Mp4BranchState, String> {
    // NON-leaky: Branch B feeds a mux, not a live-display sink, so it must never
    // drop the keyframe that seeds the init segment. Bound by time so a bursty
    // GOP cannot trip a buffer-count limit and start dropping.
    let queue_b = gst::ElementFactory::make("queue")
        .property("name", "queue_branch_b")
        .property("max-size-buffers", 0u32)
        .property("max-size-bytes", 0u32)
        .property("max-size-time", 5_000_000_000u64)
        .build()
        .map_err(|e| format!("queue_b build: {e}"))?;
    queue_b.set_property_from_str("leaky", "no");
    // config-interval=-1 keeps SPS/PPS attached so the AVC caps carry codec_data
    // for mp4mux's avcC; output stream-format negotiates to avc against mp4mux.
    let parse = gst::ElementFactory::make("h264parse")
        .property("name", "parse_branch_b")
        .property_from_str("config-interval", "-1")
        .build()
        .map_err(|e| format!("h264parse build: {e}"))?;
    // The appsrc stamps buffers with `do-timestamp=true` (arrival wall-clock),
    // so whenever the upstream H264Gate re-waits for an IDR after a WiFi RTP
    // sequence gap, the clock keeps advancing while no AU is pushed. The stream
    // resumes with a forward DTS jump equal to the stall, which mp4mux writes as
    // a baseMediaDecodeTime discontinuity. MSE's decoder rejects that gap, sets
    // HTMLMediaElement.error, and the next appendBuffer throws — the ~4s black
    // cycle. h264timestamper rebuilds a clean, monotonic, gap-free DTS/PTS
    // timeline from the H.264 SPS framerate and picture-order count, so the
    // muxed fragments stay continuous regardless of arrival jitter or gate
    // re-primes. RTSP doesn't need this (rtspsrc carries the camera's own
    // continuous RTP timeline); this appsrc path is the only jittery source.
    let timestamper = gst::ElementFactory::make("h264timestamper")
        .property("name", "timestamper_branch_b")
        .build()
        .map_err(|e| format!("h264timestamper build: {e}"))?;
    // streamable=true → ftyp+moov init segment on the first fragment, then
    // moof+mdat media fragments with no finalize. fragment-duration in ms.
    let mux = gst::ElementFactory::make("mp4mux")
        .property("fragment-duration", 100u32)
        .property("streamable", true)
        .build()
        .map_err(|e| format!("mp4mux build: {e}"))?;
    let sink = gst::ElementFactory::make("appsink")
        .property("name", "sink_mp4")
        .property("emit-signals", false)
        .property("sync", false)
        .property("max-buffers", 8u32)
        .property("drop", false)
        .build()
        .map_err(|e| format!("appsink_b build: {e}"))?;

    pipeline
        .add_many([&queue_b, &parse, &timestamper, &mux, &sink])
        .map_err(|e| format!("add_many branch B: {e}"))?;

    let tee_src_pad = tee.request_pad_simple("src_%u").ok_or_else(|| {
        // No request pad yet, but the elements ARE in the pipeline — remove
        // them so a failed attach never leaves dangling elements that a
        // later attach would trip over.
        let refs: Vec<&gst::Element> = [&queue_b, &parse, &timestamper, &mux, &sink]
            .into_iter()
            .collect();
        let _ = pipeline.remove_many(refs);
        "tee src_%u request for branch B failed".to_string()
    })?;

    // From here the request pad exists alongside the added elements: capture
    // both as a single `Mp4BranchState` so EVERY subsequent failure path can
    // call the shared detach (unlink → NULL → remove → release request pad)
    // instead of leaking elements/pads into the live pipeline. On success the
    // state is returned to the session unchanged.
    let state = Mp4BranchState {
        tee_src_pad,
        elements: vec![queue_b, parse, timestamper, mux, sink],
    };

    // `wire_and_link` performs every fallible step that follows pad acquisition;
    // on the first error the state is detached and the error is surfaced.
    if let Err(e) = wire_and_link_webrtc_branch(&state, publisher) {
        detach_mp4_branch(pipeline, tee, state);
        return Err(e);
    }

    Ok(state)
}

/// Attach Branch B behind the privacy probe: `tee_cuda → queue → nvcudah264enc
/// → h264parse → mp4mux → appsink`. The encoder consumes the anonymized CUDA
/// frames directly (no download), and its settings keep the stream MSE-friendly
/// and low-latency: no B-frames, no reorder delay, a keyframe every ~2 s so a
/// fresh subscriber and the browser's buffer trimming always have an entry
/// point. The mux mirrors the passthrough branch so the init segment layout is
/// unchanged for the client.
#[cfg(all(
    any(target_os = "linux", target_os = "windows"),
    feature = "inference-vision-gpu",
    feature = "vision-ort",
    feature = "vision-cuda-preprocess"
))]
pub(super) fn attach_mp4_branch_webrtc_encoded(
    pipeline: &gst::Pipeline,
    tee_cuda: &gst::Element,
    publisher: &Arc<Mp4StreamPublisher>,
    source_fps: u32,
) -> std::result::Result<Mp4BranchState, String> {
    let queue = gst::ElementFactory::make("queue")
        .property("name", "queue_branch_b_encoded")
        .property("max-size-buffers", 0u32)
        .property("max-size-bytes", 0u32)
        .property("max-size-time", 2_000_000_000u64)
        .build()
        .map_err(|e| format!("queue build: {e}"))?;
    queue.set_property_from_str("leaky", "downstream");
    let encoder = gst::ElementFactory::make("nvcudah264enc")
        .property("name", "nvenc_branch_b")
        .property("zero-reorder-delay", true)
        .property("b-frames", 0u32)
        .property("bitrate", 4000u32)
        .property("gop-size", super::rtsp::transcoder_key_int_max(source_fps) as i32)
        .build()
        .map_err(|e| format!("nvcudah264enc build: {e}"))?;
    encoder.set_property_from_str("preset", "p3");
    encoder.set_property_from_str("tune", "ultra-low-latency");
    encoder.set_property_from_str("rate-control", "cbr");
    let parse = gst::ElementFactory::make("h264parse")
        .property_from_str("config-interval", "-1")
        .build()
        .map_err(|e| format!("h264parse build: {e}"))?;
    let mux = gst::ElementFactory::make("mp4mux")
        .property("fragment-duration", 100u32)
        .property("streamable", true)
        .build()
        .map_err(|e| format!("mp4mux build: {e}"))?;
    let sink = gst::ElementFactory::make("appsink")
        .property("name", "sink_mp4_encoded")
        .property("emit-signals", false)
        .property("sync", false)
        .property("max-buffers", 8u32)
        .property("drop", false)
        .build()
        .map_err(|e| format!("appsink build: {e}"))?;
    let elements = vec![queue, encoder, parse, mux, sink];
    pipeline
        .add_many(elements.iter())
        .map_err(|e| format!("add_many encoded branch B: {e}"))?;
    let tee_src_pad = tee_cuda.request_pad_simple("src_%u").ok_or_else(|| {
        let _ = pipeline.remove_many(elements.iter());
        "tee_cuda src_%u request for branch B failed".to_string()
    })?;
    let state = Mp4BranchState {
        tee_src_pad,
        elements,
    };
    if let Err(e) = wire_and_link_encoded_branch(&state, publisher) {
        detach_mp4_branch(pipeline, tee_cuda, state);
        return Err(e);
    }
    Ok(state)
}

/// Link, wire and start the encoded Branch B; the tee pad is linked LAST for the
/// same reason as in `wire_and_link_webrtc_branch` (a push into a not-yet-active
/// queue marks the tee pad dead). The PTS base is taken at the branch INPUT,
/// before the encoder shifts timestamps, so it sits on the detection axis.
#[cfg(all(
    any(target_os = "linux", target_os = "windows"),
    feature = "inference-vision-gpu",
    feature = "vision-ort",
    feature = "vision-cuda-preprocess"
))]
fn wire_and_link_encoded_branch(
    state: &Mp4BranchState,
    publisher: &Arc<Mp4StreamPublisher>,
) -> std::result::Result<(), String> {
    let queue = &state.elements[0];
    let sink = &state.elements[4];
    gst::Element::link_many(state.elements.iter()).map_err(|e| format!("link encoded branch B: {e}"))?;
    wire_mp4_appsink(sink, publisher)?;
    super::rtsp::install_branch_input_base_pts_probe(queue, publisher);
    for el in &state.elements {
        el.sync_state_with_parent()
            .map_err(|e| format!("sync_state encoded branch B element: {e}"))?;
    }
    let queue_sink = queue
        .static_pad("sink")
        .ok_or_else(|| "encoded branch B queue sink pad missing".to_string())?;
    state
        .tee_src_pad
        .link(&queue_sink)
        .map_err(|e| format!("tee_cuda → encoded branch B: {e:?}"))?;
    Ok(())
}

/// Link Branch B's elements, wire the mux appsink to the publisher, and bring
/// the branch up to the pipeline's current state. Split out from
/// `attach_mp4_branch_webrtc` so a single error path can drive the shared
/// detach/cleanup on any failure after the tee request pad exists. The element
/// order in `state.elements` is `[queue_b, parse, timestamper, mux, sink]`.
fn wire_and_link_webrtc_branch(
    state: &Mp4BranchState,
    publisher: &Arc<Mp4StreamPublisher>,
) -> std::result::Result<(), String> {
    let queue_b = &state.elements[0];
    let parse = &state.elements[1];
    let sink = &state.elements[4];

    // Permanent buffer gate on the parse SINK pad. Two jobs:
    //
    // 1. Keyframe-sync at attach. Branch B attaches mid-GOP, so the first buffers
    //    off the tee are bare P-slices (NAL type 1). A standalone h264parse cannot
    //    frame those before an IDR: it would push before negotiating src caps and
    //    gstbaseparse posts a FATAL "No caps set ... GstH264Parse" bus error that
    //    tears down the WHOLE shared pipeline (Branch A included). We drop every
    //    buffer until the upstream gate's coalesced SPS+PPS+IDR keyframe AU (NAL
    //    type 5) arrives. Gating on the IDR (not a standalone SPS) is essential:
    //    an SPS-only AU has num_pps=0, the parse could not build avcC codec_data,
    //    and it would die exactly as before.
    //
    // 2. Drop param-only access units forever after. Mid-stream the upstream gate
    //    re-emits standalone SPS and PPS in their OWN au-aligned buffers (so
    //    Branch A's decodebin can re-sync after a drop). Those have no coded
    //    slice. If they reach mp4mux they become samples with no picture, and
    //    the browser/ffmpeg decoder reports "missing picture in access unit" /
    //    "no frame" → MEDIA_ERR_DECODE, which set HTMLMediaElement.error and
    //    triggered the client's ~4s reset cycle. The keyframe AU already carries
    //    SPS+PPS inline (coalesced) and h264parse config-interval=-1 re-inserts
    //    them before every keyframe, so dropping the standalone param-only AUs
    //    loses nothing the muxed stream needs. We keep only AUs that contain a
    //    VCL slice (NAL types 1..=5).
    let parse_sink = parse
        .static_pad("sink")
        .ok_or_else(|| "branch B h264parse sink pad missing".to_string())?;
    let synced = std::sync::atomic::AtomicBool::new(false);
    parse_sink.add_probe(gst::PadProbeType::BUFFER, move |_pad, info| {
        let Some(buffer) = info.buffer() else {
            return gst::PadProbeReturn::Ok;
        };
        let Ok(map) = buffer.map_readable() else {
            return gst::PadProbeReturn::Drop;
        };
        let data = map.as_slice();
        if !synced.load(std::sync::atomic::Ordering::Relaxed) {
            if annexb_contains_idr(data) {
                synced.store(true, std::sync::atomic::Ordering::Relaxed);
                return gst::PadProbeReturn::Ok;
            }
            return gst::PadProbeReturn::Drop;
        }
        // Post-sync: forward only slice-bearing access units; drop param-only
        // (SPS/PPS/SEI/AUD) buffers that would mux as picture-less samples.
        if annexb_contains_vcl_slice(data) {
            gst::PadProbeReturn::Ok
        } else {
            gst::PadProbeReturn::Drop
        }
    });

    let queue_b_sink = queue_b
        .static_pad("sink")
        .ok_or_else(|| "queue_b sink pad missing".to_string())?;
    gst::Element::link_many(state.elements.iter()).map_err(|e| format!("link branch B: {e}"))?;

    wire_mp4_appsink(sink, publisher)?;

    // Bring every new element up to the pipeline's current state so the mux
    // branch starts producing without a full pipeline restart.
    for el in &state.elements {
        el.sync_state_with_parent()
            .map_err(|e| format!("sync_state branch B element: {e}"))?;
    }

    // Pad tee linkujemy DOPIERO po aktywacji całej gałęzi. Push tee w okno
    // między linkiem a aktywacją queue_b zwraca FLUSHING, a tee trwale
    // oznacza taki pad jako usunięty i nigdy więcej do niego nie pcha —
    // gałąź wygląda na wpiętą, ale mux nie dostaje ani bajta i init segment
    // nigdy nie powstaje.
    state
        .tee_src_pad
        .link(&queue_b_sink)
        .map_err(|e| format!("tee → queue_b: {e:?}"))?;
    Ok(())
}

/// Return true if an Annex-B byte-stream buffer contains an IDR slice (NAL type
/// 5). The upstream gate coalesces SPS+PPS+IDR into the keyframe buffer, so an
/// IDR-bearing buffer is a self-contained access unit from which h264parse can
/// build avcC codec_data. We scan for `00 00 01` start codes (the 4-byte
/// `00 00 00 01` prefix shares this 3-byte suffix) and read the NAL header's low
/// 5 bits.
fn annexb_contains_idr(data: &[u8]) -> bool {
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if data[i + 3] & 0x1F == 5 {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

/// Return true if an Annex-B byte-stream buffer contains a coded VCL slice (NAL
/// types 1..=5: non-IDR/partition/IDR slices). A buffer with only parameter sets
/// (SPS=7/PPS=8), SEI (6) or AUD (9) carries no picture, so muxing it as its own
/// access unit yields a sample the decoder rejects ("missing picture in access
/// unit"). Dropping such param-only AUs from the mux branch keeps every muxed
/// sample a real frame. Scans `00 00 01` start codes (the 4-byte prefix shares
/// the 3-byte suffix) and reads the NAL header's low 5 bits.
fn annexb_contains_vcl_slice(data: &[u8]) -> bool {
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let t = data[i + 3] & 0x1F;
            if (1..=5).contains(&t) {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

/// Detach Branch B from the webrtc pipeline. Delegates to the shared RTSP
/// teardown — the unlink/NULL/remove/release-pad sequence is source-agnostic.
pub(super) fn detach_mp4_branch_webrtc(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    state: Mp4BranchState,
) {
    detach_mp4_branch(pipeline, tee, state);
}

/// Pump Annex-B chunks from the channel into the current appsrc until the stream
/// ends (sender dropped) or the pipeline rejects a push (torn down). Sends EOS
/// on exit so the pipeline can shut down cleanly.
pub async fn webrtc_pump(mut rx: mpsc::Receiver<bytes::Bytes>, appsrc: Arc<AppSrcSlot>) {
    while let Some(chunk) = rx.recv().await {
        let buffer = gst::Buffer::from_slice(chunk);
        // The slot is swapped when the session rebuilds the pipeline (privacy
        // options changed): the robot's stream keeps flowing into whatever
        // pipeline is current, no reconnect needed. While the old pipeline goes
        // to NULL its appsrc is flushing, so those pushes fail — dropping those
        // buffers is exactly right (the new graph starts at the next keyframe),
        // and ending the pump there would mean a settings change silently kills
        // the robot's video. Every session exit aborts this task, so only a
        // genuine flow error ends it.
        match appsrc.lock().push_buffer(buffer) {
            Ok(_) | Err(gst::FlowError::Flushing) | Err(gst::FlowError::Eos) => {}
            Err(_) => break,
        }
    }
    let _ = appsrc.lock().end_of_stream();
}

/// The appsrc the pump currently feeds. Behind a mutex because the session task
/// swaps it (pipeline rebuild) while the pump pushes from another task.
pub type AppSrcSlot = parking_lot::Mutex<gst_app::AppSrc>;

#[cfg(test)]
mod tests {
    use super::{annexb_contains_idr, annexb_contains_vcl_slice};

    fn nal(ty: u8) -> Vec<u8> {
        vec![0, 0, 0, 1, ty & 0x1F, 0xAA, 0xBB]
    }

    #[test]
    fn idr_detected_in_coalesced_keyframe() {
        // A coalesced SPS(7)+PPS(8)+IDR(5) keyframe buffer must report an IDR.
        let mut buf = nal(7);
        buf.extend_from_slice(&nal(8));
        buf.extend_from_slice(&nal(5));
        assert!(annexb_contains_idr(&buf));
    }

    #[test]
    fn idr_absent_in_param_only_and_pslice_buffers() {
        // Standalone SPS+PPS (no IDR) must NOT pass the gate.
        let mut params = nal(7);
        params.extend_from_slice(&nal(8));
        assert!(!annexb_contains_idr(&params));
        // A lone P-slice (type 1) must NOT pass.
        assert!(!annexb_contains_idr(&nal(1)));
    }

    #[test]
    fn idr_detected_with_3byte_start_code() {
        // 3-byte start code variant: 00 00 01 <nal-header>.
        let buf = vec![0, 0, 1, 5, 0x11];
        assert!(annexb_contains_idr(&buf));
    }

    #[test]
    fn vcl_slice_present_in_frames_absent_in_param_only() {
        // Coalesced SPS+PPS+IDR keyframe → has a VCL slice (IDR=5).
        let mut key = nal(7);
        key.extend_from_slice(&nal(8));
        key.extend_from_slice(&nal(5));
        assert!(annexb_contains_vcl_slice(&key));
        // A lone P-slice (type 1) → VCL present.
        assert!(annexb_contains_vcl_slice(&nal(1)));
        // Standalone SPS+PPS (the mid-stream param-only AU) → NO VCL slice.
        let mut params = nal(7);
        params.extend_from_slice(&nal(8));
        assert!(!annexb_contains_vcl_slice(&params));
        // SEI(6) and AUD(9) only → NO VCL slice.
        assert!(!annexb_contains_vcl_slice(&nal(6)));
        assert!(!annexb_contains_vcl_slice(&nal(9)));
    }
}

/// End-to-end check of the privacy path on real GPU hardware: an H.264 clip of a
/// photo with people runs through the SAME pipeline a robot camera uses, once
/// without and once with privacy. Needs NVDEC/NVENC, the YOLOX-tiny and YuNet
/// models (`privacy-cv` bundle) and a test photo with people
/// (`TENTAFLOW_PRIVACY_TEST_IMAGE`, e.g. Ultralytics `bus.jpg`):
/// `cargo test --release -p tentaflow-core --features vision-cuda --lib privacy_path -- --ignored --nocapture`
#[cfg(all(
    test,
    any(target_os = "linux", target_os = "windows"),
    feature = "inference-vision-gpu",
    feature = "vision-ort",
    feature = "vision-cuda-preprocess"
))]
mod privacy_path_tests {
    use super::*;
    use crate::services::camera_ingest::privacy_options::PrivacyOptions;
    use std::time::Duration;

    const W: usize = 1280;
    const H: usize = 720;

    /// Encode the photo as an H.264 clip of `frames` access units (Annex-B, AU
    /// aligned — exactly what the robot's depacketizer delivers).
    fn encode_clip(image: &str, frames: u32) -> Vec<gst::Buffer> {
        ensure_gst_initialized().unwrap();
        let desc = format!(
            "filesrc location={image} ! decodebin ! videoconvert ! videoscale ! \
             video/x-raw,width={W},height={H},format=I420 ! imagefreeze num-buffers={frames} ! \
             video/x-raw,framerate=25/1 ! x264enc key-int-max=25 bframes=0 tune=zerolatency \
             speed-preset=ultrafast ! h264parse ! video/x-h264,stream-format=byte-stream,alignment=au ! \
             appsink name=out sync=false"
        );
        let pipeline = gst::parse::launch(&desc)
            .unwrap()
            .downcast::<gst::Pipeline>()
            .unwrap();
        let sink = pipeline
            .by_name("out")
            .unwrap()
            .downcast::<gst_app::AppSink>()
            .unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        let mut aus = Vec::new();
        while let Ok(sample) = sink.pull_sample() {
            aus.push(sample.buffer_owned().unwrap());
        }
        pipeline.set_state(gst::State::Null).unwrap();
        aus
    }

    /// Run the clip through `build_webrtc_pipeline` and return the last RGB
    /// frame the mailbox received.
    async fn run(aus: &[gst::Buffer], camera_id: &str, privacy: PrivacyOptions) -> Vec<u8> {
        let mut config = CameraConfig::new_unowned(camera_id, "webrtc", "test", 25, None);
        config.privacy = privacy;
        let mailbox = Arc::new(FrameMailbox::new());
        let counters = Arc::new(FrameCounters::new());
        let (p, appsrc, _fanout) =
            build_webrtc_pipeline(&config, mailbox.clone(), counters).unwrap();
        p.pipeline.set_state(gst::State::Playing).unwrap();
        for au in aus {
            appsrc.push_buffer(au.copy()).unwrap();
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        let frame = mailbox.get().expect("a frame reached the mailbox");
        p.pipeline.set_state(gst::State::Null).unwrap();
        assert_eq!((frame.width as usize, frame.height as usize), (W, H));
        frame.data.to_vec()
    }

    /// Sum of absolute horizontal luma-ish differences inside a box — pixelation
    /// flattens it to (almost) nothing except at block edges.
    fn edge_energy(rgb: &[u8], x0: usize, y0: usize, w: usize, h: usize) -> f64 {
        let mut sum = 0u64;
        for y in y0..(y0 + h).min(H) {
            for x in x0..(x0 + w).min(W).saturating_sub(1) {
                let a = rgb[(y * W + x) * 3 + 1] as i32;
                let b = rgb[(y * W + x + 1) * 3 + 1] as i32;
                sum += (a - b).unsigned_abs() as u64;
            }
        }
        sum as f64
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs NVDEC/NVENC, the privacy-cv models and TENTAFLOW_PRIVACY_TEST_IMAGE"]
    async fn privacy_path_blurs_heads_and_publishes_persons() {
        let image = std::env::var("TENTAFLOW_PRIVACY_TEST_IMAGE")
            .expect("set TENTAFLOW_PRIVACY_TEST_IMAGE to a photo with people");
        let aus = encode_clip(&image, 50);
        assert!(aus.len() >= 40, "clip encoded ({} AUs)", aus.len());

        let reference = run(&aus, "privacy-test-ref", PrivacyOptions::OFF).await;

        let mut detections = crate::services::detection_bus::subscribe("privacy-test-cam");
        let blurred = run(
            &aus,
            "privacy-test-cam",
            PrivacyOptions {
                person_detect: true,
                face_blur: true,
            },
        )
        .await;

        // `TENTAFLOW_PRIVACY_TEST_OUT=<dir>` keeps both frames as PNG for a
        // visual check of what a viewer of the robot camera would see.
        if let Ok(dir) = std::env::var("TENTAFLOW_PRIVACY_TEST_OUT") {
            for (name, rgb) in [("reference.png", &reference), ("anonymized.png", &blurred)] {
                image::RgbImage::from_raw(W as u32, H as u32, rgb.clone())
                    .unwrap()
                    .save(std::path::Path::new(&dir).join(name))
                    .unwrap();
            }
        }

        let mut persons = Vec::new();
        while let Ok(msg) = detections.try_recv() {
            persons = msg.items;
        }
        println!("persons in the last published frame: {}", persons.len());
        assert!(!persons.is_empty(), "the probe published person detections");
        assert!(persons.iter().all(|p| p.klasa == "person" && p.track_id != 0));

        for p in &persons {
            let head = super::super::privacy::head_region(p.bbox, W as u32, H as u32);
            let (x, y, w, h) = (head.x as usize, head.y as usize, head.w as usize, head.h as usize);
            let before = edge_energy(&reference, x, y, w, h);
            let after = edge_energy(&blurred, x, y, w, h);
            println!("head {w}x{h}: edge energy {before:.0} -> {after:.0}");
            assert!(after < before * 0.35, "head region is pixelated");
        }

        // Far from every person (the top-left corner is sky/building in bus.jpg
        // and holds no head region) the picture is untouched apart from the
        // encode/decode round trip through NVENC.
        let untouched = edge_energy(&blurred, 0, 0, 120, 60);
        let original = edge_energy(&reference, 0, 0, 120, 60);
        println!("background edge energy {original:.0} -> {untouched:.0}");
        assert!(untouched > original * 0.6, "background is not pixelated");
    }

    /// Turning anonymization on must take effect on a RUNNING camera: the
    /// session rebuilds its pipeline while the robot's stream keeps flowing, so
    /// an admin does not have to reconnect the robot for "blur faces" to mean
    /// anything. Asserts the live switch both ways round: no detections and a
    /// sharp picture before, detections and pixelated heads after.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs NVDEC/NVENC, the privacy-cv models and TENTAFLOW_PRIVACY_TEST_IMAGE"]
    async fn privacy_options_apply_to_a_running_camera() {
        let image = std::env::var("TENTAFLOW_PRIVACY_TEST_IMAGE")
            .expect("set TENTAFLOW_PRIVACY_TEST_IMAGE to a photo with people");
        let aus = encode_clip(&image, 50);
        let camera_id = "privacy-live-cam";

        let (tx, rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(120);
        let mut config = CameraConfig::new_unowned(camera_id, "webrtc", "live", 25, None);
        config.privacy = PrivacyOptions::OFF;
        let handle = super::super::session::spawn_webrtc_session(config, rx).unwrap();

        // Feed the clip on a loop for the whole test, as a live robot would.
        let feeder = tokio::spawn(async move {
            loop {
                for au in &aus {
                    let bytes = bytes::Bytes::copy_from_slice(&au.map_readable().unwrap());
                    if tx.send(bytes).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(40)).await;
                }
            }
        });

        let sharp = snapshot(&handle).await;
        let mut detections = crate::services::detection_bus::subscribe(camera_id);
        assert!(
            detections.try_recv().is_err(),
            "nothing publishes detections for this camera while privacy is off"
        );

        handle
            .cmd_tx
            .send(super::super::session::SessionCommand::SetPrivacy(
                PrivacyOptions {
                    person_detect: true,
                    face_blur: true,
                },
            ))
            .await
            .unwrap();

        // The rebuilt pipeline needs the next keyframe of the live stream, then
        // a decoded frame through the detector — wait for the first published
        // detection rather than guessing a sleep.
        let mut persons = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout_at(deadline, detections.recv()).await {
                Ok(Ok(msg)) if !msg.items.is_empty() => {
                    persons = msg.items;
                    break;
                }
                Ok(Ok(_)) => continue,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(_)) | Err(_) => break,
            }
        }
        // The detection is published from the probe itself; the mailbox frame
        // behind `snapshot` comes from the decimated host branch a moment later,
        // so give that branch time or the snapshot is still the pre-switch one.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let blurred = snapshot(&handle).await;
        assert!(
            !persons.is_empty(),
            "the rebuilt pipeline publishes person detections"
        );
        for p in &persons {
            let head = super::super::privacy::head_region(p.bbox, W as u32, H as u32);
            let (x, y, w, h) = (head.x as usize, head.y as usize, head.w as usize, head.h as usize);
            let before = edge_energy(&sharp, x, y, w, h);
            let after = edge_energy(&blurred, x, y, w, h);
            println!("live switch, head {w}x{h}: edge energy {before:.0} -> {after:.0}");
            assert!(after < before * 0.35, "head pixelated after the live switch");
        }

        feeder.abort();
        let _ = handle
            .cmd_tx
            .send(super::super::session::SessionCommand::Stop)
            .await;
    }

    /// Latest frame of a running session, through the session's own snapshot
    /// command (the path the dashboard uses).
    async fn snapshot(handle: &super::super::session::CameraHandle) -> Vec<u8> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        handle
            .cmd_tx
            .send(super::super::session::SessionCommand::Snapshot(reply_tx))
            .await
            .unwrap();
        let snap = reply_rx.await.unwrap().expect("a frame reached the mailbox");
        assert_eq!((snap.width as usize, snap.height as usize), (W, H));
        snap.data
    }
}
