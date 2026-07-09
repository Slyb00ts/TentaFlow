// =============================================================================
// File: addons/notes/src/ui.rs
// Purpose: Notes panel (mockup n01) rendered with the typed ui_v1 catalog over
//          the binary CBOR protocol. Layout: Split (note list | main); the
//          "main" slot holds a responsive Flex (editor grows, links panel on
//          the right, stacking to a column on narrow containers). Every UI
//          action comes back as tool "ui.main.<action>" and is dispatched by
//          handle_ui_action, which goes through db.rs (ACL on every path) and
//          refreshes the panel via SlotContent / StatePatch. Autosave never
//          re-renders the editor slot (caret/scroll survive); it patches
//          state and refreshes the list slot only.
// =============================================================================

use serde_json::{json, Value as JsonValue};

use tentaflow_addon_sdk::ui_v1::{self as ui, backend, bound, lit, lit_value, render, state_path};
use tentaflow_addon_sdk::ui_v1::{
    Accessibility, Avatar, AvatarRef, AvatarShape, AvatarSize, Badge, BadgeVariant, BindRef,
    BorderColor, BorderEdges, BorderSide, BorderToken, Button, ButtonSize, ButtonVariant,
    CachePolicy, Card, CardVariant, CborMap, Chip, ChipVariant, Cluster, Component, CornerValues,
    Density, DimensionToken, Divider, DividerOrientation, DividerVariant, EmptyState,
    EmptyStateVariant, EventKind, FailurePolicy, FilterChipDef, FilterChips, FilterChipsMode, Flex,
    FlexAlign, FlexDirection, FlexJustify, FlexWrap, Handler, HandlerMap, Heading, IconName,
    IconRef, InputSize, PanelShell, PatchOp, PatchOpKind, RadiusValue,
    ScrollContainer, ScrollOrientation, SearchBox, SearchVariant, Select,
    SelectOption, SelectValue, ShadowToken, SlotContent, SlotDecl, SlotDefault, SlotSemantics,
    SlotVisibility, Spacing, Split, SplitOrientation, SplitSize, StateEntry, StatePatch, TagInput,
    Text, TextStyle, Textarea, Tone, UiPayload, Value as CborValue, ValueFormat, Visibility,
};
use tentaflow_addon_sdk::{state_get, state_set, StateTier};

use crate::analysis;
use crate::db::{self, NoteDetail, NoteSummary, UserCtx};

pub const ADDON_ID: &str = "notes";
pub const PANEL_ID: &str = "main";

const SLOT_LIST: &str = "list";
const SLOT_MAIN: &str = "main-area";

// Panel state paths.
const SP_SEARCH: &str = "filters.search";
const SP_SCOPE: &str = "filters.scope";
const SP_TITLE: &str = "note.title";
const SP_CONTENT: &str = "note.content";
const SP_TAGS: &str = "note.tags";
const SP_SHARE: &str = "note.share_mode";
const SP_SAVE_STATUS: &str = "editor.save_status";
const SP_SAVE_ERROR: &str = "editor.save_error";
const SP_SAVE_OK_VIS: &str = "editor.save_ok_visible";
const SP_SAVE_ERR_VIS: &str = "editor.save_err_visible";
const SP_CHAR_COUNT: &str = "editor.char_count";
const SP_LINK_PICK: &str = "links.pick";

// Hard server-side input limits (client caps mirror them where the component
// supports one). Values beyond a limit are rejected with a readable message
// in `editor.save_status` — never a panic, never a partial write.
const MAX_TITLE_CHARS: usize = 512;
const MAX_CONTENT_BYTES: usize = 256 * 1024;
const MAX_SEARCH_CHARS: usize = 256;
const MAX_TAGS: usize = 32;
const MAX_TAG_CHARS: usize = 64;

// =============================================================================
// Module state — epoch/revision statics + per-call session identity (the WASM
// instance is pooled per (addon, user); the action-carried __panel_epoch is
// the source of truth for each call, same pattern as the rag addon).
// =============================================================================

static mut PANEL_EPOCH: u64 = 1;
static mut STATE_REVISION: u64 = 0;
static mut SESSION_USER_ID: Option<String> = None;

fn panel_epoch() -> u64 {
    unsafe { PANEL_EPOCH }
}

pub fn set_session_user(user_id: Option<&str>) {
    unsafe {
        SESSION_USER_ID = user_id.filter(|u| !u.is_empty()).map(str::to_string);
    }
}

fn session_user() -> String {
    unsafe {
        #[allow(static_mut_refs)]
        SESSION_USER_ID
            .clone()
            .unwrap_or_else(|| "anon".to_string())
    }
}

pub fn reset_for_open(epoch: u64) {
    unsafe {
        PANEL_EPOCH = epoch;
        STATE_REVISION = 0;
    }
}

/// Adopts the host-validated `__panel_epoch` carried by a UI action. A pooled
/// instance may hold a stale static epoch; without adoption its StatePatch /
/// SlotContent would be rejected by the session.
pub fn adopt_action_epoch(epoch: u64) {
    unsafe {
        if PANEL_EPOCH != epoch {
            PANEL_EPOCH = epoch;
            STATE_REVISION = 0;
        }
    }
}

// =============================================================================
// Per-session UI state (scope filter, search phrase, active note) in the host
// KV. Keyed by user+epoch so pooled instances never leak values across
// sessions; Ephemeral tier = RAM only.
// =============================================================================

#[derive(Debug, Clone)]
struct Session {
    scope: String,
    search: String,
    active: String,
    /// Manual-link picker open in the links panel of the active note.
    link_picker: bool,
}

impl Default for Session {
    fn default() -> Self {
        Session {
            scope: "all".to_string(),
            search: String::new(),
            active: String::new(),
            link_picker: false,
        }
    }
}

fn session_key() -> String {
    format!("sess:{}:{}", session_user(), panel_epoch())
}

fn load_session() -> Session {
    let raw = state_get(&session_key()).ok().flatten();
    let parsed: Option<JsonValue> = raw.and_then(|b| serde_json::from_slice(&b).ok());
    match parsed {
        Some(v) => Session {
            scope: v["scope"].as_str().unwrap_or("all").to_string(),
            search: v["search"].as_str().unwrap_or("").to_string(),
            active: v["active"].as_str().unwrap_or("").to_string(),
            link_picker: v["link_picker"].as_bool().unwrap_or(false),
        },
        None => Session::default(),
    }
}

fn store_session(sess: &Session) {
    let v = json!({
        "scope": sess.scope,
        "search": sess.search,
        "active": sess.active,
        "link_picker": sess.link_picker,
    });
    let _ = state_set(
        &session_key(),
        v.to_string().as_bytes(),
        StateTier::Ephemeral,
    );
}

// =============================================================================
// Small builders
// =============================================================================

fn send(payload: &UiPayload) -> bool {
    render(payload).is_ok()
}

fn send_state_patch(ops: Vec<PatchOp>) {
    let base = unsafe { STATE_REVISION };
    let new = base + 1;
    let patch = StatePatch {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: panel_epoch(),
        base_revision: base,
        new_revision: new,
        ops,
    };
    // Advance the local revision only when the host accepted the patch.
    if send(&UiPayload::StatePatch(patch)) {
        unsafe {
            STATE_REVISION = new;
        }
    }
}

fn send_slot(slot_id: &str, fragment: Component, overlay: Option<Vec<StateEntry>>) {
    let content = SlotContent {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: panel_epoch(),
        slot_id: slot_id.into(),
        fragment,
        state_overlay: overlay,
    };
    send(&UiPayload::SlotContent(content));
}

