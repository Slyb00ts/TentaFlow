// =============================================================================
// File: addons/notes/src/ui_search.rs
// Purpose: "Szukaj" view (mockup n05). A static hero (prominent search box +
//          scope chips) lives in the panel shell; every search re-renders the
//          results slot: streamed LLM answer card with source citations,
//          ranked result cards (snippet with structural term highlight, scope
//          badge, score bar, method badge, graph-path breadcrumb) and a right
//          rail (entities in results, graph narrowing). The answer streams
//          through llm_generate_stream via StatePatch batches into a bound
//          Text; a per-session generation counter cancels a stale stream when
//          a new query or a view switch supersedes it.
// =============================================================================

use serde_json::{json, Value as JsonValue};

use tentaflow_addon_sdk::ui_v1::{self as ui, backend, bound, lit, state_path};
use tentaflow_addon_sdk::ui_v1::{
    Badge, BadgeVariant, Card, CardVariant, Chip,
    ChipVariant, Cluster, Component, DimensionToken, EmptyState, EmptyStateVariant, EventKind,
    FilterChipDef, FilterChips, FilterChipsMode, Flex, FlexAlign, FlexDirection, FlexJustify,
    FlexWrap, IconName, PatchOp, PatchOpKind, SearchBox, SearchVariant, ShadowToken, StateEntry,
    Text, TextStyle, Tone, Value as CborValue, ValueFormat,
};
use tentaflow_addon_sdk::{generate_stream_start, log, state_get, state_set, StateTier};

use crate::analysis;
use crate::db::{self, UserCtx};
use crate::search::{self, Method, SearchHit, SearchOutput};
use crate::ui::panel_epoch;

pub const SLOT_RESULTS: &str = "search-results";

// State paths of the search view.
pub const SP_QUERY: &str = "search.query";
pub const SP_SCOPE: &str = "search.scope";
const SP_ANSWER: &str = "search.answer";
const SP_STREAMING: &str = "search.streaming";

// Streaming bounds (translator pattern): per-batch wait, wall-clock deadline
// and a consecutive-empty-batch cap so a stalled backend cannot pin the
// action forever.
const STREAM_BATCH_TIMEOUT_MS: u64 = 5_000;
const STREAM_DEADLINE_MS: u128 = 90_000;
const STREAM_MAX_EMPTY_BATCHES: u32 = 12;

fn generation_key() -> String {
    format!("sgen:{}:{}", crate::ui::session_user(), panel_epoch())
}

