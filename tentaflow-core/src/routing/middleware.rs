// =============================================================================
// Plik: routing/middleware.rs
// Opis: Typy routingu i alias cache. Pre-R3b.8 plik trzymał także
//       `BackendHandle` enum + `dispatch_with_fallback` + per-handle
//       backend dispatch — to wszystko zniknęło razem z R3b.8 cutover na
//       `ModelRuntimeExecutor`. Tu zostają tylko struktury wspólne dla
//       routing path (RouteResult / RouteMetadata / ResolvedRoute) i
//       alias cache reload.
// =============================================================================

use crate::routing::router::Router;
use crate::services::runtime::quic_handle::PoolStrategy;

use tracing::{debug, warn};

// ============================================================================
// TYPY
// ============================================================================

/// Rozwiazana trasa — lista targetow i strategia wyboru. Po R3b.8 używane
/// głównie do logging / metrics, executor ma własny resolver
/// (`AliasResolver`).
pub struct ResolvedRoute {
    pub targets: Vec<String>,
    pub strategy: PoolStrategy,
}

/// Metadane trasy — serializowane do headera X-TentaFlow-Route
#[derive(Debug, Clone, serde::Serialize)]
pub struct RouteMetadata {
    pub served_by_node: String,
    pub backend_type: String,
    pub strategy_used: String,
    pub fallbacks_tried: u32,
    pub hop_count: u32,
    pub latency_ms: Option<f64>,
}

/// Wynik routingu — odpowiedz + metadane trasy
pub struct RouteResult<T> {
    pub response: T,
    pub metadata: RouteMetadata,
}

// ============================================================================
// ALIAS CACHE
// ============================================================================

/// Pre-parsed alias entry kept in the routing cache. `DbModelAlias` retains
/// the raw `fallback_targets` JSON string; we keep that for backwards
/// compatibility on the wire (mesh sync uses `DbModelAlias`) but the hot
/// dispatch path reads the parsed list to avoid serde-on-every-route.
#[derive(Clone, Debug)]
pub struct CachedAlias {
    pub alias: String,
    pub target_model: String,
    pub fallback_targets: Vec<String>,
    pub strategy: Option<String>,
}

impl CachedAlias {
    /// Build the cache entry from a DB row, parsing JSON `fallback_targets`
    /// once and warning loudly on malformed input.
    pub fn from_db(row: &crate::db::models::DbModelAlias) -> Self {
        Self {
            alias: row.alias.clone(),
            target_model: row.target_model.clone(),
            fallback_targets: parse_alias_fallback_targets(
                row.fallback_targets.as_deref(),
                &row.alias,
            ),
            strategy: row.strategy.clone(),
        }
    }
}

/// `model_aliases.fallback_targets` is canonical JSON (CLAUDE.md §9). Returns
/// the parsed list, dropping empty entries; logs a warn when the value is
/// neither empty nor valid JSON so an operator notices a writer regression
/// instead of silently losing fallback targets.
fn parse_alias_fallback_targets(raw: Option<&str>, alias_name: &str) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Vec<String>>(trimmed) {
        Ok(parsed) => parsed
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Err(e) => {
            warn!(
                alias = %alias_name,
                raw = %trimmed,
                "fallback_targets nie jest poprawnym JSON array — pomijam: {}",
                e
            );
            Vec::new()
        }
    }
}

// ============================================================================
// IMPL ROUTER — alias resolution + cache
// ============================================================================

impl Router {
    /// Czy `name` jest znanym serwisem (po service_name) w jakimkolwiek
    /// rejestrze backendow. Used by `resolve_route` to short-circuit when
    /// the requested name is already a service id.
    fn is_known_service(&self, name: &str) -> bool {
        self.service_manager.has_quic_llm_service(name)
            || self.service_manager.has_http_backends(name)
            || self.service_manager.has_local_inference_service(name)
            || self.service_manager.has_quic_stt_service(name)
            || self.service_manager.has_quic_tts_service(name)
            || self.service_manager.has_quic_embedding_service(name)
    }

    /// Wraca target aliasu jako lista jednoelementowa. Pusty target trafia
    /// do dalszej dispatch logiki bez zmian.
    fn expand_alias_target(&self, target: String) -> Vec<String> {
        vec![target]
    }