/// Backend handler with static CBOR params (event detail is merged in by the
/// client dispatcher on top of these).
fn backend_params(kind: EventKind, action_id: &str, params: Vec<(&str, CborValue)>) -> HandlerMap {
    HandlerMap(vec![(
        kind,
        Handler::Backend {
            action_id: action_id.into(),
            params: CborMap(
                params
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
            ),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )])
}

fn icon(name: IconName) -> IconRef {
    IconRef::Named {
        name,
        size: None,
        tone: None,
    }
}

/// Accessible name for label-less form components — the form renderers REJECT
/// SearchBox / Input / Textarea without one and the whole SlotContent is
/// dropped (tentavision pattern).
fn with_a11y_label(mut component: Component, label: &str) -> Component {
    component.a11y = Some(Accessibility {
        label: Some(lit(label)),
        ..Default::default()
    });
    component
}

/// Reactive visibility bound to a boolean state path (`false` hides).
fn with_visible_bound(mut component: Component, path: &str) -> Component {
    component.visibility = Some(Visibility {
        visible: Some(bound(path)),
        display_above_breakpoint: None,
        display_below_breakpoint: None,
        hidden_for_assistive: false,
    });
    component
}

/// One state patch that drives the autosave indicator: success badge with the
/// message, error badge hidden (or the reverse).
fn save_feedback_ops(ok: Option<&str>, err: Option<&str>) -> Vec<PatchOp> {
    let set = |path: &str, value: CborValue| PatchOp {
        path: state_path(path),
        op: PatchOpKind::Set { value },
    };
    vec![
        set(SP_SAVE_STATUS, CborValue::Text(ok.unwrap_or("").to_string())),
        set(SP_SAVE_ERROR, CborValue::Text(err.unwrap_or("").to_string())),
        set(SP_SAVE_OK_VIS, CborValue::Bool(ok.is_some())),
        set(SP_SAVE_ERR_VIS, CborValue::Bool(err.is_some())),
    ]
}

fn feedback_ok(message: &str) {
    send_state_patch(save_feedback_ops(Some(message), None));
}

fn feedback_err(message: &str) {
    send_state_patch(save_feedback_ops(None, Some(message)));
}

fn text_c(id: &str, content: BindRef, style: TextStyle, tone: Option<Tone>) -> Component {
    Text {
        content,
        style,
        tone,
        align: None,
        wrap: None,
        max_lines: None,
        format: None,
        streaming: None,
    }
    .into_component(id)
    .expect("Text encode")
}

/// Relative timestamp text ("2 min temu") — formatting done by the renderer.
fn rel_time(id: &str, unix_secs: i64) -> Component {
    Text {
        content: lit_value(CborValue::U64((unix_secs.max(0) as u64) * 1000)),
        style: TextStyle::Caption,
        tone: Some(Tone::Muted),
        align: None,
        wrap: None,
        max_lines: None,
        format: Some(ValueFormat::Relative),
        streaming: None,
    }
    .into_component(id)
    .expect("Text encode")
}

fn slot_decl(id: &str, semantics: SlotSemantics) -> SlotDecl {
    SlotDecl {
        id: id.into(),
        semantics,
        default_state: SlotDefault::Loading,
        cache_policy: CachePolicy::None,
        visibility: SlotVisibility::Always,
        max_payload_bytes: None,
    }
}

fn initials(display_name: &str) -> String {
    display_name
        .split_whitespace()
        .take(2)
        .filter_map(|w| w.chars().next())
        .flat_map(|c| c.to_uppercase())
        .collect()
}

// =============================================================================
// Panel shell
// =============================================================================

/// Addon topbar (mockup n01): panel title on the left, auto-graph status pill
/// on the right. The pill reflects the REAL alias state at panel open — both
/// notes-embeddings and notes-llm bound = analysis runs; otherwise the admin
/// is pointed at alias configuration. The mode switch (Notatki/Graf/Szukaj)
/// lands with the graph/search stages.
fn topbar() -> Component {
    let title = Heading {
        content: lit("Notatki"),
        level: 3,
        tone: None,
        align: None,
    }
    .into_component("topbar-title")
    .expect("Heading encode");

    let status: Component = if analysis::auto_graph_ready() {
        Badge {
            variant: BadgeVariant::Soft,
            tone: Tone::Success,
            label: lit("Auto-graf aktywny"),
            icon: Some(icon(IconName::Check)),
            count: None,
            max: 0,
            pulse: false,
        }
        .into_component("topbar-status")
        .expect("Badge encode")
    } else {
        Badge {
            variant: BadgeVariant::Soft,
            tone: Tone::Warning,
            label: lit("Auto-graf: skonfiguruj aliasy"),
            icon: Some(icon(IconName::Warning)),
            count: None,
            max: 0,
            pulse: false,
        }
        .into_component("topbar-status")
        .expect("Badge encode")
    };

    // Mockup n01: the topbar sits directly on the page background — no pill.
    Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Md,
        justify: FlexJustify::SpaceBetween,
        align: FlexAlign::Center,
        wrap: FlexWrap::Wrap,
        children: vec![title, status],
        padding: None,
        background: None,
        radius: None,
        style: None,
        responsive: None,
    }
    .into_component("topbar")
    .expect("Flex encode")
}

pub fn send_panel_shell() {
    // Mockup n01: 300px list | flexible main, 16px gap, no visible divider.
    let split = Split {
        orientation: SplitOrientation::Horizontal,
        primary_size: SplitSize::Px { value: 300 },
        min_primary: 240,
        max_primary: 420,
        resizable: false,
        primary_slot: SLOT_LIST.into(),
        secondary_slot: SLOT_MAIN.into(),
        // Xs: only phone-width containers stack the list above the editor —
        // the tablet keeps two columns like the mockup's 1180px rule.
        collapse_below: Some(ui::Breakpoint::Xs),
        divider: Some(ui::SplitDivider::None),
        // Fill the grow-Box below so both panes get the full shell height.
        grow: Some(true),
    }
    .into_component("split")
    .expect("Split encode");

    // The split must own the leftover shell height (mockup: columns flex:1,
    // min 640px) — without min-height the columns collapse to content height.
    let split_grow = ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: None,
        margin: None,
        children: vec![split],
        style: Some(ui::BoxStyle {
            min_height: Some(DimensionToken::Vh { value: 85 }),
            ..Default::default()
        }),
        direction: Some(FlexDirection::Column),
        gap: None,
        align: Some(FlexAlign::Stretch),
        justify: None,
        // Stacked mode: the page must grow with the stacked panes instead of
        // trapping them inside an 85vh box with inner scrollbars.
        responsive: Some(stacked_fit_content(640)),
    }
    .into_component("split-grow")
    .expect("Box encode");

    let layout = Flex {
        direction: FlexDirection::Column,
        gap: Spacing::Md,
        justify: FlexJustify::Start,
        align: FlexAlign::Stretch,
        wrap: FlexWrap::NoWrap,
        children: vec![topbar(), split_grow],
        padding: None,
        background: None,
        radius: None,
        style: None,
        responsive: None,
    }
    .into_component("root")
    .expect("Flex encode");

    let shell = PanelShell {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: panel_epoch(),
        layout,
        slots: vec![
            slot_decl(SLOT_LIST, SlotSemantics::SidePanel),
            slot_decl(SLOT_MAIN, SlotSemantics::MainContent),
        ],
        initial_state: vec![
            StateEntry {
                path: state_path(SP_SEARCH),
                value: CborValue::Text(String::new()),
            },
            StateEntry {
                path: state_path(SP_SCOPE),
                value: CborValue::Array(vec![CborValue::Text("all".into())]),
            },
            StateEntry {
                path: state_path(SP_TITLE),
                value: CborValue::Text(String::new()),
            },
            StateEntry {
                path: state_path(SP_CONTENT),
                value: CborValue::Text(String::new()),
            },
            StateEntry {
                path: state_path(SP_TAGS),
                value: CborValue::Array(vec![]),
            },
            StateEntry {
                path: state_path(SP_SHARE),
                value: CborValue::Text("private".into()),
            },
            StateEntry {
                path: state_path(SP_SAVE_STATUS),
                value: CborValue::Text(String::new()),
            },
            StateEntry {
                path: state_path(SP_SAVE_ERROR),
                value: CborValue::Text(String::new()),
            },
            StateEntry {
                path: state_path(SP_SAVE_OK_VIS),
                value: CborValue::Bool(false),
            },
            StateEntry {
                path: state_path(SP_SAVE_ERR_VIS),
                value: CborValue::Bool(false),
            },
            StateEntry {
                path: state_path(SP_CHAR_COUNT),
                value: CborValue::Text(String::new()),
            },
            StateEntry {
                path: state_path(SP_LINK_PICK),
                value: CborValue::Text(String::new()),
            },
        ],
        initial_commands: vec![],
    };
    send(&UiPayload::PanelShell(shell));
}

// =============================================================================
// Left column — list header (new note, search, scope chips) + note cards
// =============================================================================

pub fn send_list(ctx: &UserCtx) {
    let sess = load_session();
    let fragment = match db::list_notes(ctx, &sess.scope, &sess.search) {
        Ok(notes) => list_fragment(&notes, &sess),
        Err(e) => error_fragment("list-error", &e),
    };
    let overlay = vec![
        StateEntry {
            path: state_path(SP_SEARCH),
            value: CborValue::Text(sess.search.clone()),
        },
        StateEntry {
            path: state_path(SP_SCOPE),
            value: CborValue::Array(vec![CborValue::Text(sess.scope.clone())]),
        },
    ];
    send_slot(SLOT_LIST, fragment, Some(overlay));
}

