// =============================================================================
// File: protocol/ui/a11y.rs — Accessibility, Visibility, EventKind (catalog §1.6)
// Purpose: ARIA + responsive visibility metadata attached to every Component,
// plus the canonical EventKind enum used in Component.handlers and Event topics.
// =============================================================================

use minicbor::{Decode, Encode};

use super::bind::BindRef;
use super::tokens::{Breakpoint, LiveRegion};

/// ARIA accessibility metadata attached to a Component. All fields optional.
/// Integer-keyed CBOR map per the "all maps integer-keyed" convention.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Default)]
#[cbor(map)]
pub struct Accessibility {
    /// ARIA role override (e.g. "button", "dialog"). When None, renderer picks
    /// a default role per component tag.
    #[n(0)]
    pub role: Option<String>,
    /// Accessible label (string or bind path).
    #[n(1)]
    pub label: Option<BindRef>,
    /// `aria-labelledby` reference — id of another component whose text labels this one.
    #[n(2)]
    pub label_for: Option<String>,
    /// `aria-describedby` reference.
    #[n(3)]
    pub described_by: Option<String>,
    /// `aria-live` politeness.
    #[n(4)]
    pub live: Option<LiveRegion>,
    /// `aria-expanded` (bool BindRef).
    #[n(5)]
    pub expanded: Option<BindRef>,
    /// `aria-disabled` (bool BindRef).
    #[n(6)]
    pub disabled: Option<BindRef>,
    /// `aria-required` (bool BindRef).
    #[n(7)]
    pub required: Option<BindRef>,
    /// `aria-invalid` (bool BindRef).
    #[n(8)]
    pub invalid: Option<BindRef>,
    /// `aria-pressed` (bool BindRef) — for toggle buttons.
    #[n(9)]
    pub pressed: Option<BindRef>,
    /// `aria-selected` (bool BindRef).
    #[n(10)]
    pub selected: Option<BindRef>,
}

/// Visibility metadata attached to a Component.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Default)]
#[cbor(map)]
pub struct Visibility {
    /// Bound boolean; renderer hides the element when false. Default true.
    #[n(0)]
    pub visible: Option<BindRef>,
    /// Display only when viewport ≥ this breakpoint.
    #[n(1)]
    pub display_above_breakpoint: Option<Breakpoint>,
    /// Display only when viewport ≤ this breakpoint.
    #[n(2)]
    pub display_below_breakpoint: Option<Breakpoint>,
    /// `aria-hidden=true` regardless of visual display.
    #[n(3)]
    pub hidden_for_assistive: bool,
}

string_enum! {
    /// Canonical event identifiers used in Component.handlers keys and Event topics.
    /// Wire form: tstr; renderer pipes browser events through this whitelist.
    pub enum EventKind {
        Click = "click",
        DoubleClick = "double_click",
        LongPress = "long_press",
        ContextMenu = "context_menu",

        Change = "change",
        Input = "input",
        Submit = "submit",
        Reset = "reset",
        Commit = "commit",

        Focus = "focus",
        Blur = "blur",
        KeyDown = "key_down",
        KeyUp = "key_up",
        KeyPress = "key_press",
        SaveShortcut = "save_shortcut",

        Open = "open",
        Close = "close",
        Select = "select",
        Deselect = "deselect",
        Dismiss = "dismiss",
        Confirm = "confirm",
        Cancel = "cancel",

        DragStart = "drag_start",
        DragEnd = "drag_end",
        Drop = "drop",

        Scroll = "scroll",
        ScrollEnd = "scroll_end",
        Resize = "resize",
        Intersect = "intersect",

        PointerDown = "pointer_down",
        PointerUp = "pointer_up",
        PointerMove = "pointer_move",
        PointerCancel = "pointer_cancel",
        Wheel = "wheel",

        Play = "play",
        Pause = "pause",
        Ended = "ended",
        Loaded = "loaded",
        StreamError = "stream_error",
        Fullscreen = "fullscreen",

        StreamChunk = "stream_chunk",

        RowClick = "row_click",
        RowDoubleClick = "row_double_click",
        SelectionChange = "selection_change",
        CellClick = "cell_click",
        CellHover = "cell_hover",
        ItemClick = "item_click",
        MarkerClick = "marker_click",
        NodeClick = "node_click",
        EdgeClick = "edge_click",
        ZoomEnd = "zoom_end",
        PanEnd = "pan_end",
        PointHover = "point_hover",
        RangeSelect = "range_select",

        FilesSelected = "files_selected",
        UploadProgress = "upload_progress",
        UploadComplete = "upload_complete",
        UploadError = "upload_error",

        StepChange = "step_change",
        StepClick = "step_click",
        Expand = "expand",
        Collapse = "collapse",

        Frame = "frame",

        Remove = "remove",
        ImageClick = "image_click",
        DayClick = "day_click",
        SlotClick = "slot_click",
        EventClick = "event_click",
        EventDrop = "event_drop",
        CellToggle = "cell_toggle",
        CellChange = "cell_change",
        BulkApply = "bulk_apply",
        AddRule = "add_rule",
        RemoveRule = "remove_rule",
        ApproveRule = "approve_rule",
        MarkRead = "mark_read",

        FieldChange = "field_change",
        ScrollTop = "scroll_top",
        FilterChange = "filter_change",
        Retry = "retry",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::bind::{PathSegment, StatePath};

    fn rt<T>(v: T)
    where
        T: minicbor::Encode<()> + for<'b> minicbor::Decode<'b, ()> + PartialEq + core::fmt::Debug,
    {
        let mut buf = Vec::new();
        minicbor::encode(&v, &mut buf).unwrap();
        let d: T = minicbor::decode(&buf).unwrap();
        assert_eq!(d, v);
    }

    #[test]
    fn accessibility_default_roundtrip() {
        rt(Accessibility::default());
    }

    #[test]
    fn accessibility_populated_roundtrip() {
        rt(Accessibility {
            role: Some("button".into()),
            label: Some(BindRef::Literal(crate::protocol::value::Value::Text(
                "Save".into(),
            ))),
            label_for: None,
            described_by: Some("desc-1".into()),
            live: Some(LiveRegion::Polite),
            expanded: None,
            disabled: Some(BindRef::Bound(StatePath::new(vec![PathSegment::Key(
                "saving".into(),
            )]))),
            required: None,
            invalid: None,
            pressed: None,
            selected: None,
        });
    }

    #[test]
    fn visibility_roundtrip() {
        rt(Visibility {
            visible: Some(BindRef::Literal(crate::protocol::value::Value::Bool(true))),
            display_above_breakpoint: Some(Breakpoint::Md),
            display_below_breakpoint: None,
            hidden_for_assistive: false,
        });
    }

    #[test]
    fn event_kind_unknown_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.str("future_event").unwrap();
        let res: Result<EventKind, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn event_kind_wire_strings_spot_check() {
        assert_eq!(EventKind::Click.as_str(), "click");
        assert_eq!(EventKind::FilesSelected.as_str(), "files_selected");
        assert_eq!(EventKind::SaveShortcut.as_str(), "save_shortcut");
        assert_eq!(EventKind::CellToggle.as_str(), "cell_toggle");
    }
}