fn load_generation() -> u64 {
    state_get(&generation_key())
        .ok()
        .flatten()
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn store_generation(value: u64) {
    let _ = state_set(
        &generation_key(),
        value.to_string().as_bytes(),
        StateTier::Ephemeral,
    );
}

/// Supersedes any in-flight answer stream (new query / view switch). The
/// streaming loop reads the counter per batch and aborts on mismatch.
pub(crate) fn bump_generation() -> u64 {
    let next = load_generation() + 1;
    store_generation(next);
    next
}

// =============================================================================
// Shell subtree — hero (search box + scope chips), never re-rendered
// =============================================================================

pub(crate) fn initial_search_state() -> Vec<StateEntry> {
    vec![
        StateEntry {
            path: state_path(SP_QUERY),
            value: CborValue::Text(String::new()),
        },
        StateEntry {
            path: state_path(SP_SCOPE),
            value: CborValue::Array(vec![CborValue::Text("all".into())]),
        },
        StateEntry {
            path: state_path(SP_ANSWER),
            value: CborValue::Text(String::new()),
        },
        StateEntry {
            path: state_path(SP_STREAMING),
            value: CborValue::Bool(false),
        },
    ]
}

/// The hero: prominent search box (Enter/blur commits → run_search) and the
/// scope filter chips underneath (mockup search-hero).
pub(crate) fn search_hero_component() -> Component {
    let mut big_search = crate::ui::with_a11y_label(
        SearchBox {
            bind_path: state_path(SP_QUERY),
            placeholder: lit("Zapytaj o cokolwiek ze swoich notatek…"),
            debounce_ms: 300,
            variant: SearchVariant::Prominent,
            shortcut_hint: Some("⏎".to_string()),
            on_search_action_id: None,
        }
        .into_component("search-hero-box")
        .expect("SearchBox encode"),
        "Szukaj w notatkach",
    );
    big_search.handlers = Some(backend(EventKind::Change, "run_search"));

    let chip = |id: &str, label: &str, icon_name: IconName| FilterChipDef {
        id: id.into(),
        label: lit(label),
        icon: Some(crate::ui::icon(icon_name)),
        badge: None,
        count_path: None,
    };
    let mut chips = FilterChips {
        chips: vec![
            chip("all", "Wszystkie zakresy", IconName::Filter),
            chip("mine", "Moje", IconName::User),
            chip("group", "Grupa", IconName::Users),
            chip("org", "Organizacja", IconName::Users),
        ],
        selected_ids: state_path(SP_SCOPE),
        mode: FilterChipsMode::Single,
        clearable: false,
    }
    .into_component("search-scope-chips")
    .expect("FilterChips encode");
    chips.handlers = Some(backend(EventKind::Change, "search_scope"));

    ui::Box {
        width: None,
        grow: None,
        align_self: None,
        padding: None,
        margin: None,
        children: vec![big_search, chips],
        style: None,
        direction: Some(FlexDirection::Column),
        gap: Some(ui::Spacing::Sm),
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: None,
    }
    .into_component("search-hero")
    .expect("Box encode")
}

// =============================================================================
// Results slot fragment
// =============================================================================

fn method_badge(id: &str, method: &Method) -> Component {
    let (tone, icon_name, label) = match method {
        Method::Vector => (Tone::Primary, IconName::Sparkle, "wektorowo".to_string()),
        Method::Graph { hops, .. } => (
            Tone::Info,
            IconName::Branch,
            format!("graf · {hops} {}", if *hops == 1 { "skok" } else { "skoki" }),
        ),
        Method::Text => (Tone::Neutral, IconName::FileText, "tekstowo".to_string()),
    };
    Badge {
        variant: BadgeVariant::Soft,
        tone,
        label: lit(label),
        icon: Some(crate::ui::icon(icon_name)),
        count: None,
        max: 0,
        pulse: false,
    }
    .into_component(id)
    .expect("Badge encode")
}

/// Word-level snippet with structural highlight: matched words render as
/// BodyStrong — the content string is never treated as markup.
fn snippet_component(id: &str, snippet: &[(String, bool)]) -> Component {
    let words: Vec<Component> = snippet
        .iter()
        .enumerate()
        .map(|(i, (word, is_match))| {
            Text {
                content: lit(word),
                style: if *is_match {
                    TextStyle::CaptionStrong
                } else {
                    TextStyle::Caption
                },
                tone: if *is_match { None } else { Some(Tone::Muted) },
                align: None,
                wrap: None,
                max_lines: None,
                format: None,
                streaming: None,
            }
            .into_component(format!("{id}-w{i}"))
            .expect("Text encode")
        })
        .collect();
    Cluster {
        gap: ui::Spacing::Xs,
        align: FlexAlign::Center,
        justify: FlexJustify::Start,
        children: words,
        wrap: Some(true),
    }
    .into_component(id)
    .expect("Cluster encode")
}

fn breadcrumb(id: &str, entity: &str, via: Option<&str>, result_title: &str) -> Component {
    let node = |nid: String, label: &str, tone: Tone| {
        Chip {
            variant: ChipVariant::Soft,
            tone,
            label: lit(label),
            icon: None,
            avatar: None,
            selected: None,
            removable: false,
            dot: None,
        }
        .into_component(nid)
        .expect("Chip encode")
    };
    let arrow = |aid: String| {
        crate::ui::text_c(&aid, lit("→"), TextStyle::Caption, Some(Tone::Muted))
    };
    let mut children = vec![node(format!("{id}-e"), entity, Tone::Info)];
    if let Some(v) = via {
        children.push(arrow(format!("{id}-a1")));
        children.push(node(format!("{id}-v"), v, Tone::Neutral));
    }
    children.push(arrow(format!("{id}-a2")));
    children.push(node(format!("{id}-n"), result_title, Tone::Neutral));
    Cluster {
        gap: ui::Spacing::Xs,
        align: FlexAlign::Center,
        justify: FlexJustify::Start,
        children,
        wrap: Some(true),
    }
    .into_component(id)
    .expect("Cluster encode")
}

fn result_card(index: usize, hit: &SearchHit) -> Component {
    let id = format!("res-{index}");
    let title = if hit.title.is_empty() {
        "(bez tytułu)"
    } else {
        &hit.title
    };

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
    .into_component(format!("{id}-title"))
    .expect("Text encode");

    let date = Text {
        content: ui::lit_value(CborValue::U64((hit.updated_at.max(0) as u64) * 1000)),
        style: TextStyle::Caption,
        tone: Some(Tone::Muted),
        align: None,
        wrap: None,
        max_lines: None,
        format: Some(ValueFormat::Date {
            style: ui::DateStyle::Medium,
        }),
        streaming: None,
    }
    .into_component(format!("{id}-date"))
    .expect("Text encode");

    let meta = Cluster {
        gap: ui::Spacing::Sm,
        align: FlexAlign::Center,
        justify: FlexJustify::Start,
        children: vec![
            crate::ui::scope_badge(&format!("{id}-scope"), &hit.scope),
            date,
            crate::ui::text_c(
                &format!("{id}-author"),
                lit(db::user_display_name(&hit.owner_user_id)),
                TextStyle::Caption,
                Some(Tone::Muted),
            ),
        ],
        wrap: Some(true),
    }
    .into_component(format!("{id}-meta"))
    .expect("Cluster encode");

    let mut left_children = vec![
        title_text,
        snippet_component(&format!("{id}-snip"), &hit.snippet),
        meta,
    ];
    if let Method::Graph { entity, via, .. } = &hit.method {
        left_children.push(breadcrumb(
            &format!("{id}-path"),
            entity,
            via.as_deref(),
            title,
        ));
    }
    let left = ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: None,
        margin: None,
        children: left_children,
        style: Some(ui::BoxStyle {
            min_width: Some(DimensionToken::Px { value: 0 }),
            ..Default::default()
        }),
        direction: Some(FlexDirection::Column),
        gap: Some(ui::Spacing::Xs),
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: None,
    }
    .into_component(format!("{id}-left"))
    .expect("Box encode");

    // Score column: method badge + horizontal bar + percent (the mockup's
    // vertical bar has no token equivalent; phones use this layout anyway).
    let bar = ui::Box {
        width: Some(DimensionToken::Px { value: 64 }),
        grow: None,
        align_self: None,
        padding: None,
        margin: None,
        children: vec![ui::ProgressBar {
            value: ui::lit_value(CborValue::F64(hit.percent as f64 / 100.0)),
            max: 1.0,
            variant: ui::ProgressVariant::Default,
            tone: Tone::Primary,
            show_label: false,
            label: None,
            size: ui::ProgressSize::Sm,
        }
        .into_component(format!("{id}-bar"))
        .expect("ProgressBar encode")],
        style: None,
        direction: Some(FlexDirection::Column),
        gap: None,
        align: Some(FlexAlign::Stretch),
        justify: Some(FlexJustify::Center),
        responsive: None,
    }
    .into_component(format!("{id}-barwrap"))
    .expect("Box encode");

    let score = Cluster {
        gap: ui::Spacing::Sm,
        align: FlexAlign::Center,
        justify: FlexJustify::End,
        children: vec![
            method_badge(&format!("{id}-method"), &hit.method),
            bar,
            crate::ui::text_c(
                &format!("{id}-pct"),
                lit(format!("{}%", hit.percent)),
                TextStyle::BodyStrong,
                Some(Tone::Primary),
            ),
        ],
        wrap: Some(false),
    }
    .into_component(format!("{id}-score"))
    .expect("Cluster encode");

    let mut card = Card {
        variant: CardVariant::Outlined,
        padding: ui::Spacing::Md,
        gap: ui::Spacing::Sm,
        radius: ui::RadiusToken::Lg,
        shadow: ShadowToken::None,
        border: ui::BorderToken::Hairline,
        background: ui::BackgroundToken::Subtle,
        accent: None,
        children: vec![Flex {
            direction: FlexDirection::Row,
            gap: ui::Spacing::Md,
            justify: FlexJustify::SpaceBetween,
            align: FlexAlign::Center,
            wrap: FlexWrap::NoWrap,
            children: vec![left, score],
            padding: None,
            background: None,
            radius: None,
            style: None,
            responsive: Some(vec![ui::ResponsiveRule {
                max_width: ui::ContainerWidth::Px(600),
                direction: Some(FlexDirection::Column),
                gap: Some(ui::Spacing::Sm),
                align: Some(FlexAlign::Stretch),
                justify: None,
                padding: None,
                min_height: None,
                order: None,
                hidden: None,
                width: None,
            }]),
        }
        .into_component(format!("{id}-row"))
        .expect("Flex encode")],
        interactive: true,
        clickable: true,
        style: None,
    }
    .into_component(id.clone())
    .expect("Card encode");
    card.handlers = Some(crate::ui::backend_params(
        EventKind::Click,
        "search_open_note",
        vec![("note_id", CborValue::Text(hit.note_id.clone()))],
    ));
    card
}

