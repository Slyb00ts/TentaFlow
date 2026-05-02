// =============================================================================
// File: services/catalog/mod.rs
// Single source of truth for what `/v1/models`, the GUI, and binary
// `catalog.list` advertise. Combines locally deployed service models, mesh
// peer service models, published flows, and aliases into one snapshot type.
// =============================================================================

use serde::{Deserialize, Serialize};

pub mod guards;
mod provider;

pub use provider::{CatalogProvider, CatalogSnapshot};

// =============================================================================
// Capability axes (D.12 — three independent axes).
// =============================================================================

/// API contract a model speaks. A model can serve more than one surface (a
/// vision LLM still uses the chat surface; an omni model also uses chat).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceSurface {
    Chat,
    Embeddings,
    Stt,
    Tts,
    Rerank,
    ImageGen,
    Documents,
    Agents,
}

impl ServiceSurface {
    /// Best-effort inference from a manifest's `engine.category`. Returns
    /// `None` for categories that have no surface mapping yet (will be
    /// populated when manifests get explicit `service_surfaces` in R2g).
    pub fn from_manifest_category(category: &str) -> Option<Self> {
        match category {
            "llm" => Some(Self::Chat),
            "stt" => Some(Self::Stt),
            "tts" => Some(Self::Tts),
            "embedding" | "embeddings" => Some(Self::Embeddings),
            "rerank" | "reranker" => Some(Self::Rerank),
            "image-gen" | "image_gen" => Some(Self::ImageGen),
            "documents" => Some(Self::Documents),
            "agents" => Some(Self::Agents),
            _ => None,
        }
    }

    /// Inference from a flow's `service_type`. The mapping mirrors the
    /// manifest one — flows declare the same surface vocabulary.
    pub fn from_flow_service_type(service_type: &str) -> Option<Self> {
        match service_type {
            "chat" | "llm" => Some(Self::Chat),
            "stt" => Some(Self::Stt),
            "tts" => Some(Self::Tts),
            "embedding" | "embeddings" => Some(Self::Embeddings),
            "rag" => Some(Self::Chat),
            "rerank" | "reranker" => Some(Self::Rerank),
            "image-gen" | "image_gen" => Some(Self::ImageGen),
            "documents" => Some(Self::Documents),
            "agents" => Some(Self::Agents),
            _ => None,
        }
    }

    /// Lower-snake-case wire identifier emitted on binary `catalog.list`
    /// payloads. Fast (constant string), exhaustive (compiler enforces
    /// coverage), and decoupled from the `serde(rename_all)` attribute on
    /// the enum so a serde refactor cannot silently regress the wire shape.
    pub fn as_wire_str(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Embeddings => "embeddings",
            Self::Stt => "stt",
            Self::Tts => "tts",
            Self::Rerank => "rerank",
            Self::ImageGen => "image_gen",
            Self::Documents => "documents",
            Self::Agents => "agents",
        }
    }
}

impl InputModality {
    /// Wire identifier matching the on-disk JSON / TOML manifest convention.
    pub fn as_wire_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::Audio => "audio",
        }
    }
}

impl OutputModality {
    /// Wire identifier matching the on-disk JSON / TOML manifest convention.
    pub fn as_wire_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Audio => "audio",
            Self::Embedding => "embedding",
            Self::Image => "image",
        }
    }
}

/// Type of payload a model accepts on input. Independent from surface — a
/// vision-capable chat model has surface=Chat with `[Text, Image]` input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputModality {
    Text,
    Image,
    Audio,
}

/// Type of payload a model produces. Surface dictates the response shape;
/// modality dictates what's inside (e.g. surface=Chat with `[Text, Audio]`
/// for an omni model that responds with both text and TTS audio).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputModality {
    Text,
    Audio,
    Embedding,
    Image,
}

// =============================================================================
// Catalog entries.
// =============================================================================

/// One node hosting a model. The catalog aggregates instances across the mesh
/// so `/v1/models` shows a single id even when several nodes serve it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInstance {
    pub node_id: String,
    pub node_hostname: Option<String>,
    pub service_id: i64,
    pub status: String,
    pub backend: Option<String>,
    pub size_mb: Option<u64>,
    pub loaded: bool,
}

