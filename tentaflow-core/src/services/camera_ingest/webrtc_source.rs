// =============================================================================
// File: services/camera_ingest/webrtc_source.rs
// Purpose: Camera source backed by a WebRTC video track. An H.264 Annex-B byte
//          stream (depacketized in tentaflow-hardware, delivered over an mpsc)
//          is pushed into a GStreamer appsrc, parsed once, then fanned out by a
//          tee into Branch A (decode → RGB → appsink, the always-on frame path
//          shared with every other camera) and an on-demand Branch B
//          (mp4mux → appsink) that feeds the fMP4 publisher for smooth MSE
//          playback. This is the only appsrc pipeline in the repo.
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
pub fn build_webrtc_pipeline(
    config: &CameraConfig,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<(FakeFilePipeline, gst_app::AppSrc, gst::Element)> {
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
            &appsrc,
            &tee,
            &queue_a,
            &decodebin,
            &convert,
            &capsfilter,
            &appsink,
        ])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("add_many: {e}")))?;

    // Static segments: appsrc → tee, and convert → capsfilter → appsink.
    // tee.src_0 → queue_a → decodebin and decodebin → convert are wired by
    // request pad / dynamic pad below.
    gst::Element::link(&appsrc, &tee)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("appsrc → tee: {e}")))?;
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
    install_frame_callback(&appsink_app, config.camera_id.clone(), mailbox, counters);

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
        tee,
    ))
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

/// Pump Annex-B chunks from the channel into the appsrc until the stream ends
/// (sender dropped) or the pipeline rejects a push (torn down). Sends EOS on
/// exit so the pipeline can shut down cleanly.
pub async fn webrtc_pump(mut rx: mpsc::Receiver<bytes::Bytes>, appsrc: gst_app::AppSrc) {
    while let Some(chunk) = rx.recv().await {
        let buffer = gst::Buffer::from_slice(chunk);
        if appsrc.push_buffer(buffer).is_err() {
            break;
        }
    }
    let _ = appsrc.end_of_stream();
}

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