/// The streamed answer card: overline header + streaming chip, a bound Text
/// with the semantic streaming caret, and the numbered source chips (klik →
/// otwiera notatkę).
fn answer_card(sources: &[&SearchHit]) -> Component {
    let title = crate::ui::text_c(
        "ans-title",
        lit("Odpowiedź"),
        TextStyle::Overline,
        Some(Tone::Primary),
    );
    let streaming_chip = crate::ui::with_visible_bound(
        Chip {
            variant: ChipVariant::Soft,
            tone: Tone::Primary,
            label: lit("streaming"),
            icon: None,
            avatar: None,
            selected: None,
            removable: false,
            dot: Some(Tone::Primary),
        }
        .into_component("ans-streaming")
        .expect("Chip encode"),
        SP_STREAMING,
    );
    let head = Flex {
        direction: FlexDirection::Row,
        gap: ui::Spacing::Sm,
        justify: FlexJustify::SpaceBetween,
        align: FlexAlign::Center,
        wrap: FlexWrap::NoWrap,
        children: vec![title, streaming_chip],
        padding: None,
        background: None,
        radius: None,
        style: None,
        responsive: None,
    }
    .into_component("ans-head")
    .expect("Flex encode");

    let body = Text {
        content: bound(SP_ANSWER),
        style: TextStyle::Body,
        tone: None,
        align: None,
        wrap: Some(ui::TextWrap::Wrap),
        max_lines: None,
        format: None,
        streaming: Some(bound(SP_STREAMING)),
    }
    .into_component("ans-body")
    .expect("Text encode");

    let mut src_children: Vec<Component> = vec![crate::ui::text_c(
        "ans-src-label",
        lit("Źródła:"),
        TextStyle::Caption,
        Some(Tone::Muted),
    )];
    for (i, hit) in sources.iter().enumerate() {
        let title = if hit.title.is_empty() {
            "(bez tytułu)".to_string()
        } else {
            hit.title.clone()
        };
        let mut cite = Chip {
            variant: ChipVariant::Outline,
            tone: Tone::Neutral,
            label: lit(format!("{} {title}", i + 1)),
            icon: None,
            avatar: None,
            selected: None,
            removable: false,
            dot: Some(Tone::Primary),
        }
        .into_component(format!("ans-cite-{i}"))
        .expect("Chip encode");
        cite.handlers = Some(crate::ui::backend_params(
            EventKind::Click,
            "search_open_note",
            vec![("note_id", CborValue::Text(hit.note_id.clone()))],
        ));
        src_children.push(cite);
    }
    let sources_row = Cluster {
        gap: ui::Spacing::Xs,
        align: FlexAlign::Center,
        justify: FlexJustify::Start,
        children: src_children,
        wrap: Some(true),
    }
    .into_component("ans-sources")
    .expect("Cluster encode");

    Card {
        variant: CardVariant::Outlined,
        padding: ui::Spacing::Md,
        gap: ui::Spacing::Sm,
        radius: ui::RadiusToken::Lg,
        shadow: ShadowToken::AccentGlow,
        border: ui::BorderToken::Accent {
            tone: Tone::Primary,
        },
        background: ui::BackgroundToken::Subtle,
        accent: Some(Tone::Primary),
        children: vec![head, body, sources_row],
        interactive: false,
        clickable: false,
        style: None,
    }
    .into_component("answer-card")
    .expect("Card encode")
}

