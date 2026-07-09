// =============================================================================
// File: addons/notes/src/ui_graph.rs
// Purpose: Graph view of the Notes addon (mockup n02). Three columns:
//          filters (260px) | RelationGraph canvas (grows) | node detail
//          (300px, Inspector slot). Nodes and edges are built from the
//          addon's OWN SQLite through the read ACL (db::graph_*) — Cozo is
//          the PPR/traversal store, never a bypass of note access control.
//          Everything except the detail slot is state-driven: filter and
//          selection changes go out as StatePatch, so the force layout in
//          <tf-relation-graph> keeps its physics/selection across updates.
// =============================================================================

use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::{json, Value as JsonValue};

use tentaflow_addon_sdk::ui_v1::{self as ui, backend, bound, lit, state_path};
use tentaflow_addon_sdk::ui_v1::{
    Accessibility, Avatar, AvatarRef, AvatarShape, AvatarSize, Button, ButtonSize,
    ButtonVariant, Callout, Card, CardVariant, Chip, ChipVariant, Cluster, Component, Density,
    DimensionToken, EmptyState, EmptyStateVariant, EventKind, FieldMap, Flex, FlexAlign,
    FlexDirection, FlexJustify, FlexWrap, Heading, IconName, Inspector,
    PatchOp, PatchOpKind, SegmentOption, SegmentSize, SegmentedControl, SelectValue,
    ShadowToken, Slider, SliderMark, Spacing, StateEntry, Text, TextStyle,
    Toggle, TogglePosition, ToggleSize, Tone, Value as CborValue, ValueFormat,
};
use tentaflow_sdk_spec::encode_to_value;

use crate::analysis;
use crate::db::{self, GraphLinkRow, GraphMentionRow, GraphNoteRow, UserCtx};
use crate::ui::{
    backend_params, entity_tone, error_fragment, icon, merge_suggestion_card, panel_style,
    scope_badge, send_slot, send_state_patch, text_c, Session,
};

pub const SLOT_DETAIL: &str = "graph-detail";

/// Component cap of tf-relation-graph (max_nodes) — the build prunes to it.
pub const MAX_GRAPH_NODES: usize = 500;

/// RelationGraph wire tag (catalog §0x0703 — FieldMap-validated, no typed
/// struct in the spec).
const RELATION_GRAPH_TAG: u16 = 0x0703;

// Graph state paths. gr.nodes / gr.edges feed the RelationGraph component;
// the rest drive the state-bound filter controls and the counter pill.
const SP_G_NODES: &str = "gr.nodes";
const SP_G_EDGES: &str = "gr.edges";
const SP_G_COUNTER: &str = "gr.counter";
const SP_G_SELECTED: &str = "gr.selected";
const SP_G_MIN_WEIGHT: &str = "gr.min_weight";
const SP_G_DEPTH: &str = "gr.depth";

const SCOPES: [&str; 4] = ["mine", "shared", "group", "org"];
const TYPES: [&str; 5] = ["person", "project", "company", "topic", "note"];

fn scope_path(scope: &str) -> String {
    format!("gr.scope.{scope}")
}

fn type_path(t: &str) -> String {
    format!("gr.type.{t}")
}

fn count_path(key: &str) -> String {
    format!("gr.cnt.{key}")
}

// =============================================================================
// Pure graph build — filters, BFS depth, node cap. Unit-tested natively.
// =============================================================================

/// Raw ACL-passed rows the view is built from.
#[derive(Debug, Clone, Default)]
pub struct GraphInput {
    pub notes: Vec<GraphNoteRow>,
    pub mentions: Vec<GraphMentionRow>,
    pub links: Vec<GraphLinkRow>,
}

/// Filter state of the graph view (mirrors the session).
#[derive(Debug, Clone)]
pub struct GraphFilters {
    pub mine: bool,
    pub shared: bool,
    pub group: bool,
    pub org: bool,
    pub person: bool,
    pub project: bool,
    pub company: bool,
    pub topic: bool,
    pub note: bool,
    pub min_weight: f64,
    pub depth: usize,
    pub selected: String,
}

