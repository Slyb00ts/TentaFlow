// =============================================================================
// File: protocol/ui/specialized/mod.rs — §8 Specialized components (Parts A+B)
// 13 typed: VideoStream/LiveCameraTile/Audio (media), MapView (map),
// CodeEditor/Terminal (text_io), FpsCounter/Stopwatch (telemetry),
// ImageGallery/Carousel/PdfViewer (gallery), StepProgress (wizard), IFrame (iframe).
// Part C (graphics surfaces Canvas2D/WebGLSurface/WGPUSurface + DrawCommand
// tagged union + VirtualizedLog) lands in chunk 1.8e3.
// =============================================================================

pub mod gallery;
pub mod iframe;
pub mod log;
pub mod map;
pub mod media;
pub mod telemetry;
pub mod text_io;
pub mod wizard;

pub use gallery::{Carousel, ImageGallery, PdfViewer};
pub use iframe::IFrame;
pub use log::VirtualizedLog;
pub use map::MapView;
pub use media::{Audio, AudioCapture, LiveCameraTile, VideoStream};
pub use telemetry::{FpsCounter, Stopwatch};
pub use text_io::{CodeEditor, Terminal};
pub use wizard::StepProgress;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::bind::{BindRef, PathSegment, StatePath};
    use crate::protocol::ui::component::{Component, FieldMap};
    use crate::protocol::ui::inline::{AspectRatio, DimensionToken, StepDef};
    use crate::protocol::ui::tokens::{
        AudioCaptureMode, AudioCaptureVariant, AudioControls, AudioVariant, CarouselGestures,
        CodeEditorTheme, Density,
        FpsVariant,
        IFrameReferrerPolicy, IFrameSandbox, ImageFit, LogLevel, LogVariant, PdfZoomMode, Spacing,
        StepProgressVariant, StopwatchVariant, TerminalTheme, TileProvider, Tone, VideoControls,
    };
    use crate::protocol::value::Value;

    fn p(s: &str) -> StatePath {
        StatePath { segments: vec![PathSegment::Key(s.into())] }
    }
    fn lit(s: &str) -> BindRef {
        BindRef::Literal(Value::Text(s.into()))
    }

    fn rt<T: PartialEq + std::fmt::Debug + Clone>(
        make: T,
        into: impl Fn(T) -> Component,
        from: impl Fn(&Component) -> Result<T, minicbor::decode::Error>,
    ) {
        let c = into(make.clone());
        assert_eq!(from(&c).unwrap(), make);
    }

    #[test]
    fn video_stream_roundtrip() {
        let v = VideoStream {
            stream_id: lit("cam-1"), width_px: Some(1280),
            aspect_ratio: AspectRatio::R16To9,
            controls: VideoControls::Full,
            autoplay: false, muted: true, object_fit: ImageFit::Contain,
            poster_ref: Some("poster123".into()),
        };
        rt(v, |m| m.into_component("vs").unwrap(), VideoStream::try_from_component);
    }

    #[test]
    fn live_camera_tile_roundtrip() {
        let v = LiveCameraTile {
            stream_id: lit("cam-2"), camera_label: lit("Front door"),
            status: lit("online"),
            fps: Some(BindRef::Literal(Value::F64(30.0))),
            show_overlay: true, show_fullscreen_button: true,
            aspect_ratio: AspectRatio::R4To3,
        };
        rt(v, |m| m.into_component("lct").unwrap(), LiveCameraTile::try_from_component);
    }

    #[test]
    fn audio_roundtrip() {
        let v = Audio {
            src_ref: lit("audio123"),
            controls: AudioControls::Full,
            autoplay: false, r#loop: true, variant: AudioVariant::Waveform,
        };
        rt(v, |m| m.into_component("au").unwrap(), Audio::try_from_component);
    }

    #[test]
    fn audio_capture_roundtrip() {
        let v = AudioCapture {
            action_id: "utteranceCaptured".into(),
            mode: AudioCaptureMode::Vad,
            silence_ms: Some(800),
            min_speech_ms: Some(200),
            language_hint: Some("pl".into()),
            recording_path: Some(p("recording")),
            disabled: Some(BindRef::Literal(Value::Bool(false))),
            active_path: Some(p("dictation.active")),
            variant: Some(AudioCaptureVariant::Docked),
        };
        rt(v, |m| m.into_component("ac").unwrap(), AudioCapture::try_from_component);
    }

    #[test]
    fn audio_capture_minimal_roundtrip() {
        // Optional fields absent — decode must not require them.
        let v = AudioCapture {
            action_id: "utteranceCaptured".into(),
            mode: AudioCaptureMode::PushToTalk,
            silence_ms: None,
            min_speech_ms: None,
            language_hint: None,
            recording_path: None,
            disabled: None,
            active_path: None,
            variant: None,
        };
        rt(v, |m| m.into_component("ac2").unwrap(), AudioCapture::try_from_component);
    }

    #[test]
    fn map_view_roundtrip() {
        let v = MapView {
            center_path: p("center"), zoom_path: p("zoom"),
            tile_provider: TileProvider::Osm,
            tile_server_url: None,
            height: DimensionToken::Px { value: 400 },
            markers_path: p("markers"),
            polygons_path: None, heatmap_path: None,
            interactive: true, show_attribution: true,
        };
        rt(v, |m| m.into_component("mv").unwrap(), MapView::try_from_component);
    }

    #[test]
    fn code_editor_roundtrip() {
        let v = CodeEditor {
            bind_path: p("source"), language: "rust".into(),
            read_only: false, line_numbers: true, word_wrap: false,
            theme: CodeEditorTheme::Dark,
            min_height_px: 200, max_height_px: Some(800),
            tab_size: 4, indent_with_tabs: false,
            bracket_matching: true, autocomplete: true,
            linting_action_id: Some("lintRust".into()),
        };
        rt(v, |m| m.into_component("ce").unwrap(), CodeEditor::try_from_component);
    }

    #[test]
    fn terminal_roundtrip() {
        let v = Terminal {
            stream_id: lit("log-stream"),
            rows: 24, cols: 80,
            theme: TerminalTheme::HighContrast,
            searchable: true, copyable: true,
            max_buffer_lines: 5_000,
        };
        rt(v, |m| m.into_component("tm").unwrap(), Terminal::try_from_component);
    }

    #[test]
    fn fps_counter_roundtrip() {
        let v = FpsCounter {
            source_path: p("fps"),
            variant: FpsVariant::Detailed,
            history_secs: 30,
        };
        rt(v, |m| m.into_component("fc").unwrap(), FpsCounter::try_from_component);
    }

    #[test]
    fn stopwatch_roundtrip() {
        let v = Stopwatch {
            started_at_path: p("ts"),
            variant: StopwatchVariant::Full,
            tone: Tone::Primary,
        };
        rt(v, |m| m.into_component("sw").unwrap(), Stopwatch::try_from_component);
    }

    #[test]
    fn image_gallery_roundtrip() {
        let v = ImageGallery {
            images_path: p("images"), columns: 3,
            aspect_ratio: AspectRatio::R1To1, gap: Spacing::Md,
            lightbox: true, lazy_load: true,
        };
        rt(v, |m| m.into_component("ig").unwrap(), ImageGallery::try_from_component);
    }

    #[test]
    fn carousel_roundtrip() {
        let v = Carousel {
            items_path: p("items"), current_index_path: p("idx"),
            autoplay: true, autoplay_ms: 3000, r#loop: true,
            show_indicators: true, show_arrows: true,
            gestures: CarouselGestures::Swipe,
        };
        rt(v, |m| m.into_component("car").unwrap(), Carousel::try_from_component);
    }

    #[test]
    fn pdf_viewer_roundtrip() {
        let v = PdfViewer {
            src_ref: "ref123".into(),
            page_path: Some(p("page")),
            height: DimensionToken::Vh { value: 80 },
            zoom_mode: PdfZoomMode::FitWidth, searchable: true,
        };
        rt(v, |m| m.into_component("pdf").unwrap(), PdfViewer::try_from_component);
    }

    #[test]
    fn step_progress_roundtrip() {
        let v = StepProgress {
            steps: vec![StepDef {
                id: "s1".into(), label: lit("Step 1"),
                optional: false, status: None, description: None,
            }],
            current_id_path: p("current"),
            variant: StepProgressVariant::Horizontal,
            clickable_completed: true,
        };
        rt(v, |m| m.into_component("sp").unwrap(), StepProgress::try_from_component);
    }

    #[test]
    fn iframe_roundtrip() {
        let v = IFrame {
            src: "https://example.com/embed".into(),
            sandbox: vec![IFrameSandbox::AllowScripts, IFrameSandbox::AllowForms],
            width: DimensionToken::Px { value: 800 },
            height: DimensionToken::Px { value: 600 },
            title: "Embedded chart".into(),
            referrer_policy: IFrameReferrerPolicy::NoReferrer,
        };
        rt(v, |m| m.into_component("if").unwrap(), IFrame::try_from_component);
    }

    #[test]
    fn virtualized_log_roundtrip() {
        let v = VirtualizedLog {
            events_path: p("events"),
            variant: LogVariant::Expanded,
            max_buffer_events: 5_000,
            auto_scroll: true, searchable: true,
            filter_levels: vec![LogLevel::Info, LogLevel::Warn, LogLevel::Error],
            show_timestamps: true, show_source: false, copyable: true,
            height: DimensionToken::Full,
            max_height: Some(DimensionToken::Px { value: 600 }),
            density: Density::Compact,
        };
        rt(v, |m| m.into_component("vl").unwrap(), VirtualizedLog::try_from_component);
    }

    #[test]
    fn virtualized_log_default_max_buffer_events() {
        // max_buffer_events absent → defaults to 10_000.
        let v = VirtualizedLog {
            events_path: p("ev"),
            variant: LogVariant::Default,
            max_buffer_events: 10_000,
            auto_scroll: true, searchable: false,
            filter_levels: vec![],
            show_timestamps: true, show_source: false, copyable: false,
            height: DimensionToken::Full, max_height: None,
            density: Density::Default,
        };
        let mut c = v.clone().into_component("vl").unwrap();
        c.fields.0.retain(|(k, _)| *k != 2);
        assert_eq!(VirtualizedLog::try_from_component(&c).unwrap(), v);
    }

    #[test]
    fn virtualized_log_default_height_full() {
        // height absent → defaults to DimensionToken::Full.
        let v = VirtualizedLog {
            events_path: p("ev"),
            variant: LogVariant::Default,
            max_buffer_events: 1000,
            auto_scroll: true, searchable: false,
            filter_levels: vec![],
            show_timestamps: false, show_source: false, copyable: false,
            height: DimensionToken::Full, max_height: None,
            density: Density::Default,
        };
        let mut c = v.clone().into_component("vl").unwrap();
        c.fields.0.retain(|(k, _)| *k != 9);
        assert_eq!(VirtualizedLog::try_from_component(&c).unwrap(), v);
    }

    #[test]
    fn tag_mismatch_rejected() {
        let bogus = Component {
            tag: 0x9999, id: "x".into(), fields: FieldMap::default(),
            handlers: None, bind: None, a11y: None, visibility: None, test_id: None,
        };
        assert!(VideoStream::try_from_component(&bogus).is_err());
        assert!(MapView::try_from_component(&bogus).is_err());
        assert!(Terminal::try_from_component(&bogus).is_err());
    }
}