fn rail_title(id: &str, label: &str) -> Component {
    crate::ui::text_c(id, lit(label), TextStyle::Overline, Some(Tone::Primary))
}

fn rail_card(id: &str, children: Vec<Component>) -> Component {
    Card {
        variant: CardVariant::Outlined,
        padding: ui::Spacing::Md,
        gap: ui::Spacing::Sm,
        radius: ui::RadiusToken::Lg,
        shadow: ShadowToken::None,
        border: ui::BorderToken::Hairline,
        background: ui::BackgroundToken::Subtle,
        accent: None,
        children,
        interactive: false,
        clickable: false,
        style: None,
    }
    .into_component(id)
    .expect("Card encode")
}

fn right_rail(output: &SearchOutput, narrow_name: Option<&str>) -> Component {
    let mut cards: Vec<Component> = Vec::new();

    // Active narrowing indicator with a clear affordance.
    if let Some(name) = narrow_name {
        let mut clear = Chip {
            variant: ChipVariant::Soft,
            tone: Tone::Info,
            label: lit(format!("Zawężono: {name}")),
            icon: None,
            avatar: None,
            selected: None,
            removable: true,
            dot: None,
        }
        .into_component("rail-narrow-active")
        .expect("Chip encode");
        clear.handlers = Some(backend(EventKind::Remove, "search_narrow_clear"));
        cards.push(rail_card("rail-active", vec![clear]));
    }

    // Entities in results.
    let mut ent_children = vec![rail_title("rail-ent-title", "Encje w wynikach")];
    if output.entities.is_empty() {
        ent_children.push(crate::ui::text_c(
            "rail-ent-empty",
            lit("Brak wykrytych encji w wynikach."),
            TextStyle::Caption,
            Some(Tone::Muted),
        ));
    } else {
        let chips: Vec<Component> = output
            .entities
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let mut chip = Chip {
                    variant: ChipVariant::Soft,
                    tone: Tone::Neutral,
                    label: lit(format!("{} · {}", e.name, e.count)),
                    icon: None,
                    avatar: None,
                    selected: None,
                    removable: false,
                    dot: Some(crate::ui::entity_tone(&e.entity_type)),
                }
                .into_component(format!("rail-ent-{i}"))
                .expect("Chip encode");
                chip.handlers = Some(crate::ui::backend_params(
                    EventKind::Click,
                    "search_narrow",
                    vec![
                        ("entity_id", CborValue::Text(e.id.clone())),
                        ("entity_name", CborValue::Text(e.name.clone())),
                    ],
                ));
                chip
            })
            .collect();
        ent_children.push(
            Cluster {
                gap: ui::Spacing::Xs,
                align: FlexAlign::Center,
                justify: FlexJustify::Start,
                children: chips,
                wrap: Some(true),
            }
            .into_component("rail-ent-chips")
            .expect("Cluster encode"),
        );
    }
    cards.push(rail_card("rail-entities", ent_children));

    // Graph narrowing suggestions (top entities).
    let mut narrow_children = vec![rail_title("rail-narrow-title", "Zawęź przez graf")];
    if output.entities.is_empty() {
        narrow_children.push(crate::ui::text_c(
            "rail-narrow-empty",
            lit("Sugestie pojawią się, gdy wyniki będą miały encje."),
            TextStyle::Caption,
            Some(Tone::Muted),
        ));
    }
    for (i, e) in output
        .entities
        .iter()
        .take(search::MAX_NARROW_SUGGESTIONS)
        .enumerate()
    {
        let mut item = Card {
            variant: CardVariant::Outlined,
            padding: ui::Spacing::Sm,
            gap: ui::Spacing::Xs,
            radius: ui::RadiusToken::Md,
            shadow: ShadowToken::None,
            border: ui::BorderToken::Hairline,
            background: ui::BackgroundToken::Subtle,
            accent: None,
            children: vec![Cluster {
                gap: ui::Spacing::Xs,
                align: FlexAlign::Center,
                justify: FlexJustify::Start,
                children: vec![
                    crate::ui::text_c(
                        &format!("rail-nr-{i}-pre"),
                        lit("tylko notatki połączone z:"),
                        TextStyle::Caption,
                        Some(Tone::Muted),
                    ),
                    crate::ui::text_c(
                        &format!("rail-nr-{i}-name"),
                        lit(&e.name),
                        TextStyle::BodyStrong,
                        None,
                    ),
                ],
                wrap: Some(true),
            }
            .into_component(format!("rail-nr-{i}-row"))
            .expect("Cluster encode")],
            interactive: true,
            clickable: true,
            style: None,
        }
        .into_component(format!("rail-nr-{i}"))
        .expect("Card encode");
        item.handlers = Some(crate::ui::backend_params(
            EventKind::Click,
            "search_narrow",
            vec![
                ("entity_id", CborValue::Text(e.id.clone())),
                ("entity_name", CborValue::Text(e.name.clone())),
            ],
        ));
        narrow_children.push(item);
    }
    cards.push(rail_card("rail-narrow", narrow_children));

    ui::Box {
        width: Some(DimensionToken::Px { value: 300 }),
        grow: None,
        align_self: None,
        padding: None,
        margin: None,
        children: cards,
        style: Some(ui::BoxStyle {
            min_width: Some(DimensionToken::Px { value: 300 }),
            ..Default::default()
        }),
        direction: Some(FlexDirection::Column),
        gap: Some(ui::Spacing::Md),
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: Some(vec![ui::ResponsiveRule {
            max_width: ui::ContainerWidth::Px(1080),
            direction: None,
            gap: None,
            align: None,
            justify: None,
            padding: None,
            min_height: None,
            order: None,
            hidden: None,
            width: Some(DimensionToken::Full),
        }]),
    }
    .into_component("search-rail")
    .expect("Box encode")
}