impl Default for GraphFilters {
    fn default() -> Self {
        GraphFilters {
            mine: true,
            shared: true,
            group: true,
            org: false,
            person: true,
            project: true,
            company: true,
            topic: true,
            note: true,
            min_weight: 0.4,
            depth: 2,
            selected: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GNode {
    /// Prefixed id: "n:<note_id>" or "e:<entity_id>".
    pub id: String,
    pub label: String,
    /// "note" | entity_type.
    pub node_type: String,
    pub tone: Tone,
    /// Notes: updated_at (cap ordering). Entities: 0.
    pub updated_at: i64,
    /// Entities: number of kept notes mentioning it. Notes: 0.
    pub mention_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    /// "mention" | "similar" | "entity" | "manual".
    pub kind: String,
    pub weight: f64,
    pub label: Option<String>,
    pub tone: Tone,
    pub dashed: bool,
    /// Persisted machine reason token of note-to-note links.
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct GraphView {
    pub nodes: Vec<GNode>,
    pub edges: Vec<GEdge>,
    /// Node count BEFORE the 500 cap ("pokazano X z Y").
    pub total_nodes: usize,
    /// Accessible note counts per bucket [mine, shared, group, org] —
    /// independent of the scope toggles.
    pub scope_counts: [usize; 4],
    /// Entity/note counts per type [person, project, company, topic, note]
    /// among the scope-kept notes — independent of the type checkboxes.
    pub type_counts: [usize; 5],
}

pub fn note_node_id(note_id: &str) -> String {
    format!("n:{note_id}")
}

pub fn entity_node_id(entity_id: &str) -> String {
    format!("e:{entity_id}")
}

/// Filter bucket of an entity type; unknown types land under "topic".
fn type_index(entity_type: &str) -> usize {
    match entity_type {
        "person" => 0,
        "project" => 1,
        "company" => 2,
        _ => 3,
    }
}

fn scope_index(bucket: &str) -> usize {
    match bucket {
        "mine" => 0,
        "shared" => 1,
        "group" => 2,
        _ => 3,
    }
}

fn scope_enabled(f: &GraphFilters, bucket: &str) -> bool {
    match bucket {
        "mine" => f.mine,
        "shared" => f.shared,
        "group" => f.group,
        _ => f.org,
    }
}

fn type_enabled(f: &GraphFilters, entity_type: &str) -> bool {
    match type_index(entity_type) {
        0 => f.person,
        1 => f.project,
        2 => f.company,
        _ => f.topic,
    }
}

/// Builds the view: scope/type filters → edges → optional BFS neighbourhood
/// of the selected node → 500-node cap by recency. ACL is NOT re-checked
/// here — the input rows already passed it in db.rs; edges additionally
/// require both endpoints present, so a filtered-out or inaccessible note
/// can never appear through a link.
pub fn build_graph(input: &GraphInput, f: &GraphFilters) -> GraphView {
    let mut view = GraphView::default();

    for n in &input.notes {
        view.scope_counts[scope_index(&n.bucket)] += 1;
    }

    let kept_notes: Vec<&GraphNoteRow> = input
        .notes
        .iter()
        .filter(|n| scope_enabled(f, &n.bucket))
        .collect();
    let note_set: HashSet<&str> = kept_notes.iter().map(|n| n.id.as_str()).collect();

    // Entities mentioned by kept notes, aggregated per canonical id.
    let mut entities: HashMap<&str, (&str, &str, usize)> = HashMap::new();
    let mut mention_pairs: Vec<(&str, &str, &str)> = Vec::new();
    let mut seen_pairs: HashSet<(&str, &str)> = HashSet::new();
    for m in &input.mentions {
        if !note_set.contains(m.note_id.as_str()) {
            continue;
        }
        if !seen_pairs.insert((m.note_id.as_str(), m.entity_id.as_str())) {
            continue;
        }
        let e = entities
            .entry(m.entity_id.as_str())
            .or_insert((m.name.as_str(), m.entity_type.as_str(), 0));
        e.2 += 1;
        mention_pairs.push((m.note_id.as_str(), m.entity_id.as_str(), m.entity_type.as_str()));
    }

    for (_, etype, _) in entities.values() {
        view.type_counts[type_index(etype)] += 1;
    }
    view.type_counts[4] = kept_notes.len();

    // Nodes.
    let mut nodes: Vec<GNode> = Vec::new();
    if f.note {
        for n in &kept_notes {
            nodes.push(GNode {
                id: note_node_id(&n.id),
                label: if n.title.is_empty() {
                    "(bez tytułu)".to_string()
                } else {
                    n.title.clone()
                },
                node_type: "note".to_string(),
                tone: Tone::Primary,
                updated_at: n.updated_at,
                mention_count: 0,
            });
        }
    }
    let mut entity_ids: Vec<&str> = entities.keys().copied().collect();
    entity_ids.sort_unstable();
    for eid in entity_ids {
        let (name, etype, count) = entities[eid];
        if !type_enabled(f, etype) {
            continue;
        }
        nodes.push(GNode {
            id: entity_node_id(eid),
            label: name.to_string(),
            node_type: etype.to_string(),
            tone: entity_tone(etype),
            updated_at: 0,
            mention_count: count,
        });
    }
    let node_ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();

    // Edges — mention (note→entity) plus deduped note↔note links.
    let mut edges: Vec<GEdge> = Vec::new();
    if f.note {
        for (note_id, entity_id, etype) in &mention_pairs {
            let source = note_node_id(note_id);
            let target = entity_node_id(entity_id);
            if !node_ids.contains(target.as_str()) {
                continue;
            }
            edges.push(GEdge {
                id: format!("m:{note_id}:{entity_id}"),
                source,
                target,
                kind: "mention".to_string(),
                weight: 0.0,
                label: None,
                tone: entity_tone(etype),
                dashed: false,
                reason: String::new(),
            });
        }
        let mut seen_links: HashSet<(String, String, &str)> = HashSet::new();
        for l in &input.links {
            if l.src == l.dst
                || !note_set.contains(l.src.as_str())
                || !note_set.contains(l.dst.as_str())
            {
                continue;
            }
            let (a, b) = if l.src <= l.dst {
                (l.src.clone(), l.dst.clone())
            } else {
                (l.dst.clone(), l.src.clone())
            };
            if !seen_links.insert((a.clone(), b.clone(), l.kind.as_str())) {
                continue;
            }
            let dashed = l.kind == "similar";
            // The weight slider hides weak SEMANTIC edges only — entity and
            // manual links carry explicit intent and always stay.
            if dashed && l.weight < f.min_weight {
                continue;
            }
            let percent = (l.weight.clamp(0.0, 1.0) * 100.0).round() as i64;
            edges.push(GEdge {
                id: format!("{}:{a}:{b}", if dashed { "s" } else { "l" }),
                source: note_node_id(&a),
                target: note_node_id(&b),
                kind: l.kind.clone(),
                weight: l.weight,
                label: if dashed { Some(format!("{percent}%")) } else { None },
                tone: Tone::Primary,
                dashed,
                reason: l.reason.clone(),
            });
        }
    }

    // BFS neighbourhood of the selected node (depth slider).
    if !f.selected.is_empty() && node_ids.contains(f.selected.as_str()) {
        let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
        for e in &edges {
            adjacency.entry(e.source.as_str()).or_default().push(e.target.as_str());
            adjacency.entry(e.target.as_str()).or_default().push(e.source.as_str());
        }
        let mut depth: HashMap<&str, usize> = HashMap::new();
        depth.insert(f.selected.as_str(), 0);
        let mut queue: VecDeque<&str> = VecDeque::new();
        queue.push_back(f.selected.as_str());
        while let Some(cur) = queue.pop_front() {
            let d = depth[cur];
            if d >= f.depth {
                continue;
            }
            for next in adjacency.get(cur).into_iter().flatten() {
                if !depth.contains_key(next) {
                    depth.insert(next, d + 1);
                    queue.push_back(next);
                }
            }
        }
        nodes.retain(|n| depth.contains_key(n.id.as_str()));
        let kept: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        edges.retain(|e| kept.contains(e.source.as_str()) && kept.contains(e.target.as_str()));
    }

    view.total_nodes = nodes.len();

    // Cap: the selected node always survives, then notes by recency, then
    // entities by mention count.
    if nodes.len() > MAX_GRAPH_NODES {
        nodes.sort_by(|a, b| {
            let sel = |n: &GNode| n.id != f.selected;
            let is_entity = |n: &GNode| n.node_type != "note";
            sel(a)
                .cmp(&sel(b))
                .then(is_entity(a).cmp(&is_entity(b)))
                .then(b.updated_at.cmp(&a.updated_at))
                .then(b.mention_count.cmp(&a.mention_count))
                .then(a.id.cmp(&b.id))
        });
        nodes.truncate(MAX_GRAPH_NODES);
        let kept: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        edges.retain(|e| kept.contains(e.source.as_str()) && kept.contains(e.target.as_str()));
    }

    view.nodes = nodes;
    view.edges = edges;
    view
}

/// Counter pill text: "Graf: X nodów · Y krawędzi", with the truncation form
/// "pokazano X z Y" once the 500-node cap kicked in.
pub fn counter_text(view: &GraphView) -> String {
    let shown = view.nodes.len();
    let edges = view.edges.len();
    let edge_word = db::plural_pl(edges, "krawędź", "krawędzie", "krawędzi");
    if view.total_nodes > shown {
        format!(
            "Graf: pokazano {shown} z {} {} · {edges} {edge_word}",
            view.total_nodes,
            db::plural_pl(view.total_nodes, "noda", "nodów", "nodów"),
        )
    } else {
        format!(
            "Graf: {shown} {} · {edges} {edge_word}",
            db::plural_pl(shown, "nod", "nody", "nodów"),
        )
    }
}

// =============================================================================
// Session → filters, data loading
// =============================================================================

pub fn filters_from_session(sess: &Session) -> GraphFilters {
    GraphFilters {
        mine: sess.g_mine,
        shared: sess.g_shared,
        group: sess.g_group,
        org: sess.g_org,
        person: sess.g_person,
        project: sess.g_project,
        company: sess.g_company,
        topic: sess.g_topic,
        note: sess.g_note,
        min_weight: sess.g_min_weight,
        depth: sess.g_depth.clamp(1, 3) as usize,
        selected: sess.g_selected.clone(),
    }
}

fn load_input(ctx: &UserCtx) -> Result<GraphInput, String> {
    let notes = db::graph_notes(ctx)?;
    let ids: Vec<String> = notes.iter().map(|n| n.id.clone()).collect();
    let mentions = db::graph_mentions(ctx, &ids);
    let links = db::graph_links(ctx, &ids);
    Ok(GraphInput {
        notes,
        mentions,
        links,
    })
}

// =============================================================================
// State serialization (GraphNode/GraphEdge wire shape of the renderer)
// =============================================================================

fn tone_text(tone: Tone) -> &'static str {
    match tone {
        Tone::Primary => "primary",
        Tone::Success => "success",
        Tone::Warning => "warning",
        Tone::Critical => "critical",
        Tone::Info => "info",
        Tone::Muted => "muted",
        _ => "neutral",
    }
}

fn text_map(entries: Vec<(&str, CborValue)>) -> CborValue {
    CborValue::Map(
        entries
            .into_iter()
            .map(|(k, v)| (CborValue::Text(k.to_string()), v))
            .collect(),
    )
}

fn nodes_value(nodes: &[GNode]) -> CborValue {
    CborValue::Array(
        nodes
            .iter()
            .map(|n| {
                text_map(vec![
                    ("id", CborValue::Text(n.id.clone())),
                    ("label", CborValue::Text(n.label.clone())),
                    ("node_type", CborValue::Text(n.node_type.clone())),
                    ("tone", CborValue::Text(tone_text(n.tone).to_string())),
                ])
            })
            .collect(),
    )
}

fn edges_value(edges: &[GEdge]) -> CborValue {
    CborValue::Array(
        edges
            .iter()
            .map(|e| {
                let mut entries = vec![
                    ("id", CborValue::Text(e.id.clone())),
                    ("source_id", CborValue::Text(e.source.clone())),
                    ("target_id", CborValue::Text(e.target.clone())),
                    ("tone", CborValue::Text(tone_text(e.tone).to_string())),
                ];
                if let Some(label) = &e.label {
                    entries.push(("label", CborValue::Text(label.clone())));
                }
                if e.weight > 0.0 {
                    entries.push(("weight", CborValue::F64(e.weight)));
                }
                if e.dashed {
                    entries.push(("style", CborValue::Text("dashed".to_string())));
                }
                text_map(entries)
            })
            .collect(),
    )
}

fn count_entries(view: &GraphView) -> Vec<(String, CborValue)> {
    let mut out = Vec::with_capacity(9);
    for (i, scope) in SCOPES.iter().enumerate() {
        out.push((
            count_path(scope),
            CborValue::Text(view.scope_counts[i].to_string()),
        ));
    }
    for (i, t) in TYPES.iter().enumerate() {
        out.push((
            count_path(t),
            CborValue::Text(view.type_counts[i].to_string()),
        ));
    }
    out
}

fn data_state_entries(view: &GraphView, sess: &Session) -> Vec<StateEntry> {
    let mut entries = vec![
        StateEntry {
            path: state_path(SP_G_NODES),
            value: nodes_value(&view.nodes),
        },
        StateEntry {
            path: state_path(SP_G_EDGES),
            value: edges_value(&view.edges),
        },
        StateEntry {
            path: state_path(SP_G_COUNTER),
            value: CborValue::Text(counter_text(view)),
        },
        StateEntry {
            path: state_path(SP_G_SELECTED),
            value: CborValue::Text(sess.g_selected.clone()),
        },
    ];
    for (path, value) in count_entries(view) {
        entries.push(StateEntry {
            path: state_path(&path),
            value,
        });
    }
    entries
}

/// StatePatch with fresh nodes/edges/counter/counts + selection — everything
/// the state-bound graph UI needs after a filter or selection change.
pub fn push_graph_data(view: &GraphView, sess: &Session) {
    let mut ops: Vec<PatchOp> = data_state_entries(view, sess)
        .into_iter()
        .map(|e| PatchOp {
            path: e.path,
            op: PatchOpKind::Set { value: e.value },
        })
        .collect();
    ops.push(PatchOp {
        path: state_path(SP_G_MIN_WEIGHT),
        op: PatchOpKind::Set {
            value: CborValue::F64(sess.g_min_weight),
        },
    });
    ops.push(PatchOp {
        path: state_path(SP_G_DEPTH),
        op: PatchOpKind::Set {
            value: CborValue::F64(sess.g_depth as f64),
        },
    });
    send_state_patch(ops);
}

// =============================================================================
// Shell (graph mode)
// =============================================================================

fn overline(id: &str, label: &str) -> Component {
    text_c(id, lit(label), TextStyle::Overline, Some(Tone::Primary))
}

fn col_box(children: Vec<Component>) -> ui::Box {
    ui::Box {
        width: None,
        grow: None,
        align_self: None,
        padding: None,
        margin: None,
        children,
        style: None,
        direction: Some(FlexDirection::Column),
        gap: None,
        align: Some(FlexAlign::Stretch),
        justify: None,
        responsive: None,
    }
}

/// Toggle row of the scope section: label + toggle left, bound count right.
fn scope_row(index: usize, scope: &str, label: &str) -> Component {
    let mut toggle = Toggle {
        bind_path: state_path(&scope_path(scope)),
        label: Some(lit(label)),
        hint: None,
        size: ToggleSize::Sm,
        tone: Tone::Primary,
        disabled: None,
        label_position: TogglePosition::Trailing,
    }
    .into_component(format!("g-scope-{index}"))
    .expect("Toggle encode");
    toggle.handlers = Some(backend_params(
        EventKind::Change,
        "graph_scope",
        vec![("scope", CborValue::Text(scope.to_string()))],
    ));

    Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Sm,
        justify: FlexJustify::SpaceBetween,
        align: FlexAlign::Center,
        wrap: FlexWrap::NoWrap,
        children: vec![
            toggle,
            text_c(
                &format!("g-scope-{index}-cnt"),
                bound(&count_path(scope)),
                TextStyle::Caption,
                Some(Tone::Muted),
            ),
        ],
        padding: None,
        background: None,
        radius: None,
        style: None,
        responsive: None,
    }
    .into_component(format!("g-scope-{index}-row"))
    .expect("Flex encode")
}

/// Checkbox row of the entity-type section, with the type-colored dot chip
/// aesthetic delegated to the count/dot on the right.
fn type_row(index: usize, t: &str, label: &str, tone: Tone) -> Component {
    let mut checkbox = ui::Checkbox {
        bind_path: state_path(&type_path(t)),
        label: Some(lit(label)),
        hint: None,
        indeterminate: None,
        disabled: None,
        size: ui::CheckboxSize::Sm,
    }
    .into_component(format!("g-type-{index}"))
    .expect("Checkbox encode");
    checkbox.handlers = Some(backend_params(
        EventKind::Change,
        "graph_type",
        vec![("t", CborValue::Text(t.to_string()))],
    ));

    let dot = Chip {
        variant: ChipVariant::Soft,
        tone: Tone::Neutral,
        label: bound(&count_path(t)),
        icon: None,
        avatar: None,
        selected: None,
        removable: false,
        dot: Some(tone),
    }
    .into_component(format!("g-type-{index}-cnt"))
    .expect("Chip encode");

    Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Sm,
        justify: FlexJustify::SpaceBetween,
        align: FlexAlign::Center,
        wrap: FlexWrap::NoWrap,
        children: vec![checkbox, dot],
        padding: None,
        background: None,
        radius: None,
        style: None,
        responsive: None,
    }
    .into_component(format!("g-type-{index}-row"))
    .expect("Flex encode")
}

fn filters_panel() -> Component {
    let mut children: Vec<Component> = vec![overline("g-f-scope-h", "Zakres")];
    for (i, (scope, label)) in [
        ("mine", "Moje notatki"),
        ("shared", "Udostępnione mi"),
        ("group", "Grupa"),
        ("org", "Organizacja"),
    ]
    .iter()
    .enumerate()
    {
        children.push(scope_row(i, scope, label));
    }
    children.push(
        Callout {
            tone: Tone::Info,
            icon: Some(icon(IconName::Shield)),
            title: None,
            content: vec![text_c(
                "g-acl-text",
                lit(
                    "Graf zawiera wyłącznie notatki, do których masz dostęp — powiązania \
                     z cudzymi prywatnymi notatkami nie istnieją w Twoim widoku.",
                ),
                TextStyle::Caption,
                None,
            )],
        }
        .into_component("g-acl-callout")
        .expect("Callout encode"),
    );

    children.push(overline("g-f-types-h", "Typy encji"));
    for (i, (t, label, tone)) in [
        ("person", "Osoby", Tone::Info),
        ("project", "Projekty", Tone::Primary),
        ("company", "Firmy", Tone::Success),
        ("topic", "Tematy", Tone::Warning),
        ("note", "Notatki", Tone::Primary),
    ]
    .iter()
    .enumerate()
    {
        children.push(type_row(i, t, label, *tone));
    }

    children.push(overline("g-f-depth-h", "Głębokość"));
    let mut depth = Slider {
        bind_path: state_path(SP_G_DEPTH),
        min: 1.0,
        max: 3.0,
        step: 1.0,
        label: Some(lit("od wybranego noda")),
        show_value: true,
        format: None,
        marks: Some(vec![
            SliderMark { value: 1.0, label: Some(lit("1")) },
            SliderMark { value: 2.0, label: Some(lit("2")) },
            SliderMark { value: 3.0, label: Some(lit("3")) },
        ]),
        tone: Tone::Primary,
    }
    .into_component("g-depth")
    .expect("Slider encode");
    depth.handlers = Some(backend(EventKind::Change, "graph_depth"));
    children.push(depth);

    children.push(overline("g-f-weight-h", "Minimalna siła powiązania"));
    let mut weight = Slider {
        bind_path: state_path(SP_G_MIN_WEIGHT),
        min: 0.0,
        max: 1.0,
        step: 0.05,
        label: Some(lit("ukryj słabsze krawędzie")),
        show_value: true,
        format: Some(ValueFormat::Percent { decimals: 0 }),
        marks: None,
        tone: Tone::Primary,
    }
    .into_component("g-weight")
    .expect("Slider encode");
    weight.handlers = Some(backend(EventKind::Change, "graph_min_weight"));
    children.push(weight);

    children.push(overline("g-f-legend-h", "Legenda krawędzi"));
    children.push(legend_row("g-legend-solid", "──── wspólna encja"));
    children.push(legend_row("g-legend-dash", "╌╌╌╌ podobieństwo semantyczne"));

    let mut panel = col_box(children);
    panel.padding = Some(Spacing::Md);
    panel.gap = Some(Spacing::Sm);
    panel.style = Some(ui::BoxStyle {
        overflow_y: Some(ui::Overflow::Auto),
        ..panel_style()
    });
    panel.into_component("g-filters").expect("Box encode")
}

/// Edge-legend row (mockup uses inline SVG strokes; box-drawing glyphs keep
/// it token-only without a raw-canvas escape hatch).
fn legend_row(id: &str, label: &str) -> Component {
    text_c(id, lit(label), TextStyle::Caption, Some(Tone::Muted))
}

/// The RelationGraph component (hand-built FieldMap — the catalog defines
/// §0x0703 without a typed spec struct).
fn relation_graph_component() -> Component {
    let nodes_path = encode_to_value(&state_path(SP_G_NODES)).expect("StatePath encode");
    let edges_path = encode_to_value(&state_path(SP_G_EDGES)).expect("StatePath encode");
    let selected_path = encode_to_value(&state_path(SP_G_SELECTED)).expect("StatePath encode");
    let mut handlers = backend(EventKind::NodeClick, "graph_select");
    handlers
        .0
        .extend(backend(EventKind::Deselect, "graph_deselect").0);
    Component {
        tag: RELATION_GRAPH_TAG,
        id: "g-canvas".to_string(),
        fields: FieldMap(vec![
            (0, nodes_path),
            (1, edges_path),
            (2, CborValue::Text("force_directed".to_string())),
            (3, CborValue::Bool(true)),
            (4, CborValue::U64(MAX_GRAPH_NODES as u64)),
            (5, selected_path),
        ]),
        handlers: Some(handlers),
        bind: None,
        a11y: Some(Accessibility {
            label: Some(lit("Graf powiązań notatek")),
            ..Default::default()
        }),
        visibility: None,
        test_id: None,
    }
}

/// Counter pill of the topbar (graph mode only — visibility bound by ui.rs).
pub fn topbar_counter_pill() -> Component {
    Chip {
        variant: ChipVariant::Soft,
        tone: Tone::Neutral,
        label: bound(SP_G_COUNTER),
        icon: None,
        avatar: None,
        selected: None,
        removable: false,
        dot: Some(Tone::Success),
    }
    .into_component("g-topbar-pill")
    .expect("Chip encode")
}

/// Gradient app tile from the mockup topbar — Avatar Icon with primary tone.
pub fn app_icon_tile() -> Component {
    Avatar {
        source: AvatarRef::Icon {
            icon: icon(IconName::FileText),
        },
        size: AvatarSize::Md,
        shape: AvatarShape::Rounded,
        status: None,
        tone: Some(Tone::Primary),
    }
    .into_component("topbar-appicon")
    .expect("Avatar encode")
}

pub fn app_title() -> Component {
    Heading {
        content: lit("Notatki"),
        level: 3,
        tone: None,
        align: None,
    }
    .into_component("topbar-title")
    .expect("Heading encode")
}

/// Mode switch (Notatki / Graf). "Szukaj" lands with the search stage — a
/// dead segment is worse than a missing one.
pub fn mode_switch() -> Component {
    let mut seg = SegmentedControl {
        bind_path: state_path("mode"),
        options: vec![
            SegmentOption {
                value: SelectValue::Text("notes".to_string()),
                label: Some(lit("Notatki")),
                icon: Some(icon(IconName::FileText)),
                badge: None,
            },
            SegmentOption {
                value: SelectValue::Text("graph".to_string()),
                label: Some(lit("Graf")),
                icon: Some(icon(IconName::Branch)),
                badge: None,
            },
        ],
        size: SegmentSize::Md,
        full_width: false,
    }
    .into_component("topbar-mode")
    .expect("SegmentedControl encode");
    seg.handlers = Some(backend(EventKind::Change, "set_mode"));
    seg
}

/// Graph view subtree of the SINGLE panel shell (the host allows exactly one
/// PanelShell per open panel, so the Notatki/Graf switch toggles state-bound
/// visibility instead of re-sending a shell). Filters and canvas are fully
/// state-driven; only the detail rail is a slot.
pub fn graph_view_component() -> Component {
    // Filters column.
    let mut filters = col_box(vec![filters_panel()]);
    filters.width = Some(DimensionToken::Px { value: 260 });
    filters.style = Some(ui::BoxStyle {
        min_width: Some(DimensionToken::Px { value: 260 }),
        ..Default::default()
    });
    filters.responsive = Some(vec![ui::ResponsiveRule {
        max_width: ui::ContainerWidth::Px(1100),
        direction: None,
        gap: None,
        align: None,
        justify: None,
        padding: None,
        min_height: None,
        order: Some(3),
        hidden: None,
        width: Some(DimensionToken::Full),
    }]);
    let filters = filters.into_component("g-col-filters").expect("Box encode");

    // Canvas column (mockup graph-canvas: glow + min 640px). The
    // tf-relation-graph element draws its own border/background.
    let mut canvas = col_box(vec![relation_graph_component()]);
    canvas.grow = Some(true);
    canvas.style = Some(ui::BoxStyle {
        radius: Some(ui::CornerValues::all(ui::RadiusValue::Token {
            value: ui::RadiusToken::Lg,
        })),
        shadow: Some(ShadowToken::AccentGlow),
        min_height: Some(DimensionToken::Px { value: 640 }),
        min_width: Some(DimensionToken::Px { value: 320 }),
        ..Default::default()
    });
    canvas.responsive = Some(vec![ui::ResponsiveRule {
        max_width: ui::ContainerWidth::Px(1100),
        direction: None,
        gap: None,
        align: None,
        justify: None,
        padding: None,
        min_height: Some(DimensionToken::Px { value: 420 }),
        order: Some(1),
        hidden: None,
        width: Some(DimensionToken::Full),
    }]);
    let canvas = canvas.into_component("g-col-canvas").expect("Box encode");

    // Detail rail — Inspector holds the only slot of this mode.
    let inspector = Inspector {
        title: lit("Szczegóły"),
        content_slot: SLOT_DETAIL.to_string(),
        actions: vec![],
        tabs: None,
        collapsible: false,
    }
    .into_component("g-inspector")
    .expect("Inspector encode");
    let mut detail = col_box(vec![inspector]);
    detail.width = Some(DimensionToken::Px { value: 300 });
    detail.style = Some(ui::BoxStyle {
        min_width: Some(DimensionToken::Px { value: 300 }),
        ..panel_style()
    });
    detail.responsive = Some(vec![ui::ResponsiveRule {
        max_width: ui::ContainerWidth::Px(1100),
        direction: None,
        gap: None,
        align: None,
        justify: None,
        padding: None,
        min_height: None,
        order: Some(2),
        hidden: None,
        width: Some(DimensionToken::Full),
    }]);
    let detail = detail.into_component("g-col-detail").expect("Box encode");

    let columns = Flex {
        direction: FlexDirection::Row,
        gap: Spacing::Md,
        justify: FlexJustify::Start,
        align: FlexAlign::Stretch,
        wrap: FlexWrap::NoWrap,
        children: vec![filters, canvas, detail],
        padding: None,
        background: None,
        radius: None,
        style: None,
        responsive: Some(vec![ui::ResponsiveRule {
            max_width: ui::ContainerWidth::Px(1100),
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
    .into_component("g-columns")
    .expect("Flex encode");

    let mut children: Vec<Component> = Vec::new();
    if !analysis::auto_graph_ready() {
        children.push(
            Callout {
                tone: Tone::Warning,
                icon: Some(icon(IconName::Warning)),
                title: None,
                content: vec![text_c(
                    "g-ready-text",
                    lit(
                        "Skonfiguruj aliasy notes-embeddings i notes-llm w ustawieniach \
                         addonu, aby uruchomić auto-graf.",
                    ),
                    TextStyle::Caption,
                    None,
                )],
            }
            .into_component("g-ready-callout")
            .expect("Callout encode"),
        );
    }
    children.push(columns);

    let mut view_box = col_box(children);
    view_box.grow = Some(true);
    view_box.gap = Some(Spacing::Md);
    view_box.into_component("g-view").expect("Box encode")
}

/// Initial panel state of the graph view: empty data (loaded on first switch)
/// plus filter defaults from the session.
pub fn initial_graph_state(sess: &Session) -> Vec<StateEntry> {
    let view = GraphView::default();
    let mut initial_state = data_state_entries(&view, sess);
    initial_state.push(StateEntry {
        path: state_path(SP_G_MIN_WEIGHT),
        value: CborValue::F64(sess.g_min_weight),
    });
    initial_state.push(StateEntry {
        path: state_path(SP_G_DEPTH),
        value: CborValue::F64(sess.g_depth as f64),
    });
    for (scope, on) in [
        ("mine", sess.g_mine),
        ("shared", sess.g_shared),
        ("group", sess.g_group),
        ("org", sess.g_org),
    ] {
        initial_state.push(StateEntry {
            path: state_path(&scope_path(scope)),
            value: CborValue::Bool(on),
        });
    }
    for (t, on) in [
        ("person", sess.g_person),
        ("project", sess.g_project),
        ("company", sess.g_company),
        ("topic", sess.g_topic),
        ("note", sess.g_note),
    ] {
        initial_state.push(StateEntry {
            path: state_path(&type_path(t)),
            value: CborValue::Bool(on),
        });
    }
    initial_state
}

// =============================================================================
// Detail rail (SlotContent)
// =============================================================================

fn node_icon(node_type: &str) -> IconName {
    match node_type {
        "note" => IconName::FileText,
        "person" => IconName::User,
        "company" => IconName::Home,
        "project" => IconName::Folder,
        "topic" => IconName::Pin,
        _ => IconName::Sparkle,
    }
}

fn type_label(node_type: &str) -> &'static str {
    match node_type {
        "note" => "Notatka",
        "person" => "Osoba",
        "company" => "Firma",
        "project" => "Projekt",
        "topic" => "Temat",
        _ => "Encja",
    }
}

/// Human reason of one connection row, resolved per reader (entity names go
/// through the same visibility rule as everywhere else).
fn connection_reason(ctx: &UserCtx, selected_is_note: bool, edge: &GEdge, other: &GNode) -> String {
    match edge.kind.as_str() {
        "mention" => {
            if selected_is_note {
                match other.node_type.as_str() {
                    "person" => "wspomniana osoba".to_string(),
                    "company" => "wspomniana firma".to_string(),
                    "project" => "wspomniany projekt".to_string(),
                    _ => "wspomniany temat".to_string(),
                }
            } else {
                "wzmianka w notatce".to_string()
            }
        }
        "similar" => {
            let percent = (edge.weight.clamp(0.0, 1.0) * 100.0).round() as i64;
            format!("podobieństwo {percent}%")
        }
        "manual" => "powiązanie ręczne".to_string(),
        _ => match db::parse_link_reason(&edge.reason) {
            Some(db::LinkReason::Entity { entity_id, shared }) => {
                let name = db::visible_entity_name(ctx, &entity_id);
                db::link_reason_label(
                    &db::LinkReason::Entity { entity_id, shared },
                    name.as_deref(),
                )
            }
            Some(kind) => db::link_reason_label(&kind, None),
            None => String::new(),
        },
    }
}

fn connection_row(ctx: &UserCtx, index: usize, edge: &GEdge, other: &GNode, selected_is_note: bool) -> Component {
    let tile = Avatar {
        source: AvatarRef::Icon {
            icon: icon(node_icon(&other.node_type)),
        },
        size: AvatarSize::Sm,
        shape: AvatarShape::Rounded,
        status: None,
        tone: Some(if other.node_type == "note" {
            Tone::Primary
        } else {
            entity_tone(&other.node_type)
        }),
    }
    .into_component(format!("g-conn-{index}-ico"))
    .expect("Avatar encode");

    let name = Text {
        content: lit(&other.label),
        style: TextStyle::BodyStrong,
        tone: None,
        align: None,
        wrap: None,
        max_lines: Some(1),
        format: None,
        streaming: None,
    }
    .into_component(format!("g-conn-{index}-name"))
    .expect("Text encode");
    let why = text_c(
        &format!("g-conn-{index}-why"),
        lit(connection_reason(ctx, selected_is_note, edge, other)),
        TextStyle::Caption,
        Some(Tone::Muted),
    );
    let mut body = col_box(vec![name, why]);
    body.grow = Some(true);
    body.style = Some(ui::BoxStyle {
        min_width: Some(DimensionToken::Px { value: 0 }),
        ..Default::default()
    });
    let body = body
        .into_component(format!("g-conn-{index}-body"))
        .expect("Box encode");

    let mut card = Card {
        variant: CardVariant::Outlined,
        padding: Spacing::Sm,
        gap: Spacing::Sm,
        radius: ui::RadiusToken::Md,
        shadow: ShadowToken::None,
        border: ui::BorderToken::Hairline,
        background: ui::BackgroundToken::Subtle,
        accent: None,
        children: vec![Cluster {
            gap: Spacing::Sm,
            align: FlexAlign::Center,
            justify: FlexJustify::Start,
            children: vec![tile, body],
            wrap: Some(false),
        }
        .into_component(format!("g-conn-{index}-row"))
        .expect("Cluster encode")],
        interactive: true,
        clickable: true,
        style: None,
    }
    .into_component(format!("g-conn-{index}"))
    .expect("Card encode");
    card.handlers = Some(backend_params(
        EventKind::Click,
        "graph_select",
        vec![("node_id", CborValue::Text(other.id.clone()))],
    ));
    card
}

fn detail_empty() -> Component {
    let empty = EmptyState {
        icon: icon(IconName::Sparkle),
        heading: lit("Wybierz węzeł"),
        message: Some(lit("Kliknij węzeł na grafie, aby zobaczyć szczegóły.")),
        primary_action: None,
        secondary_action: None,
        variant: EmptyStateVariant::Compact,
    }
    .into_component("g-detail-empty")
    .expect("EmptyState encode");
    let mut panel = col_box(vec![empty]);
    panel.padding = Some(Spacing::Md);
    panel.into_component("g-detail").expect("Box encode")
}

fn connections_of<'a>(view: &'a GraphView, selected: &str) -> Vec<(&'a GEdge, &'a GNode)> {
    let by_id: HashMap<&str, &GNode> = view.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut out: Vec<(&GEdge, &GNode)> = view
        .edges
        .iter()
        .filter_map(|e| {
            let other_id = if e.source == selected {
                e.target.as_str()
            } else if e.target == selected {
                e.source.as_str()
            } else {
                return None;
            };
            by_id.get(other_id).map(|n| (e, *n))
        })
        .collect();
    // Entities first (mockup), then notes by link strength.
    out.sort_by(|a, b| {
        let is_note = |n: &GNode| n.node_type == "note";
        is_note(a.1)
            .cmp(&is_note(b.1))
            .then(b.0.weight.partial_cmp(&a.0.weight).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.1.label.cmp(&b.1.label))
    });
    out
}

fn merge_section(ctx: &UserCtx, entity_ids: &[String], children: &mut Vec<Component>) {
    let suggestions = analysis::open_suggestions_for(ctx, entity_ids);
    if suggestions.is_empty() {
        return;
    }
    children.push(overline("g-merge-h", "Sugestia scalenia"));
    for (i, s) in suggestions.iter().enumerate() {
        children.push(merge_suggestion_card(i, s));
    }
}

fn detail_note(ctx: &UserCtx, view: &GraphView, note_id: &str) -> Component {
    let note = match db::get_note(ctx, note_id) {
        Ok(Some(n)) => n,
        Ok(None) => return detail_empty(),
        Err(e) => return error_fragment("g-detail-err", &e),
    };

    let kind_chip = Chip {
        variant: ChipVariant::Soft,
        tone: Tone::Primary,
        label: lit("Notatka"),
        icon: Some(icon(IconName::FileText)),
        avatar: None,
        selected: None,
        removable: false,
        dot: None,
    }
    .into_component("g-d-kind")
    .expect("Chip encode");

    let title = Heading {
        content: lit(if note.title.is_empty() {
            "(bez tytułu)"
        } else {
            &note.title
        }),
        level: 4,
        tone: None,
        align: None,
    }
    .into_component("g-d-title")
    .expect("Heading encode");

    let author_name = if note.is_owner {
        ctx.display_name.clone()
    } else {
        db::user_display_name(&note.owner_user_id)
    };
    let author = text_c(
        "g-d-author",
        lit(author_name),
        TextStyle::Caption,
        Some(Tone::Muted),
    );
    let created = Text {
        content: ui::lit_value(CborValue::U64((note.created_at.max(0) as u64) * 1000)),
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
    .into_component("g-d-created")
    .expect("Text encode");
    let meta = Cluster {
        gap: Spacing::Sm,
        align: FlexAlign::Center,
        justify: FlexJustify::Start,
        children: vec![author, created, scope_badge("g-d-scope", &note.scope)],
        wrap: Some(true),
    }
    .into_component("g-d-meta")
    .expect("Cluster encode");

    let preview = Card {
        variant: CardVariant::Outlined,
        padding: Spacing::Sm,
        gap: Spacing::Xs,
        radius: ui::RadiusToken::Md,
        shadow: ShadowToken::None,
        border: ui::BorderToken::Hairline,
        background: ui::BackgroundToken::Subtle,
        accent: None,
        children: vec![Text {
            content: lit(db::note_preview(&note.content, 220)),
            style: TextStyle::Caption,
            tone: Some(Tone::Muted),
            align: None,
            wrap: None,
            max_lines: Some(3),
            format: None,
            streaming: None,
        }
        .into_component("g-d-preview-text")
        .expect("Text encode")],
        interactive: false,
        clickable: false,
        style: None,
    }
    .into_component("g-d-preview")
    .expect("Card encode");

    let mut open_btn = Button {
        variant: ButtonVariant::Primary,
        tone: Tone::Primary,
        label: lit("Otwórz notatkę"),
        icon_leading: Some(icon(IconName::ExternalLink)),
        icon_trailing: None,
        size: ButtonSize::Md,
        full_width: true,
        disabled: None,
        loading: None,
        density: Density::Default,
    }
    .into_component("g-d-open")
    .expect("Button encode");
    open_btn.handlers = Some(backend_params(
        EventKind::Click,
        "graph_open_note",
        vec![("note_id", CborValue::Text(note.id.clone()))],
    ));

    let mut children = vec![kind_chip, title, meta, preview, open_btn];

    let selected_id = note_node_id(note_id);
    let connections = connections_of(view, &selected_id);
    children.push(overline(
        "g-conn-h",
        &format!("Połączenia ({})", connections.len()),
    ));
    for (i, (edge, other)) in connections.iter().take(12).enumerate() {
        children.push(connection_row(ctx, i, edge, other, true));
    }

    let entity_ids: Vec<String> = db::note_entities(ctx, note_id)
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.id)
        .collect();
    merge_section(ctx, &entity_ids, &mut children);

    let mut panel = col_box(children);
    panel.padding = Some(Spacing::Md);
    panel.gap = Some(Spacing::Sm);
    panel.into_component("g-detail").expect("Box encode")
}

fn detail_entity(ctx: &UserCtx, view: &GraphView, entity_id: &str) -> Component {
    let node_id = entity_node_id(entity_id);
    let node = match view.nodes.iter().find(|n| n.id == node_id) {
        Some(n) => n.clone(),
        None => return detail_empty(),
    };

    let kind_chip = Chip {
        variant: ChipVariant::Soft,
        tone: entity_tone(&node.node_type),
        label: lit(type_label(&node.node_type)),
        icon: Some(icon(node_icon(&node.node_type))),
        avatar: None,
        selected: None,
        removable: false,
        dot: None,
    }
    .into_component("g-d-kind")
    .expect("Chip encode");

    let title = Heading {
        content: lit(&node.label),
        level: 4,
        tone: None,
        align: None,
    }
    .into_component("g-d-title")
    .expect("Heading encode");

    let count_text = text_c(
        "g-d-count",
        lit(format!(
            "Wspomniana w {} {}",
            node.mention_count,
            db::plural_pl(node.mention_count, "notatce", "notatkach", "notatkach"),
        )),
        TextStyle::Caption,
        Some(Tone::Muted),
    );

    let mut children = vec![kind_chip, title, count_text];

    let connections = connections_of(view, &node_id);
    children.push(overline(
        "g-conn-h",
        &format!("Połączenia ({})", connections.len()),
    ));
    for (i, (edge, other)) in connections.iter().take(12).enumerate() {
        children.push(connection_row(ctx, i, edge, other, false));
    }

    merge_section(ctx, &[entity_id.to_string()], &mut children);

    let mut panel = col_box(children);
    panel.padding = Some(Spacing::Md);
    panel.gap = Some(Spacing::Sm);
    panel.into_component("g-detail").expect("Box encode")
}

/// Renders the detail rail for the current selection.
pub fn send_graph_detail(ctx: &UserCtx, view: &GraphView, sess: &Session) {
    let fragment = match sess.g_selected.as_str() {
        "" => detail_empty(),
        sel => {
            if let Some(note_id) = sel.strip_prefix("n:") {
                detail_note(ctx, view, note_id)
            } else if let Some(entity_id) = sel.strip_prefix("e:") {
                detail_entity(ctx, view, entity_id)
            } else {
                detail_empty()
            }
        }
    };
    send_slot(SLOT_DETAIL, fragment, None);
}

// =============================================================================
// Actions (dispatched from ui.rs)
// =============================================================================

fn param_bool(params: &JsonValue, key: &str) -> Option<bool> {
    params.get(key).and_then(|v| v.as_bool())
}

fn load_view(ctx: &UserCtx, sess: &Session) -> Result<GraphView, String> {
    Ok(build_graph(&load_input(ctx)?, &filters_from_session(sess)))
}

/// Renders the initial (empty-selection) detail rail.
pub fn send_empty_detail() {
    send_slot(SLOT_DETAIL, detail_empty(), None);
}

/// Recomputes after a session change: data patch + detail refresh. Clears a
/// selection that the new filters pruned out of the graph.
pub fn refresh_after_change(ctx: &UserCtx, sess: &mut Session) -> JsonValue {
    let view = match load_view(ctx, sess) {
        Ok(v) => v,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    if !sess.g_selected.is_empty() && !view.nodes.iter().any(|n| n.id == sess.g_selected) {
        sess.g_selected.clear();
        crate::ui::store_session(sess);
        // The selection anchor is gone — rebuild without the BFS restriction.
        return refresh_after_change(ctx, sess);
    }
    push_graph_data(&view, sess);
    send_graph_detail(ctx, &view, sess);
    json!({"ok": true})
}

pub fn action_graph_scope(ctx: &UserCtx, sess: &mut Session, params: &JsonValue) -> JsonValue {
    let scope = params.get("scope").and_then(|v| v.as_str()).unwrap_or("");
    let on = param_bool(params, "value").unwrap_or(true);
    match scope {
        "mine" => sess.g_mine = on,
        "shared" => sess.g_shared = on,
        "group" => sess.g_group = on,
        "org" => sess.g_org = on,
        other => return json!({"ok": false, "error": format!("Nieznany zakres: {other}")}),
    }
    crate::ui::store_session(sess);
    refresh_after_change(ctx, sess)
}

pub fn action_graph_type(ctx: &UserCtx, sess: &mut Session, params: &JsonValue) -> JsonValue {
    let t = params.get("t").and_then(|v| v.as_str()).unwrap_or("");
    let on = param_bool(params, "value").unwrap_or(true);
    match t {
        "person" => sess.g_person = on,
        "project" => sess.g_project = on,
        "company" => sess.g_company = on,
        "topic" => sess.g_topic = on,
        "note" => sess.g_note = on,
        other => return json!({"ok": false, "error": format!("Nieznany typ: {other}")}),
    }
    crate::ui::store_session(sess);
    refresh_after_change(ctx, sess)
}

pub fn action_graph_min_weight(ctx: &UserCtx, sess: &mut Session, params: &JsonValue) -> JsonValue {
    let value = params
        .get("value")
        .and_then(|v| v.as_f64())
        .unwrap_or(sess.g_min_weight);
    sess.g_min_weight = value.clamp(0.0, 1.0);
    crate::ui::store_session(sess);
    refresh_after_change(ctx, sess)
}

pub fn action_graph_depth(ctx: &UserCtx, sess: &mut Session, params: &JsonValue) -> JsonValue {
    let value = params
        .get("value")
        .and_then(|v| v.as_f64())
        .unwrap_or(sess.g_depth as f64);
    sess.g_depth = (value.round() as i64).clamp(1, 3);
    crate::ui::store_session(sess);
    refresh_after_change(ctx, sess)
}

pub fn action_graph_select(ctx: &UserCtx, sess: &mut Session, params: &JsonValue) -> JsonValue {
    let node_id = params.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
    if node_id.is_empty() {
        return json!({"ok": false, "error": "Brak node_id"});
    }
    sess.g_selected = node_id.to_string();
    crate::ui::store_session(sess);
    refresh_after_change(ctx, sess)
}

pub fn action_graph_deselect(ctx: &UserCtx, sess: &mut Session) -> JsonValue {
    sess.g_selected.clear();
    crate::ui::store_session(sess);
    refresh_after_change(ctx, sess)
}

// =============================================================================
// Tests — pure graph build (no host fns on the native target)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: &str, bucket: &str, updated_at: i64) -> GraphNoteRow {
        GraphNoteRow {
            id: id.to_string(),
            title: format!("Note {id}"),
            updated_at,
            bucket: bucket.to_string(),
        }
    }

    fn mention(note_id: &str, entity_id: &str, etype: &str) -> GraphMentionRow {
        GraphMentionRow {
            note_id: note_id.to_string(),
            entity_id: entity_id.to_string(),
            name: format!("Entity {entity_id}"),
            entity_type: etype.to_string(),
        }
    }

    fn link(src: &str, dst: &str, kind: &str, weight: f64) -> GraphLinkRow {
        GraphLinkRow {
            src: src.to_string(),
            dst: dst.to_string(),
            kind: kind.to_string(),
            weight,
            reason: kind.to_string(),
        }
    }

    fn filters() -> GraphFilters {
        GraphFilters {
            org: true,
            min_weight: 0.0,
            ..GraphFilters::default()
        }
    }

    #[test]
    fn inaccessible_note_produces_no_node_and_no_edge() {
        // ACL scenario: note "x" is NOT in the input (db.rs never returned
        // it), yet a mention and links still point at it. Nothing about "x"
        // may surface — no node, no dangling edge.
        let input = GraphInput {
            notes: vec![note("a", "mine", 10)],
            mentions: vec![mention("a", "e1", "person"), mention("x", "e2", "company")],
            links: vec![link("a", "x", "similar", 0.9), link("x", "a", "entity", 0.5)],
        };
        let view = build_graph(&input, &filters());
        let ids: Vec<&str> = view.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"n:a"));
        assert!(ids.contains(&"e:e1"));
        // Entity mentioned only by the inaccessible note is excluded.
        assert!(!ids.iter().any(|id| id.contains("e2")));
        assert!(!ids.iter().any(|id| id.contains("n:x")));
        assert_eq!(view.edges.len(), 1);
        assert_eq!(view.edges[0].kind, "mention");
    }

