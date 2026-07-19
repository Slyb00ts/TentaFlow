// =============================================================================
// File: addons/notes/src/ui_share.rs
// Purpose: note-sharing modal (mockup n03). The editor's "Udostępnij" button
//          opens a Modal (body/footer as SlotSemantics::Modal slots, the
//          tentavision pattern) over a per-session DRAFT kept in the host KV:
//          live directory suggestions (state-driven — the search box is never
//          re-rendered while typing), Osoby/Grupy sections with per-entry
//          access selects, an org-wide read-only toggle and a graph-impact
//          callout. "Zapisz dostępy" commits the draft transactionally
//          (owner-only, directory-validated) via db::replace_all_shares.
// =============================================================================

use serde_json::{json, Value as JsonValue};

use tentaflow_addon_sdk::ui_v1::{self as ui, backend, bound, lit, state_path};

use tentaflow_addon_sdk::ui_v1::{
    Avatar, AvatarGroup, AvatarRef, AvatarShape, AvatarSize, Button, ButtonSize,
    ButtonVariant, Callout, Card, CardVariant, CborMap, Chip, ChipVariant, Cluster, Component,
    Density, EventKind, FailurePolicy, Flex, FlexAlign, FlexDirection, FlexJustify, FlexWrap,
    Handler, HandlerMap, IconButton, IconName, InputSize, ModalSize, PatchOp, PatchOpKind,
    SearchBox, SearchVariant, Select, SelectOption, SelectValue, ShadowToken, StateEntry,
    TextStyle, Toggle, TogglePosition, ToggleSize, Tone,
    Value as CborValue,
};
use tentaflow_addon_sdk::{
    directory_groups, directory_org, directory_users, state_get, state_set, StateTier,
};

use crate::db::{self, ShareEntry, UserCtx};
use crate::ui::panel_epoch;

pub const SLOT_SHARE_BODY: &str = "share-body";
pub const SLOT_SHARE_FOOT: &str = "share-foot";

// State paths of the modal.
const SP_SEARCH: &str = "share.search";
const SP_SUMMARY: &str = "share.summary";
const SP_ORG: &str = "share.org";

/// Live suggestion rows pre-rendered in the body (state-driven visibility).
const SUGGESTION_ROWS: usize = 5;

fn sug_path(i: usize, field: &str) -> String {
    format!("share.sug.{i}.{field}")
}

// =============================================================================
// Draft (per user+epoch, host KV) — the modal edits this, save commits it.
// =============================================================================

#[derive(Debug, Clone, Default)]
pub(crate) struct DraftEntry {
    pub id: String,
    pub name: String,
    /// Email (users) / member-count line (groups).
    pub sub: String,
    /// "read" | "write".
    pub access: String,
    /// RBAC role of a user entry (`user`|`power_user`|`admin`); empty for groups.
    pub role: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ShareDraft {
    pub note_id: String,
    pub users: Vec<DraftEntry>,
    pub groups: Vec<DraftEntry>,
    pub org_read: bool,
    /// Subject added by the LAST pick — its row gets the "nowe" chip.
    pub last_added: String,
}

fn draft_key() -> String {
    format!("sharedraft:{}:{}", crate::ui::session_user(), panel_epoch())
}

fn suggestions_key() -> String {
    format!("sharesug:{}:{}", crate::ui::session_user(), panel_epoch())
}

fn entry_to_json(e: &DraftEntry) -> JsonValue {
    json!({"id": e.id, "name": e.name, "sub": e.sub, "access": e.access, "role": e.role})
}

fn entry_from_json(v: &JsonValue) -> DraftEntry {
    DraftEntry {
        id: v["id"].as_str().unwrap_or("").to_string(),
        name: v["name"].as_str().unwrap_or("").to_string(),
        sub: v["sub"].as_str().unwrap_or("").to_string(),
        access: v["access"].as_str().unwrap_or("read").to_string(),
        role: v["role"].as_str().unwrap_or("").to_string(),
    }
}

pub(crate) fn store_draft(draft: &ShareDraft) {
    let v = json!({
        "note_id": draft.note_id,
        "users": draft.users.iter().map(entry_to_json).collect::<Vec<_>>(),
        "groups": draft.groups.iter().map(entry_to_json).collect::<Vec<_>>(),
        "org_read": draft.org_read,
        "last_added": draft.last_added,
    });
    let _ = state_set(&draft_key(), v.to_string().as_bytes(), StateTier::Ephemeral);
}

pub(crate) fn load_draft() -> ShareDraft {
    let raw = state_get(&draft_key()).ok().flatten();
    let parsed: Option<JsonValue> = raw.and_then(|b| serde_json::from_slice(&b).ok());
    match parsed {
        Some(v) => ShareDraft {
            note_id: v["note_id"].as_str().unwrap_or("").to_string(),
            users: v["users"]
                .as_array()
                .map(|a| a.iter().map(entry_from_json).collect())
                .unwrap_or_default(),
            groups: v["groups"]
                .as_array()
                .map(|a| a.iter().map(entry_from_json).collect())
                .unwrap_or_default(),
            org_read: v["org_read"].as_bool().unwrap_or(false),
            last_added: v["last_added"].as_str().unwrap_or("").to_string(),
        },
        None => ShareDraft::default(),
    }
}

// =============================================================================
// Pure helpers (unit-tested natively)
// =============================================================================

/// One live suggestion (user or group).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Suggestion {
    pub is_group: bool,
    pub id: String,
    pub name: String,
    pub sub: String,
    /// RBAC role of a user suggestion; empty for groups.
    pub role: String,
}

/// Directory row inputs of the suggestion filter (kept dumb so the filter is
/// testable without host fns).
pub(crate) struct DirUserRow {
    pub id: String,
    pub name: String,
    pub email: String,
    pub role: String,
}

/// Polish label for an RBAC role shown next to a person (mockup n03 role chip).
pub(crate) fn role_label_pl(role: &str) -> String {
    match role {
        "admin" => "org_admin".to_string(),
        "power_user" => "power user".to_string(),
        _ => "użytkownik".to_string(),
    }
}