/// Full results-slot fragment. `with_answer` renders the streamed answer card
/// (hybrid mode with hits); the whole tree is visibility-bound to the search
/// view because the slot container lives outside the shell subtree.
fn results_fragment(
    output: &SearchOutput,
    query: &str,
    narrow_name: Option<&str>,
    with_answer: bool,
) -> Component {
    let mut main_children: Vec<Component> = Vec::new();
    if with_answer {
        let sources: Vec<&SearchHit> = output.hits.iter().take(search::ANSWER_SOURCES).collect();
        main_children.push(answer_card(&sources));
    }
    if output.hits.is_empty() {
        let (heading, message) = if query.trim().is_empty() {
            (
                "Szukaj w notatkach",
                "Zadaj pytanie w polu powyżej — wyniki połączą podobieństwo semantyczne z grafem powiązań.",
            )
        } else {
            (
                "Brak wyników",
                "Żadna dostępna notatka nie pasuje do zapytania.",
            )
        };
        main_children.push(
            EmptyState {
                icon: crate::ui::icon(IconName::Search),
                heading: lit(heading),
                message: Some(lit(message)),
                primary_action: None,
                secondary_action: None,
                variant: EmptyStateVariant::Default,
            }
            .into_component("search-empty")
            .expect("EmptyState encode"),
        );
    }
    for (i, hit) in output.hits.iter().enumerate() {
        main_children.push(result_card(i, hit));
    }

    let main = ui::Box {
        width: None,
        grow: Some(true),
        align_self: None,
        padding: None,
        margin: None,
        children: main_children,
        style: Some(ui::BoxStyle {
            min_width: Some(DimensionToken::Px { value: 0 }),
            ..Default::default()
        }),
        direction: Some(FlexDirection::Column),
        gap: Some(ui::Spacing::Sm),
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: None,
    }
    .into_component("search-main")
    .expect("Box encode");

    let layout = Flex {
        direction: FlexDirection::Row,
        gap: ui::Spacing::Md,
        justify: FlexJustify::Start,
        align: FlexAlign::Start,
        wrap: FlexWrap::NoWrap,
        children: vec![main, right_rail(output, narrow_name)],
        padding: None,
        background: None,
        radius: None,
        style: None,
        responsive: Some(vec![ui::ResponsiveRule {
            max_width: ui::ContainerWidth::Px(1080),
            direction: Some(FlexDirection::Column),
            gap: Some(ui::Spacing::Md),
            align: None,
            justify: None,
            padding: None,
            min_height: None,
            order: None,
            hidden: None,
            width: None,
        }]),
    }
    .into_component("search-layout")
    .expect("Flex encode");

    // The slot container sits at the shell root (outside the view subtrees),
    // so the fragment root carries the view-visibility binding itself.
    crate::ui::with_visible_bound(
        ui::Box {
            width: None,
            grow: None,
            align_self: None,
            padding: None,
            margin: None,
            children: vec![layout],
            style: None,
            direction: Some(FlexDirection::Column),
            gap: None,
            align: Some(FlexAlign::Stretch),
            justify: None,
            responsive: None,
        }
        .into_component("search-root")
        .expect("Box encode"),
        crate::ui::SP_VIEW_SEARCH,
    )
}