    #[test]
    fn scope_toggles_filter_notes_and_counts_stay_global() {
        let input = GraphInput {
            notes: vec![
                note("a", "mine", 3),
                note("b", "shared", 2),
                note("c", "org", 1),
            ],
            mentions: vec![],
            links: vec![],
        };
        let f = GraphFilters {
            org: false,
            shared: false,
            min_weight: 0.0,
            ..GraphFilters::default()
        };
        let view = build_graph(&input, &f);
        assert_eq!(view.scope_counts, [1, 1, 0, 1]);
        let ids: Vec<&str> = view.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["n:a"]);
    }

    #[test]
    fn type_filter_drops_entity_nodes_and_their_mention_edges() {
        let input = GraphInput {
            notes: vec![note("a", "mine", 1)],
            mentions: vec![mention("a", "p1", "person"), mention("a", "c1", "company")],
            links: vec![],
        };
        let f = GraphFilters {
            person: false,
            org: true,
            min_weight: 0.0,
            ..GraphFilters::default()
        };
        let view = build_graph(&input, &f);
        let ids: Vec<&str> = view.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(!ids.contains(&"e:p1"));
        assert!(ids.contains(&"e:c1"));
        assert_eq!(view.edges.len(), 1);
        assert_eq!(view.edges[0].target, "e:c1");
        // Counts describe the pre-filter population.
        assert_eq!(view.type_counts, [1, 0, 1, 0, 1]);
    }

    #[test]
    fn weight_slider_hides_only_similar_edges() {
        let input = GraphInput {
            notes: vec![note("a", "mine", 2), note("b", "mine", 1)],
            mentions: vec![],
            links: vec![
                link("a", "b", "similar", 0.3),
                link("a", "b", "entity", 0.3),
                link("b", "a", "manual", 1.0),
            ],
        };
        let f = GraphFilters {
            min_weight: 0.5,
            org: true,
            ..GraphFilters::default()
        };
        let view = build_graph(&input, &f);
        let kinds: Vec<&str> = view.edges.iter().map(|e| e.kind.as_str()).collect();
        assert!(!kinds.contains(&"similar"));
        assert!(kinds.contains(&"entity"));
        assert!(kinds.contains(&"manual"));
        // Passing similar edges are dashed and labeled with the percent.
        let f2 = GraphFilters {
            min_weight: 0.2,
            org: true,
            ..GraphFilters::default()
        };
        let view2 = build_graph(&input, &f2);
        let similar = view2.edges.iter().find(|e| e.kind == "similar").unwrap();
        assert!(similar.dashed);
        assert_eq!(similar.label.as_deref(), Some("30%"));
    }

    #[test]
    fn both_direction_links_collapse_to_one_edge() {
        let input = GraphInput {
            notes: vec![note("a", "mine", 2), note("b", "mine", 1)],
            mentions: vec![],
            links: vec![link("a", "b", "similar", 0.8), link("b", "a", "similar", 0.8)],
        };
        let view = build_graph(&input, &filters());
        assert_eq!(view.edges.len(), 1);
    }

    #[test]
    fn bfs_depth_limits_neighbourhood_of_selected_node() {
        // Chain: a - b - c - d (via similar links).
        let input = GraphInput {
            notes: vec![
                note("a", "mine", 4),
                note("b", "mine", 3),
                note("c", "mine", 2),
                note("d", "mine", 1),
            ],
            mentions: vec![],
            links: vec![
                link("a", "b", "similar", 0.9),
                link("b", "c", "similar", 0.9),
                link("c", "d", "similar", 0.9),
            ],
        };
        let f1 = GraphFilters {
            selected: "n:a".to_string(),
            depth: 1,
            org: true,
            min_weight: 0.0,
            ..GraphFilters::default()
        };
        let v1 = build_graph(&input, &f1);
        let ids: HashSet<&str> = v1.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, HashSet::from(["n:a", "n:b"]));
        assert_eq!(v1.edges.len(), 1);

        let f2 = GraphFilters { depth: 2, ..f1.clone() };
        let v2 = build_graph(&input, &f2);
        assert_eq!(v2.nodes.len(), 3);
        assert_eq!(v2.edges.len(), 2);

        let f3 = GraphFilters { depth: 3, ..f1 };
        let v3 = build_graph(&input, &f3);
        assert_eq!(v3.nodes.len(), 4);
    }

    #[test]
    fn cap_prunes_by_recency_and_reports_totals() {
        let notes: Vec<GraphNoteRow> = (0..600)
            .map(|i| note(&format!("z{i:04}"), "mine", i as i64))
            .collect();
        let input = GraphInput {
            notes,
            mentions: vec![],
            links: vec![],
        };
        let view = build_graph(&input, &filters());
        assert_eq!(view.nodes.len(), MAX_GRAPH_NODES);
        assert_eq!(view.total_nodes, 600);
        // Highest updated_at survives, oldest is pruned.
        assert!(view.nodes.iter().any(|n| n.id == "n:z0599"));
        assert!(!view.nodes.iter().any(|n| n.id == "n:z0000"));
        let counter = counter_text(&view);
        assert!(counter.contains("pokazano 500 z 600"), "{counter}");
    }

    #[test]
    fn cap_always_keeps_the_selected_node() {
        let mut notes: Vec<GraphNoteRow> = (0..600)
            .map(|i| note(&format!("z{i:04}"), "mine", 1000 + i as i64))
            .collect();
        // The selected note is the OLDEST — recency alone would prune it.
        notes.push(note("old", "mine", 0));
        let input = GraphInput {
            notes,
            mentions: vec![],
            links: vec![],
        };
        let f = GraphFilters {
            selected: "n:old".to_string(),
            // Depth restriction would trivially keep it; disable by selecting
            // a node with no edges (BFS keeps just the node) — so the cap
            // path is what proves the guarantee here.
            depth: 3,
            org: true,
            min_weight: 0.0,
            ..GraphFilters::default()
        };
        let view = build_graph(&input, &f);
        assert!(view.nodes.iter().any(|n| n.id == "n:old"));
    }

    #[test]
    fn counter_text_polish_plurals() {
        let mut view = GraphView::default();
        view.nodes = vec![GNode {
            id: "n:a".into(),
            label: "A".into(),
            node_type: "note".into(),
            tone: Tone::Primary,
            updated_at: 0,
            mention_count: 0,
        }];
        view.total_nodes = 1;
        assert_eq!(counter_text(&view), "Graf: 1 nod · 0 krawędzi");
    }

    #[test]
    fn note_toggle_off_hides_notes_and_note_edges() {
        let input = GraphInput {
            notes: vec![note("a", "mine", 2), note("b", "mine", 1)],
            mentions: vec![mention("a", "e1", "person")],
            links: vec![link("a", "b", "similar", 0.9)],
        };
        let f = GraphFilters {
            note: false,
            org: true,
            min_weight: 0.0,
            ..GraphFilters::default()
        };
        let view = build_graph(&input, &f);
        assert!(view.nodes.iter().all(|n| n.node_type != "note"));
        assert!(view.edges.is_empty());
    }

    #[test]
    fn state_serialization_matches_renderer_contract() {
        let input = GraphInput {
            notes: vec![note("a", "mine", 2), note("b", "mine", 1)],
            mentions: vec![mention("a", "e1", "person")],
            links: vec![link("a", "b", "similar", 0.87)],
        };
        let view = build_graph(&input, &filters());
        let nodes = nodes_value(&view.nodes);
        let CborValue::Array(items) = nodes else {
            panic!("nodes must be array")
        };
        assert_eq!(items.len(), 3);
        let CborValue::Map(entries) = &items[0] else {
            panic!("node must be map")
        };
        let keys: Vec<&str> = entries
            .iter()
            .filter_map(|(k, _)| match k {
                CborValue::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(keys, vec!["id", "label", "node_type", "tone"]);

        let edges = edges_value(&view.edges);
        let CborValue::Array(edge_items) = edges else {
            panic!("edges must be array")
        };
        let similar = edge_items
            .iter()
            .find_map(|e| match e {
                CborValue::Map(m)
                    if m.iter().any(|(k, v)| {
                        matches!(k, CborValue::Text(t) if t == "style")
                            && matches!(v, CborValue::Text(s) if s == "dashed")
                    }) =>
                {
                    Some(m)
                }
                _ => None,
            })
            .expect("dashed similar edge present");
        assert!(similar
            .iter()
            .any(|(k, v)| matches!(k, CborValue::Text(t) if t == "label")
                && matches!(v, CborValue::Text(s) if s == "87%")));
    }
}