/// Strategy used to pick among an alias's primary + fallback targets at
/// dispatch time. Only `FirstAvailable` and `RoundRobin` are wired up
/// (D.11 — `LeastLoaded` is out of scope until a load signal exists).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    FirstAvailable,
    RoundRobin,
}

impl Strategy {
    /// Parse the strategy string stored in `model_aliases.strategy`. NULL,
    /// empty, and "first_available" all map to `FirstAvailable`. An unknown
    /// non-empty value also falls back, but emits a warning so an operator
    /// notices a typo / a deferred strategy (`least_loaded`) being set.
    pub fn from_db(value: Option<&str>) -> Self {
        let normalised = value.map(str::trim).map(str::to_ascii_lowercase);
        match normalised.as_deref() {
            None | Some("") => Self::FirstAvailable,
            Some("first_available") | Some("first-available") | Some("firstavailable") => {
                Self::FirstAvailable
            }
            Some("round_robin") | Some("round-robin") | Some("roundrobin") => Self::RoundRobin,
            Some(other) => {
                tracing::warn!(
                    strategy = other,
                    "unknown alias strategy stored in DB; defaulting to first_available"
                );
                Self::FirstAvailable
            }
        }
    }

    /// Wire identifier consumed by `catalog.list` and the GUI.
    pub fn as_wire_str(&self) -> &'static str {
        match self {
            Self::FirstAvailable => "first_available",
            Self::RoundRobin => "round_robin",
        }
    }
}

/// What a single catalog entry actually represents. The id is stable across
/// kinds — clients reach a flow, an alias, or a service model with the same
/// `model` field on a chat completion request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CatalogEntryKind {
    /// A model deployed locally or on a mesh peer. Multiple instances mean
    /// either a load-balanced local pool or several mesh nodes hosting the
    /// same name.
    ServiceModel { instances: Vec<ModelInstance> },
    /// A flow exposed under `published_model_name` — clients call it like
    /// any other model; the flow engine handles dispatch internally.
    Flow {
        flow_id: i64,
        published_name: String,
    },
    /// An alias mapping one name onto a primary target plus optional
    /// fallbacks. Modalities advertised here equal the **primary** target's
    /// (D.17) — fallbacks may differ and are filtered per-request at
    /// resolve time.
    Alias {
        target: String,
        fallback_targets: Vec<String>,
        strategy: Strategy,
    },
}

/// Diagnostic states attached to an entry. Some are blocking (entry hidden
/// from `/v1/models`); some are informational only (entry still visible, GUI
/// can surface a warning). See R1.0d/e for the filtering rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CatalogDiagnostic {
    /// Our locally owned entry hides a remote with the same name. The remote
    /// stays in the catalog as a diagnostic so an operator can see the
    /// collision; the client-visible model id resolves to our local entry.
    RemoteShadowed { local_owner: String },
    /// A remote node owns this name; we keep its entry but the local
    /// configuration would override it. Surface this so operators can pick.
    LocalOverride { conflicting_remote_node: String },
    /// An alias has fallbacks that don't satisfy the primary target's
    /// modalities. The alias is still advertised — the resolver filters
    /// fallbacks per-request — but operators see which capabilities the
    /// fallbacks won't honor.
    IncompatibleAliasTargets {
        alias: String,
        missing_modalities: Vec<InputModality>,
    },
}

impl CatalogDiagnostic {
    /// Blocking diagnostics hide the entry from `/v1/models` and binary
    /// `catalog.list`. Non-blocking ones surface in GUI but remain
    /// queryable — an alias with one mismatched fallback may still resolve
    /// fine for requests that match the primary.
    pub fn is_blocking(&self) -> bool {
        matches!(
            self,
            Self::RemoteShadowed { .. } | Self::LocalOverride { .. }
        )
    }
}

/// One advertised model in the unified catalog. The `id` is what clients
/// pass as `model` on requests; the kind determines how dispatch proceeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub kind: CatalogEntryKind,
    pub service_surfaces: Vec<ServiceSurface>,
    pub input_modalities: Vec<InputModality>,
    pub output_modalities: Vec<OutputModality>,
    pub diagnostic: Option<CatalogDiagnostic>,
}