/// Placeholder pushed at panel open / view entry (before the first query).
pub(crate) fn send_search_placeholder() {
    let output = SearchOutput::default();
    crate::ui::send_slot(
        SLOT_RESULTS,
        results_fragment(&output, "", None, false),
        None,
    );
}

// =============================================================================
// Actions
// =============================================================================

fn set_answer_ops(text: &str, streaming: bool) -> Vec<PatchOp> {
    vec![
        PatchOp {
            path: state_path(SP_ANSWER),
            op: PatchOpKind::Set {
                value: CborValue::Text(text.to_string()),
            },
        },
        PatchOp {
            path: state_path(SP_STREAMING),
            op: PatchOpKind::Set {
                value: CborValue::Bool(streaming),
            },
        },
    ]
}

/// Runs a search (query commit, scope change, narrowing). Renders the results
/// slot first, then — in hybrid mode — streams the answer synthesis into the
/// bound Text, superseded by any newer generation.
pub(crate) fn run_search(ctx: &UserCtx) -> JsonValue {
    let sess = crate::ui::load_session();
    let query = sess.s_query.clone();
    let scope = sess.s_scope.clone();
    let narrow = if sess.s_narrow.is_empty() {
        None
    } else {
        Some(sess.s_narrow.as_str())
    };
    let narrow_name = if sess.s_narrow_name.is_empty() {
        None
    } else {
        Some(sess.s_narrow_name.as_str())
    };

    let generation = bump_generation();

    let output = match search::run_hybrid(ctx, &query, &scope, narrow) {
        Ok(o) => o,
        Err(e) => {
            crate::ui::send_slot(
                SLOT_RESULTS,
                crate::ui::with_visible_bound(
                    crate::ui::error_fragment("search-error", &e),
                    crate::ui::SP_VIEW_SEARCH,
                ),
                None,
            );
            return json!({"ok": false, "error": e});
        }
    };

    let with_answer = !output.text_fallback && !output.hits.is_empty();
    crate::ui::send_slot(
        SLOT_RESULTS,
        results_fragment(&output, &query, narrow_name, with_answer),
        None,
    );
    crate::ui::send_state_patch(set_answer_ops("", with_answer));

    if with_answer {
        stream_answer(&query, &output, generation);
    }
    json!({"ok": true, "results": output.hits.len(), "fallback": output.text_fallback})
}

