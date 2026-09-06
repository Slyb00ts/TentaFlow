// =============================================================================
// File: services/catalog/provider.rs
// Versioned catalog snapshot + lock-free reader. Writers rebuild the entire
// snapshot from `MeshServicesRegistry` + DB (aliases, flows) and atomically
// publish via ArcSwap. Readers (HTTP /v1/models, binary catalog.list, GUI
// callbacks) take a single load_full() and walk the slice — no locks, no
// cloning entries.
// =============================================================================

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use arc_swap::ArcSwap;
use parking_lot::Mutex;

use crate::db::{repository, DbPool};
use crate::services::mesh_registry::MeshServicesRegistry;

use super::{
    CatalogDiagnostic, CatalogEntry, CatalogEntryKind, InputModality, ModelInstance,
    OutputModality, ServiceSurface, Strategy,
};

/// Immutable snapshot. `version` rises monotonically — readers can compare
/// versions to detect "new since last poll" without locking.
#[derive(Debug, Clone)]
pub struct CatalogSnapshot {
    pub entries: Arc<[CatalogEntry]>,
    pub version: u64,
}

impl CatalogSnapshot {
    fn empty() -> Self {
        Self {
            entries: Arc::from(Vec::<CatalogEntry>::new().into_boxed_slice()),
            version: 0,
        }
    }

    /// Entries safe to advertise to clients. Strips blocking diagnostics
    /// (RemoteShadowed / LocalOverride). Non-blocking diagnostics
    /// (IncompatibleAliasTargets) stay — the alias may still resolve fine
    /// for requests that match its primary target.
    pub fn advertised_entries(&self) -> impl Iterator<Item = &CatalogEntry> {
        self.entries.iter().filter(|e| match &e.diagnostic {
            Some(d) => !d.is_blocking(),
            None => true,
        })
    }
}

/// Thread-safe holder. `current` is swapped wholesale on every rebuild so
/// readers never observe a partially built snapshot. The `rebuild_lock`
/// serialises writers — concurrent `rebuild()` calls (e.g. supervisor tick
/// racing with an alias mutation) cannot publish snapshots out of freshness
/// order. Without that lock a slow rebuild started before a fast one could
/// still `store()` afterwards and overwrite newer data with a higher
/// version number, leaving readers temporarily stuck on stale entries.
pub struct CatalogProvider {
    current: ArcSwap<CatalogSnapshot>,
    /// Held only for the duration of `build_entries` + `store`. Readers do
    /// not touch this lock — they go straight through `current`.
    rebuild_lock: Mutex<()>,
}

impl Default for CatalogProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CatalogProvider {
    pub fn new() -> Self {
        Self {
            current: ArcSwap::from_pointee(CatalogSnapshot::empty()),
            rebuild_lock: Mutex::new(()),
        }
    }

    /// Lock-free load. Cheap (one Arc clone) — call it on every request.
    pub fn snapshot(&self) -> Arc<CatalogSnapshot> {
        self.current.load_full()
    }

    /// Replace the snapshot. The mutex makes rebuild publication strictly
    /// sequential: the snapshot we read inside the lock is exactly the one
    /// we publish, and the next rebuild always sees this publication
    /// before starting its own walk of registry+DB. Versions therefore
    /// match build order as well as publish order.
    pub fn rebuild(&self, registry: &MeshServicesRegistry, pool: &DbPool) -> Result<u64> {
        let _guard = self.rebuild_lock.lock();
        let entries = build_entries(registry, pool)?;
        let previous_version = self.current.load().version;
        let version = previous_version.saturating_add(1);
        let snapshot = CatalogSnapshot {
            entries: Arc::from(entries.into_boxed_slice()),
            version,
        };
        self.current.store(Arc::new(snapshot));
        Ok(version)
    }
}

// =============================================================================
// Snapshot construction.
// =============================================================================

/// Build the full set of catalog entries from the mesh registry plus the
/// local DB (flows, aliases). Order in the returned vec is service models
/// first, then published flows, then aliases — keeps `/v1/models` listings
/// predictable for clients that don't sort.
fn build_entries(registry: &MeshServicesRegistry, pool: &DbPool) -> Result<Vec<CatalogEntry>> {
    let mut entries = Vec::new();
    let mut taken_ids: HashSet<String> = HashSet::new();

    let service_entries = build_service_model_entries(registry, pool);
    for entry in service_entries {
        taken_ids.insert(entry.id.clone());
        entries.push(entry);
    }

    let flow_entries = build_flow_entries(pool, &taken_ids)?;
    for entry in flow_entries {
        taken_ids.insert(entry.id.clone());
        entries.push(entry);
    }

    // Aliases reference both service models and published flows by id;
    // a single index covers both kinds because they live side by side
    // in the entry slice we just built.
    let capabilities_index = build_capabilities_index(&entries);
    let (alias_entries, shadow_markers) =
        build_alias_entries(pool, &taken_ids, &capabilities_index)?;
    for entry in alias_entries {
        entries.push(entry);
    }

    // Stamp existing entries with the diagnostics produced by alias
    // collisions. We do this in a second pass so the diagnostic survives
    // even if the original entry has its own diagnostic already (we
    // overwrite — the collision warning is more actionable than the
    // pre-existing one, and only one diagnostic ships per entry).
    if !shadow_markers.is_empty() {
        for entry in entries.iter_mut() {
            if let Some(marker) = shadow_markers.get(&entry.id) {
                entry.diagnostic = Some(marker.clone());
            }
        }
    }

    Ok(entries)
}

#[derive(Debug, Clone, Default)]
struct EntryCapabilities {
    surfaces: Vec<ServiceSurface>,
    input: Vec<InputModality>,
    output: Vec<OutputModality>,
    reasoning_levels: Vec<String>,
}

fn build_capabilities_index(entries: &[CatalogEntry]) -> HashMap<String, EntryCapabilities> {
    let mut map = HashMap::with_capacity(entries.len());
    for entry in entries {
        map.insert(
            entry.id.clone(),
            EntryCapabilities {
                surfaces: entry.service_surfaces.clone(),
                input: entry.input_modalities.clone(),
                output: entry.output_modalities.clone(),
                reasoning_levels: entry.reasoning_levels.clone(),
            },
        );
    }
    map
}

// -----------------------------------------------------------------------------
// Service models — aggregated across mesh nodes.
// -----------------------------------------------------------------------------

