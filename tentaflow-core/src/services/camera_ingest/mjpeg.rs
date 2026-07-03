// =============================================================================
// Plik: services/camera_ingest/mjpeg.rs — konektor kamer MJPEG po HTTP
// Opis: Pipeline GStreamer dla kamer strumieniujących MJPEG przez HTTP
//       (multipart/x-mixed-replace, np. Axis: http://IP/axis-cgi/mjpg/video.cgi).
//       Pętlę sesji (reconnect/backoff/health) współdzieli z RTSP —
//       `run_rtsp_session` dobiera builder pipeline'u wg `vendor`.
// =============================================================================
//
// Pipeline:
//   souphttpsrc location=<url> is-live=true ! multipartdemux ! jpegparse !
//     tee name=mjpeg_tee
//   Gałąź A (analiza, zawsze obecna):
//     tee ! queue ! jpegdec ! queue leaky=downstream max-size-buffers=5 !
//       videoconvert ! video/x-raw,format=RGB ! appsink
//   Gałąź B (podgląd MSE, dowieszana na żądanie — MSE wymaga H.264, więc
//   transkodujemy JPEG → H.264 na CPU):
//     tee ! queue ! jpegdec ! queue leaky=downstream max-size-buffers=5 !
//       videoconvert ! x264enc tune=zerolatency
//       speed-preset=veryfast bitrate=4000 key-int-max=50 ! h264parse !
//       mp4mux (fMP4) ! appsink
//
//   Kolejki leaky stoją ZA jpegdec (nie przed) — przy spiętrzeniu gubimy
//   całe zdekodowane klatki, nigdy dane wejściowe dekodera.
//
// Gałąź A używa DOKŁADNIE tego samego kontraktu appsink (RGB24, LatestFrame,
// pts_ns) co RTSP — `rtsp::build_appsink` instaluje wspólny callback, więc
// FrameStorage / StreamingBus / detekcje działają bez zmian. Gałąź B reużywa
// `rtsp::wire_mp4_appsink` i probe bazy PTS na wejściu gałęzi
// (`install_branch_input_base_pts_probe` — oba warianty transkodują przez
// x264enc, który przesuwa timestampy), więc kontrakt publishera fMP4
// (init segment + fragmenty) jest identyczny.

use std::sync::Arc;

use gstreamer as gst;
use gstreamer::prelude::*;

use super::credentials::credentials_cipher;
use super::error::{CameraIngestError, Result};
use super::fakefile::{FrameCounters, FrameMailbox};
use super::rtsp::{
    build_appsink, build_raw_leaky_queue, install_branch_input_base_pts_probe,
    transcoder_key_int_max, wire_mp4_appsink, Mp4BranchState, RtspPipelineHandles,
    detach_mp4_branch,
};
use super::session::CameraConfig;
use super::stream_publisher::Mp4StreamPublisher;

/// Walidacja URL-a MJPEG — akceptujemy wyłącznie http:// i https:// z
/// niepustym hostem. Resztę (ścieżka CGI, query) zostawiamy souphttpsrc.
pub fn validate_mjpeg_url(url: &str) -> Result<()> {
    if url.is_empty() {
        return Err(CameraIngestError::InvalidUrl("empty".into()));
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(CameraIngestError::InvalidUrl(
            "missing http:// or https:// scheme".into(),
        ));
    }
    let after_scheme = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or("");
    if after_scheme.is_empty() {
        return Err(CameraIngestError::InvalidUrl("missing host".into()));
    }
    Ok(())
}

/// Odszyfrowuje blob poświadczeń kamery do pary `(user, password)` dla
/// souphttpsrc (`user-id`/`user-pw`). Blob przechowuje plaintext w formie
/// `user:pass` — tej samej, którą RTSP nakłada na URL. Hasło może zawierać
/// `:` (dzielimy na PIERWSZYM dwukropku). `None` gdy kamera nie ma
/// poświadczeń. Plaintext żyje tylko na stosie tej funkcji i callera.
pub(super) fn resolve_http_credentials(
    config: &CameraConfig,
) -> std::result::Result<Option<(String, String)>, String> {
    let Some(blob) = config.credentials_encrypted.as_ref() else {
        return Ok(None);
    };
    let creds = credentials_cipher()
        .decrypt(blob)
        .map_err(|e| e.to_string())?;
    let Some((user, pass)) = creds.split_once(':') else {
        return Err("creds must be user:pass".into());
    };
    Ok(Some((user.to_string(), pass.to_string())))
}