/// Streams the LLM synthesis into SP_ANSWER (translator pattern): batches →
/// StatePatch, wall-clock deadline, empty-batch cap, generation guard, and an
/// explicit cancel on every early exit so no host stream slot leaks.
fn stream_answer(query: &str, output: &SearchOutput, generation: u64) {
    let sources: Vec<(String, String)> = output
        .hits
        .iter()
        .take(search::ANSWER_SOURCES)
        .map(|h| {
            (
                if h.title.is_empty() {
                    "(bez tytułu)".to_string()
                } else {
                    h.title.clone()
                },
                h.content.clone(),
            )
        })
        .collect();
    let prompt = search::build_answer_prompt(query, &sources);

    let mut stream = match generate_stream_start(
        &prompt,
        Some(analysis::ANSWER_ALIAS),
        Some(&json!({"temperature": 0.3, "max_tokens": 700})),
    ) {
        Ok(s) => s,
        Err(e) => {
            log::warn(&format!("notes: answer stream start failed: {e:?}"));
            crate::ui::send_state_patch(set_answer_ops(
                "Nie udało się uruchomić syntezy odpowiedzi — sprawdź alias notes-llm.",
                false,
            ));
            return;
        }
    };

    let started = db::now_unix_ms();
    let mut answer = String::new();
    // Drops [n] markers pointing outside the numbered sources; buffers a
    // marker split across batch boundaries.
    let mut citations = search::CitationFilter::new(sources.len());
    let mut empty_batches: u32 = 0;
    let mut finished_cleanly = false;
    loop {
        // A newer query / view switch supersedes this stream.
        if load_generation() != generation {
            break;
        }
        if db::now_unix_ms() - started > STREAM_DEADLINE_MS {
            log::warn("notes: answer stream exceeded deadline");
            break;
        }
        let batch = match stream.next_batch(STREAM_BATCH_TIMEOUT_MS) {
            Ok(b) => b,
            Err(e) => {
                log::warn(&format!("notes: answer stream next failed: {e:?}"));
                break;
            }
        };
        if batch.chunks.is_empty() && !batch.finished {
            empty_batches += 1;
            if empty_batches >= STREAM_MAX_EMPTY_BATCHES {
                log::warn("notes: answer stream stalled");
                break;
            }
        } else {
            empty_batches = 0;
        }
        if !batch.chunks.is_empty() {
            for c in &batch.chunks {
                answer.push_str(&citations.push(c));
            }
            // The addon gets no panel-close callback (the host consumes
            // PanelClose without calling into the instance), but the host
            // DOES reject outbound StatePatch for a closed panel — a refused
            // patch is therefore the close signal: stop consuming and cancel
            // the host stream instead of generating into the void until the
            // 90 s deadline. The host's 60 s idle reaper backstops paths that
            // never reach this send.
            if !crate::ui::send_state_patch(set_answer_ops(&answer, true)) {
                log::info("notes: answer stream aborted — panel closed");
                break;
            }
        }
        if batch.finished {
            finished_cleanly = true;
            if let Some(err) = batch.error {
                log::warn(&format!("notes: answer stream error: {err}"));
            }
            break;
        }
    }
    if !finished_cleanly {
        let _ = stream.cancel();
    }
    // A dangling partial marker withheld by the filter is prose, not a
    // citation — it belongs in the final text.
    answer.push_str(&citations.finish());
    // Only the OWNING generation clears the streaming chip — a superseding
    // search already repainted the card.
    if load_generation() == generation {
        crate::ui::send_state_patch(set_answer_ops(&answer, false));
    }
}

