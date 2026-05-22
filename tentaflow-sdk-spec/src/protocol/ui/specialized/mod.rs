// =============================================================================
// File: protocol/ui/specialized/mod.rs — §8 Specialized components (Part A)
// 8 typed: VideoStream/LiveCameraTile/Audio (media), MapView (map),
// CodeEditor/Terminal (text_io), FpsCounter/Stopwatch (telemetry).
// Parts B (gallery/carousel/pdf/step/iframe) and C (graphics surfaces +
// DrawCommand + VirtualizedLog) land in chunks 1.8e2 / 1.8e3.
// =============================================================================

pub mod map;
pub mod media;
pub mod telemetry;
pub mod text_io;

pub use map::MapView;
pub use media::{Audio, LiveCameraTile, VideoStream};
pub use telemetry::{FpsCounter, Stopwatch};
pub use text_io::{CodeEditor, Terminal};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::bind::{BindRef, PathSegment, StatePath};
    use crate::protocol::ui::component::{Component, FieldMap};
    use crate::protocol::ui::inline::{AspectRatio, DimensionToken};
    use crate::protocol::ui::tokens::{
        AudioControls, AudioVariant, CodeEditorTheme, FpsVariant, ImageFit, StopwatchVariant,
        TerminalTheme, TileProvider, Tone, VideoControls,
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