fn list_fragment(notes: &[NoteSummary], sess: &Session) -> Component {
    let mut new_btn = Button {
        variant: ButtonVariant::Primary,
        tone: Tone::Primary,
        label: lit("Nowa notatka"),
        icon_leading: Some(icon(IconName::Plus)),
        icon_trailing: None,
        size: ButtonSize::Md,
        full_width: true,
        disabled: None,
        loading: None,
        density: Density::Default,
    }
    .into_component("btn-new-note")
    .expect("Button encode");
    new_btn.handlers = Some(backend(EventKind::Click, "new_note"));

    // Change (commit/blur/Enter) instead of Input: the search box lives in this
    // slot, so a per-keystroke re-render would destroy it and kill focus.
    let mut search = with_a11y_label(
        SearchBox {
            bind_path: state_path(SP_SEARCH),
            placeholder: lit("Szukaj w notatkach…"),
            debounce_ms: 300,
            variant: SearchVariant::Default,
            shortcut_hint: None,
            on_search_action_id: None,
        }
        .into_component("list-search")
        .expect("SearchBox encode"),
        "Szukaj w notatkach",
    );
    search.handlers = Some(backend(EventKind::Change, "set_search"));

    let chip = |id: &str, label: &str| FilterChipDef {
        id: id.into(),
        label: lit(label),
        icon: None,
        badge: None,
        count_path: None,
    };
    let mut chips = FilterChips {
        chips: vec![
            chip("all", "Wszystkie"),
            chip("mine", "Moje"),
            chip("shared", "Udostępnione mi"),
            chip("group", "Grupa"),
            chip("org", "Organizacja"),
        ],
        selected_ids: state_path(SP_SCOPE),
        mode: FilterChipsMode::Single,
        clearable: false,
    }
    .into_component("scope-chips")
    .expect("FilterChips encode");
    // tf-filter-chips re-emits 'change' with detail {chip_id} (not 'select').
    chips.handlers = Some(backend(EventKind::Change, "set_filter"));

    let body: Component = if notes.is_empty() {
        EmptyState {
            icon: icon(IconName::FileText),
            heading: lit("Brak notatek"),
            message: Some(lit("Utwórz pierwszą notatkę przyciskiem powyżej.")),
            primary_action: None,
            secondary_action: None,
            variant: EmptyStateVariant::Compact,
        }
        .into_component("list-empty")
        .expect("EmptyState encode")
    } else {
        ScrollContainer {
            orientation: ScrollOrientation::Vertical,
            height: DimensionToken::Full,
            max_height: None,
            children: notes
                .iter()
                .enumerate()
                .map(|(i, n)| note_card(i, n, &sess.active))
                .collect(),
            sticky_header_slot: None,
            virtualize: false,
            gap: Some(Spacing::Sm),
        }
        .into_component("note-list")
        .expect("ScrollContainer encode")
    };

    // Mockup n01 list-head: framed header strip (new note / search / filters)
    // with a bottom hairline; the card list scrolls under it.
    let head = ui::Box {
        width: None,
        grow: None,
        align_self: None,
        padding: Some(Spacing::Md),
        margin: None,
        children: vec![new_btn, search, chips],
        style: Some(ui::BoxStyle {
            border: Some(BorderEdges {
                bottom: Some(BorderSide::new(1, BorderColor::Default)),
                ..Default::default()
            }),
            ..Default::default()
        }),
        direction: Some(FlexDirection::Column),
        gap: Some(Spacing::Sm),
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: None,
    }
    .into_component("list-head")
    .expect("Box encode");

    let list_body = ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: Some(Spacing::Sm),
        margin: None,
        children: vec![body],
        style: Some(ui::BoxStyle {
            min_height: Some(DimensionToken::Px { value: 0 }),
            overflow_y: Some(ui::Overflow::Auto),
            ..Default::default()
        }),
        direction: Some(FlexDirection::Column),
        gap: None,
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: Some(stacked_fit_content(640)),
    }
    .into_component("list-body")
    .expect("Box encode");

    ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: None,
        margin: None,
        children: vec![head, list_body],
        style: Some(panel_style()),
        direction: Some(FlexDirection::Column),
        gap: None,
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: Some(stacked_fit_content(640)),
    }
    .into_component("list-col")
    .expect("Box encode")
}

fn scope_badge(id: &str, scope: &str) -> Component {
    let (tone, icon_name, label) = match scope {
        "org" => (Tone::Primary, IconName::Users, "Organizacja"),
        "group" => (Tone::Info, IconName::Users, "Grupa"),
        "user" => (Tone::Success, IconName::User, "Udostępniona"),
        _ => (Tone::Neutral, IconName::User, "Prywatna"),
    };
    Badge {
        variant: BadgeVariant::Soft,
        tone,
        label: lit(label),
        icon: Some(icon(icon_name)),
        count: None,
        max: 0,
        pulse: false,
    }
    .into_component(id)
    .expect("Badge encode")
}

fn note_card(index: usize, note: &NoteSummary, active_id: &str) -> Component {
    let is_active = !active_id.is_empty() && note.id == active_id;

    let title = Text {
        content: lit(if note.title.is_empty() {
            "(bez tytułu)"
        } else {
            &note.title
        }),
        style: TextStyle::BodyStrong,
        tone: None,
        align: None,
        wrap: None,
        max_lines: Some(2),
        format: None,
        streaming: None,
    }
    .into_component(format!("nc-{index}-title"))
    .expect("Text encode");

    let preview = Text {
        content: lit(&note.preview),
        style: TextStyle::Caption,
        tone: Some(Tone::Muted),
        align: None,
        wrap: None,
        max_lines: Some(2),
        format: None,
        streaming: None,
    }
    .into_component(format!("nc-{index}-preview"))
    .expect("Text encode");

    let meta = Cluster {
        gap: Spacing::Sm,
        align: FlexAlign::Center,
        justify: FlexJustify::Start,
        children: vec![
            rel_time(&format!("nc-{index}-date"), note.updated_at),
            scope_badge(&format!("nc-{index}-scope"), &note.scope),
        ],
        wrap: Some(true),
    }
    .into_component(format!("nc-{index}-meta"))
    .expect("Cluster encode");

    let mut children = vec![title, preview, meta];
    if !note.entities.is_empty() {
        let chips = Cluster {
            gap: Spacing::Xs,
            align: FlexAlign::Center,
            justify: FlexJustify::Start,
            children: note
                .entities
                .iter()
                .enumerate()
                .map(|(j, (name, etype))| {
                    entity_chip_id(&format!("nc-{index}-ent-{j}"), name, etype)
                })
                .collect(),
            wrap: Some(true),
        }
        .into_component(format!("nc-{index}-ents"))
        .expect("Cluster encode");
        children.push(chips);
    }

    let mut card = Card {
        variant: CardVariant::Outlined,
        padding: Spacing::Sm,
        gap: Spacing::Xs,
        radius: ui::RadiusToken::Md,
        shadow: ShadowToken::None,
        border: if is_active {
            BorderToken::Accent {
                tone: Tone::Primary,
            }
        } else {
            BorderToken::Hairline
        },
        background: ui::BackgroundToken::Subtle,
        accent: if is_active { Some(Tone::Primary) } else { None },
        children,
        interactive: true,
        clickable: true,
        style: None,
    }
    .into_component(format!("nc-{index}"))
    .expect("Card encode");
    card.handlers = Some(backend_params(
        EventKind::Click,
        "open_note",
        vec![("note_id", CborValue::Text(note.id.clone()))],
    ));
    card
}

// =============================================================================
// Main area — editor (grows) + links panel, stacking to a column on narrow
// containers. Rendered as ONE slot: everything that re-renders it happens on
// explicit navigation (open/new/delete), never while the user is typing.
// =============================================================================

pub fn send_main(ctx: &UserCtx, note: Option<&NoteDetail>) {
    let (editor, overlay) = match note {
        Some(n) => (editor_fragment(ctx, n), editor_overlay(n)),
        None => (empty_editor_fragment(), empty_editor_overlay()),
    };
    let links = match note {
        Some(n) => links_fragment(ctx, n),
        None => links_placeholder(),
    };

    let editor_col = ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: None,
        margin: None,
        children: vec![editor],
        style: Some(min_width_style(320)),
        direction: Some(FlexDirection::Column),
        gap: None,
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: Some(stacked_fit_content(900)),
    }
    .into_component("editor-pane")
    .expect("Box encode");

    // Mockup n01: fixed 280px links panel; the editor takes the leftover.
    let links_col = ui::Box {
        width: Some(DimensionToken::Px { value: 280 }),
        grow: None,
        align_self: None,
        padding: None,
        margin: None,
        children: vec![links],
        style: Some(ui::BoxStyle {
            min_width: Some(DimensionToken::Px { value: 280 }),
            ..full_height_style()
        }),
        direction: Some(FlexDirection::Column),
        gap: None,
        align: Some(FlexAlign::Stretch),
        justify: None,
        // Stacked mode (narrow container): the panel goes full width.
        responsive: Some(vec![ui::ResponsiveRule {
            max_width: ui::ContainerWidth::Px(900),
            direction: None,
            gap: None,
            align: None,
            justify: None,
            padding: None,
            min_height: None,
            order: None,
            hidden: None,
            width: Some(DimensionToken::Full),
        }
        .min_height(DimensionToken::FitContent)]),
    }
    .into_component("links-pane")
    .expect("Box encode");

    let fragment = Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Md,
        justify: FlexJustify::Start,
        align: FlexAlign::Stretch,
        wrap: FlexWrap::NoWrap,
        children: vec![editor_col, links_col],
        padding: None,
        background: None,
        radius: None,
        style: Some(full_height_style()),
        // Narrow container (tablet/phone): links panel drops under the editor.
        responsive: Some(vec![ui::ResponsiveRule {
            max_width: ui::ContainerWidth::Px(900),
            direction: Some(FlexDirection::Column),
            gap: Some(Spacing::Md),
            align: None,
            justify: None,
            padding: None,
            min_height: None,
            order: None,
            hidden: None,
            width: None,
        }]),
    }
    .into_component("main-row")
    .expect("Flex encode");

    send_slot(SLOT_MAIN, fragment, Some(overlay));
}

fn min_width_style(px: u32) -> ui::BoxStyle {
    ui::BoxStyle {
        min_width: Some(DimensionToken::Px { value: px }),
        ..full_height_style()
    }
}

/// Stacked-mode fix: a `grow` child (flex-basis 0) contributes no height when
/// its parent is content-sized, so below the stacking breakpoint the body must
/// be at least fit-content tall or it collapses to zero.
fn stacked_fit_content(threshold_px: u16) -> Vec<ui::ResponsiveRule> {
    vec![ui::ResponsiveRule {
        max_width: ui::ContainerWidth::Px(threshold_px),
        direction: None,
        gap: None,
        align: None,
        justify: None,
        padding: None,
        min_height: Some(DimensionToken::FitContent),
        order: None,
        hidden: None,
        width: None,
    }]
}

fn editor_overlay(n: &NoteDetail) -> Vec<StateEntry> {
    vec![
        StateEntry {
            path: state_path(SP_TITLE),
            value: CborValue::Text(n.title.clone()),
        },
        StateEntry {
            path: state_path(SP_CONTENT),
            value: CborValue::Text(n.content.clone()),
        },
        StateEntry {
            path: state_path(SP_TAGS),
            value: CborValue::Array(n.tags.iter().map(|t| CborValue::Text(t.clone())).collect()),
        },
        StateEntry {
            path: state_path(SP_SHARE),
            value: CborValue::Text(n.share_mode.clone()),
        },
        StateEntry {
            path: state_path(SP_CHAR_COUNT),
            value: CborValue::Text(db::counter_label(&n.content)),
        },
    ]
    .into_iter()
    .chain(feedback_reset_entries())
    .collect()
}

