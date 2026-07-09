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
    Avatar, AvatarRef, AvatarShape, AvatarSize, Badge, BadgeVariant, BindRef, BorderToken, Button,
    ButtonSize, ButtonVariant, CachePolicy, Card, CardVariant, CborMap, Chip, ChipVariant, Cluster,
    Component, Density, DimensionToken, Divider, DividerOrientation, DividerVariant, EmptyState,
    EmptyStateVariant, EventKind, FailurePolicy, FilterChipDef, FilterChips, FilterChipsMode, Flex,
    FlexAlign, FlexDirection, FlexJustify, FlexWrap, Handler, HandlerMap, IconName, IconRef, Input,
    InputSize, InputType, PanelShell, PatchOp, PatchOpKind, ScrollContainer, ScrollOrientation,
    SearchBox, SearchVariant, SectionHeader, Select, SelectOption, SelectValue, ShadowToken,
    SlotContent, SlotDecl, SlotDefault, SlotSemantics, SlotVisibility, Spacing, Split,
    SplitOrientation, SplitSize, StateEntry, StatePatch, TagInput, Text, TextStyle, Textarea, Tone,
    UiPayload, Value as CborValue, ValueFormat,
};
use tentaflow_addon_sdk::{state_get, state_set, StateTier};

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
const SP_CHAR_COUNT: &str = "editor.char_count";

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
}

impl Default for Session {
    fn default() -> Self {
        Session {
            scope: "all".to_string(),
            search: String::new(),
            active: String::new(),
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
        },
        None => Session::default(),
    }
}