/// Buduje pipeline MJPEG. Zwraca te same uchwyty co builder RTSP
/// (`pipeline` + `tee`), więc pętla sesji i attach/detach gałęzi B działają
/// bez rozgałęzień poza wyborem buildera. `tee` niesie sparsowane ramki
/// `image/jpeg` (po jpegparse) — każda ramka JPEG jest samodzielna
/// (keyframe), więc fan-out przed dekodem jest bezpieczny.
pub(super) fn build_mjpeg_pipeline(
    camera_id: String,
    url: &str,
    creds: Option<&(String, String)>,
    timeout_secs: u32,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<RtspPipelineHandles> {
    let pipeline = gst::Pipeline::new();

    // Źródło HTTP. `is-live=true` — strumień na żywo (bez seek, timestamps od
    // zegara). `retries=0` — retry zarządzamy na poziomie sesji (ten sam
    // backoff co RTSP), wewnętrzny retry souphttpsrc tylko by go maskował.
    let src = gst::ElementFactory::make("souphttpsrc")
        .property("location", url)
        .property("is-live", true)
        .property("timeout", timeout_secs)
        .property("retries", 0i32)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("souphttpsrc: {e}")))?;
    if let Some((user, pass)) = creds {
        src.set_property("user-id", user);
        src.set_property("user-pw", pass);
    }

    // multipart/x-mixed-replace → pojedyncze części (ramki JPEG). Pad src
    // powstaje dynamicznie po odczycie pierwszej części strumienia.
    let demux = gst::ElementFactory::make("multipartdemux")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("multipartdemux: {e}")))?;

    // jpegparse pilnuje granic ramek i uzupełnia caps (width/height/framerate)
    // zanim strumień trafi na tee.
    let jpegparse = gst::ElementFactory::make("jpegparse")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("jpegparse: {e}")))?;

    // Fan-out JPEG między gałąź A (analiza) i dowieszaną gałąź B (fMP4).
    // `allow-not-linked=true` toleruje okresy bez gałęzi B — jak w RTSP.
    let tee = gst::ElementFactory::make("tee")
        .property("name", "mjpeg_tee")
        .property("allow-not-linked", true)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("tee: {e}")))?;

    // Kolejka gałęzi A — odcina dekod od fan-outu. NIE-leaky: gubienie klatek
    // przy spiętrzeniu odbywa się dopiero ZA jpegdec (queue_dec niżej), gdzie
    // bufor to pełna zdekodowana klatka — ta sama zasada co w RTSP.
    let queue_a = gst::ElementFactory::make("queue")
        .property("name", "queue_branch_a")
        .property("max-size-buffers", 30u32)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("queue_a: {e}")))?;

    let jpegdec = gst::ElementFactory::make("jpegdec")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("jpegdec: {e}")))?;
    // Kolejka leaky ZA dekoderem — zrzuca najstarsze zdekodowane klatki gdy
    // konsument nie nadąża (analiza i tak bierze tylko najnowszą klatkę).
    let queue_dec = build_raw_leaky_queue("queue_decoded_a")?;
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
    // Wspólny appsink RTSP/MJPEG — ten sam callback ramki (LatestFrame,
    // pts_ns, FrameStorage, StreamingBus).
    let appsink = build_appsink(camera_id, mailbox, counters)?;

    pipeline
        .add_many([
            &src,
            &demux,
            &jpegparse,
            &tee,
            &queue_a,
            &jpegdec,
            &queue_dec,
            &convert,
            &capsfilter,
            &appsink,
        ])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("add_many mjpeg: {e}")))?;

    // Segmenty statyczne: src → demux oraz jpegparse → tee → queue_a → ogon
    // gałęzi A. Segment demux → jpegparse jest dynamiczny (pad-added niżej).
    gst::Element::link(&src, &demux)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("src → demux: {e}")))?;
    gst::Element::link(&jpegparse, &tee)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("jpegparse → tee: {e}")))?;
    let tee_src_a = tee
        .request_pad_simple("src_%u")
        .ok_or_else(|| CameraIngestError::PipelineBuild("tee src_%u request failed".into()))?;
    let queue_a_sink = queue_a
        .static_pad("sink")
        .ok_or_else(|| CameraIngestError::PipelineBuild("queue_a sink pad missing".into()))?;
    tee_src_a
        .link(&queue_a_sink)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("tee → queue_a: {e:?}")))?;
    gst::Element::link_many([&queue_a, &jpegdec, &queue_dec, &convert, &capsfilter, &appsink])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("link_many tail: {e}")))?;

    // multipartdemux tworzy pad per część multipart — kamera MJPEG serwuje
    // jeden strumień obrazów, więc linkujemy pierwszy pad z caps image/jpeg.
    // Kolejne pady (nietypowe serwery mieszające typy) ignorujemy z logiem.
    let parse_weak = jpegparse.downgrade();
    demux.connect_pad_added(move |_demux, src_pad| {
        let Some(parse) = parse_weak.upgrade() else {
            return;
        };
        let Some(sink_pad) = parse.static_pad("sink") else {
            return;
        };
        if sink_pad.is_linked() {
            tracing::debug!("mjpeg: kolejny pad multipartdemux zignorowany (już zlinkowano)");
            return;
        }
        if let Some(caps) = src_pad.current_caps() {
            let is_jpeg = caps
                .structure(0)
                .map(|s| s.name().starts_with("image/jpeg"))
                .unwrap_or(false);
            if !is_jpeg {
                tracing::warn!(
                    caps = %caps,
                    "mjpeg: multipartdemux wyeksponował część inną niż image/jpeg — pomijam"
                );
                return;
            }
        }
        if let Err(e) = src_pad.link(&sink_pad) {
            tracing::warn!("mjpeg: multipartdemux → jpegparse link nieudany: {e:?}");
        } else {
            tracing::info!("mjpeg: pad image/jpeg zlinkowany");
        }
    });

    tracing::info!("mjpeg: pipeline zbudowany (souphttpsrc → multipartdemux → jpegparse → tee)");
    Ok(RtspPipelineHandles { pipeline, tee })
}