fn empty_editor_overlay() -> Vec<StateEntry> {
    std::iter::once(StateEntry {
        path: state_path(SP_CHAR_COUNT),
        value: CborValue::Text(String::new()),
    })
    .chain(feedback_reset_entries())
    .collect()
}

/// Overlay entries clearing the autosave indicator and the link picker
/// selection on every editor (re)render.
fn feedback_reset_entries() -> Vec<StateEntry> {
    vec![
        StateEntry {
            path: state_path(SP_SAVE_STATUS),
            value: CborValue::Text(String::new()),
        },
        StateEntry {
            path: state_path(SP_SAVE_ERROR),
            value: CborValue::Text(String::new()),
        },
        StateEntry {
            path: state_path(SP_SAVE_OK_VIS),
            value: CborValue::Bool(false),
        },
        StateEntry {
            path: state_path(SP_SAVE_ERR_VIS),
            value: CborValue::Bool(false),
        },
        StateEntry {
            path: state_path(SP_LINK_PICK),
            value: CborValue::Text(String::new()),
        },
    ]
}

fn empty_editor_fragment() -> Component {
    let mut new_btn = Button {
        variant: ButtonVariant::Primary,
        tone: Tone::Primary,
        label: lit("Nowa notatka"),
        icon_leading: Some(icon(IconName::Plus)),
        icon_trailing: None,
        size: ButtonSize::Md,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Default,
    }
    .into_component("empty-new-note")
    .expect("Button encode");
    new_btn.handlers = Some(backend(EventKind::Click, "new_note"));

    let empty = EmptyState {
        icon: icon(IconName::FileText),
        heading: lit("Wybierz notatkę"),
        message: Some(lit("Wybierz notatkę z listy po lewej albo utwórz nową.")),
        primary_action: Some(new_btn),
        secondary_action: None,
        variant: EmptyStateVariant::Default,
    }
    .into_component("editor-empty")
    .expect("EmptyState encode");

    // Same framed panel as the populated editor (mockup col-editor).
    ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: Some(Spacing::Md),
        margin: None,
        children: vec![empty],
        style: Some(panel_style()),
        direction: Some(FlexDirection::Column),
        gap: None,
        align: Some(FlexAlign::Center),
        justify: Some(FlexJustify::Center),
        responsive: None,
    }
    .into_component("editor-empty-panel")
    .expect("Box encode")
}

fn editor_fragment(ctx: &UserCtx, n: &NoteDetail) -> Component {
    let readonly_bind = if n.can_write {
        None
    } else {
        Some(BindRef::Literal(CborValue::Bool(true)))
    };

    // Mockup n01: the title is a borderless h1-like inline field. A single-row
    // auto-grow Textarea (not Input) so a long title wraps and stays fully
    // visible; save_note strips any pasted newlines.
    let mut title = with_a11y_label(
        Textarea {
            bind_path: state_path(SP_TITLE),
            placeholder: Some(lit("Tytuł notatki…")),
            label: None,
            hint: None,
            validators: vec![],
            max_length: Some(MAX_TITLE_CHARS as u16),
            min_length: None,
            disabled: None,
            readonly: readonly_bind.clone(),
            error: None,
            size: InputSize::Lg,
            rows: 1,
            autoresize: true,
            max_rows: None,
            monospace: false,
            variant: Some(ui::InputVariant::Ghost),
        }
        .into_component("note-title")
        .expect("Textarea encode"),
        "Tytuł notatki",
    );
    if n.can_write {
        title.handlers = Some(backend_params(
            EventKind::Change,
            "save_note",
            vec![
                ("note_id", CborValue::Text(n.id.clone())),
                ("field", CborValue::Text("title".into())),
            ],
        ));
    }

    let author_name = if n.is_owner {
        ctx.display_name.clone()
    } else {
        db::user_display_name(&n.owner_user_id)
    };
    let avatar = Avatar {
        source: AvatarRef::Initials {
            initials: initials(&author_name),
        },
        size: AvatarSize::Sm,
        shape: AvatarShape::Circle,
        status: None,
        tone: Some(Tone::Primary),
    }
    .into_component("author-avatar")
    .expect("Avatar encode");

    let created = Text {
        content: lit_value(CborValue::U64((n.created_at.max(0) as u64) * 1000)),
        style: TextStyle::Caption,
        tone: Some(Tone::Muted),
        align: None,
        wrap: None,
        max_lines: None,
        format: Some(ValueFormat::DateTime {
            style: ui::DateTimeStyle::Medium,
        }),
        streaming: None,
    }
    .into_component("note-created")
    .expect("Text encode");

    let mut tags = TagInput {
        values_path: state_path(SP_TAGS),
        placeholder: Some(lit("Dodaj tag…")),
        validators: vec![ui::ValidationRule::MaxLength {
            value: MAX_TAG_CHARS as u16,
        }],
        max_tags: Some(MAX_TAGS as u32),
        separator: vec![",".into()],
        dedupe: true,
    }
    .into_component("note-tags")
    .expect("TagInput encode");
    if n.can_write {
        tags.handlers = Some(backend_params(
            EventKind::Change,
            "save_tags",
            vec![("note_id", CborValue::Text(n.id.clone()))],
        ));
    }

    let meta_left_cluster = Cluster {
        gap: Spacing::Sm,
        align: FlexAlign::Center,
        justify: FlexJustify::Start,
        children: vec![
            avatar,
            text_c(
                "author-name",
                lit(&author_name),
                TextStyle::BodyStrong,
                None,
            ),
            created,
            tags,
        ],
        wrap: Some(true),
    }
    .into_component("editor-meta-left")
    .expect("Cluster encode");
    // grow + min-width 0: the left side shrinks/wraps internally, keeping the
    // share pill on the same row at the right edge (mockup btn-share).
    let meta_left = ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: None,
        margin: None,
        children: vec![meta_left_cluster],
        style: Some(ui::BoxStyle {
            min_width: Some(DimensionToken::Px { value: 0 }),
            ..Default::default()
        }),
        direction: Some(FlexDirection::Column),
        gap: None,
        align: Some(FlexAlign::Stretch),
        justify: Some(FlexJustify::Center),
        responsive: None,
    }
    .into_component("editor-meta-left-grow")
    .expect("Box encode");

    let share_control: Component = if n.is_owner {
        let mut options = vec![
            SelectOption {
                value: SelectValue::Text("private".into()),
                label: lit("Prywatna"),
                icon: Some(icon(IconName::Lock)),
                disabled: false,
                group_id: None,
                description: None,
            },
            SelectOption {
                value: SelectValue::Text("org:read".into()),
                label: lit("Organizacja — odczyt"),
                icon: Some(icon(IconName::Users)),
                disabled: false,
                group_id: None,
                description: None,
            },
            SelectOption {
                value: SelectValue::Text("org:write".into()),
                label: lit("Organizacja — edycja"),
                icon: Some(icon(IconName::Users)),
                disabled: false,
                group_id: None,
                description: None,
            },
        ];
        for (gid, name) in db::group_names(&ctx.group_ids) {
            options.push(SelectOption {
                value: SelectValue::Text(format!("group:{gid}")),
                label: lit(format!("Grupa: {name} — odczyt")),
                icon: Some(icon(IconName::Users)),
                disabled: false,
                group_id: None,
                description: None,
            });
        }
        // Compact control without a stacked label (mockup: small share pill).
        let mut select = with_a11y_label(
            Select {
                bind_path: state_path(SP_SHARE),
                options,
                placeholder: None,
                label: None,
                searchable: false,
                clearable: false,
                virtualize: false,
                disabled: None,
                size: InputSize::Sm,
                groups: None,
            }
            .into_component("share-select")
            .expect("Select encode"),
            "Udostępnij",
        );
        select.handlers = Some(backend_params(
            EventKind::Change,
            "set_share",
            vec![("note_id", CborValue::Text(n.id.clone()))],
        ));
        // Keep the control pill-sized (mockup btn-share) — without the wrapper
        // the select stretches to the full row width.
        ui::Box {
            width: Some(DimensionToken::Px { value: 220 }),
            grow: None,
            align_self: None,
            padding: None,
            margin: None,
            children: vec![select],
            style: None,
            direction: Some(FlexDirection::Column),
            gap: None,
            align: Some(FlexAlign::Stretch),
            justify: None,
            responsive: None,
        }
        .into_component("share-wrap")
        .expect("Box encode")
    } else {
        scope_badge("editor-scope-badge", &n.scope)
    };

    let meta = Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Md,
        justify: FlexJustify::SpaceBetween,
        align: FlexAlign::Center,
        wrap: FlexWrap::NoWrap,
        children: vec![meta_left, share_control],
        padding: None,
        background: None,
        radius: None,
        style: None,
        responsive: None,
    }
    .into_component("editor-meta")
    .expect("Flex encode");

    let divider = Divider {
        orientation: DividerOrientation::Horizontal,
        variant: DividerVariant::Subtle,
        spacing: Spacing::Sm,
        label: None,
    }
    .into_component("editor-divider")
    .expect("Divider encode");

    let mut content = with_a11y_label(
        Textarea {
            bind_path: state_path(SP_CONTENT),
            placeholder: Some(lit("Zacznij pisać…")),
            label: None,
            hint: None,
            validators: vec![],
            max_length: None,
            min_length: None,
            disabled: None,
            readonly: readonly_bind,
            error: None,
            size: InputSize::Md,
            rows: 18,
            autoresize: true,
            max_rows: None,
            monospace: false,
            // Mockup n01: the note body is borderless editor content.
            variant: Some(ui::InputVariant::Ghost),
        }
        .into_component("note-content")
        .expect("Textarea encode"),
        "Treść notatki",
    );
    if n.can_write {
        content.handlers = Some(backend_params(
            EventKind::Change,
            "save_note",
            vec![
                ("note_id", CborValue::Text(n.id.clone())),
                ("field", CborValue::Text("content".into())),
            ],
        ));
    }

    let counter = text_c(
        "char-counter",
        bound(SP_CHAR_COUNT),
        TextStyle::Caption,
        Some(Tone::Muted),
    );
    // Autosave indicator (mockup n01: "✓ Zapisano · przed chwilą"): a success
    // badge and a danger badge sharing the spot, toggled by StatePatch only —
    // the editor slot is never re-rendered while typing.
    let save_ok = with_visible_bound(
        Badge {
            variant: BadgeVariant::Soft,
            tone: Tone::Success,
            label: bound(SP_SAVE_STATUS),
            icon: Some(icon(IconName::Check)),
            count: None,
            max: 0,
            pulse: false,
        }
        .into_component("save-ok")
        .expect("Badge encode"),
        SP_SAVE_OK_VIS,
    );
    let save_err = with_visible_bound(
        Badge {
            variant: BadgeVariant::Soft,
            tone: Tone::Critical,
            label: bound(SP_SAVE_ERROR),
            icon: Some(icon(IconName::Warning)),
            count: None,
            max: 0,
            pulse: false,
        }
        .into_component("save-err")
        .expect("Badge encode"),
        SP_SAVE_ERR_VIS,
    );

    let mut toolbar_children = vec![counter];
    let mut right = vec![save_ok, save_err];
    if n.can_write {
        let mut delete_btn = Button {
            variant: ButtonVariant::Ghost,
            tone: Tone::Critical,
            label: lit("Usuń"),
            icon_leading: Some(icon(IconName::Trash)),
            icon_trailing: None,
            size: ButtonSize::Sm,
            full_width: false,
            disabled: None,
            loading: None,
            density: Density::Compact,
        }
        .into_component("btn-delete-note")
        .expect("Button encode");
        delete_btn.handlers = Some(backend_params(
            EventKind::Click,
            "delete_note",
            vec![("note_id", CborValue::Text(n.id.clone()))],
        ));
        right.push(delete_btn);
    }
    toolbar_children.push(
        Cluster {
            gap: Spacing::Md,
            align: FlexAlign::Center,
            justify: FlexJustify::End,
            children: right,
            wrap: Some(false),
        }
        .into_component("toolbar-right")
        .expect("Cluster encode"),
    );

    // Mockup n01: the toolbar sits at the panel's bottom edge, separated by a
    // full-bleed hairline; the body above it scrolls.
    let toolbar = Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Md,
        justify: FlexJustify::SpaceBetween,
        align: FlexAlign::Center,
        wrap: FlexWrap::Wrap,
        children: toolbar_children,
        padding: Some(Spacing::Sm),
        background: Some(ui::BackgroundToken::Subtle),
        radius: None,
        style: Some(ui::BoxStyle {
            border: Some(BorderEdges {
                top: Some(BorderSide::new(1, BorderColor::Default)),
                ..Default::default()
            }),
            ..Default::default()
        }),
        responsive: None,
    }
    .into_component("editor-toolbar")
    .expect("Flex encode");

    let body = ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: Some(Spacing::Md),
        margin: None,
        children: vec![title, meta, divider, content],
        style: Some(ui::BoxStyle {
            min_height: Some(DimensionToken::Px { value: 0 }),
            overflow_y: Some(ui::Overflow::Auto),
            ..Default::default()
        }),
        direction: Some(FlexDirection::Column),
        gap: Some(Spacing::Sm),
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: Some(stacked_fit_content(900)),
    }
    .into_component("editor-body")
    .expect("Box encode");

    // Mockup col-editor: the active panel carries an accent border + glow.
    ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: None,
        margin: None,
        children: vec![body, toolbar],
        style: Some(ui::BoxStyle {
            border: Some(BorderEdges::all(BorderSide::new(1, BorderColor::Accent))),
            shadow: Some(ShadowToken::AccentGlow),
            ..panel_style()
        }),
        direction: Some(FlexDirection::Column),
        gap: None,
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: Some(stacked_fit_content(900)),
    }
    .into_component("editor-col")
    .expect("Box encode")
}