/// Query commit from the hero search box.
pub(crate) fn action_run_search(ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("");
    let mut sess = crate::ui::load_session();
    sess.s_query = value.trim().to_string();
    // A fresh query drops the previous narrowing.
    sess.s_narrow.clear();
    sess.s_narrow_name.clear();
    crate::ui::store_session(&sess);
    run_search(ctx)
}

/// Scope chip change.
pub(crate) fn action_search_scope(ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let scope = params.get("chip_id").and_then(|v| v.as_str()).unwrap_or("all");
    let scope = if matches!(scope, "all" | "mine" | "group" | "org") {
        scope
    } else {
        "all"
    };
    let mut sess = crate::ui::load_session();
    sess.s_scope = scope.to_string();
    crate::ui::store_session(&sess);
    run_search(ctx)
}

/// Narrow results to notes connected to an entity (right rail).
pub(crate) fn action_search_narrow(ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    let entity_id = params.get("entity_id").and_then(|v| v.as_str()).unwrap_or("");
    if entity_id.is_empty() {
        return json!({"ok": false, "error": "Brak entity_id"});
    }
    let name = params.get("entity_name").and_then(|v| v.as_str()).unwrap_or("");
    let mut sess = crate::ui::load_session();
    sess.s_narrow = entity_id.to_string();
    sess.s_narrow_name = name.to_string();
    crate::ui::store_session(&sess);
    run_search(ctx)
}

pub(crate) fn action_search_narrow_clear(ctx: &UserCtx) -> JsonValue {
    let mut sess = crate::ui::load_session();
    sess.s_narrow.clear();
    sess.s_narrow_name.clear();
    crate::ui::store_session(&sess);
    run_search(ctx)
}

/// Result / citation click: jump to the notes view with that note open (same
/// flow as the graph detail rail).
pub(crate) fn action_search_open_note(ctx: &UserCtx, params: &JsonValue) -> JsonValue {
    crate::ui::open_note_from_other_view(ctx, params)
}