fn store_session(sess: &Session) {
    let v = json!({"scope": sess.scope, "search": sess.search, "active": sess.active});
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

fn patch_set(key: &str, value: CborValue) {
    send_state_patch(vec![PatchOp {
        path: state_path(key),
        op: PatchOpKind::Set { value },
    }]);
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

pub fn send_panel_shell() {
    let layout = Split {
        orientation: SplitOrientation::Horizontal,
        primary_size: SplitSize::Px { value: 300 },
        min_primary: 240,
        max_primary: 420,
        resizable: true,
        primary_slot: SLOT_LIST.into(),
        secondary_slot: SLOT_MAIN.into(),
    }
    .into_component("root")
    .expect("Split encode");

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
                path: state_path(SP_CHAR_COUNT),
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
    let mut search = SearchBox {
        bind_path: state_path(SP_SEARCH),
        placeholder: lit("Szukaj w notatkach…"),
        debounce_ms: 300,
        variant: SearchVariant::Default,
        shortcut_hint: None,
        on_search_action_id: None,
    }
    .into_component("list-search")
    .expect("SearchBox encode");
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

    ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: Some(Spacing::Sm),
        margin: None,
        children: vec![new_btn, search, chips, body],
        style: Some(full_height_style()),
        direction: Some(FlexDirection::Column),
        gap: Some(Spacing::Sm),
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: None,
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
        max_lines: Some(1),
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
        children: vec![title, preview, meta],
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
        responsive: None,
    }
    .into_component("editor-pane")
    .expect("Box encode");

    let links_col = ui::Box {
        width: None,
        grow: None,
        align_self: None,
        padding: None,
        margin: None,
        children: vec![links],
        style: Some(min_width_style(240)),
        direction: Some(FlexDirection::Column),
        gap: None,
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: None,
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
        StateEntry {
            path: state_path(SP_SAVE_STATUS),
            value: CborValue::Text(String::new()),
        },
    ]
}

fn empty_editor_overlay() -> Vec<StateEntry> {
    vec![
        StateEntry {
            path: state_path(SP_SAVE_STATUS),
            value: CborValue::Text(String::new()),
        },
        StateEntry {
            path: state_path(SP_CHAR_COUNT),
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

    EmptyState {
        icon: icon(IconName::FileText),
        heading: lit("Wybierz notatkę"),
        message: Some(lit("Wybierz notatkę z listy po lewej albo utwórz nową.")),
        primary_action: Some(new_btn),
        secondary_action: None,
        variant: EmptyStateVariant::Default,
    }
    .into_component("editor-empty")
    .expect("EmptyState encode")
}

fn editor_fragment(ctx: &UserCtx, n: &NoteDetail) -> Component {
    let readonly_bind = if n.can_write {
        None
    } else {
        Some(BindRef::Literal(CborValue::Bool(true)))
    };

    let mut title = Input {
        r#type: InputType::Text,
        bind_path: state_path(SP_TITLE),
        placeholder: Some(lit("Tytuł notatki…")),
        label: None,
        hint: None,
        leading_icon: None,
        trailing_icon: None,
        prefix: None,
        suffix: None,
        validators: vec![],
        max_length: Some(MAX_TITLE_CHARS as u16),
        min_length: None,
        pattern: None,
        autocomplete: None,
        input_mode: None,
        disabled: None,
        readonly: readonly_bind.clone(),
        error: None,
        size: InputSize::Lg,
    }
    .into_component("note-title")
    .expect("Input encode");
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

    let meta_left = Cluster {
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
        let mut select = Select {
            bind_path: state_path(SP_SHARE),
            options,
            placeholder: None,
            label: Some(lit("Udostępnij")),
            searchable: false,
            clearable: false,
            virtualize: false,
            disabled: None,
            size: InputSize::Sm,
            groups: None,
        }
        .into_component("share-select")
        .expect("Select encode");
        select.handlers = Some(backend_params(
            EventKind::Change,
            "set_share",
            vec![("note_id", CborValue::Text(n.id.clone()))],
        ));
        select
    } else {
        scope_badge("editor-scope-badge", &n.scope)
    };

    let meta = Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Md,
        justify: FlexJustify::SpaceBetween,
        align: FlexAlign::Center,
        wrap: FlexWrap::Wrap,
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

    let mut content = Textarea {
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
    }
    .into_component("note-content")
    .expect("Textarea encode");
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
    let save_status = text_c(
        "save-status",
        bound(SP_SAVE_STATUS),
        TextStyle::Caption,
        Some(Tone::Success),
    );

    let mut toolbar_children = vec![counter];
    let mut right = vec![save_status];
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

    let toolbar = Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Md,
        justify: FlexJustify::SpaceBetween,
        align: FlexAlign::Center,
        wrap: FlexWrap::Wrap,
        children: toolbar_children,
        padding: Some(Spacing::Sm),
        background: Some(ui::BackgroundToken::Subtle),
        radius: Some(ui::RadiusToken::Md),
        style: None,
        responsive: None,
    }
    .into_component("editor-toolbar")
    .expect("Flex encode");

    ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: Some(Spacing::Md),
        margin: None,
        children: vec![title, meta, divider, content, toolbar],
        style: Some(full_height_style()),
        direction: Some(FlexDirection::Column),
        gap: Some(Spacing::Sm),
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: None,
    }
    .into_component("editor-col")
    .expect("Box encode")
}

// =============================================================================
// Right column — links panel (real empty states; data appears once the
// auto-graph stage populates note_links / note_entities)
// =============================================================================

fn links_header() -> Component {
    SectionHeader {
        title: lit("Powiązania"),
        subtitle: Some(lit("Auto")),
        actions: vec![],
        divider: true,
    }
    .into_component("links-header")
    .expect("SectionHeader encode")
}

fn links_placeholder() -> Component {
    ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: Some(Spacing::Md),
        margin: None,
        children: vec![
            links_header(),
            EmptyState {
                icon: icon(IconName::Sparkle),
                heading: lit("Brak powiązań"),
                message: Some(lit("Otwórz notatkę, aby zobaczyć jej powiązania.")),
                primary_action: None,
                secondary_action: None,
                variant: EmptyStateVariant::Compact,
            }
            .into_component("links-none")
            .expect("EmptyState encode"),
        ],
        style: Some(full_height_style()),
        direction: Some(FlexDirection::Column),
        gap: Some(Spacing::Md),
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: None,
    }
    .into_component("links-col")
    .expect("Box encode")
}