// =============================================================================
// Right column — links panel: analysis status, related-note cards (weight %,
// reason, "nowe" chip for links younger than 24 h), detected entity chips,
// open merge suggestions (Scal / Odrzuć) and recent merges (Cofnij).
// =============================================================================

/// Links younger than this show the "nowe" chip (mockup n01).
const FRESH_LINK_SECS: i64 = 86_400;

/// Header strip (mockup n01 links-head): uppercase accent title with a small
/// "Auto" chip, separated from the body by a full-bleed hairline.
fn links_header() -> Component {
    let title = text_c(
        "links-title",
        lit("Powiązania"),
        TextStyle::Overline,
        Some(Tone::Primary),
    );
    let auto_tag = Chip {
        variant: ChipVariant::Soft,
        tone: Tone::Primary,
        label: lit("Auto"),
        icon: None,
        avatar: None,
        selected: None,
        removable: false,
        dot: None,
    }
    .into_component("links-auto-tag")
    .expect("Chip encode");

    Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Sm,
        justify: FlexJustify::SpaceBetween,
        align: FlexAlign::Center,
        wrap: FlexWrap::NoWrap,
        children: vec![title, auto_tag],
        padding: Some(Spacing::Md),
        background: None,
        radius: None,
        style: Some(ui::BoxStyle {
            border: Some(BorderEdges {
                bottom: Some(BorderSide::new(1, BorderColor::Default)),
                ..Default::default()
            }),
            ..Default::default()
        }),
        responsive: None,
    }
    .into_component("links-header")
    .expect("Flex encode")
}

/// Uppercase accent-toned section label inside the links body (mockup n01
/// links-section-title).
fn links_section_title(id: &str, label: &str) -> Component {
    text_c(id, lit(label), TextStyle::Overline, Some(Tone::Muted))
}

/// Framed links column: header strip + scrollable body content.
fn links_panel(children: Vec<Component>) -> Component {
    let body = ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: Some(Spacing::Md),
        margin: None,
        children,
        style: Some(ui::BoxStyle {
            min_height: Some(DimensionToken::Px { value: 0 }),
            overflow_y: Some(ui::Overflow::Auto),
            ..Default::default()
        }),
        direction: Some(FlexDirection::Column),
        gap: Some(Spacing::Md),
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: Some(stacked_fit_content(900)),
    }
    .into_component("links-body")
    .expect("Box encode");

    ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: None,
        margin: None,
        children: vec![links_header(), body],
        style: Some(panel_style()),
        direction: Some(FlexDirection::Column),
        gap: None,
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: Some(stacked_fit_content(900)),
    }
    .into_component("links-col")
    .expect("Box encode")
}

fn links_placeholder() -> Component {
    links_panel(vec![EmptyState {
        icon: icon(IconName::Sparkle),
        heading: lit("Brak powiązań"),
        message: Some(lit("Otwórz notatkę, aby zobaczyć jej powiązania.")),
        primary_action: None,
        secondary_action: None,
        variant: EmptyStateVariant::Compact,
    }
    .into_component("links-none")
    .expect("EmptyState encode")])
}

fn links_fragment(ctx: &UserCtx, n: &NoteDetail) -> Component {
    let related = db::related_notes(ctx, &n.id).unwrap_or_default();
    let entities = db::note_entities(ctx, &n.id).unwrap_or_default();
    let entity_ids: Vec<String> = entities.iter().map(|e| e.id.clone()).collect();
    let suggestions = analysis::open_suggestions_for(ctx, &entity_ids);
    let recent_merges = analysis::recent_merges_for(ctx, &entity_ids);
    let now = db::now_unix();
    let picker_open = load_session().link_picker;

    let mut children: Vec<Component> = Vec::new();

    if let Some((attempts, last_error)) = analysis::queue_state(&n.id) {
        let status = if analysis::is_pending(attempts) {
            text_c(
                "analysis-status",
                lit("W kolejce analizy…"),
                TextStyle::Caption,
                Some(Tone::Muted),
            )
        } else {
            let detail = if last_error.is_empty() {
                "Analiza nie powiodła się.".to_string()
            } else {
                format!("Analiza nie powiodła się: {last_error}")
            };
            text_c(
                "analysis-status",
                lit(detail),
                TextStyle::Caption,
                Some(Tone::Critical),
            )
        };
        children.push(status);
    }

    let related_section: Component = if related.is_empty() {
        EmptyState {
            icon: icon(IconName::Sparkle),
            heading: lit("Brak powiązań"),
            message: Some(lit("Powiązania pojawią się po analizie treści.")),
            primary_action: None,
            secondary_action: None,
            variant: EmptyStateVariant::Compact,
        }
        .into_component("links-empty")
        .expect("EmptyState encode")
    } else {
        ScrollContainer {
            orientation: ScrollOrientation::Vertical,
            height: DimensionToken::Auto,
            max_height: None,
            children: related
                .iter()
                .enumerate()
                .map(|(i, r)| related_card(i, r, now))
                .collect(),
            sticky_header_slot: None,
            virtualize: false,
            gap: Some(Spacing::Sm),
        }
        .into_component("related-list")
        .expect("ScrollContainer encode")
    };
    children.push(related_section);

    children.push(links_section_title("entities-header", "Wykryte encje"));

    let entities_section: Component = if entities.is_empty() {
        text_c(
            "entities-empty",
            lit("Brak wykrytych encji."),
            TextStyle::Caption,
            Some(Tone::Muted),
        )
    } else {
        Cluster {
            gap: Spacing::Xs,
            align: FlexAlign::Center,
            justify: FlexJustify::Start,
            children: entities
                .iter()
                .enumerate()
                .map(|(i, e)| entity_chip(i, &e.name, &e.entity_type))
                .collect(),
            wrap: Some(true),
        }
        .into_component("entity-chips")
        .expect("Cluster encode")
    };
    children.push(entities_section);

    if !suggestions.is_empty() {
        children.push(links_section_title("merge-header", "Sugestia scalenia"));
        for (i, s) in suggestions.iter().enumerate() {
            children.push(merge_suggestion_card(i, s));
        }
    }

    for (i, m) in recent_merges.iter().enumerate() {
        children.push(recent_merge_card(i, m));
    }

    if picker_open {
        children.push(manual_link_picker(ctx, n));
    } else {
        let mut add_btn = Button {
            variant: ButtonVariant::Ghost,
            tone: Tone::Neutral,
            label: lit("Dodaj powiązanie ręcznie"),
            icon_leading: Some(icon(IconName::Plus)),
            icon_trailing: None,
            size: ButtonSize::Sm,
            full_width: true,
            disabled: None,
            loading: None,
            density: Density::Compact,
        }
        .into_component("btn-add-link")
        .expect("Button encode");
        add_btn.handlers = Some(backend(EventKind::Click, "manual_link_open"));
        children.push(add_btn);
    }

    links_panel(children)
}