/// Splits `name` into (before, match, after) around the first case-insensitive
/// occurrence of `query`, so the matched span can render bold/accented as
/// separate structural Text nodes (never markup in a string). No match →
/// everything lands in `before`.
pub(crate) fn highlight_split(name: &str, query: &str) -> (String, String, String) {
    let q = query.trim();
    if q.is_empty() {
        return (name.to_string(), String::new(), String::new());
    }
    let name_lc = name.to_lowercase();
    let q_lc = q.to_lowercase();
    match name_lc.find(&q_lc) {
        // Lowercasing can shift byte lengths (e.g. some non-Latin scripts), so
        // the lowercase indices may not be char boundaries in the original.
        // Fall back to no-highlight rather than risk a slice panic.
        Some(byte_idx) => {
            let end = byte_idx + q_lc.len();
            if name.is_char_boundary(byte_idx) && name.is_char_boundary(end) {
                (
                    name[..byte_idx].to_string(),
                    name[byte_idx..end].to_string(),
                    name[end..].to_string(),
                )
            } else {
                (name.to_string(), String::new(), String::new())
            }
        }
        None => (name.to_string(), String::new(), String::new()),
    }
}

pub(crate) struct DirGroupRow {
    pub id: String,
    pub name: String,
    pub member_count: u64,
}

/// Case-insensitive substring match over name/email/group name; the owner and
/// already-added subjects are excluded; users first, capped at SUGGESTION_ROWS.
pub(crate) fn filter_suggestions(
    users: &[DirUserRow],
    groups: &[DirGroupRow],
    query: &str,
    exclude_users: &[String],
    exclude_groups: &[String],
    owner_id: &str,
) -> Vec<Suggestion> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<Suggestion> = Vec::new();
    for u in users {
        if u.id == owner_id || exclude_users.iter().any(|x| x == &u.id) {
            continue;
        }
        if u.name.to_lowercase().contains(&q) || u.email.to_lowercase().contains(&q) {
            out.push(Suggestion {
                is_group: false,
                id: u.id.clone(),
                name: u.name.clone(),
                sub: u.email.clone(),
                role: u.role.clone(),
            });
            if out.len() >= SUGGESTION_ROWS {
                return out;
            }
        }
    }
    for g in groups {
        if exclude_groups.iter().any(|x| x == &g.id) {
            continue;
        }
        if g.name.to_lowercase().contains(&q) {
            out.push(Suggestion {
                is_group: true,
                id: g.id.clone(),
                name: g.name.clone(),
                sub: group_member_label(g.member_count),
                role: String::new(),
            });
            if out.len() >= SUGGESTION_ROWS {
                return out;
            }
        }
    }
    out
}

/// "8 osób · grupa organizacyjna"-style member line of a group row (mockup n03).
pub(crate) fn group_member_label(count: u64) -> String {
    format!(
        "{count} {} · grupa organizacyjna",
        db::plural_pl(count as usize, "osoba", "osoby", "osób")
    )
}

/// Org-share toggle label (mockup n03): names the organization when the
/// directory resolves it, otherwise a bare "Cała organizacja". Pure.
pub(crate) fn org_toggle_label(org_name: Option<&str>) -> String {
    match org_name {
        Some(name) if !name.trim().is_empty() => {
            format!("Cała organizacja {} — tylko odczyt", name.trim())
        }
        _ => "Cała organizacja — tylko odczyt".to_string(),
    }
}

/// "Grupy (N)" section header label with the live count of shared groups.
pub(crate) fn group_section_label(count: usize) -> String {
    format!("Grupy ({count})")
}

/// Footer summary: "Udostępniasz: 3 osoby · 1 grupa · cała organizacja".
pub(crate) fn summary_label(users: usize, groups: usize, org: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    if users > 0 {
        parts.push(format!(
            "{users} {}",
            db::plural_pl(users, "osoba", "osoby", "osób")
        ));
    }
    if groups > 0 {
        parts.push(format!(
            "{groups} {}",
            db::plural_pl(groups, "grupa", "grupy", "grup")
        ));
    }
    if org {
        parts.push("cała organizacja".to_string());
    }
    if parts.is_empty() {
        return "Notatka prywatna — nikt inny jej nie zobaczy.".to_string();
    }
    format!("Udostępniasz: {}", parts.join(" · "))
}