/// Dowiesza gałąź B (fMP4/MSE) do działającego pipeline'u MJPEG. Przeglądarka
/// (MSE) wymaga H.264, więc transkodujemy: jpegdec → videoconvert → x264enc.
/// Kontrakt publishera (init segment, fragmenty, baza PTS) jest ten sam co w
/// RTSP — reużywamy `wire_mp4_appsink`; bazę PTS zdejmuje
/// `install_branch_input_base_pts_probe` na wejściu gałęzi (oś detekcji),
/// bo x264enc przesuwa timestampy za sobą o stały offset.
/// Gałąź jest budowana od zera przy każdym attachu, więc x264enc zaczyna od
/// klatki kluczowej — init segment MSE jest kompletny bez force-key-unit.
/// `preview = true` to wariant kafelków Live view: dokładamy videoscale do
/// 1280x720 i obniżamy bitrate do 1,5 Mbit/s, żeby nie saturować łącza WAN;
/// pełna jakość (`false`) transkoduje w natywnej rozdzielczości przy 4 Mbit/s.
/// Oba warianty mogą wisieć na tee równocześnie (osobne sloty w sesji), stąd
/// sufiks nazw elementów per wariant.
pub(super) fn attach_mp4_branch_mjpeg(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    publisher: &Arc<Mp4StreamPublisher>,
    preview: bool,
    source_fps: u32,
) -> std::result::Result<Mp4BranchState, String> {
    let name_suffix = if preview { "_preview" } else { "" };
    // Kolejka gałęzi B — NIE-leaky przed dekoderem; gubienie przy spiętrzeniu
    // dopiero ZA jpegdec (queue_dec niżej), na pełnych zdekodowanych klatkach.
    // Drop klatki przed x264enc jest bezpieczny dla fMP4 — enkoder generuje
    // spójny strumień z tego, co dostanie.
    let queue_b = gst::ElementFactory::make("queue")
        .property("name", format!("queue_branch_b{name_suffix}"))
        .property("max-size-buffers", 30u32)
        .build()
        .map_err(|e| format!("queue_b build: {e}"))?;
    let jpegdec = gst::ElementFactory::make("jpegdec")
        .build()
        .map_err(|e| format!("jpegdec build: {e}"))?;
    let queue_dec = build_raw_leaky_queue(&format!("queue_decoded_b{name_suffix}"))
        .map_err(|e| e.to_string())?;
    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(|e| format!("videoconvert build: {e}"))?;
    // Wariant podglądu skaluje do 720p przed enkoderem — kafelki są małe,
    // pełna rozdzielczość marnowałaby pasmo i CPU enkodera.
    let scaler = if preview {
        let scale = gst::ElementFactory::make("videoscale")
            .build()
            .map_err(|e| format!("videoscale build: {e}"))?;
        let caps = gst::Caps::builder("video/x-raw")
            .field("width", 1280i32)
            .field("height", 720i32)
            .build();
        let filter = gst::ElementFactory::make("capsfilter")
            .property("caps", &caps)
            .build()
            .map_err(|e| format!("preview capsfilter build: {e}"))?;
        Some((scale, filter))
    } else {
        None
    };
    // Transkoder CPU: zerolatency (bez opóźnienia B-ramek — podgląd live),
    // veryfast (niski koszt CPU), 4 Mbit/s (pełna jakość) albo 1,5 Mbit/s
    // (podgląd 720p), keyframe co ~2 s (skalowane do fps źródła) — MSE
    // potrzebuje regularnych punktów wejścia.
    let enc = gst::ElementFactory::make("x264enc")
        .property("bitrate", if preview { 1500u32 } else { 4000u32 })
        .property("key-int-max", transcoder_key_int_max(source_fps))
        .build()
        .map_err(|e| format!("x264enc build: {e}"))?;
    enc.set_property_from_str("tune", "zerolatency");
    enc.set_property_from_str("speed-preset", "veryfast");
    // mp4mux wymaga AVC (NALU z prefiksem długości) — jak w gałęzi B RTSP.
    let parse = gst::ElementFactory::make("h264parse")
        .property_from_str("config-interval", "-1")
        .build()
        .map_err(|e| format!("h264parse build: {e}"))?;
    // Te same parametry fMP4 co RTSP: ftyp+moov na pierwszym fragmencie
    // (init segment MSE), fragmenty moof+mdat co 200 ms.
    let mux = gst::ElementFactory::make("mp4mux")
        .property("fragment-duration", 200u32)
        .property("streamable", true)
        .build()
        .map_err(|e| format!("mp4mux build: {e}"))?;
    let sink = gst::ElementFactory::make("appsink")
        .property("name", format!("sink_mp4{name_suffix}"))
        .property("emit-signals", false)
        .property("sync", false)
        .property("max-buffers", 8u32)
        .property("drop", false)
        .build()
        .map_err(|e| format!("appsink_b build: {e}"))?;

    // Kolejność linkowania: convert → [videoscale → capsfilter 720p] → enc.
    let mut elements: Vec<gst::Element> =
        vec![queue_b.clone(), jpegdec.clone(), queue_dec.clone(), convert.clone()];
    if let Some((scale, filter)) = &scaler {
        elements.push(scale.clone());
        elements.push(filter.clone());
    }
    elements.extend([enc.clone(), parse.clone(), mux.clone(), sink.clone()]);
    let element_refs: Vec<&gst::Element> = elements.iter().collect();

    pipeline
        .add_many(&element_refs)
        .map_err(|e| format!("add_many branch B: {e}"))?;

    let queue_b_sink = queue_b
        .static_pad("sink")
        .ok_or_else(|| "queue_b sink pad missing".to_string())?;
    gst::Element::link_many(&element_refs)
        .map_err(|e| format!("link branch B: {e}"))?;

    wire_mp4_appsink(&sink, publisher)?;

    // Ta sama semantyka resetu bazy PTS co w RTSP (rebuild gałęzi = nowa oś),
    // ale probe stoi na WEJŚCIU gałęzi (src pad queue_b za tee, przed jpegdec):
    // oba warianty MJPEG transkodują przez x264enc, który przesuwa timestampy
    // o stały offset — baza zdjęta za enkoderem leżałaby w innej osi niż PTS
    // detekcji (Branch A) i overlay w trybie PTS nie rysowałby boxów.
    publisher.reset_base_pts_ns();
    install_branch_input_base_pts_probe(&queue_b, publisher);

    for el in &element_refs {
        el.sync_state_with_parent()
            .map_err(|e| format!("sync_state branch B element: {e}"))?;
    }

    // Pad tee linkujemy DOPIERO po aktywacji całej gałęzi. Push tee w okno
    // między linkiem a aktywacją queue_b zwraca FLUSHING, a tee trwale
    // oznacza taki pad jako usunięty i nigdy więcej do niego nie pcha —
    // gałąź wygląda na wpiętą, ale mux nie dostaje ani bajta i init segment
    // nigdy nie powstaje.
    // Gałąź jest już AKTYWNA w pipeline — przy błędzie tego kroku trzeba ją
    // rozebrać (Null + remove), inaczej kolejny attach wywali się na kolizji
    // stałych nazw elementów, a osierocone elementy dalej mieliłyby dane.
    let Some(tee_src_pad) = tee.request_pad_simple("src_%u") else {
        for el in &element_refs {
            let _ = el.set_state(gst::State::Null);
        }
        let _ = pipeline.remove_many(&element_refs);
        return Err("tee src_%u request for branch B failed".to_string());
    };
    if let Err(e) = tee_src_pad.link(&queue_b_sink) {
        detach_mp4_branch(
            pipeline,
            tee,
            Mp4BranchState {
                tee_src_pad,
                elements,
            },
        );
        return Err(format!("tee → queue_b: {e:?}"));
    }

    Ok(Mp4BranchState {
        tee_src_pad,
        elements,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_mjpeg_url_accepts_http_https() {
        assert!(validate_mjpeg_url("http://10.0.0.7/axis-cgi/mjpg/video.cgi").is_ok());
        assert!(validate_mjpeg_url("https://cam.local:8080/mjpg/1/video.mjpg").is_ok());
    }

    #[test]
    fn validate_mjpeg_url_rejects_other_schemes() {
        for bad in ["", "rtsp://cam/stream", "http://", "https://", "cam.local/mjpg"] {
            assert!(validate_mjpeg_url(bad).is_err(), "should reject: {bad}");
        }
    }
}