/// Manual-link picker: a select over the notes the user can read (minus the
/// open note), confirm-on-change, plus a cancel button. Selecting a note
/// inserts a note_links row of kind 'manual' (weight 1.0, both directions).
fn manual_link_picker(ctx: &UserCtx, n: &NoteDetail) -> Component {
    let candidates = db::list_notes(ctx, "all", "").unwrap_or_default();
    let options: Vec<SelectOption> = candidates
        .iter()
        .filter(|c| c.id != n.id)
        .take(50)
        .map(|c| SelectOption {
            value: SelectValue::Text(c.id.clone()),
            label: lit(if c.title.is_empty() {
                "(bez tytułu)"
            } else {
                &c.title
            }),
            icon: Some(icon(IconName::FileText)),
            disabled: false,
            group_id: None,
            description: None,
        })
        .collect();

    let mut select = Select {
        bind_path: state_path(SP_LINK_PICK),
        options,
        placeholder: Some(lit("Wybierz notatkę…")),
        label: Some(lit("Dodaj powiązanie")),
        searchable: true,
        clearable: false,
        virtualize: false,
        disabled: None,
        size: InputSize::Sm,
        groups: None,
    }
    .into_component("link-pick")
    .expect("Select encode");
    select.handlers = Some(backend_params(
        EventKind::Change,
        "manual_link_add",
        vec![("note_id", CborValue::Text(n.id.clone()))],
    ));

    let mut cancel = Button {
        variant: ButtonVariant::Ghost,
        tone: Tone::Neutral,
        label: lit("Anuluj"),
        icon_leading: None,
        icon_trailing: None,
        size: ButtonSize::Sm,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Compact,
    }
    .into_component("link-pick-cancel")
    .expect("Button encode");
    cancel.handlers = Some(backend(EventKind::Click, "manual_link_close"));

    ui::Box {
        width: None,
        grow: None,
        align_self: None,
        padding: None,
        margin: None,
        children: vec![select, cancel],
        style: None,
        direction: Some(FlexDirection::Column),
        gap: Some(Spacing::Sm),
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: None,
    }
    .into_component("link-picker")
    .expect("Box encode")
}

fn related_card(index: usize, r: &db::RelatedNote, now: i64) -> Component {
    let title = if r.title.is_empty() {
        "(bez tytułu)"
    } else {
        &r.title
    };
    let is_fresh = now - r.created_at < FRESH_LINK_SECS;
    let percent = (r.weight.clamp(0.0, 1.0) * 100.0).round() as i64;

    let title_text = Text {
        content: lit(title),
        style: TextStyle::BodyStrong,
        tone: None,
        align: None,
        wrap: None,
        max_lines: Some(2),
        format: None,
        streaming: None,
    }
    .into_component(format!("rel-{index}-title"))
    .expect("Text encode");

    let title_row: Component = if is_fresh {
        let fresh = Chip {
            variant: ChipVariant::Soft,
            tone: Tone::Success,
            label: lit("nowe"),
            icon: None,
            avatar: None,
            selected: None,
            removable: false,
            dot: None,
        }
        .into_component(format!("rel-{index}-fresh"))
        .expect("Chip encode");
        Cluster {
            gap: Spacing::Xs,
            align: FlexAlign::Center,
            justify: FlexJustify::Start,
            children: vec![title_text, fresh],
            wrap: Some(false),
        }
        .into_component(format!("rel-{index}-head"))
        .expect("Cluster encode")
    } else {
        title_text
    };

    // Mockup n01 rel-card: similarity bar with the percent value on the right,
    // then the reason line underneath.
    let bar = ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: None,
        margin: None,
        children: vec![ui::ProgressBar {
            value: lit_value(CborValue::F64(r.weight.clamp(0.0, 1.0))),
            max: 1.0,
            variant: ui::ProgressVariant::Default,
            tone: Tone::Primary,
            show_label: false,
            label: None,
            size: ui::ProgressSize::Sm,
        }
        .into_component(format!("rel-{index}-bar"))
        .expect("ProgressBar encode")],
        style: None,
        direction: Some(FlexDirection::Column),
        gap: None,
        align: Some(FlexAlign::Stretch),
        justify: Some(FlexJustify::Center),
        responsive: None,
    }
    .into_component(format!("rel-{index}-bar-grow"))
    .expect("Box encode");

    let sim_row = Cluster {
        gap: Spacing::Sm,
        align: FlexAlign::Center,
        justify: FlexJustify::Start,
        children: vec![
            bar,
            text_c(
                &format!("rel-{index}-pct"),
                lit(format!("{percent}%")),
                TextStyle::Caption,
                Some(Tone::Primary),
            ),
        ],
        wrap: Some(false),
    }
    .into_component(format!("rel-{index}-sim"))
    .expect("Cluster encode");

    let mut children = vec![title_row, sim_row];
    if !r.reason.is_empty() {
        children.push(text_c(
            &format!("rel-{index}-reason"),
            lit(&r.reason),
            TextStyle::Caption,
            Some(Tone::Muted),
        ));
    }

    let mut card = Card {
        variant: CardVariant::Outlined,
        padding: Spacing::Sm,
        gap: Spacing::Xs,
        radius: ui::RadiusToken::Md,
        shadow: ShadowToken::None,
        border: BorderToken::Hairline,
        background: ui::BackgroundToken::Subtle,
        accent: if is_fresh { Some(Tone::Primary) } else { None },
        children,
        interactive: true,
        clickable: true,
        style: None,
    }
    .into_component(format!("rel-{index}"))
    .expect("Card encode");
    card.handlers = Some(backend_params(
        EventKind::Click,
        "open_note",
        vec![("note_id", CborValue::Text(r.id.clone()))],
    ));
    card
}

fn merge_suggestion_card(index: usize, s: &analysis::MergeSuggestionView) -> Component {
    let percent = (s.similarity.clamp(0.0, 1.0) * 100.0).round() as i64;
    let text = text_c(
        &format!("msug-{index}-text"),
        lit(format!(
            "„{}” i „{}” wyglądają na tę samą encję — zbieżność nazw {percent}%.",
            s.name_a, s.name_b
        )),
        TextStyle::Caption,
        None,
    );

    let mut accept = Button {
        variant: ButtonVariant::Primary,
        tone: Tone::Primary,
        label: lit("Scal"),
        icon_leading: None,
        icon_trailing: None,
        size: ButtonSize::Sm,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Compact,
    }
    .into_component(format!("msug-{index}-accept"))
    .expect("Button encode");
    accept.handlers = Some(backend_params(
        EventKind::Click,
        "merge_accept",
        vec![("suggestion_id", CborValue::Text(s.id.clone()))],
    ));

    let mut reject = Button {
        variant: ButtonVariant::Ghost,
        tone: Tone::Neutral,
        label: lit("Odrzuć"),
        icon_leading: None,
        icon_trailing: None,
        size: ButtonSize::Sm,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Compact,
    }
    .into_component(format!("msug-{index}-reject"))
    .expect("Button encode");
    reject.handlers = Some(backend_params(
        EventKind::Click,
        "merge_reject",
        vec![("suggestion_id", CborValue::Text(s.id.clone()))],
    ));

    let actions = Cluster {
        gap: Spacing::Sm,
        align: FlexAlign::Center,
        justify: FlexJustify::Start,
        children: vec![accept, reject],
        wrap: Some(false),
    }
    .into_component(format!("msug-{index}-actions"))
    .expect("Cluster encode");

    Card {
        variant: CardVariant::Outlined,
        padding: Spacing::Sm,
        gap: Spacing::Sm,
        radius: ui::RadiusToken::Md,
        shadow: ShadowToken::None,
        border: BorderToken::Accent { tone: Tone::Info },
        background: ui::BackgroundToken::Subtle,
        accent: Some(Tone::Info),
        children: vec![text, actions],
        interactive: false,
        clickable: false,
        style: None,
    }
    .into_component(format!("msug-{index}"))
    .expect("Card encode")
}