/// Maps the draft to the rows committed by share_save.
pub(crate) fn draft_to_entries(draft: &ShareDraft) -> Vec<ShareEntry> {
    let mut out: Vec<ShareEntry> = Vec::new();
    for u in &draft.users {
        out.push(ShareEntry {
            subject_type: "user".into(),
            subject_id: u.id.clone(),
            access: u.access.clone(),
        });
    }
    for g in &draft.groups {
        out.push(ShareEntry {
            subject_type: "group".into(),
            subject_id: g.id.clone(),
            access: g.access.clone(),
        });
    }
    if draft.org_read {
        out.push(ShareEntry {
            subject_type: "org".into(),
            subject_id: String::new(),
            access: "read".into(),
        });
    }
    out
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
// Editor-meta share control: "Udostępnij" button + avatar group (owner only)
// =============================================================================

/// Share pill of the editor meta row: an avatar group of current sharers
/// (initials of users, a users-icon for each group) and the button that opens
/// the modal.
pub(crate) fn share_button(note_id: &str, shares: &[ShareEntry]) -> Component {
    let mut children: Vec<Component> = Vec::new();

    let mut avatars: Vec<Component> = Vec::new();
    for (i, s) in shares.iter().enumerate() {
        let source = match s.subject_type.as_str() {
            "user" => AvatarRef::Initials {
                initials: initials(&db::user_display_name(&s.subject_id)),
            },
            _ => AvatarRef::Icon {
                icon: crate::ui::icon(IconName::Users),
            },
        };
        // Initials sources auto-color from their text (B2); the group icon
        // keeps an explicit Info tone.
        let tone = match s.subject_type.as_str() {
            "user" => None,
            _ => Some(Tone::Info),
        };
        avatars.push(
            Avatar {
                source,
                size: AvatarSize::Sm,
                shape: AvatarShape::Circle,
                status: None,
                tone,
            }
            .into_component(format!("share-av-{i}"))
            .expect("Avatar encode"),
        );
    }
    if !avatars.is_empty() {
        children.push(
            AvatarGroup {
                avatars,
                max_visible: 2,
                overlap: ui::AvatarOverlap::Tight,
                size: AvatarSize::Sm,
            }
            .into_component("share-avgroup")
            .expect("AvatarGroup encode"),
        );
    }

    let mut btn = Button {
        variant: ButtonVariant::Secondary,
        tone: Tone::Primary,
        label: lit("Udostępnij"),
        icon_leading: Some(crate::ui::icon(IconName::Share)),
        icon_trailing: None,
        size: ButtonSize::Sm,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Compact,
    }
    .into_component("btn-share-open")
    .expect("Button encode");
    btn.handlers = Some(crate::ui::backend_params(
        EventKind::Click,
        "share_open",
        vec![("note_id", CborValue::Text(note_id.to_string()))],
    ));
    children.push(btn);

    Cluster {
        gap: ui::Spacing::Sm,
        align: FlexAlign::Center,
        justify: FlexJustify::End,
        children,
        wrap: Some(false),
    }
    .into_component("share-pill")
    .expect("Cluster encode")
}

// =============================================================================
// Modal shell + body + footer fragments
// =============================================================================

/// The Modal component placed in the main-area tree while the modal is open
/// (tentavision pattern). Dismiss (×/backdrop/ESC) routes to share_close.
pub(crate) fn share_modal_component() -> Component {
    let mut modal = ui::Modal {
        title: lit("Udostępnij notatkę"),
        subtitle: None,
        body_slot: SLOT_SHARE_BODY.into(),
        footer_slot: Some(SLOT_SHARE_FOOT.into()),
        size: ModalSize::Lg,
        dismissible: true,
        prevent_scroll: true,
        closable: true,
        icon: Some(crate::ui::icon(IconName::Share)),
    }
    .into_component("share-modal")
    .expect("Modal encode");
    modal.handlers = Some(HandlerMap(vec![(
        EventKind::Dismiss,
        Handler::Backend {
            action_id: "share_close".into(),
            params: CborMap(vec![]),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    modal
}

fn section_label(id: &str, icon_name: IconName, label: &str) -> Component {
    let _ = icon_name;
    crate::ui::text_c(id, lit(label), TextStyle::Overline, Some(Tone::Primary))
}

/// One pre-rendered suggestion row: everything bound to `share.sug.{i}.*`
/// state, so typing only patches state and never re-renders the search box.
fn suggestion_row(i: usize) -> Component {
    let user_avatar = crate::ui::with_visible_bound(
        Avatar {
            source: AvatarRef::Icon {
                icon: crate::ui::icon(IconName::User),
            },
            size: AvatarSize::Sm,
            shape: AvatarShape::Circle,
            status: None,
            tone: Some(Tone::Primary),
        }
        .into_component(format!("sug-{i}-avu"))
        .expect("Avatar encode"),
        &sug_path(i, "is_user"),
    );
    let group_avatar = crate::ui::with_visible_bound(
        Avatar {
            source: AvatarRef::Icon {
                icon: crate::ui::icon(IconName::Users),
            },
            size: AvatarSize::Sm,
            shape: AvatarShape::Circle,
            status: None,
            tone: Some(Tone::Info),
        }
        .into_component(format!("sug-{i}-avg"))
        .expect("Avatar encode"),
        &sug_path(i, "is_group"),
    );

    // Name with the matched query span accent-colored (E3): three bound Text
    // pieces, so the highlight is structural — never markup inside a string.
    let name_line = Cluster {
        gap: ui::Spacing::Zero,
        align: FlexAlign::Center,
        justify: FlexJustify::Start,
        children: vec![
            crate::ui::text_c(&format!("sug-{i}-npre"), bound(&sug_path(i, "npre")), TextStyle::BodyStrong, None),
            crate::ui::text_c(&format!("sug-{i}-nmark"), bound(&sug_path(i, "nmark")), TextStyle::BodyStrong, Some(Tone::Primary)),
            crate::ui::text_c(&format!("sug-{i}-npost"), bound(&sug_path(i, "npost")), TextStyle::BodyStrong, None),
        ],
        wrap: Some(true),
    }
    .into_component(format!("sug-{i}-nameline"))
    .expect("Cluster encode");

    let name_block = ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: None,
        margin: None,
        children: vec![
            name_line,
            crate::ui::text_c(&format!("sug-{i}-sub"), bound(&sug_path(i, "sub")), TextStyle::Caption, Some(Tone::Muted)),
        ],
        style: None,
        direction: Some(FlexDirection::Column),
        gap: None,
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: None,
    }
    .into_component(format!("sug-{i}-info"))
    .expect("Box encode");

    let type_chip = Chip {
        variant: ChipVariant::Soft,
        tone: Tone::Neutral,
        label: bound(&sug_path(i, "type")),
        icon: None,
        avatar: None,
        selected: None,
        removable: false,
        dot: None,
    }
    .into_component(format!("sug-{i}-type"))
    .expect("Chip encode");

    // Row 0 is always the top match (the filter fills rows top-down), so it
    // carries the "selected card" look (accent border + tinted background) via
    // the shared Filled+accent-primary styling — mockup n03 first-suggestion.
    let highlight = i == 0;
    let mut card = Card {
        variant: if highlight { CardVariant::Filled } else { CardVariant::Outlined },
        padding: ui::Spacing::Sm,
        gap: ui::Spacing::Xs,
        radius: ui::RadiusToken::Md,
        shadow: ShadowToken::None,
        border: ui::BorderToken::Hairline,
        background: ui::BackgroundToken::Subtle,
        accent: if highlight { Some(Tone::Primary) } else { None },
        children: vec![Cluster {
            gap: ui::Spacing::Sm,
            align: FlexAlign::Center,
            justify: FlexJustify::Start,
            children: vec![user_avatar, group_avatar, name_block, type_chip],
            wrap: Some(false),
        }
        .into_component(format!("sug-{i}-row"))
        .expect("Cluster encode")],
        interactive: true,
        clickable: true,
        style: None,
    }
    .into_component(format!("sug-{i}"))
    .expect("Card encode");
    card.handlers = Some(crate::ui::backend_params(
        EventKind::Click,
        "share_pick",
        vec![("index", CborValue::Text(i.to_string()))],
    ));
    crate::ui::with_visible_bound(card, &sug_path(i, "vis"))
}

/// One committed share row (user or group). Rendered per body render — the
/// access select binds to an overlay-seeded per-row path; its Change carries
/// the subject identity as static params.
fn share_row(idx: usize, subject_type: &str, e: &DraftEntry, is_new: bool) -> Component {
    let avatar = if subject_type == "user" {
        Avatar {
            source: AvatarRef::Initials {
                initials: initials(&e.name),
            },
            size: AvatarSize::Sm,
            shape: AvatarShape::Circle,
            status: None,
            // No explicit tone → the renderer derives a deterministic color
            // from the initials (B2), so people read as distinct chips.
            tone: None,
        }
    } else {
        Avatar {
            source: AvatarRef::Icon {
                icon: crate::ui::icon(IconName::Users),
            },
            size: AvatarSize::Sm,
            shape: AvatarShape::Circle,
            status: None,
            tone: Some(Tone::Info),
        }
    }
    .into_component(format!("shr-{subject_type}-{idx}-av"))
    .expect("Avatar encode");

    let mut name_row_children = vec![crate::ui::text_c(
        &format!("shr-{subject_type}-{idx}-name"),
        lit(&e.name),
        TextStyle::BodyStrong,
        None,
    )];
    // RBAC role chip next to a person's name (mockup n03). org_admin stands out.
    if subject_type == "user" && !e.role.is_empty() {
        name_row_children.push(
            Chip {
                variant: ChipVariant::Soft,
                tone: if e.role == "admin" { Tone::Warning } else { Tone::Neutral },
                label: lit(role_label_pl(&e.role)),
                icon: None,
                avatar: None,
                selected: None,
                removable: false,
                dot: None,
            }
            .into_component(format!("shr-{subject_type}-{idx}-role"))
            .expect("Chip encode"),
        );
    }
    if is_new {
        name_row_children.push(
            Chip {
                variant: ChipVariant::Soft,
                tone: Tone::Primary,
                label: lit("nowe"),
                icon: Some(crate::ui::icon(IconName::Sparkle)),
                avatar: None,
                selected: None,
                removable: false,
                dot: None,
            }
            .into_component(format!("shr-{subject_type}-{idx}-new"))
            .expect("Chip encode"),
        );
    }
    let name_row = Cluster {
        gap: ui::Spacing::Xs,
        align: FlexAlign::Center,
        justify: FlexJustify::Start,
        children: name_row_children,
        wrap: Some(true),
    }
    .into_component(format!("shr-{subject_type}-{idx}-namerow"))
    .expect("Cluster encode");

    let info = ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: None,
        margin: None,
        children: vec![
            name_row,
            crate::ui::text_c(
                &format!("shr-{subject_type}-{idx}-sub"),
                lit(&e.sub),
                TextStyle::Caption,
                Some(Tone::Muted),
            ),
        ],
        style: Some(ui::BoxStyle {
            min_width: Some(ui::DimensionToken::Px { value: 0 }),
            ..Default::default()
        }),
        direction: Some(FlexDirection::Column),
        gap: None,
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: None,
    }
    .into_component(format!("shr-{subject_type}-{idx}-info"))
    .expect("Box encode");

    let mut select = crate::ui::with_a11y_label(
        Select {
            bind_path: state_path(&format!("share.acc.{subject_type}.{idx}")),
            options: vec![
                SelectOption {
                    value: SelectValue::Text("read".into()),
                    label: lit("Odczyt"),
                    icon: Some(crate::ui::icon(IconName::Eye)),
                    disabled: false,
                    group_id: None,
                    description: None,
                },
                SelectOption {
                    value: SelectValue::Text("write".into()),
                    label: lit("Edycja"),
                    icon: Some(crate::ui::icon(IconName::Edit)),
                    disabled: false,
                    group_id: None,
                    description: None,
                },
            ],
            placeholder: None,
            label: None,
            searchable: false,
            clearable: false,
            virtualize: false,
            disabled: None,
            size: InputSize::Sm,
            groups: None,
        }
        .into_component(format!("shr-{subject_type}-{idx}-acc"))
        .expect("Select encode"),
        "Poziom dostępu",
    );
    select.handlers = Some(crate::ui::backend_params(
        EventKind::Change,
        "share_access",
        vec![
            ("stype", CborValue::Text(subject_type.to_string())),
            ("sid", CborValue::Text(e.id.clone())),
        ],
    ));
    let select_wrap = ui::Box {
        width: Some(ui::DimensionToken::Px { value: 120 }),
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
    .into_component(format!("shr-{subject_type}-{idx}-accwrap"))
    .expect("Box encode");

    let mut remove = IconButton {
        icon: crate::ui::icon(IconName::X),
        variant: ButtonVariant::Ghost,
        tone: Tone::Critical,
        size: ButtonSize::Sm,
        aria_label: "Usuń dostęp".to_string(),
        disabled: None,
        loading: None,
    }
    .into_component(format!("shr-{subject_type}-{idx}-rm"))
    .expect("IconButton encode");
    remove.handlers = Some(crate::ui::backend_params(
        EventKind::Click,
        "share_remove",
        vec![
            ("stype", CborValue::Text(subject_type.to_string())),
            ("sid", CborValue::Text(e.id.clone())),
        ],
    ));

    Card {
        variant: CardVariant::Outlined,
        padding: ui::Spacing::Sm,
        gap: ui::Spacing::Xs,
        radius: ui::RadiusToken::Md,
        shadow: ShadowToken::None,
        border: if is_new {
            ui::BorderToken::Accent {
                tone: Tone::Primary,
            }
        } else {
            ui::BorderToken::Hairline
        },
        background: ui::BackgroundToken::Subtle,
        accent: None,
        children: vec![Cluster {
            gap: ui::Spacing::Sm,
            align: FlexAlign::Center,
            justify: FlexJustify::Start,
            children: vec![avatar, info, select_wrap, remove],
            wrap: Some(false),
        }
        .into_component(format!("shr-{subject_type}-{idx}-row"))
        .expect("Cluster encode")],
        interactive: false,
        clickable: false,
        style: None,
    }
    .into_component(format!("shr-{subject_type}-{idx}"))
    .expect("Card encode")
}

/// Body fragment + its state overlay. The overlay reseeds the search box,
/// suggestion rows (hidden) and per-row access selects on every re-render.
fn body_fragment(draft: &ShareDraft) -> (Component, Vec<StateEntry>) {
    let mut search = crate::ui::with_a11y_label(
        SearchBox {
            bind_path: state_path(SP_SEARCH),
            placeholder: lit("Dodaj osobę lub grupę…"),
            debounce_ms: 250,
            variant: SearchVariant::Prominent,
            shortcut_hint: None,
            on_search_action_id: None,
        }
        .into_component("share-search")
        .expect("SearchBox encode"),
        "Dodaj osobę lub grupę",
    );
    // Input (per keystroke, debounced by the component) drives suggestions —
    // the response is a pure StatePatch, so focus survives.
    search.handlers = Some(backend(EventKind::Input, "share_suggest"));

    let mut children: Vec<Component> = vec![search];
    for i in 0..SUGGESTION_ROWS {
        children.push(suggestion_row(i));
    }

    children.push(section_label("share-sec-users", IconName::User, "Osoby"));
    if draft.users.is_empty() {
        children.push(crate::ui::text_c(
            "share-users-empty",
            lit("Nikt jeszcze nie ma indywidualnego dostępu."),
            TextStyle::Caption,
            Some(Tone::Muted),
        ));
    }
    for (i, u) in draft.users.iter().enumerate() {
        let is_new = draft.last_added == format!("user:{}", u.id);
        children.push(share_row(i, "user", u, is_new));
    }

    children.push(section_label(
        "share-sec-groups",
        IconName::Users,
        &group_section_label(draft.groups.len()),
    ));
    if draft.groups.is_empty() {
        children.push(crate::ui::text_c(
            "share-groups-empty",
            lit("Brak grup z dostępem."),
            TextStyle::Caption,
            Some(Tone::Muted),
        ));
    }
    for (i, g) in draft.groups.iter().enumerate() {
        let is_new = draft.last_added == format!("group:{}", g.id);
        children.push(share_row(i, "group", g, is_new));
    }

    children.push(section_label("share-sec-org", IconName::Users, "Organizacja"));
    let org_name = directory_org().ok().map(|o| o.name);
    let mut org_toggle = Toggle {
        bind_path: state_path(SP_ORG),
        label: Some(lit(org_toggle_label(org_name.as_deref()))),
        hint: None,
        size: ToggleSize::Md,
        tone: Tone::Primary,
        disabled: None,
        label_position: TogglePosition::Trailing,
    }
    .into_component("share-org-toggle")
    .expect("Toggle encode");
    org_toggle.handlers = Some(backend(EventKind::Change, "share_org"));
    let org_desc = crate::ui::text_c(
        "share-org-desc",
        lit(
            "Każdy zalogowany użytkownik organizacji zobaczy tę notatkę w wyszukiwaniu \
             i w grafie. Edycja pozostaje przy osobach i grupach z listy powyżej.",
        ),
        TextStyle::Caption,
        Some(Tone::Muted),
    );
    children.push(
        ui::Box {
            width: None,
            grow: None,
            align_self: None,
            padding: Some(ui::Spacing::Sm),
            margin: None,
            children: vec![org_toggle, org_desc],
            style: Some(ui::BoxStyle {
                border: Some(ui::BorderEdges::all(ui::BorderSide::new(
                    1,
                    ui::BorderColor::Default,
                ))),
                radius: Some(ui::CornerValues::all(ui::RadiusValue::Token {
                    value: ui::RadiusToken::Md,
                })),
                background: Some(ui::BackgroundToken::Subtle),
                ..Default::default()
            }),
            direction: Some(FlexDirection::Column),
            gap: Some(ui::Spacing::Xs),
            align: Some(FlexAlign::Stretch),
            justify: None,
            responsive: None,
        }
        .into_component("share-org-row")
        .expect("Box encode"),
    );

    children.push(
        Callout {
            tone: Tone::Primary,
            icon: Some(crate::ui::icon(IconName::Branch)),
            title: None,
            content: vec![crate::ui::text_c(
                "share-callout-text",
                lit(
                    "Po udostępnieniu notatka wejdzie do grafu powiązań odbiorców — encje \
                     i podobieństwa policzą się automatycznie. Cofnięcie dostępu usuwa też \
                     jej krawędzie z ich grafu.",
                ),
                TextStyle::Caption,
                None,
            )],
        }
        .into_component("share-callout")
        .expect("Callout encode"),
    );

    let body = ui::Box {
        width: None,
        grow: None,
        align_self: None,
        padding: None,
        margin: None,
        children,
        style: None,
        direction: Some(FlexDirection::Column),
        gap: Some(ui::Spacing::Md),
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: None,
    }
    .into_component("share-body-root")
    .expect("Box encode");

    let mut overlay = vec![
        StateEntry {
            path: state_path(SP_SEARCH),
            value: CborValue::Text(String::new()),
        },
        StateEntry {
            path: state_path(SP_ORG),
            value: CborValue::Bool(draft.org_read),
        },
    ];
    overlay.extend(suggestion_reset_entries());
    for (i, u) in draft.users.iter().enumerate() {
        overlay.push(StateEntry {
            path: state_path(&format!("share.acc.user.{i}")),
            value: CborValue::Text(u.access.clone()),
        });
    }
    for (i, g) in draft.groups.iter().enumerate() {
        overlay.push(StateEntry {
            path: state_path(&format!("share.acc.group.{i}")),
            value: CborValue::Text(g.access.clone()),
        });
    }
    (body, overlay)
}

fn footer_fragment() -> Component {
    let summary = Cluster {
        gap: ui::Spacing::Xs,
        align: FlexAlign::Center,
        justify: FlexJustify::Start,
        children: vec![crate::ui::text_c(
            "share-summary",
            bound(SP_SUMMARY),
            TextStyle::Caption,
            Some(Tone::Muted),
        )],
        wrap: Some(false),
    }
    .into_component("share-foot-summary")
    .expect("Cluster encode");
    let summary_grow = ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: None,
        margin: None,
        children: vec![summary],
        style: None,
        direction: Some(FlexDirection::Column),
        gap: None,
        align: Some(FlexAlign::Start),
        justify: Some(FlexJustify::Center),
        responsive: None,
    }
    .into_component("share-foot-summary-grow")
    .expect("Box encode");

    let mut cancel = Button {
        variant: ButtonVariant::Ghost,
        tone: Tone::Neutral,
        label: lit("Anuluj"),
        icon_leading: None,
        icon_trailing: None,
        size: ButtonSize::Md,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Default,
    }
    .into_component("share-cancel")
    .expect("Button encode");
    cancel.handlers = Some(backend(EventKind::Click, "share_close"));

    let mut save = Button {
        variant: ButtonVariant::Primary,
        tone: Tone::Primary,
        label: lit("Zapisz dostępy"),
        icon_leading: Some(crate::ui::icon(IconName::Check)),
        icon_trailing: None,
        size: ButtonSize::Md,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Default,
    }
    .into_component("share-save")
    .expect("Button encode");
    save.handlers = Some(backend(EventKind::Click, "share_save"));

    Flex {
        direction: FlexDirection::Row,
        gap: ui::Spacing::Sm,
        justify: FlexJustify::End,
        align: FlexAlign::Center,
        wrap: FlexWrap::Wrap,
        children: vec![summary_grow, cancel, save],
        padding: None,
        background: None,
        radius: None,
        style: None,
        responsive: None,
    }
    .into_component("share-foot-root")
    .expect("Flex encode")
}

/// State entries hiding all suggestion rows and clearing their texts.
fn suggestion_reset_entries() -> Vec<StateEntry> {
    let mut out: Vec<StateEntry> = Vec::with_capacity(SUGGESTION_ROWS * 5);
    for i in 0..SUGGESTION_ROWS {
        for (field, value) in [
            ("vis", CborValue::Bool(false)),
            ("is_user", CborValue::Bool(false)),
            ("is_group", CborValue::Bool(false)),
        ] {
            out.push(StateEntry {
                path: state_path(&sug_path(i, field)),
                value,
            });
        }
        for field in ["npre", "nmark", "npost", "sub", "type"] {
            out.push(StateEntry {
                path: state_path(&sug_path(i, field)),
                value: CborValue::Text(String::new()),
            });
        }
    }
    out
}

/// Initial-state entries of the modal paths, declared with the panel shell so
/// bound components always have a value to resolve.
pub(crate) fn initial_share_state() -> Vec<StateEntry> {
    let mut out = vec![
        StateEntry {
            path: state_path(SP_SEARCH),
            value: CborValue::Text(String::new()),
        },
        StateEntry {
            path: state_path(SP_SUMMARY),
            value: CborValue::Text(String::new()),
        },
        StateEntry {
            path: state_path(SP_ORG),
            value: CborValue::Bool(false),
        },
    ];
    out.extend(suggestion_reset_entries());
    out
}

/// Re-renders body + footer slots and patches the summary for the current
/// draft.
fn push_modal_content(draft: &ShareDraft) {
    let (body, overlay) = body_fragment(draft);
    crate::ui::send_slot(SLOT_SHARE_BODY, body, Some(overlay));
    crate::ui::send_slot(SLOT_SHARE_FOOT, footer_fragment(), None);
    push_summary(draft);
}

fn push_summary(draft: &ShareDraft) {
    crate::ui::send_state_patch(vec![PatchOp {
        path: state_path(SP_SUMMARY),
        op: PatchOpKind::Set {
            value: CborValue::Text(summary_label(
                draft.users.len(),
                draft.groups.len(),
                draft.org_read,
            )),
        },
    }]);
}

// =============================================================================
// Actions
// =============================================================================

/// Opens the modal for a note the acting user OWNS: seeds the draft from the
/// persisted shares + directory display data, re-renders the main slot (which
/// now contains the Modal) and fills its body/footer slots.
pub(crate) fn action_share_open(ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let note_id = match params.get("note_id").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => return json!({"ok": false, "error": "Brak note_id"}),
    };
    let note = match db::get_note(ctx, &note_id) {
        Ok(Some(n)) => n,
        Ok(None) => return json!({"ok": false, "error": "Notatka nie istnieje."}),
        Err(e) => return json!({"ok": false, "error": e}),
    };
    if !note.is_owner {
        return json!({"ok": false, "error": "Tylko właściciel może udostępniać notatkę."});
    }

    let users = directory_users().unwrap_or_default();
    let groups = directory_groups().unwrap_or_default();
    let shares = match db::note_shares_list(ctx, &note_id) {
        Ok(s) => s,
        Err(e) => return json!({"ok": false, "error": e}),
    };

    let mut draft = ShareDraft {
        note_id: note_id.clone(),
        ..Default::default()
    };
    for s in &shares {
        match s.subject_type.as_str() {
            "user" => {
                let (name, sub, role) = users
                    .iter()
                    .find(|u| u.id == s.subject_id)
                    .map(|u| {
                        let name = if u.display_name.is_empty() {
                            u.username.clone()
                        } else {
                            u.display_name.clone()
                        };
                        (name, u.email.clone().unwrap_or_default(), u.role.clone())
                    })
                    .unwrap_or_else(|| (s.subject_id.clone(), String::new(), String::new()));
                draft.users.push(DraftEntry {
                    id: s.subject_id.clone(),
                    name,
                    sub,
                    access: s.access.clone(),
                    role,
                });
            }
            "group" => {
                let (name, sub) = groups
                    .iter()
                    .find(|g| g.id == s.subject_id)
                    .map(|g| (g.name.clone(), group_member_label(g.member_count)))
                    .unwrap_or_else(|| (s.subject_id.clone(), String::new()));
                draft.groups.push(DraftEntry {
                    id: s.subject_id.clone(),
                    name,
                    sub,
                    access: s.access.clone(),
                    role: String::new(),
                });
            }
            "org" => draft.org_read = true,
            _ => {}
        }
    }
    store_draft(&draft);

    let mut sess = crate::ui::load_session();
    sess.share_open = true;
    crate::ui::store_session(&sess);

    crate::ui::send_main(ctx, Some(&note));
    push_modal_content(&draft);
    json!({"ok": true})
}

/// Closes the modal without committing the draft.
pub(crate) fn action_share_close(ctx: &UserCtx) -> JsonValue {
    let mut sess = crate::ui::load_session();
    sess.share_open = false;
    crate::ui::store_session(&sess);
    crate::ui::refresh_active_main(ctx);
    json!({"ok": true})
}

/// Live directory suggestions for the search box — pure StatePatch response.
pub(crate) fn action_share_suggest(ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let query = params.get("value").and_then(|v| v.as_str()).unwrap_or("");
    let draft = load_draft();
    let users: Vec<DirUserRow> = directory_users()
        .unwrap_or_default()
        .into_iter()
        .filter(|u| u.is_active)
        .map(|u| DirUserRow {
            id: u.id,
            name: if u.display_name.is_empty() {
                u.username
            } else {
                u.display_name
            },
            email: u.email.unwrap_or_default(),
            role: u.role,
        })
        .collect();
    let groups: Vec<DirGroupRow> = directory_groups()
        .unwrap_or_default()
        .into_iter()
        .map(|g| DirGroupRow {
            id: g.id,
            name: g.name,
            member_count: g.member_count,
        })
        .collect();
    let exclude_users: Vec<String> = draft.users.iter().map(|u| u.id.clone()).collect();
    let exclude_groups: Vec<String> = draft.groups.iter().map(|g| g.id.clone()).collect();
    let suggestions = filter_suggestions(
        &users,
        &groups,
        query,
        &exclude_users,
        &exclude_groups,
        &ctx.user_id,
    );

    // Persist for share_pick (index → subject) in the session KV.
    let stored: Vec<JsonValue> = suggestions
        .iter()
        .map(|s| json!({"is_group": s.is_group, "id": s.id, "name": s.name, "sub": s.sub, "role": s.role}))
        .collect();
    let _ = state_set(
        &suggestions_key(),
        JsonValue::Array(stored).to_string().as_bytes(),
        StateTier::Ephemeral,
    );

    let mut ops: Vec<PatchOp> = Vec::new();
    let set = |path: String, value: CborValue| PatchOp {
        path: state_path(&path),
        op: PatchOpKind::Set { value },
    };
    for i in 0..SUGGESTION_ROWS {
        match suggestions.get(i) {
            Some(s) => {
                ops.push(set(sug_path(i, "vis"), CborValue::Bool(true)));
                ops.push(set(sug_path(i, "is_user"), CborValue::Bool(!s.is_group)));
                ops.push(set(sug_path(i, "is_group"), CborValue::Bool(s.is_group)));
                let (pre, mark, post) = highlight_split(&s.name, query);
                ops.push(set(sug_path(i, "npre"), CborValue::Text(pre)));
                ops.push(set(sug_path(i, "nmark"), CborValue::Text(mark)));
                ops.push(set(sug_path(i, "npost"), CborValue::Text(post)));
                ops.push(set(sug_path(i, "sub"), CborValue::Text(s.sub.clone())));
                ops.push(set(
                    sug_path(i, "type"),
                    CborValue::Text(if s.is_group { "grupa" } else { "użytkownik" }.to_string()),
                ));
            }
            None => {
                ops.push(set(sug_path(i, "vis"), CborValue::Bool(false)));
            }
        }
    }
    crate::ui::send_state_patch(ops);
    json!({"ok": true, "count": suggestions.len()})
}

/// Adds the picked suggestion to the draft (default access: read).
pub(crate) fn action_share_pick(_ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let index: usize = params
        .get("index")
        .and_then(|v| v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_u64().map(|n| n as usize)))
        .unwrap_or(usize::MAX);
    let stored: Vec<JsonValue> = state_get(&suggestions_key())
        .ok()
        .flatten()
        .and_then(|b| serde_json::from_slice::<JsonValue>(&b).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    let Some(picked) = stored.get(index) else {
        return json!({"ok": false, "error": "Podpowiedź wygasła — wyszukaj ponownie."});
    };
    let is_group = picked["is_group"].as_bool().unwrap_or(false);
    let entry = DraftEntry {
        id: picked["id"].as_str().unwrap_or("").to_string(),
        name: picked["name"].as_str().unwrap_or("").to_string(),
        sub: picked["sub"].as_str().unwrap_or("").to_string(),
        access: "read".to_string(),
        role: picked["role"].as_str().unwrap_or("").to_string(),
    };
    if entry.id.is_empty() {
        return json!({"ok": false, "error": "Nieprawidłowa podpowiedź."});
    }

    let mut draft = load_draft();
    let already = if is_group {
        draft.groups.iter().any(|g| g.id == entry.id)
    } else {
        draft.users.iter().any(|u| u.id == entry.id)
    };
    if !already {
        draft.last_added = format!("{}:{}", if is_group { "group" } else { "user" }, entry.id);
        if is_group {
            draft.groups.push(entry);
        } else {
            draft.users.push(entry);
        }
        store_draft(&draft);
    }
    push_modal_content(&draft);
    json!({"ok": true})
}

/// Removes a subject from the draft.
pub(crate) fn action_share_remove(_ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let stype = params.get("stype").and_then(|v| v.as_str()).unwrap_or("");
    let sid = params.get("sid").and_then(|v| v.as_str()).unwrap_or("");
    let mut draft = load_draft();
    match stype {
        "user" => draft.users.retain(|u| u.id != sid),
        "group" => draft.groups.retain(|g| g.id != sid),
        _ => return json!({"ok": false, "error": "Nieznany typ podmiotu."}),
    }
    draft.last_added.clear();
    store_draft(&draft);
    push_modal_content(&draft);
    json!({"ok": true})
}

/// Changes the access level of one draft entry (no re-render needed — the
/// select already shows the picked value).
pub(crate) fn action_share_access(_ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let stype = params.get("stype").and_then(|v| v.as_str()).unwrap_or("");
    let sid = params.get("sid").and_then(|v| v.as_str()).unwrap_or("");
    let access = params.get("value").and_then(|v| v.as_str()).unwrap_or("read");
    if !matches!(access, "read" | "write") {
        return json!({"ok": false, "error": "Nieznany poziom dostępu."});
    }
    let mut draft = load_draft();
    let entry = match stype {
        "user" => draft.users.iter_mut().find(|u| u.id == sid),
        "group" => draft.groups.iter_mut().find(|g| g.id == sid),
        _ => None,
    };
    match entry {
        Some(e) => e.access = access.to_string(),
        None => return json!({"ok": false, "error": "Wpis nie istnieje w szkicu."}),
    }
    store_draft(&draft);
    json!({"ok": true})
}

/// Toggles the org-wide read share in the draft.
pub(crate) fn action_share_org(_ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let enabled = params.get("value").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut draft = load_draft();
    draft.org_read = enabled;
    store_draft(&draft);
    push_summary(&draft);
    json!({"ok": true})
}

/// Commits the draft transactionally and closes the modal. Ownership and
/// directory validation happen inside db::replace_all_shares.
pub(crate) fn action_share_save(ctx: &UserCtx) -> JsonValue {
    let draft = load_draft();
    if draft.note_id.is_empty() {
        return json!({"ok": false, "error": "Szkic udostępnień wygasł."});
    }
    let entries = draft_to_entries(&draft);
    if let Err(e) = db::replace_all_shares(ctx, &draft.note_id, &entries) {
        return json!({"ok": false, "error": e});
    }
    let mut sess = crate::ui::load_session();
    sess.share_open = false;
    crate::ui::store_session(&sess);
    crate::ui::refresh_active_main(ctx);
    crate::ui::send_list(ctx);
    json!({"ok": true})
}

// =============================================================================
// Tests — pure helpers only (no host fns on the native target)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn users() -> Vec<DirUserRow> {
        vec![
            DirUserRow {
                id: "u1".into(),
                name: "Piotr Jarocki".into(),
                email: "piotr@ex.pl".into(),
                role: "admin".into(),
            },
            DirUserRow {
                id: "u2".into(),
                name: "Marta Wiśniewska".into(),
                email: "marta.wisniewska@ex.pl".into(),
                role: "user".into(),
            },
            DirUserRow {
                id: "u3".into(),
                name: "Marek Kowal".into(),
                email: "marek.kowal@ex.pl".into(),
                role: "power_user".into(),
            },
        ]
    }

    fn groups() -> Vec<DirGroupRow> {
        vec![
            DirGroupRow {
                id: "g1".into(),
                name: "Marketing".into(),
                member_count: 12,
            },
            DirGroupRow {
                id: "g2".into(),
                name: "Zespół R&D".into(),
                member_count: 8,
            },
        ]
    }

    #[test]
    fn suggestions_match_name_email_and_group_excluding_owner_and_added() {
        let s = filter_suggestions(&users(), &groups(), "mar", &[], &[], "u1");
        let names: Vec<&str> = s.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["Marta Wiśniewska", "Marek Kowal", "Marketing"]);
        assert!(!s[0].is_group);
        assert!(s[2].is_group);
        // Owner never suggested even when matching.
        let own = filter_suggestions(&users(), &groups(), "piotr", &[], &[], "u1");
        assert!(own.is_empty());
        // Already-added subjects drop out.
        let after_add =
            filter_suggestions(&users(), &groups(), "mar", &["u2".into()], &["g1".into()], "u1");
        let names2: Vec<&str> = after_add.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names2, vec!["Marek Kowal"]);
        // Email matches too.
        let by_mail = filter_suggestions(&users(), &groups(), "kowal@", &[], &[], "u1");
        assert_eq!(by_mail.len(), 1);
        assert_eq!(by_mail[0].id, "u3");
        // Empty query yields nothing.
        assert!(filter_suggestions(&users(), &groups(), "  ", &[], &[], "u1").is_empty());
    }

    #[test]
    fn summary_label_uses_polish_plurals() {
        assert_eq!(summary_label(3, 1, false), "Udostępniasz: 3 osoby · 1 grupa");
        assert_eq!(summary_label(1, 0, false), "Udostępniasz: 1 osoba");
        assert_eq!(summary_label(5, 2, true), "Udostępniasz: 5 osób · 2 grupy · cała organizacja");
        assert_eq!(summary_label(0, 0, true), "Udostępniasz: cała organizacja");
        assert!(summary_label(0, 0, false).contains("prywatna"));
    }

    #[test]
    fn draft_maps_to_share_entries_with_org_read_only() {
        let draft = ShareDraft {
            note_id: "n1".into(),
            users: vec![DraftEntry {
                id: "u2".into(),
                name: "Marta".into(),
                sub: String::new(),
                access: "write".into(),
                role: "user".into(),
            }],
            groups: vec![DraftEntry {
                id: "g2".into(),
                name: "R&D".into(),
                sub: String::new(),
                access: "read".into(),
                role: String::new(),
            }],
            org_read: true,
            last_added: String::new(),
        };
        let entries = draft_to_entries(&draft);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].subject_type, "user");
        assert_eq!(entries[0].access, "write");
        assert_eq!(entries[1].subject_type, "group");
        let org = &entries[2];
        assert_eq!(org.subject_type, "org");
        assert_eq!(org.access, "read");
        assert!(org.subject_id.is_empty());
        // Owner-only + directory validation runs in db::replace_all_shares
        // (validate_share_entries) — covered by db.rs tests.
    }

    #[test]
    fn group_member_label_pluralizes_with_org_group_suffix() {
        assert_eq!(group_member_label(1), "1 osoba · grupa organizacyjna");
        assert_eq!(group_member_label(3), "3 osoby · grupa organizacyjna");
        assert_eq!(group_member_label(12), "12 osób · grupa organizacyjna");
    }

    #[test]
    fn org_toggle_label_names_the_organization_when_known() {
        assert_eq!(
            org_toggle_label(Some("Euvic")),
            "Cała organizacja Euvic — tylko odczyt"
        );
        assert_eq!(
            org_toggle_label(None),
            "Cała organizacja — tylko odczyt"
        );
        assert_eq!(
            org_toggle_label(Some("   ")),
            "Cała organizacja — tylko odczyt"
        );
    }

    #[test]
    fn group_section_label_carries_the_count() {
        assert_eq!(group_section_label(0), "Grupy (0)");
        assert_eq!(group_section_label(3), "Grupy (3)");
    }

    #[test]
    fn role_labels_map_to_polish() {
        assert_eq!(role_label_pl("admin"), "org_admin");
        assert_eq!(role_label_pl("power_user"), "power user");
        assert_eq!(role_label_pl("user"), "użytkownik");
        // Unknown roles degrade to the plain-user label.
        assert_eq!(role_label_pl("whatever"), "użytkownik");
    }

    #[test]
    fn suggestions_carry_user_role_and_empty_for_groups() {
        let s = filter_suggestions(&users(), &groups(), "mar", &[], &[], "u1");
        // "Marta Wiśniewska" (user), "Marek Kowal" (power_user), "Marketing" (group).
        assert_eq!(s[0].role, "user");
        assert_eq!(s[1].role, "power_user");
        assert!(s[2].is_group);
        assert!(s[2].role.is_empty());
    }

    #[test]
    fn highlight_split_matches_prefix_case_insensitively() {
        assert_eq!(
            highlight_split("Marta Wiśniewska", "mar"),
            ("".into(), "Mar".into(), "ta Wiśniewska".into())
        );
        // Match in the middle keeps the surrounding text.
        assert_eq!(
            highlight_split("Zespół R&D", "R&D"),
            ("Zespół ".into(), "R&D".into(), "".into())
        );
        // No match → everything in the prefix, nothing highlighted.
        assert_eq!(
            highlight_split("Anna", "xyz"),
            ("Anna".into(), "".into(), "".into())
        );
        // Empty query → no highlight.
        assert_eq!(
            highlight_split("Anna", "  "),
            ("Anna".into(), "".into(), "".into())
        );
    }
}