/// Group every model advertised by the mesh registry into one entry per
/// model name. Local instances win deduplication ties (so a model present
/// both locally and on a peer reports its local node first).
///
/// Boot-order note: callers must invoke this only after the supervisor's
/// `run_first_tick` has populated `mesh_registry`. If called earlier the
/// `LocalNodeSnapshot::default()` returns an empty `node_id`, which makes
/// the local-vs-remote sort inverted (every service compares equal to the
/// blank id). `Router::start()` enforces this ordering by calling
/// `rebuild_catalog()` after `set_mesh_services_registry`.
fn build_service_model_entries(
    registry: &MeshServicesRegistry,
    pool: &DbPool,
) -> Vec<CatalogEntry> {
    let local = registry.local();
    let local_node_id = local.node_id.clone();
    let manifests = crate::services::manifest::registry();
    // Environment per node_id (ROADMAP Z12) — one batch query instead of one
    // per mesh service entry. The local node's own environment does not live
    // in `trusted_nodes` (it never trust-pairs with itself), so it is read
    // separately and takes priority over any stale row that might exist.
    let local_environment = crate::services::environment::get_node_environment(pool);
    let peer_environments = repository::list_trusted_node_environments(pool).unwrap_or_default();

    // (model_name) → instances grouped with their local/remote tag
    let mut by_name: HashMap<String, Vec<(bool, ModelInstance)>> = HashMap::new();
    // model_name → set of surfaces seen (HashSet so duplicates from many
    // peers serving the same `engine.category` collapse in O(n)).
    let mut surfaces_by_name: HashMap<String, HashSet<ServiceSurface>> = HashMap::new();
    // model_name → modality sets unioned across every service that exposes
    // the model. Same model id served by two engines that disagree on
    // modalities is rare but tolerated — the catalog reports the union so
    // routing won't reject a request that one of them can satisfy.
    let mut inputs_by_name: HashMap<String, HashSet<InputModality>> = HashMap::new();
    let mut outputs_by_name: HashMap<String, HashSet<OutputModality>> = HashMap::new();
    // Poziomy rozumowania sa wlasnoscia modelu, wiec zbieramy je tak samo jak
    // modalnosci — po nazwie modelu, przez wszystkie instancje w siatce.
    let mut reasoning_by_name: HashMap<String, HashSet<String>> = HashMap::new();

    for svc in registry.visible_services() {
        let is_local = svc.node_id == local_node_id;
        // Environment fence (ROADMAP Z12, P2-7 / N7 delta-review) — checked
        // per SERVICE (the environment is a node-level attribute, not a
        // per-model one) and BEFORE any capability aggregation. Filtering
        // only the `ModelInstance` further down while still folding a
        // cross-env service's surfaces/modalities/reasoning levels into the
        // entry-level union would advertise a capability that no VISIBLE
        // instance actually offers — skip the whole service up front instead.
        let service_environment = if is_local {
            Some(local_environment)
        } else {
            peer_environments.get(&svc.node_id).copied().flatten()
        };
        if service_environment != Some(local_environment) {
            continue;
        }
        let manifest = manifests.by_id(&svc.engine_id);
        for model in &svc.models {
            // Resolve this service's effective capabilities. Surfaces and
            // modalities all share the same preset > engine > category
            // fallback chain, so we compute them once per (svc, model)
            // pair and use them for both per-instance metadata and the
            // entry-level union below.
            let mut svc_surfaces: HashSet<ServiceSurface> = HashSet::new();
            let mut svc_inputs: HashSet<InputModality> = HashSet::new();
            let mut svc_outputs: HashSet<OutputModality> = HashSet::new();
            let mut svc_reasoning: HashSet<String> = HashSet::new();
            // Surfaces anonsowane przez właściciela (policzone z JEGO manifestu)
            // są autorytatywne — działają nawet gdy TEN node nie ma manifestu
            // silnika, a `category` (np. `vision`) nie mapuje się na ServiceSurface.
            for s in &model.service_surfaces {
                if let Some(v) = ServiceSurface::from_wire_str(s) {
                    svc_surfaces.insert(v);
                }
            }
            if let Some(m) = manifest {
                // Wiersz modelu moze nosic nazwe presetu (`p.id`, lokalne
                // silniki) albo realny identyfikator API providera (`p.repo`,
                // cloud external — patrz `models_from_manifest`). Dopasowanie
                // po obu konwencjach gwarantuje, ze preset-level overrides
                // dzialaja niezaleznie od tego, ktora nazwa trafila do
                // `model_registry`.
                let preset = m
                    .model_presets
                    .iter()
                    .find(|p| p.id == model.model_name || p.repo == model.model_name);
                for s in m.engine.effective_service_surfaces(preset) {
                    if let Some(v) = ServiceSurface::from_wire_str(&s) {
                        svc_surfaces.insert(v);
                    }
                }
                for s in m.engine.effective_input_modalities(preset) {
                    if let Some(v) = InputModality::from_wire_str(&s) {
                        svc_inputs.insert(v);
                    }
                }
                for s in m.engine.effective_output_modalities(preset) {
                    if let Some(v) = OutputModality::from_wire_str(&s) {
                        svc_outputs.insert(v);
                    }
                }
                for level in m.engine.effective_reasoning_levels(preset) {
                    svc_reasoning.insert(level);
                }
            } else {
                // Manifest nieznany na tym nodzie (np. serwis z mesh od noda
                // z innym zestawem silnikow) — surfaces i modalnosci spadaja
                // na domyslne wartosci kategorii zapisanej w `services.category`.
                if let Some(s) = ServiceSurface::from_manifest_category(&svc.category) {
                    svc_surfaces.insert(s);
                }
                if let Some(cat) =
                    crate::services::manifest::Category::from_category_tag(&svc.category)
                {
                    for s in cat.default_input_modalities() {
                        if let Some(v) = InputModality::from_wire_str(s) {
                            svc_inputs.insert(v);
                        }
                    }
                    for s in cat.default_output_modalities() {
                        if let Some(v) = OutputModality::from_wire_str(s) {
                            svc_outputs.insert(v);
                        }
                    }
                }
            }

            // Entry-level union — keeps `/v1/models` and the catalog
            // overview showing the full set across instances.
            surfaces_by_name
                .entry(model.model_name.clone())
                .or_default()
                .extend(svc_surfaces.iter().copied());
            inputs_by_name
                .entry(model.model_name.clone())
                .or_default()
                .extend(svc_inputs.iter().copied());
            reasoning_by_name
                .entry(model.model_name.clone())
                .or_default()
                .extend(svc_reasoning.iter().cloned());
            outputs_by_name
                .entry(model.model_name.clone())
                .or_default()
                .extend(svc_outputs.iter().copied());

            let mut instance_inputs: Vec<InputModality> = svc_inputs.into_iter().collect();
            instance_inputs.sort_by_key(|v| *v as u8);
            let mut instance_outputs: Vec<OutputModality> = svc_outputs.into_iter().collect();
            instance_outputs.sort_by_key(|v| *v as u8);
            // Environment fence (ROADMAP Z12, P2-7) already excluded the
            // whole service above (fail-closed on unknown per P1-2) — at
            // catalog CONSTRUCTION time, not merely annotated: the resolver,
            // `/v1/models` and alias entries all read instances off this same
            // catalog snapshot, so an instance that never enters `by_name`
            // can never be selectable anywhere downstream.
            let instance = ModelInstance {
                node_id: svc.node_id.clone(),
                // Display name carries the human-readable peer label (e.g.
                // "Piotr's MacBook"); keep an Option so an empty string
                // collapses to None rather than being rendered as a blank.
                node_hostname: Some(svc.display_name.clone()).filter(|s| !s.is_empty()),
                service_id: svc.id,
                status: svc.status.clone(),
                backend: Some(svc.engine_id.clone()),
                size_mb: None,
                loaded: matches!(svc.status.as_str(), "running" | "ready"),
                input_modalities: instance_inputs,
                output_modalities: instance_outputs,
                environment: local_environment,
            };
            by_name
                .entry(model.model_name.clone())
                .or_default()
                .push((is_local, instance));
        }
    }

    let mut out = Vec::with_capacity(by_name.len());
    for (model_name, mut rows) in by_name {
        // Local instances first — keeps local routing decisions deterministic.
        rows.sort_by(|(a_local, _), (b_local, _)| b_local.cmp(a_local));
        let instances: Vec<ModelInstance> = rows.into_iter().map(|(_, inst)| inst).collect();

        let mut surfaces: Vec<ServiceSurface> = surfaces_by_name
            .remove(&model_name)
            .unwrap_or_default()
            .into_iter()
            .collect();
        surfaces.sort_by_key(|s| *s as u8);

        let mut inputs: Vec<InputModality> = inputs_by_name
            .remove(&model_name)
            .unwrap_or_default()
            .into_iter()
            .collect();
        inputs.sort_by_key(|v| *v as u8);
        let mut outputs: Vec<OutputModality> = outputs_by_name
            .remove(&model_name)
            .unwrap_or_default()
            .into_iter()
            .collect();
        outputs.sort_by_key(|v| *v as u8);

        let model_name_for_reasoning = model_name.clone();
        out.push(CatalogEntry {
            id: model_name,
            kind: CatalogEntryKind::ServiceModel { instances },
            service_surfaces: surfaces,
            input_modalities: inputs,
            output_modalities: outputs,
            reasoning_levels: {
                let mut v: Vec<String> = reasoning_by_name
                    .remove(&model_name_for_reasoning)
                    .unwrap_or_default()
                    .into_iter()
                    .collect();
                v.sort();
                v
            },
            diagnostic: None,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

// -----------------------------------------------------------------------------
// Published flows.
// -----------------------------------------------------------------------------

/// Pull flows with `published_model_name IS NOT NULL` from the DB and turn
/// them into catalog entries. Skips names that already exist as service
/// models — those collisions are caught earlier by the publish guard
/// (R1.0h); here we just refuse to overwrite the service entry.
///
/// Modalities are inferred from the flow's graph (D.18). A flow with an
/// `stt` node on the trigger path accepts audio input; a `tts` node before
/// the output path emits audio; a vision-capable node accepts image input.
/// This makes alias inheritance (D.17) actually work for audio/image flows
/// — without the inference an alias targeting an audio chat flow would be
/// advertised as text-only and the resolver would reject audio requests.
fn build_flow_entries(pool: &DbPool, taken_ids: &HashSet<String>) -> Result<Vec<CatalogEntry>> {
    let flows = repository::list_flows(pool, 0, i64::MAX)?;
    let mut entries = Vec::new();
    for flow in flows {
        let Some(published_name) = flow.published_model_name.clone() else {
            continue;
        };
        if flow.status != "active" {
            continue;
        }
        if taken_ids.contains(&published_name) {
            continue;
        }

        let surfaces: Vec<ServiceSurface> = flow
            .service_type
            .as_deref()
            .and_then(ServiceSurface::from_flow_service_type)
            .into_iter()
            .collect();

        let (input_modalities, output_modalities) = infer_flow_modalities(&flow.flow_json);

        entries.push(CatalogEntry {
            // Flow nie jest modelem i nie ma wlasnych poziomow rozumowania —
            // decyduje o nich model wewnatrz jego wezlow llm.
            reasoning_levels: Vec::new(),
            id: published_name.clone(),
            kind: CatalogEntryKind::Flow {
                flow_id: flow.id,
                published_name,
            },
            service_surfaces: surfaces,
            input_modalities,
            output_modalities,
            diagnostic: None,
        });
    }
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(entries)
}

/// Node types that contribute media modalities to the inferred flow
/// signature. Listed explicitly so a `flow_node_templates` snapshot test
/// can detect any seeded type that this function does not handle (drift
/// would make the catalog advertise wrong modalities for new node kinds).
///
/// Used only by the `every_seeded_node_type_has_modality_decision` test
/// (declarative drift guard). `infer_flow_modalities` itself reads node
/// types directly via match arms.
#[allow(dead_code)]
pub(crate) const MODALITY_CONTRIBUTING_NODE_TYPES: &[&str] = &[
    "stt",
    "whisper",
    "transcription",
    "tts",
    "voice_output",
    "speech",
    "vision",
    "image_input",
    "image_gen",
    "image_generation",
    "embeddings",
    // Mostek chunk→store: wektoryzuje chunki (text input → embedding output).
    "embed_chunks",
    "llm",
    "chat",
    "memory",
    "conversation_history",
    // PARTIA 1 (flow-ingest RAG): rasteryzacja PDF emituje obrazy stron.
    "pdf_rasterize",
    // PARTIA 2 (flow-ingest RAG): węzły zależne od modeli biorą obraz strony.
    // vision_parse / table_structure / ocr produkują też tekst; page_detect /
    // graphic_elements zwracają JSON regionów (brak medialnego outputu, samo
    // Image-input constraint).
    "vision_parse",
    "page_detect",
    "table_structure",
    "graphic_elements",
    "ocr",
    // VLM page parsing (page image → structured markdown).
    "document_parse",
    // Project Studio knowledge search (text query → text with citations).
    "project_knowledge",
    // Knowledge-graph extraction: reads chunk TEXT through a chat model. It
    // emits no media of its own (the graph is a side effect and the payload
    // passes through), so it contributes the input constraint only.
    "graph_extract",
];

/// Node types that intentionally do not contribute modalities — they do
/// flow control, side-effects, or transform existing payloads in place.
/// Together with `MODALITY_CONTRIBUTING_NODE_TYPES` this list must cover
/// every type seeded into `flow_node_templates`; the snapshot test
/// `every_seeded_node_type_has_modality_decision` enforces it.
#[allow(dead_code)]
pub(crate) const MODALITY_PASSTHROUGH_NODE_TYPES: &[&str] = &[
    "trigger",
    // Reactive entry: same passthrough modality role as `trigger` — it forwards
    // the seeded child-result envelope and declares no media constraint.
    "on_subagent_complete",
    "output",
    "condition",
    "combine",
    "pii_filter",
    "tts_clean",
    "sentence_buffer",
    "session_context",
    "speaker_context",
    "persist_turn",
    // Harness control / sub-agent orchestration nodes (no media modality of
    // their own — they steer the graph or run other flows / agents).
    "agent",
    // Bramki potoku Code Harness: decyduja, CZY tura leci dalej, i przepuszczaja
    // envelope bez zmiany nosnika.
    "critic_gate",
    "task_gate",
    "agent_context",
    "agent_router",
    "ask_user",
    "await_subagents",
    "compact_context",
    "interval",
    "loop",
    "map",
    "spawn",
    "subagent_status",
    "subflow",
    "tool_exec",
    // PARTIA 1 (flow-ingest RAG): czysto-rustowe węzły ingestu bez własnej
    // modalności medialnej — klasyfikują/routują plik, ekstrahują tekst,
    // chunkują, scalają strony i zapisują wektory (transform/side-effect).
    "document_router",
    "text_extract",
    "excel_extract",
    "word_extract",
    "pptx_extract",
    "chunk",
    "document_merge",
    "store",
    // Batch-owe warianty gałęzi PDF: operują na liście stron jako JSON (nie
    // pojedynczy obraz), więc nie deklarują medialnej modalności wejścia —
    // ograniczenie Image jest już w `pdf_rasterize`/single-image węzłach.
    "vision_parse_pages",
    "page_detect_pages",
    "ocr_pages",
    // Per-platform switch: forwards the payload unchanged to the target_os port.
    "platform_switch",
    // Code Studio (§16.4): all four act on a repository worktree and a session,
    // never on a media payload — they add context, gate on a human decision,
    // run a command, or delegate a turn to an external CLI.
    "workspace_context",
    "patch_review",
    "exec_command",
    "delegate_cli",
];

/// Best-effort capability inference from a stored flow graph. Walks
/// `nodes[].type` looking for media-handling steps:
/// - `stt` / `whisper` / `transcription` → `Audio` on input
/// - `vision` / `image_input` → `Image` on input
/// - `tts` / `voice_output` / `speech` → `Audio` on output
/// - `image_gen` / `image_generation` → `Image` on output
/// - `embeddings` → `Embedding` on output
/// - `llm` / `chat` / `memory` / `conversation_history` → text I/O
/// Anything else (passthrough, control, transform) leaves modalities
/// empty so the resolver treats them as "no declared constraint" (D.17).
fn infer_flow_modalities(flow_json: &str) -> (Vec<InputModality>, Vec<OutputModality>) {
    let parsed: serde_json::Value = match serde_json::from_str(flow_json) {
        Ok(v) => v,
        Err(_) => return (Vec::new(), Vec::new()),
    };
    let Some(nodes) = parsed.get("nodes").and_then(|n| n.as_array()) else {
        return (Vec::new(), Vec::new());
    };

    let mut inputs: HashSet<InputModality> = HashSet::new();
    let mut outputs: HashSet<OutputModality> = HashSet::new();
    let mut has_text_output = false;

    for node in nodes {
        let Some(t) = node.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        match t {
            "stt" | "whisper" | "transcription" => {
                inputs.insert(InputModality::Audio);
                has_text_output = true;
            }
            "tts" | "voice_output" | "speech" => {
                outputs.insert(OutputModality::Audio);
            }
            "vision" | "image_input" => {
                inputs.insert(InputModality::Image);
                has_text_output = true;
            }
            "image_gen" | "image_generation" => {
                outputs.insert(OutputModality::Image);
            }
            // PARTIA 1 (flow-ingest RAG): pdf_rasterize bierze plik PDF i emituje
            // ALBO obrazy stron (skan/obraz → vision), ALBO gotową warstwę
            // tekstową (PDF z osadzonym tekstem → szybka ścieżka, pomija vision).
            // Wejście to plik (Other ≈ brak deklarowanej modalności medialnej),
            // wyjście to obraz lub tekst (zależnie od wybranej ścieżki).
            "pdf_rasterize" => {
                outputs.insert(OutputModality::Image);
                has_text_output = true;
            }
            // PARTIA 2 (flow-ingest RAG): biorą obraz strony. vision_parse /
            // table_structure / ocr emitują tekst; page_detect / graphic_elements
            // zwracają JSON regionów (brak medialnego outputu — deklarujemy tylko
            // ograniczenie wejścia Image).
            // document_parse: VLM page-image → structured markdown, same
            // shape as the other page-parsing nodes.
            "vision_parse" | "table_structure" | "ocr" | "document_parse" => {
                inputs.insert(InputModality::Image);
                has_text_output = true;
            }
            "page_detect" | "graphic_elements" => {
                inputs.insert(InputModality::Image);
            }
            // graph_extract reads chunk text with a chat model and writes a
            // graph; its output port carries the INPUT payload through, so it
            // adds a Text input constraint and no output modality.
            "graph_extract" => {
                inputs.insert(InputModality::Text);
            }
            "embeddings" => {
                // Codex R3b.1 round 2 M2: declare text input so
                // `execute_embeddings` (which requires `Text` input) can
                // resolve embedding flows. Without this the resolver
                // filters every embeddings flow out before dispatch.
                inputs.insert(InputModality::Text);
                outputs.insert(OutputModality::Embedding);
            }
            // Mostek chunk→store: bierze tekst chunków, emituje wektory.
            "embed_chunks" => {
                inputs.insert(InputModality::Text);
                outputs.insert(OutputModality::Embedding);
            }
            // project_knowledge: text query in, text passages out.
            "llm" | "chat" | "memory" | "conversation_history" | "project_knowledge" => {
                // Default chat-like output shape.
                has_text_output = true;
            }
            _ => {}
        }
    }

    if has_text_output {
        outputs.insert(OutputModality::Text);
        // Text input is implicit when a text-emitting node exists. Listing
        // it explicitly keeps the contract obvious for callers that
        // intersect modalities (resolver, alias diagnostic).
        inputs.insert(InputModality::Text);
    }

    let mut input_vec: Vec<InputModality> = inputs.into_iter().collect();
    input_vec.sort_by_key(|m| *m as u8);
    let mut output_vec: Vec<OutputModality> = outputs.into_iter().collect();
    output_vec.sort_by_key(|m| *m as u8);
    (input_vec, output_vec)
}

// -----------------------------------------------------------------------------
// Aliases.
// -----------------------------------------------------------------------------

/// Convert each active alias into an entry. Modalities follow the **primary
/// target** (D.17 — never the intersection of primary + fallbacks), so an
/// alias keeps advertising its primary's full surface even when one of its
/// fallbacks is simpler. A non-blocking diagnostic flags fallbacks that
/// would not satisfy the primary's modalities — the resolver filters them
/// per-request.
///
/// The `taken_ids` set carries names already claimed by service models or
/// published flows. When an alias collides with such a name we drop the
/// alias entry but keep the original — and **mark the original** with a
/// `RemoteShadowed` / `LocalOverride` diagnostic so an operator can see
/// that two owners want the same id (D.19). This matters because callers
/// reach `id` directly; without the diagnostic the GUI surfaces the entry
/// as if no collision happened.
fn build_alias_entries(
    pool: &DbPool,
    taken_ids: &HashSet<String>,
    capabilities_index: &HashMap<String, EntryCapabilities>,
) -> Result<(Vec<CatalogEntry>, HashMap<String, CatalogDiagnostic>)> {
    let aliases = repository::list_model_aliases(pool)?;
    let mut entries = Vec::with_capacity(aliases.len());
    let mut shadow_markers: HashMap<String, CatalogDiagnostic> = HashMap::new();
    for alias in aliases {
        if !alias.is_active {
            continue;
        }

        let target = alias.target_model.trim().to_string();
        if target.is_empty() {
            continue;
        }

        // Collision: a service model or published flow already claims this
        // id. Skip the alias (its id is unreachable anyway) and stamp the
        // existing entry with a `RemoteShadowed` diagnostic so the GUI can
        // warn the operator. We only have three diagnostic variants on the
        // wire (D.19); local-vs-local does not map cleanly to any, but
        // `RemoteShadowed` semantically says "this entry is hiding another
        // owner of the same id" — formatting the `local_owner` field with
        // the alias name and target keeps the wire output honest about
        // what was hidden.
        if taken_ids.contains(&alias.alias) {
            shadow_markers.entry(alias.alias.clone()).or_insert(
                CatalogDiagnostic::RemoteShadowed {
                    local_owner: format!("local alias '{}' targeting '{}'", alias.alias, target),
                },
            );
            continue;
        }

        let fallback_targets = parse_fallback_targets(alias.fallback_targets.as_deref());
        let strategy = Strategy::from_db(alias.strategy.as_deref());

        // Modalities + surfaces inherit from the primary target — looked up
        // in the unified index that already merged service models and
        // published flows.
        let primary_caps = capabilities_index.get(&target).cloned().unwrap_or_default();

        // Diagnostic: any fallback whose declared input modalities do not
        // cover the primary's? Empty fallback modalities are treated as
        // "unconstrained" (no diagnostic).
        let diagnostic = fallback_diagnostic(
            &alias.alias,
            &primary_caps.input,
            &fallback_targets,
            capabilities_index,
        );

        entries.push(CatalogEntry {
            id: alias.alias.clone(),
            kind: CatalogEntryKind::Alias {
                target,
                fallback_targets,
                strategy,
            },
            service_surfaces: primary_caps.surfaces,
            input_modalities: primary_caps.input,
            output_modalities: primary_caps.output,
            // Alias dziedziczy poziomy po celu — to ten sam model, tylko pod inna
            // nazwa; wlasnych zdolnosci nie ma.
            reasoning_levels: primary_caps.reasoning_levels,
            diagnostic,
        });
    }
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    Ok((entries, shadow_markers))
}

/// `fallback_targets` is stored as a JSON array string. Empty / NULL / not
/// an array all collapse to no fallbacks; we never error here because a
/// malformed value is a bug in the writer, not a reason to drop the alias.
fn parse_fallback_targets(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<String>>(trimmed)
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn fallback_diagnostic(
    alias_name: &str,
    primary_input: &[InputModality],
    fallbacks: &[String],
    capabilities_index: &HashMap<String, EntryCapabilities>,
) -> Option<CatalogDiagnostic> {
    if primary_input.is_empty() || fallbacks.is_empty() {
        return None;
    }
    let mut missing: HashSet<InputModality> = HashSet::new();
    for fallback in fallbacks {
        let Some(caps) = capabilities_index.get(fallback) else {
            // Unknown fallback — could be a forward reference or a stale
            // entry; resolve-time will fail loudly, no need to flag here.
            continue;
        };
        if caps.input.is_empty() {
            // Unconstrained fallback — assume it matches.
            continue;
        }
        for required in primary_input {
            if !caps.input.contains(required) {
                missing.insert(*required);
            }
        }
    }
    if missing.is_empty() {
        None
    } else {
        let mut missing_sorted: Vec<InputModality> = missing.into_iter().collect();
        missing_sorted.sort_by_key(|m| *m as u8);
        Some(CatalogDiagnostic::IncompatibleAliasTargets {
            alias: alias_name.to_string(),
            missing_modalities: missing_sorted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn fresh_db() -> DbPool {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        crate::db::seed::seed_defaults(&conn).unwrap();
        Arc::new(crate::db::Db::from_connection(conn))
    }

    #[test]
    fn snapshot_starts_empty_with_version_zero() {
        let provider = CatalogProvider::new();
        let snap = provider.snapshot();
        assert_eq!(snap.version, 0);
        assert!(snap.entries.is_empty());
    }

    #[test]
    fn rebuild_advances_version_monotonically() {
        let provider = CatalogProvider::new();
        let registry = MeshServicesRegistry::new();
        let pool = fresh_db();

        let v1 = provider.rebuild(&registry, &pool).unwrap();
        let v2 = provider.rebuild(&registry, &pool).unwrap();
        let v3 = provider.rebuild(&registry, &pool).unwrap();
        assert!(v2 > v1);
        assert!(v3 > v2);
        assert_eq!(provider.snapshot().version, v3);
    }

    /// Multiple threads racing on `rebuild` must publish snapshots in build
    /// order. Without the rebuild lock the version counter alone wouldn't
    /// stop a slow build from overwriting a faster one with a higher
    /// version number — that's exactly the regression this guards against.
    #[test]
    fn concurrent_rebuilds_publish_in_build_order() {
        use std::sync::Arc as StdArc;
        use std::thread;

        let provider = StdArc::new(CatalogProvider::new());
        let registry = StdArc::new(MeshServicesRegistry::new());
        let pool = fresh_db();

        let handles: Vec<_> = (0..16)
            .map(|_| {
                let provider = provider.clone();
                let registry = registry.clone();
                let pool = pool.clone();
                thread::spawn(move || provider.rebuild(&registry, &pool).unwrap())
            })
            .collect();

        let mut versions: Vec<u64> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        versions.sort();
        // Every rebuild produced a distinct version, and the published
        // snapshot is the one with the highest version.
        for window in versions.windows(2) {
            assert!(window[0] < window[1], "duplicate rebuild version returned");
        }
        assert_eq!(provider.snapshot().version, *versions.last().unwrap());
    }

    #[test]
    fn published_flow_appears_in_catalog() {
        let pool = fresh_db();
        // Mark the seeded LLM flow as published under "chat-pl".
        {
            let conn = pool.write().unwrap();
            conn.execute(
                "UPDATE flows SET published_model_name = 'chat-pl' \
                 WHERE name = 'Default Chat'",
                [],
            )
            .unwrap();
        }

        let provider = CatalogProvider::new();
        let registry = MeshServicesRegistry::new();
        provider.rebuild(&registry, &pool).unwrap();
        let snap = provider.snapshot();
        let flow_entry = snap
            .entries
            .iter()
            .find(|e| e.id == "chat-pl")
            .expect("published flow must appear under its published_model_name");
        assert!(matches!(flow_entry.kind, CatalogEntryKind::Flow { .. }));
        assert_eq!(flow_entry.owned_by(), "tentaflow-flow");
        assert_eq!(flow_entry.service_surfaces, vec![ServiceSurface::Chat]);
    }

    #[test]
    fn advertised_entries_strip_blocking_diagnostics() {
        let entries = vec![
            CatalogEntry {
                reasoning_levels: Vec::new(),
                id: "good".into(),
                kind: CatalogEntryKind::ServiceModel { instances: vec![] },
                service_surfaces: vec![],
                input_modalities: vec![],
                output_modalities: vec![],
                diagnostic: None,
            },
            CatalogEntry {
                reasoning_levels: Vec::new(),
                id: "shadowed".into(),
                kind: CatalogEntryKind::ServiceModel { instances: vec![] },
                service_surfaces: vec![],
                input_modalities: vec![],
                output_modalities: vec![],
                diagnostic: Some(CatalogDiagnostic::RemoteShadowed {
                    local_owner: "n".into(),
                }),
            },
            CatalogEntry {
                reasoning_levels: Vec::new(),
                id: "info".into(),
                kind: CatalogEntryKind::Alias {
                    target: "good".into(),
                    fallback_targets: vec![],
                    strategy: Strategy::FirstAvailable,
                },
                service_surfaces: vec![],
                input_modalities: vec![],
                output_modalities: vec![],
                diagnostic: Some(CatalogDiagnostic::IncompatibleAliasTargets {
                    alias: "info".into(),
                    missing_modalities: vec![InputModality::Audio],
                }),
            },
        ];
        let snap = CatalogSnapshot {
            entries: Arc::from(entries.into_boxed_slice()),
            version: 1,
        };
        let visible: Vec<&str> = snap.advertised_entries().map(|e| e.id.as_str()).collect();
        assert_eq!(visible, vec!["good", "info"]);
    }

    /// Same collision semantics as the alias-vs-flow case but the survivor
    /// is a service model. The mesh registry feeds a synthetic local
    /// service whose model name matches a seeded alias.
    #[test]
    fn alias_colliding_with_service_model_marks_service_with_diagnostic() {
        use tentaflow_protocol::{ServiceInfo, ServiceModelEntry};
        let pool = fresh_db();
        // Seed an alias to collide with the local service model below.
        {
            let conn = pool.write().unwrap();
            conn.execute(
                "INSERT INTO model_aliases (alias, target_model, is_active) \
                 VALUES ('test-alias', 'embeddings-gemma', 1)",
                [],
            )
            .unwrap();
        }
        let registry = MeshServicesRegistry::new();
        let local_node = "node-test".to_string();
        registry.replace_local(
            local_node.clone(),
            vec![ServiceInfo {
                id: 1,
                node_id: local_node,
                engine_id: "llama-cpp".into(),
                category: "llm".into(),
                display_name: "test-host".into(),
                deploy_method: "native_embedded".into(),
                transport: "embedded".into(),
                status: "running".into(),
                pinned: false,
                paused: false,
                runtime_pid: None,
                runtime_port: None,
                sidecar_quic_port: None,
                endpoint_url: None,
                restart_count: 0,
                health_last_err: None,
                active_deploy_id: String::new(),
                last_deploy_id: String::new(),
                deployment_progress_pct: 0,
                progress_message: None,
                usage_json: None,
                usage_updated_at: None,
                models: vec![ServiceModelEntry {
                    // Same name as the seeded alias above.
                    model_name: "test-alias".into(),
                    display_name: None,
                    capabilities: vec![],
                    context_length: None,
                    quantization: None,
                    is_default: true,
                    service_surfaces: Vec::new(),
                }],
                update_available: false,
                created_at: String::new(),
                updated_at: String::new(),
                request_time_parameters: Default::default(),
                gpu_selection: String::new(),
                cluster_deployment_id: String::new(),
            }],
        );

        let provider = CatalogProvider::new();
        provider.rebuild(&registry, &pool).unwrap();
        let snap = provider.snapshot();

        let entries: Vec<&CatalogEntry> = snap
            .entries
            .iter()
            .filter(|e| e.id == "test-alias")
            .collect();
        assert_eq!(entries.len(), 1, "alias must be dropped on collision");
        let survivor = entries[0];
        assert!(matches!(
            survivor.kind,
            CatalogEntryKind::ServiceModel { .. }
        ));
        match &survivor.diagnostic {
            Some(CatalogDiagnostic::RemoteShadowed { local_owner }) => {
                assert!(
                    local_owner.contains("test-alias"),
                    "diagnostic should mention the colliding alias name, got: {local_owner}"
                );
            }
            other => panic!("expected RemoteShadowed, got {:?}", other),
        }
    }

    /// P1.2: a service whose engine_id matches a real manifest must have
    /// its `input_modalities` and `output_modalities` populated from the
    /// manifest registry (preset > engine > category fallback). Whisper
    /// is a Stt-category engine without explicit modality overrides, so
    /// the catalog entry must end up with Audio in / Text out.
    #[test]
    fn service_model_modalities_are_populated_from_manifest() {
        use tentaflow_protocol::{ServiceInfo, ServiceModelEntry};
        let pool = fresh_db();
        let registry = MeshServicesRegistry::new();
        let local_node = "node-test".to_string();
        registry.replace_local(
            local_node.clone(),
            vec![ServiceInfo {
                id: 1,
                node_id: local_node,
                engine_id: "whisper".into(),
                category: "stt".into(),
                display_name: "stt-host".into(),
                deploy_method: "native_python_bundle".into(),
                transport: "sidecar_quic".into(),
                status: "running".into(),
                pinned: false,
                paused: false,
                runtime_pid: None,
                runtime_port: None,
                sidecar_quic_port: None,
                endpoint_url: None,
                restart_count: 0,
                health_last_err: None,
                active_deploy_id: String::new(),
                last_deploy_id: String::new(),
                deployment_progress_pct: 0,
                progress_message: None,
                usage_json: None,
                usage_updated_at: None,
                models: vec![ServiceModelEntry {
                    model_name: "whisper-base".into(),
                    display_name: None,
                    capabilities: vec![],
                    context_length: None,
                    quantization: None,
                    is_default: true,
                    service_surfaces: Vec::new(),
                }],
                update_available: false,
                created_at: String::new(),
                updated_at: String::new(),
                request_time_parameters: Default::default(),
                gpu_selection: String::new(),
                cluster_deployment_id: String::new(),
            }],
        );

        let provider = CatalogProvider::new();
        provider.rebuild(&registry, &pool).unwrap();
        let snap = provider.snapshot();
        let entry = snap
            .entries
            .iter()
            .find(|e| e.id == "whisper-base")
            .expect("whisper-base entry must exist");
        assert_eq!(entry.input_modalities, vec![InputModality::Audio]);
        assert_eq!(entry.output_modalities, vec![OutputModality::Text]);
    }

    /// R3.P1a: when the same `model_name` is exposed by two services with
    /// disagreeing capabilities (whisper STT vs. llama-cpp LLM both
    /// happen to advertise model id "shared"), the entry-level fields
    /// must report the union (so `/v1/models` is honest about every
    /// modality the mesh can satisfy) AND every `ModelInstance` must
    /// carry the originating service's modalities only — otherwise
    /// dispatch could send audio to the text-only instance.
    #[test]
    fn multi_service_collision_unions_modalities_per_instance_differs() {
        use tentaflow_protocol::{ServiceInfo, ServiceModelEntry};
        let pool = fresh_db();
        let registry = MeshServicesRegistry::new();
        let local_node = "node-test".to_string();
        let make_service = |id: i64, engine: &str, category: &str, transport: &str| ServiceInfo {
            id,
            node_id: local_node.clone(),
            engine_id: engine.into(),
            category: category.into(),
            display_name: "host".into(),
            deploy_method: "native_embedded".into(),
            transport: transport.into(),
            status: "running".into(),
            pinned: false,
            paused: false,
            runtime_pid: None,
            runtime_port: None,
            sidecar_quic_port: None,
            endpoint_url: None,
            restart_count: 0,
            health_last_err: None,
            active_deploy_id: String::new(),
            last_deploy_id: String::new(),
            deployment_progress_pct: 0,
            progress_message: None,
            usage_json: None,
            usage_updated_at: None,
            models: vec![ServiceModelEntry {
                model_name: "shared".into(),
                display_name: None,
                capabilities: vec![],
                context_length: None,
                quantization: None,
                is_default: true,
                service_surfaces: Vec::new(),
            }],
            update_available: false,
            created_at: String::new(),
            updated_at: String::new(),
            request_time_parameters: Default::default(),
            gpu_selection: String::new(),
            cluster_deployment_id: String::new(),
        };
        registry.replace_local(
            local_node.clone(),
            vec![
                make_service(1, "llama-cpp", "llm", "embedded"),
                make_service(2, "whisper", "stt", "sidecar_quic"),
            ],
        );

        let provider = CatalogProvider::new();
        provider.rebuild(&registry, &pool).unwrap();
        let snap = provider.snapshot();
        let entry = snap
            .entries
            .iter()
            .find(|e| e.id == "shared")
            .expect("shared entry must exist");

        // Entry-level union: covers both LLM (Text) and STT (Audio) inputs.
        assert!(entry.input_modalities.contains(&InputModality::Text));
        assert!(entry.input_modalities.contains(&InputModality::Audio));

        // Per-instance: each service carries only its own modality set.
        let CatalogEntryKind::ServiceModel { instances } = &entry.kind else {
            panic!("expected ServiceModel kind, got {:?}", entry.kind);
        };
        assert_eq!(instances.len(), 2);
        let llama = instances
            .iter()
            .find(|i| i.backend.as_deref() == Some("llama-cpp"))
            .expect("llama-cpp instance");
        assert_eq!(llama.input_modalities, vec![InputModality::Text]);
        let whisper = instances
            .iter()
            .find(|i| i.backend.as_deref() == Some("whisper"))
            .expect("whisper instance");
        assert_eq!(whisper.input_modalities, vec![InputModality::Audio]);
    }

    /// Codex P2 review fix: a draft flow that already claims a publish
    /// name must block another flow from grabbing the same name. Pre-fix
    /// the guard only looked at active flows, so a hidden draft would
    /// quietly collide on activation.
    #[test]
    fn draft_flow_publish_name_still_triggers_guard() {
        use crate::services::catalog::guards::{check_flow_publish_collision, GuardError};
        let pool = fresh_db();
        // Manually publish the seeded LLM flow but mark it draft so it
        // doesn't show up in advertised_entries.
        {
            let conn = pool.write().unwrap();
            conn.execute(
                "UPDATE flows SET published_model_name = 'chat-pl', status = 'draft' \
                 WHERE name = 'Default Chat'",
                [],
            )
            .unwrap();
        }
        match check_flow_publish_collision(&pool, "chat-pl", None) {
            Err(GuardError::FlowVsFlow { name }) => assert_eq!(name, "chat-pl"),
            other => panic!("expected FlowVsFlow even for draft, got {:?}", other),
        }
    }

    /// When an alias and a published flow claim the same id, the alias is
    /// dropped (its target would be unreachable through the catalog) and
    /// the surviving entry must carry a diagnostic so the GUI can warn
    /// operators. Without the diagnostic the collision was invisible.
    #[test]
    fn alias_colliding_with_published_flow_marks_flow_with_diagnostic() {
        let pool = fresh_db();
        // Seed an alias and publish the LLM flow under the same id so they
        // collide on the catalog id space.
        {
            let conn = pool.write().unwrap();
            conn.execute(
                "INSERT INTO model_aliases (alias, target_model, is_active) \
                 VALUES ('test-alias', 'embeddings-gemma', 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "UPDATE flows SET published_model_name = 'test-alias' \
                 WHERE name = 'Default Chat'",
                [],
            )
            .unwrap();
        }

        let provider = CatalogProvider::new();
        let registry = MeshServicesRegistry::new();
        provider.rebuild(&registry, &pool).unwrap();
        let snap = provider.snapshot();

        let collisions: Vec<&CatalogEntry> = snap
            .entries
            .iter()
            .filter(|e| e.id == "test-alias")
            .collect();
        assert_eq!(
            collisions.len(),
            1,
            "exactly one entry should claim 'test-alias'; alias must be dropped"
        );
        let survivor = collisions[0];
        assert!(matches!(survivor.kind, CatalogEntryKind::Flow { .. }));
        assert!(
            matches!(
                survivor.diagnostic,
                Some(CatalogDiagnostic::RemoteShadowed { .. })
            ),
            "expected RemoteShadowed diagnostic, got {:?}",
            survivor.diagnostic
        );
    }

    /// `ServiceInfo` dla external cloud providera (engine_id + category +
    /// pojedynczy model). Wspolny builder dla testow ponizej.
    fn external_service_info(
        id: i64,
        node_id: &str,
        engine_id: &str,
        category: &str,
        model_name: &str,
    ) -> tentaflow_protocol::ServiceInfo {
        use tentaflow_protocol::{ServiceInfo, ServiceModelEntry};
        ServiceInfo {
            id,
            node_id: node_id.to_string(),
            engine_id: engine_id.into(),
            category: category.into(),
            display_name: "OpenAI".into(),
            deploy_method: "external".into(),
            transport: "external_http".into(),
            status: "running".into(),
            pinned: false,
            paused: false,
            runtime_pid: None,
            runtime_port: None,
            sidecar_quic_port: None,
            endpoint_url: Some("https://api.openai.com/v1".into()),
            restart_count: 0,
            health_last_err: None,
            active_deploy_id: String::new(),
            last_deploy_id: String::new(),
            deployment_progress_pct: 100,
            progress_message: None,
            usage_json: None,
            usage_updated_at: None,
            models: vec![ServiceModelEntry {
                model_name: model_name.into(),
                display_name: None,
                capabilities: vec!["chat".into()],
                context_length: None,
                quantization: None,
                is_default: true,
                service_surfaces: Vec::new(),
            }],
            update_available: false,
            created_at: String::new(),
            updated_at: String::new(),
            request_time_parameters: Default::default(),
            gpu_selection: String::new(),
            cluster_deployment_id: String::new(),
        }
    }

    /// External cloud provider (openai) z presetem o `id != repo`
    /// ("gpt-5-5" vs "gpt-5.5"). Wpis katalogu musi dostac surfaces=[Chat]
    /// i tekstowe modalnosci niezaleznie od tego, ktora konwencja nazwy
    /// trafila do `model_registry` (preset id ze starszych deployow albo
    /// repo z `models_from_manifest` dla cloud external).
    #[test]
    fn external_openai_models_get_chat_surface_for_both_name_conventions() {
        let registry = MeshServicesRegistry::new();
        let local_node = "node-test".to_string();
        registry.replace_local(
            local_node.clone(),
            vec![
                external_service_info(1, &local_node, "openai", "llm", "gpt-5-5"),
                external_service_info(2, &local_node, "openai", "llm", "gpt-5.5"),
            ],
        );
        let entries = build_service_model_entries(&registry, &fresh_db());
        for name in ["gpt-5-5", "gpt-5.5"] {
            let entry = entries
                .iter()
                .find(|e| e.id == name)
                .unwrap_or_else(|| panic!("entry '{name}' must exist"));
            assert_eq!(
                entry.service_surfaces,
                vec![ServiceSurface::Chat],
                "entry '{name}' surfaces"
            );
            assert_eq!(entry.input_modalities, vec![InputModality::Text]);
            assert_eq!(entry.output_modalities, vec![OutputModality::Text]);
        }
    }

    /// Serwis z mesh, ktorego silnika nie ma w lokalnym rejestrze manifestow
    /// (np. nod z innym zestawem silnikow). Fallback z `services.category`
    /// musi dac zarowno surface jak i domyslne modalnosci kategorii.
    #[test]
    fn unknown_engine_falls_back_to_category_surfaces_and_modalities() {
        let registry = MeshServicesRegistry::new();
        // `peer-node` represents a PEER offering this external service (a
        // remote node with its own provider integration, reached via
        // MeshForward) — a real scenario the environment fence (ROADMAP Z12,
        // P2-7) must not reject just because this test never trust-paired
        // it. Stamp it same-environment (Prod, the ledger default) so the
        // instance is not silently dropped as "unknown environment".
        let db = fresh_db();
        repository::add_trusted_node(&db, "peer-node", "pub", "peer-node", "test-harness", None)
            .expect("trust peer-node for test");
        repository::set_trusted_node_environment(
            &db,
            "peer-node",
            tentaflow_protocol::environment::NodeEnvironment::Prod,
        )
        .expect("stamp peer-node environment");
        registry.replace_local(
            "local-node".to_string(),
            vec![external_service_info(
                1,
                "peer-node",
                "engine-not-in-registry",
                "llm",
                "mystery-model",
            )],
        );
        let entries = build_service_model_entries(&registry, &db);
        let entry = entries
            .iter()
            .find(|e| e.id == "mystery-model")
            .expect("mystery-model entry must exist");
        assert_eq!(entry.service_surfaces, vec![ServiceSurface::Chat]);
        assert_eq!(entry.input_modalities, vec![InputModality::Text]);
        assert_eq!(entry.output_modalities, vec![OutputModality::Text]);
    }

    /// End-to-end: wpis external openai zbudowany przez provider musi
    /// rozwiazywac sie w resolverze dla surface=Chat output=[Text] —
    /// dokladnie ten zestaw wymagan, ktory wysyla `stream_chat`.
    #[test]
    fn external_openai_entry_resolves_for_chat_text_request() {
        use crate::services::handles_cache::LiveHandlesCache;
        use crate::services::runtime::context::ExecutionContext;
        use crate::services::runtime::resolver::{AliasResolver, ResolveRequest};

        let pool = fresh_db();
        let registry = MeshServicesRegistry::new();
        // `peer-node` offers this external service via MeshForward — same
        // reasoning as `unknown_engine_falls_back_to_category_surfaces_and_
        // modalities` above, must be trust-paired + same-environment so the
        // ROADMAP Z12 (P2-7) fence does not drop it as "unknown".
        repository::add_trusted_node(&pool, "peer-node", "pub", "peer-node", "test-harness", None)
            .expect("trust peer-node for test");
        repository::set_trusted_node_environment(
            &pool,
            "peer-node",
            tentaflow_protocol::environment::NodeEnvironment::Prod,
        )
        .expect("stamp peer-node environment");
        registry.replace_local(
            "local-node".to_string(),
            vec![external_service_info(
                1,
                "peer-node",
                "openai",
                "llm",
                "gpt-5-5",
            )],
        );
        let provider = CatalogProvider::new();
        provider.rebuild(&registry, &pool).unwrap();
        let snap = provider.snapshot();

        let resolver = AliasResolver::new_with_static_id(
            Arc::new(LiveHandlesCache::new()),
            "local-node".to_string(),
        );
        let mut ctx = ExecutionContext::new(
            None,
            crate::flow_engine::dispatcher::FlowOrigin::System,
            crate::flow_engine::dispatcher::FlowActor::system(),
        );
        let req = ResolveRequest {
            requested_model: "gpt-5-5",
            required_surface: ServiceSurface::Chat,
            required_input_modalities: &[],
            required_output_modalities: &[OutputModality::Text],
        };
        let outcome = resolver
            .resolve(&req, &snap, &mut ctx)
            .expect("external openai model must resolve for chat/text");
        assert_eq!(outcome.candidates.len(), 1);
        assert_eq!(outcome.candidates[0].requested_model(), "gpt-5-5");
    }

    #[test]
    fn infer_flow_modalities_pulls_audio_in_for_stt_node() {
        let json = r#"{
            "nodes": [
                {"id":"t","type":"trigger"},
                {"id":"s","type":"stt"},
                {"id":"l","type":"llm"},
                {"id":"o","type":"output"}
            ],
            "edges": []
        }"#;
        let (inputs, outputs) = infer_flow_modalities(json);
        assert!(inputs.contains(&InputModality::Audio));
        assert!(inputs.contains(&InputModality::Text));
        assert!(outputs.contains(&OutputModality::Text));
    }

    #[test]
    fn infer_flow_modalities_pulls_audio_out_for_tts_node() {
        let json = r#"{
            "nodes": [
                {"id":"t","type":"trigger"},
                {"id":"l","type":"llm"},
                {"id":"v","type":"tts"},
                {"id":"o","type":"output"}
            ]
        }"#;
        let (_inputs, outputs) = infer_flow_modalities(json);
        assert!(outputs.contains(&OutputModality::Audio));
        assert!(outputs.contains(&OutputModality::Text));
    }

    #[test]
    fn infer_flow_modalities_handles_pure_image_gen() {
        let json = r#"{"nodes":[{"id":"g","type":"image_gen"}]}"#;
        let (inputs, outputs) = infer_flow_modalities(json);
        assert!(inputs.is_empty());
        assert_eq!(outputs, vec![OutputModality::Image]);
    }

    /// Every node type seeded into `flow_node_templates` must appear in
    /// either `MODALITY_CONTRIBUTING_NODE_TYPES` or
    /// `MODALITY_PASSTHROUGH_NODE_TYPES`. Otherwise `infer_flow_modalities`
    /// silently classifies a new template as "no contribution", which
    /// would make the catalog advertise wrong modalities for any flow
    /// that uses it. The fix when this fails is to add the new type to
    /// one of the two lists (and a match arm if it contributes).
    #[test]
    fn every_seeded_node_type_has_modality_decision() {
        use std::collections::BTreeSet;
        let pool = fresh_db();
        let conn = pool.read().unwrap();
        let mut stmt = conn
            .prepare("SELECT node_type FROM flow_node_templates")
            .unwrap();
        let seeded: BTreeSet<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        let known: BTreeSet<&str> = MODALITY_CONTRIBUTING_NODE_TYPES
            .iter()
            .chain(MODALITY_PASSTHROUGH_NODE_TYPES.iter())
            .copied()
            .collect();

        let unhandled: Vec<&String> = seeded
            .iter()
            .filter(|t| !known.contains(t.as_str()))
            .collect();
        assert!(
            unhandled.is_empty(),
            "node types in flow_node_templates not handled by infer_flow_modalities: {:?}\n\
             add each to either MODALITY_CONTRIBUTING_NODE_TYPES (with a match arm) or \
             MODALITY_PASSTHROUGH_NODE_TYPES",
            unhandled
        );
    }

    #[test]
    fn infer_flow_modalities_returns_empty_for_garbage_json() {
        assert_eq!(infer_flow_modalities("not-json"), (vec![], vec![]));
        assert_eq!(infer_flow_modalities("{}"), (vec![], vec![]));
        assert_eq!(
            infer_flow_modalities(r#"{"nodes":"oops"}"#),
            (vec![], vec![])
        );
    }

    #[test]
    fn parse_fallback_targets_handles_garbage() {
        assert!(parse_fallback_targets(None).is_empty());
        assert!(parse_fallback_targets(Some("")).is_empty());
        assert!(parse_fallback_targets(Some("not-json")).is_empty());
        assert_eq!(
            parse_fallback_targets(Some(r#"["a","b"," c "]"#)),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }
}