impl CatalogEntry {
    /// `owned_by` reported on `/v1/models`. Stable strings — clients (e.g.
    /// LangChain) pattern-match on these.
    pub fn owned_by(&self) -> &'static str {
        match &self.kind {
            CatalogEntryKind::ServiceModel { .. } => "tentaflow-service",
            CatalogEntryKind::Flow { .. } => "tentaflow-flow",
            CatalogEntryKind::Alias { .. } => "tentaflow-alias",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_blocking_classification() {
        assert!(CatalogDiagnostic::RemoteShadowed {
            local_owner: "n".into()
        }
        .is_blocking());
        assert!(CatalogDiagnostic::LocalOverride {
            conflicting_remote_node: "n".into()
        }
        .is_blocking());
        assert!(!CatalogDiagnostic::IncompatibleAliasTargets {
            alias: "a".into(),
            missing_modalities: vec![InputModality::Audio],
        }
        .is_blocking());
    }

    #[test]
    fn surface_inference_from_manifest_category() {
        assert_eq!(
            ServiceSurface::from_manifest_category("llm"),
            Some(ServiceSurface::Chat)
        );
        assert_eq!(
            ServiceSurface::from_manifest_category("tts"),
            Some(ServiceSurface::Tts)
        );
        assert_eq!(ServiceSurface::from_manifest_category("unknown"), None);
    }

    #[test]
    fn strategy_db_parsing_falls_back_to_first_available() {
        assert_eq!(Strategy::from_db(None), Strategy::FirstAvailable);
        assert_eq!(Strategy::from_db(Some("")), Strategy::FirstAvailable);
        assert_eq!(Strategy::from_db(Some("FirstAvailable")), Strategy::FirstAvailable);
        assert_eq!(Strategy::from_db(Some("round_robin")), Strategy::RoundRobin);
        assert_eq!(Strategy::from_db(Some("Round-Robin")), Strategy::RoundRobin);
        assert_eq!(Strategy::from_db(Some("garbage")), Strategy::FirstAvailable);
    }

    /// `as_wire_str` is what protocol/GUI sees. Round-trip through the
    /// `from_*` lookup must hold for every variant — protects against an
    /// accidental serde rename or typo silently breaking the wire shape.
    #[test]
    fn service_surface_wire_strings_round_trip_through_inference() {
        let cases = [
            (ServiceSurface::Chat, "chat", "llm"),
            (ServiceSurface::Embeddings, "embeddings", "embedding"),
            (ServiceSurface::Stt, "stt", "stt"),
            (ServiceSurface::Tts, "tts", "tts"),
            (ServiceSurface::Rerank, "rerank", "rerank"),
            (ServiceSurface::ImageGen, "image_gen", "image-gen"),
            (ServiceSurface::Documents, "documents", "documents"),
            (ServiceSurface::Agents, "agents", "agents"),
        ];
        for (surface, wire, manifest_category) in cases {
            assert_eq!(surface.as_wire_str(), wire);
            assert_eq!(
                ServiceSurface::from_manifest_category(manifest_category),
                Some(surface),
                "manifest category '{}' must map to {:?}",
                manifest_category,
                surface
            );
        }
    }

    #[test]
    fn input_modality_wire_strings_cover_all_variants() {
        assert_eq!(InputModality::Text.as_wire_str(), "text");
        assert_eq!(InputModality::Image.as_wire_str(), "image");
        assert_eq!(InputModality::Audio.as_wire_str(), "audio");
    }

    #[test]
    fn output_modality_wire_strings_cover_all_variants() {
        assert_eq!(OutputModality::Text.as_wire_str(), "text");
        assert_eq!(OutputModality::Audio.as_wire_str(), "audio");
        assert_eq!(OutputModality::Embedding.as_wire_str(), "embedding");
        assert_eq!(OutputModality::Image.as_wire_str(), "image");
    }

    #[test]
    fn strategy_wire_string_round_trips_through_db_parse() {
        for s in [Strategy::FirstAvailable, Strategy::RoundRobin] {
            let wire = s.as_wire_str();
            assert_eq!(
                Strategy::from_db(Some(wire)),
                s,
                "wire '{}' did not round-trip back to {:?}",
                wire,
                s
            );
        }
    }

    #[test]
    fn owned_by_is_kind_specific() {
        let svc = CatalogEntry {
            id: "m".into(),
            kind: CatalogEntryKind::ServiceModel { instances: vec![] },
            service_surfaces: vec![],
            input_modalities: vec![],
            output_modalities: vec![],
            diagnostic: None,
        };
        assert_eq!(svc.owned_by(), "tentaflow-service");
    }
}