fn recent_merge_card(index: usize, m: &analysis::RecentMergeView) -> Component {
    let text = text_c(
        &format!("mundo-{index}-text"),
        lit(format!("Scalono „{}” z „{}”.", m.from_name, m.into_name)),
        TextStyle::Caption,
        Some(Tone::Muted),
    );
    let mut undo = Button {
        variant: ButtonVariant::Ghost,
        tone: Tone::Neutral,
        label: lit("Cofnij scalenie"),
        icon_leading: None,
        icon_trailing: None,
        size: ButtonSize::Sm,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Compact,
    }
    .into_component(format!("mundo-{index}-btn"))
    .expect("Button encode");
    undo.handlers = Some(backend_params(
        EventKind::Click,
        "merge_undo",
        vec![("merge_id", CborValue::Text(m.merge_id.clone()))],
    ));

    Card {
        variant: CardVariant::Outlined,
        padding: Spacing::Sm,
        gap: Spacing::Xs,
        radius: ui::RadiusToken::Md,
        shadow: ShadowToken::None,
        border: BorderToken::Hairline,
        background: ui::BackgroundToken::Subtle,
        accent: None,
        children: vec![text, undo],
        interactive: false,
        clickable: false,
        style: None,
    }
    .into_component(format!("mundo-{index}"))
    .expect("Card encode")
}

fn entity_chip(index: usize, name: &str, entity_type: &str) -> Component {
    entity_chip_id(&format!("ent-{index}"), name, entity_type)
}

/// Neutral micro-chip with a type-colored leading dot (mockup n01 ent-chip).
fn entity_chip_id(id: &str, name: &str, entity_type: &str) -> Component {
    let tone = match entity_type {
        "person" => Tone::Info,
        "company" => Tone::Success,
        "project" => Tone::Primary,
        "topic" => Tone::Warning,
        _ => Tone::Neutral,
    };
    Chip {
        variant: ChipVariant::Soft,
        tone: Tone::Neutral,
        label: lit(name),
        icon: None,
        avatar: None,
        selected: None,
        removable: false,
        dot: Some(tone),
    }
    .into_component(id)
    .expect("Chip encode")
}

// =============================================================================
// Shared fragments
// =============================================================================

fn error_fragment(id: &str, message: &str) -> Component {
    EmptyState {
        icon: icon(IconName::Warning),
        heading: lit("Coś poszło nie tak"),
        message: Some(lit(message)),
        primary_action: None,
        secondary_action: None,
        variant: EmptyStateVariant::Compact,
    }
    .into_component(id)
    .expect("EmptyState encode")
}

/// Framed panel column (mockup n01): subtle background, hairline border and
/// rounded corners through tokens, full height.
fn panel_style() -> ui::BoxStyle {
    ui::BoxStyle {
        background: Some(ui::BackgroundToken::Subtle),
        border: Some(BorderEdges::all(BorderSide::new(1, BorderColor::Default))),
        radius: Some(CornerValues::all(RadiusValue::Token {
            value: ui::RadiusToken::Lg,
        })),
        ..full_height_style()
    }
}

fn full_height_style() -> ui::BoxStyle {
    ui::BoxStyle {
        margin: None,
        padding: None,
        border: None,
        background: None,
        radius: None,
        width: None,
        height: Some(DimensionToken::Full),
        min_width: None,
        min_height: None,
        max_width: None,
        max_height: None,
        overflow_x: None,
        overflow_y: None,
        shadow: None,
    }
}

// =============================================================================
// UI action dispatcher
// =============================================================================

pub fn handle_ui_action(action_id: &str, params: &JsonValue) -> JsonValue {
    let user_id = session_user();
    let ctx = match db::resolve_user_ctx(&user_id) {
        Ok(c) => c,
        Err(e) => return json!({"ok": false, "error": e}),
    };

    let result = match action_id {
        "list_notes" => {
            send_list(&ctx);
            json!({"ok": true})
        }
        "new_note" => action_new_note(&ctx),
        "open_note" => action_open_note(&ctx, params),
        "save_note" => action_save_note(&ctx, params),
        "save_tags" => action_save_tags(&ctx, params),
        "set_share" => action_set_share(&ctx, params),
        "delete_note" => action_delete_note(&ctx, params),
        "set_filter" => action_set_filter(&ctx, params),
        "set_search" => action_set_search(&ctx, params),
        "merge_accept" => action_merge_decision(&ctx, params, true),
        "merge_reject" => action_merge_decision(&ctx, params, false),
        "merge_undo" => action_merge_undo(&ctx, params),
        "manual_link_open" => action_link_picker(&ctx, true),
        "manual_link_close" => action_link_picker(&ctx, false),
        "manual_link_add" => action_manual_link_add(&ctx, params),
        other => json!({"ok": false, "error": format!("Nieznana akcja: {other}")}),
    };

    // Opportunistic analysis drain: at most ONE queued note per request so the
    // UI response is delayed by at most one embed+extract pass. The 3 s
    // debounce keeps the note being typed out of the drain; set_search is
    // excluded as the highest-frequency action. Heavy lifting (LLM/embed) runs
    // as host calls and does not burn wasm fuel — the in-wasm work (chunking,
    // JSON parsing) is far below the 200M fuel budget.
    if action_id != "set_search" {
        let processed = analysis::process_queue(1);
        if !processed.is_empty() {
            let active = load_session().active;
            // Refresh the panel only when the open note gained fresh links and
            // the user is not mid-typing (save_note keeps the caret alive).
            if processed.iter().any(|id| id == &active) && action_id != "save_note" {
                if let Ok(Some(note)) = db::get_note(&ctx, &active) {
                    send_main(&ctx, Some(&note));
                }
            }
        }
    }

    result
}

fn action_merge_decision(ctx: &UserCtx, params: &JsonValue, accept: bool) -> JsonValue {
    let suggestion_id = match param_str(params, "suggestion_id") {
        Some(id) => id.to_string(),
        None => return json!({"ok": false, "error": "Brak suggestion_id"}),
    };
    let outcome = if accept {
        analysis::merge_accept(ctx, &suggestion_id)
    } else {
        analysis::merge_reject(ctx, &suggestion_id)
    };
    match outcome {
        Ok(()) => {
            refresh_active_main(ctx);
            json!({"ok": true})
        }
        Err(e) => {
            feedback_err(&e);
            json!({"ok": false, "error": e})
        }
    }
}

fn action_merge_undo(ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let merge_id = match param_str(params, "merge_id") {
        Some(id) => id.to_string(),
        None => return json!({"ok": false, "error": "Brak merge_id"}),
    };
    match analysis::merge_undo(ctx, &merge_id) {
        Ok(()) => {
            refresh_active_main(ctx);
            json!({"ok": true})
        }
        Err(e) => {
            feedback_err(&e);
            json!({"ok": false, "error": e})
        }
    }
}

fn action_link_picker(ctx: &UserCtx, open: bool) -> JsonValue {
    let mut sess = load_session();
    sess.link_picker = open;
    store_session(&sess);
    refresh_active_main(ctx);
    json!({"ok": true})
}

fn action_manual_link_add(ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let src = match param_str(params, "note_id") {
        Some(id) => id.to_string(),
        None => return json!({"ok": false, "error": "Brak note_id"}),
    };
    let dst = match param_str(params, "value") {
        Some(id) => id.to_string(),
        None => return json!({"ok": false, "error": "Brak wybranej notatki"}),
    };
    match db::manual_link(ctx, &src, &dst) {
        Ok(()) => {
            let mut sess = load_session();
            sess.link_picker = false;
            store_session(&sess);
            refresh_active_main(ctx);
            feedback_ok("Dodano powiązanie");
            json!({"ok": true})
        }
        Err(e) => {
            feedback_err(&e);
            json!({"ok": false, "error": e})
        }
    }
}

/// Re-renders the main slot for the currently open note (links panel data
/// changed outside the editor: merge decision, background analysis).
fn refresh_active_main(ctx: &UserCtx) {
    let active = load_session().active;
    if active.is_empty() {
        return;
    }
    if let Ok(Some(note)) = db::get_note(ctx, &active) {
        send_main(ctx, Some(&note));
    }
}

/// Renders the full panel for a fresh open: shell, list and the empty editor.
pub fn render_full(user_id: &str) {
    send_panel_shell();
    let sess = Session::default();
    store_session(&sess);
    match db::resolve_user_ctx(user_id) {
        Ok(ctx) => {
            send_list(&ctx);
            send_main(&ctx, None);
        }
        Err(e) => {
            send_slot(SLOT_LIST, error_fragment("list-error", &e), None);
            send_slot(SLOT_MAIN, error_fragment("main-error", &e), None);
        }
    }
}

fn param_str<'a>(params: &'a JsonValue, key: &str) -> Option<&'a str> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

fn action_new_note(ctx: &UserCtx) -> JsonValue {
    let id = match db::create_note(ctx) {
        Ok(id) => id,
        Err(e) => {
            feedback_err(&e);
            return json!({"ok": false, "error": e});
        }
    };
    let mut sess = load_session();
    sess.active = id.clone();
    store_session(&sess);
    open_active(ctx, &id);
    json!({"ok": true, "note_id": id})
}

fn action_open_note(ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let note_id = match param_str(params, "note_id") {
        Some(id) => id.to_string(),
        None => return json!({"ok": false, "error": "Brak note_id"}),
    };
    let mut sess = load_session();
    sess.active = note_id.clone();
    store_session(&sess);
    open_active(ctx, &note_id);
    json!({"ok": true})
}