    /// Rozwiazuje nazwe modelu na liste targetow i strategie. Po R3b.8
    /// używane już tylko do logging / metrics — executor ma własny
    /// resolver (`AliasResolver`) z modality-aware filtering.
    pub(crate) fn resolve_route(&self, model: &str) -> ResolvedRoute {
        let snap = self.service_manager.current_snapshot();
        if snap.models_by_name.contains_key(model)
            || snap.services.iter().any(|s| s.engine_id == model)
        {
            debug!("resolve_route: snapshot hit for {}", model);
            return ResolvedRoute {
                targets: vec![model.to_string()],
                strategy: PoolStrategy::FirstAvailable,
            };
        }

        if self.is_known_service(model) {
            return ResolvedRoute {
                targets: vec![model.to_string()],
                strategy: PoolStrategy::FirstAvailable,
            };
        }

        {
            let cache = self.alias_cache.read();
            if let Some(cached) = cache.get(model) {
                let mut raw_targets = vec![cached.target_model.clone()];
                raw_targets.extend(cached.fallback_targets.iter().cloned());
                let targets: Vec<String> = raw_targets
                    .into_iter()
                    .flat_map(|t| self.expand_alias_target(t))
                    .collect();
                let strategy = cached
                    .strategy
                    .as_deref()
                    .map(PoolStrategy::parse)
                    .unwrap_or(PoolStrategy::FirstAvailable);
                debug!(
                    "resolve_route: alias cache {} -> {:?} ({})",
                    model, targets, strategy
                );
                return ResolvedRoute { targets, strategy };
            }
        }

        ResolvedRoute {
            targets: vec![model.to_string()],
            strategy: PoolStrategy::FirstAvailable,
        }
    }

    /// Mesh sync handler — replaces the in-memory alias cache after the
    /// supervisor pushes a fresh `model_aliases` snapshot. Each entry is
    /// pre-parsed via `CachedAlias::from_db` so the hot dispatch path
    /// avoids serde-on-every-route.
    pub fn update_alias_cache_from_sync(&self, aliases: Vec<crate::db::models::DbModelAlias>) {
        let mut cache = self.alias_cache.write();
        cache.clear();
        for alias in aliases {
            if alias.is_active {
                cache.insert(alias.alias.clone(), CachedAlias::from_db(&alias));
            }
        }
        tracing::debug!("Alias cache zaktualizowany z sync: {} wpisow", cache.len());
    }

    /// Laduje alias cache z bazy danych. Patrz `update_alias_cache_from_sync`
    /// na temat pre-parsowania.
    pub(crate) fn reload_alias_cache(&self) {
        let db = match &self.db {
            Some(db) => db,
            None => return,
        };

        match crate::db::repository::list_model_aliases(db) {
            Ok(aliases) => {
                let mut cache = self.alias_cache.write();
                cache.clear();
                for alias in aliases {
                    if alias.is_active {
                        cache.insert(alias.alias.clone(), CachedAlias::from_db(&alias));
                    }
                }
                debug!("Alias cache przeladowany: {} wpisow", cache.len());
            }
            Err(e) => {
                debug!("Blad ladowania alias cache: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod middleware_tests {
    use super::*;

    // ========================================================================
    // parse_alias_fallback_targets — JSON list parsing
    // ========================================================================

    #[test]
    fn parse_alias_fallback_targets_json_array() {
        let raw = Some(r#"["a","b","c"]"#);
        let parsed = parse_alias_fallback_targets(raw, "alias-a");
        assert_eq!(parsed, vec!["a", "b", "c"]);
    }

    #[test]
    fn parse_alias_fallback_targets_handles_empty_and_none() {
        assert!(parse_alias_fallback_targets(None, "alias-none").is_empty());
        assert!(parse_alias_fallback_targets(Some(""), "alias-empty").is_empty());
        assert!(parse_alias_fallback_targets(Some("   "), "alias-ws").is_empty());
        assert!(parse_alias_fallback_targets(Some("[]"), "alias-empty-arr").is_empty());
    }

    #[test]
    fn parse_alias_fallback_targets_trims_inner_whitespace() {
        let raw = Some(r#"["  a  ", "b", ""]"#);
        let parsed = parse_alias_fallback_targets(raw, "alias-trim");
        assert_eq!(parsed, vec!["a", "b"]);
    }

    /// CLAUDE.md §9: CSV legacy format must reject (writer / reader sync
    /// regressions used to silently turn `"a,b,c"` into a single-element
    /// `Vec<String>` containing the comma string).
    #[test]
    fn parse_alias_fallback_targets_rejects_csv_legacy_format() {
        let parsed = parse_alias_fallback_targets(Some("a,b,c"), "alias-csv");
        assert!(parsed.is_empty(), "CSV format must not be parsed as JSON");
    }

    #[test]
    fn route_metadata_serializes_to_json() {
        let m = RouteMetadata {
            served_by_node: "node-1".into(),
            backend_type: "http".into(),
            strategy_used: "first_available".into(),
            fallbacks_tried: 1,
            hop_count: 0,
            latency_ms: Some(12.34),
        };
        let json = serde_json::to_string(&m).expect("serialize");
        assert!(json.contains("\"served_by_node\":\"node-1\""));
        assert!(json.contains("\"backend_type\":\"http\""));
        assert!(json.contains("\"latency_ms\":12.34"));
    }
}