fn links_fragment(ctx: &UserCtx, n: &NoteDetail) -> Component {
    let related = db::related_notes(ctx, &n.id).unwrap_or_default();
    let entities = db::note_entities(ctx, &n.id).unwrap_or_default();

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
                .map(|(i, r)| related_card(i, r))
                .collect(),
            sticky_header_slot: None,
            virtualize: false,
            gap: Some(Spacing::Sm),
        }
        .into_component("related-list")
        .expect("ScrollContainer encode")
    };

    let entities_header = SectionHeader {
        title: lit("Wykryte encje"),
        subtitle: None,
        actions: vec![],
        divider: false,
    }
    .into_component("entities-header")
    .expect("SectionHeader encode");

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
                .map(|(i, (name, etype))| entity_chip(i, name, etype))
                .collect(),
            wrap: Some(true),
        }
        .into_component("entity-chips")
        .expect("Cluster encode")
    };

    ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: Some(Spacing::Md),
        margin: None,
        children: vec![
            links_header(),
            related_section,
            entities_header,
            entities_section,
        ],
        style: Some(full_height_style()),
        direction: Some(FlexDirection::Column),
        gap: Some(Spacing::Md),
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: None,
    }
    .into_component("links-col")
    .expect("Box encode")
}

fn related_card(index: usize, r: &JsonValue) -> Component {
    let title = r["title"].as_str().unwrap_or("(bez tytułu)");
    let reason = r["reason"].as_str().unwrap_or("");
    let note_id = r["id"].as_str().unwrap_or("");

    let mut children = vec![text_c(
        &format!("rel-{index}-title"),
        lit(title),
        TextStyle::BodyStrong,
        None,
    )];
    if !reason.is_empty() {
        children.push(text_c(
            &format!("rel-{index}-reason"),
            lit(reason),
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
        accent: None,
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
        vec![("note_id", CborValue::Text(note_id.to_string()))],
    ));
    card
}

fn entity_chip(index: usize, name: &str, entity_type: &str) -> Component {
    let tone = match entity_type {
        "person" => Tone::Info,
        "company" => Tone::Success,
        "project" => Tone::Primary,
        "topic" => Tone::Warning,
        _ => Tone::Neutral,
    };
    Chip {
        variant: ChipVariant::Soft,
        tone,
        label: lit(name),
        icon: None,
        avatar: None,
        selected: None,
        removable: false,
    }
    .into_component(format!("ent-{index}"))
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

    match action_id {
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
        other => json!({"ok": false, "error": format!("Nieznana akcja: {other}")}),
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
            patch_set(SP_SAVE_STATUS, CborValue::Text(e.clone()));
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
            patch_set(
                SP_SAVE_STATUS,
                CborValue::Text("Notatka nie istnieje lub nie masz do niej dostępu.".into()),
            );
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
    let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("");
    if let Err(e) = validate_note_field(field, value) {
        patch_set(SP_SAVE_STATUS, CborValue::Text(e.clone()));
        return json!({"ok": false, "error": e});
    }

    match db::update_note_field(ctx, &note_id, field, value) {
        Ok(()) => {
            let mut ops = vec![PatchOp {
                path: state_path(SP_SAVE_STATUS),
                op: PatchOpKind::Set {
                    value: CborValue::Text("Zapisano przed chwilą".into()),
                },
            }];
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
            patch_set(SP_SAVE_STATUS, CborValue::Text(e.clone()));
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
        patch_set(SP_SAVE_STATUS, CborValue::Text(e.clone()));
        return json!({"ok": false, "error": e});
    }
    match db::set_tags(ctx, &note_id, &tags) {
        Ok(()) => {
            patch_set(
                SP_SAVE_STATUS,
                CborValue::Text("Zapisano przed chwilą".into()),
            );
            json!({"ok": true})
        }
        Err(e) => {
            patch_set(SP_SAVE_STATUS, CborValue::Text(e.clone()));
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
            send_state_patch(vec![
                PatchOp {
                    path: state_path(SP_SHARE),
                    op: PatchOpKind::Set {
                        value: CborValue::Text(mode.to_string()),
                    },
                },
                PatchOp {
                    path: state_path(SP_SAVE_STATUS),
                    op: PatchOpKind::Set {
                        value: CborValue::Text("Zapisano przed chwilą".into()),
                    },
                },
            ]);
            send_list(ctx);
            json!({"ok": true})
        }
        Err(e) => {
            patch_set(SP_SAVE_STATUS, CborValue::Text(e.clone()));
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
            patch_set(SP_SAVE_STATUS, CborValue::Text(e.clone()));
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
        patch_set(SP_SAVE_STATUS, CborValue::Text(e.clone()));
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
        // Box → children[3] is the body (EmptyState tag 0x0003).
        let body_tag = {
            let boxed = ui::Box::try_from_component(&frag).expect("Box decode");
            boxed.children[3].tag
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