fn open_active(ctx: &UserCtx, note_id: &str) {
    match db::get_note(ctx, note_id) {
        Ok(Some(note)) => {
            send_main(ctx, Some(&note));
        }
        Ok(None) => {
            send_main(ctx, None);
            feedback_err("Notatka nie istnieje lub nie masz do niej dostępu.");
        }
        Err(e) => {
            send_slot(SLOT_MAIN, error_fragment("main-error", &e), None);
        }
    }
    send_list(ctx);
}

/// Hard-limit check for one save_note value. Pure — unit-tested natively.
fn validate_note_field(field: &str, value: &str) -> Result<(), String> {
    match field {
        "title" if value.chars().count() > MAX_TITLE_CHARS => Err(format!(
            "Tytuł przekracza limit {MAX_TITLE_CHARS} znaków — nie zapisano."
        )),
        "content" if value.len() > MAX_CONTENT_BYTES => Err(format!(
            "Treść przekracza limit {} KB — nie zapisano.",
            MAX_CONTENT_BYTES / 1024
        )),
        _ => Ok(()),
    }
}

/// Hard-limit check for the tag set. Pure — unit-tested natively.
fn validate_tags(tags: &[String]) -> Result<(), String> {
    if tags.len() > MAX_TAGS {
        return Err(format!("Za dużo tagów (limit {MAX_TAGS}) — nie zapisano."));
    }
    if tags.iter().any(|t| t.chars().count() > MAX_TAG_CHARS) {
        return Err(format!(
            "Tag przekracza limit {MAX_TAG_CHARS} znaków — nie zapisano."
        ));
    }
    Ok(())
}

fn action_save_note(ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let note_id = match param_str(params, "note_id") {
        Some(id) => id.to_string(),
        None => return json!({"ok": false, "error": "Brak note_id"}),
    };
    let field = param_str(params, "field").unwrap_or("content");
    let raw = params.get("value").and_then(|v| v.as_str()).unwrap_or("");
    // The title renders as a single-row auto-grow textarea — typed or pasted
    // line breaks become spaces so the stored title stays one logical line.
    let value: std::borrow::Cow<'_, str> = if field == "title" && raw.contains(['\r', '\n']) {
        std::borrow::Cow::Owned(raw.replace(['\r', '\n'], " "))
    } else {
        std::borrow::Cow::Borrowed(raw)
    };
    let value = value.as_ref();
    if let Err(e) = validate_note_field(field, value) {
        feedback_err(&e);
        return json!({"ok": false, "error": e});
    }

    match db::update_note_field(ctx, &note_id, field, value) {
        Ok(()) => {
            analysis::enqueue(&note_id);
            let mut ops = save_feedback_ops(Some("Zapisano · przed chwilą"), None);
            if field == "content" {
                ops.push(PatchOp {
                    path: state_path(SP_CHAR_COUNT),
                    op: PatchOpKind::Set {
                        value: CborValue::Text(db::counter_label(value)),
                    },
                });
            }
            send_state_patch(ops);
            // The editor slot is untouched (keeps caret/scroll); the list gets
            // a fresh title/preview/updated-at.
            send_list(ctx);
            json!({"ok": true})
        }
        Err(e) => {
            feedback_err(&e);
            json!({"ok": false, "error": e})
        }
    }
}

fn action_save_tags(ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let note_id = match param_str(params, "note_id") {
        Some(id) => id.to_string(),
        None => return json!({"ok": false, "error": "Brak note_id"}),
    };
    let tags: Vec<String> = params
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|t| t.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if let Err(e) = validate_tags(&tags) {
        feedback_err(&e);
        return json!({"ok": false, "error": e});
    }
    match db::set_tags(ctx, &note_id, &tags) {
        Ok(()) => {
            analysis::enqueue(&note_id);
            feedback_ok("Zapisano · przed chwilą");
            json!({"ok": true})
        }
        Err(e) => {
            feedback_err(&e);
            json!({"ok": false, "error": e})
        }
    }
}

fn action_set_share(ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let note_id = match param_str(params, "note_id") {
        Some(id) => id.to_string(),
        None => return json!({"ok": false, "error": "Brak note_id"}),
    };
    let mode = params
        .get("value")
        .and_then(|v| v.as_str())
        .unwrap_or("private");
    match db::set_share_mode(ctx, &note_id, mode) {
        Ok(()) => {
            let mut ops = save_feedback_ops(Some("Zapisano · przed chwilą"), None);
            ops.push(PatchOp {
                path: state_path(SP_SHARE),
                op: PatchOpKind::Set {
                    value: CborValue::Text(mode.to_string()),
                },
            });
            send_state_patch(ops);
            send_list(ctx);
            json!({"ok": true})
        }
        Err(e) => {
            feedback_err(&e);
            json!({"ok": false, "error": e})
        }
    }
}

fn action_delete_note(ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let note_id = match param_str(params, "note_id") {
        Some(id) => id.to_string(),
        None => return json!({"ok": false, "error": "Brak note_id"}),
    };
    match db::delete_note(ctx, &note_id) {
        Ok(()) => {
            // The queue worker sees deleted_at and runs the tombstone cleanup
            // (graph node + vectors through the outbox).
            analysis::enqueue(&note_id);
            let mut sess = load_session();
            if sess.active == note_id {
                sess.active.clear();
                store_session(&sess);
            }
            send_main(ctx, None);
            send_list(ctx);
            json!({"ok": true})
        }
        Err(e) => {
            feedback_err(&e);
            json!({"ok": false, "error": e})
        }
    }
}

fn action_set_filter(ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let scope = params
        .get("chip_id")
        .and_then(|v| v.as_str())
        .unwrap_or("all");
    let scope = if matches!(scope, "all" | "mine" | "shared" | "group" | "org") {
        scope
    } else {
        "all"
    };
    let mut sess = load_session();
    sess.scope = scope.to_string();
    store_session(&sess);
    send_list(ctx);
    json!({"ok": true, "scope": scope})
}

fn action_set_search(ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("");
    if value.chars().count() > MAX_SEARCH_CHARS {
        let e = format!("Fraza wyszukiwania przekracza limit {MAX_SEARCH_CHARS} znaków.");
        feedback_err(&e);
        return json!({"ok": false, "error": e});
    }
    let mut sess = load_session();
    sess.search = value.trim().to_string();
    store_session(&sess);
    send_list(ctx);
    json!({"ok": true})
}

// =============================================================================
// Tests — pure UI helpers (no host fns on the native target)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_take_first_letters_of_two_words() {
        assert_eq!(initials("Piotr Jarocki"), "PJ");
        assert_eq!(initials("Ala"), "A");
        assert_eq!(initials("żaneta kowalska-nowak"), "ŻK");
        assert_eq!(initials(""), "");
    }

    #[test]
    fn note_card_click_carries_note_id_param() {
        let n = NoteSummary {
            id: "note_1".into(),
            title: "T".into(),
            preview: "p".into(),
            updated_at: 1,
            scope: "private".into(),
            entities: vec![("Nexadata".into(), "company".into())],
        };
        let card = note_card(0, &n, "note_1");
        let handlers = card.handlers.expect("handlers");
        match &handlers.0[0] {
            (
                EventKind::Click,
                Handler::Backend {
                    action_id, params, ..
                },
            ) => {
                assert_eq!(action_id, "open_note");
                assert_eq!(
                    params.0,
                    vec![("note_id".to_string(), CborValue::Text("note_1".into()))]
                );
            }
            other => panic!("expected backend click handler, got {other:?}"),
        }
    }

    #[test]
    fn list_fragment_renders_empty_state_without_notes() {
        let frag = list_fragment(&[], &Session::default());
        // list-col → [list-head, list-body]; list-body wraps the EmptyState.
        let body_tag = {
            let col = ui::Box::try_from_component(&frag).expect("Box decode");
            let body = ui::Box::try_from_component(&col.children[1]).expect("Box decode");
            body.children[0].tag
        };
        assert_eq!(body_tag, EmptyState::TAG);
    }

    #[test]
    fn note_field_limits_reject_oversized_values() {
        assert!(validate_note_field("title", &"t".repeat(MAX_TITLE_CHARS)).is_ok());
        assert!(validate_note_field("title", &"t".repeat(MAX_TITLE_CHARS + 1)).is_err());
        assert!(validate_note_field("content", &"c".repeat(MAX_CONTENT_BYTES)).is_ok());
        assert!(validate_note_field("content", &"c".repeat(MAX_CONTENT_BYTES + 1)).is_err());
        // Unknown fields pass here — db::update_note_field rejects them.
        assert!(validate_note_field("other", "x").is_ok());
    }

    #[test]
    fn tag_limits_reject_too_many_and_too_long() {
        let ok: Vec<String> = (0..MAX_TAGS).map(|i| format!("t{i}")).collect();
        assert!(validate_tags(&ok).is_ok());
        let too_many: Vec<String> = (0..MAX_TAGS + 1).map(|i| format!("t{i}")).collect();
        assert!(validate_tags(&too_many).is_err());
        assert!(validate_tags(&["x".repeat(MAX_TAG_CHARS + 1)]).is_err());
        assert!(validate_tags(&["x".repeat(MAX_TAG_CHARS)]).is_ok());
    }

    #[test]
    fn scope_badge_maps_all_scopes() {
        for (scope, label_contains) in [
            ("org", "Organizacja"),
            ("group", "Grupa"),
            ("user", "Udostępniona"),
            ("private", "Prywatna"),
        ] {
            let badge = scope_badge("b", scope);
            let decoded = Badge::try_from_component(&badge).expect("Badge decode");
            assert_eq!(decoded.label, lit(label_contains), "scope {scope}");
        }
    }
}
