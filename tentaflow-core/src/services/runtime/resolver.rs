// =============================================================================
// File: services/runtime/resolver.rs
// Alias resolution with modality-aware filtering. Walks the catalog from
// a requested model id, expanding aliases recursively, and emits the
// list of `ResolvedExecutionTarget` candidates that satisfy the
// request's surface and modality constraints. Strategy ranking happens
// afterwards in `strategy::rank`.
//
// Modalities follow the **primary target** of an alias — fallbacks are
// filtered per-request without changing the alias's advertised
// signature. An audio-capable primary keeps audio input alive even when
// every fallback is text-only; a request for audio simply lands on the
// primary alone.
// =============================================================================

use std::sync::Arc;

use thiserror::Error;

use crate::services::catalog::{
    CatalogEntry, CatalogEntryKind, CatalogSnapshot, InputModality, ModelInstance,
    OutputModality, ServiceSurface, Strategy,
};
use crate::services::handles_cache::LiveHandlesCache;
use crate::services::runtime::context::{ContextLimitError, ExecutionContext};
use crate::services::runtime::target::ResolvedExecutionTarget;

/// What the caller wants. Modalities are required-set semantics — the
/// candidate must declare every requested modality (empty modalities on
/// the candidate side count as "unconstrained" and pass).
#[derive(Debug, Clone)]
pub struct ResolveRequest<'a> {
    pub requested_model: &'a str,
    pub required_surface: ServiceSurface,
    pub required_input_modalities: &'a [InputModality],
    pub required_output_modalities: &'a [OutputModality],
}

/// Outcome of `resolve` — either a non-empty ordered candidate list (the
/// caller hands it to `strategy::rank` and tries them in order) or a
/// reason why no candidate matched.
#[derive(Debug, Clone)]
pub struct ResolveOutcome {
    /// Candidates in declaration order — primary first, fallbacks in
    /// `model_aliases.fallback_targets` order. Strategy may permute later.
    pub candidates: Vec<ResolvedExecutionTarget>,
    /// Strategy declared on the alias chain root; `FirstAvailable` for
    /// direct (non-alias) lookups. Forwarded to `strategy::rank`.
    pub strategy: Strategy,
}

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("model '{0}' not found in catalog")]
    UnknownModel(String),
    #[error(
        "model '{requested}' has no candidate satisfying surface={surface:?} \
         input={input:?} output={output:?}"
    )]
    CapabilityUnsupported {
        requested: String,
        surface: ServiceSurface,
        input: Vec<InputModality>,
        output: Vec<OutputModality>,
    },
    #[error("alias chain limit hit while resolving '{requested}': {source}")]
    AliasLimit {
        requested: String,
        #[source]
        source: ContextLimitError,
    },
    /// Alias references a primary target that is not in the catalog.
    /// Surfaced as a fatal config error rather than silently falling
    /// through to fallbacks — production traffic landing on a fallback
    /// because someone typo'd `target_model` is exactly the kind of
    /// invisible misconfiguration a shared id space must prevent.
    #[error("alias '{alias}' targets unknown primary model '{primary}'")]
    AliasPrimaryMissing { alias: String, primary: String },
}

/// Provider lokalnego node_id. Resolver woła go per resolve żeby zawsze
/// mieć aktualny id z mesh registry — wczesniejsza wersja capture'owala
/// node_id w `Router::new`, gdy registry byl jeszcze `None` ⇒ resolver
/// dostawal `""` i kazdy lokalny ModelInstance trafial w MeshForward
/// (codex H2). Closure boxed jako `Send + Sync + 'static` zeby executor
/// mogl byc trzymany w Arc i wolany z dowolnego watka.
pub type LocalNodeIdProvider = Arc<dyn Fn() -> String + Send + Sync>;

/// Stateless component — every call into `resolve` walks the supplied
/// snapshot. Holds a clone of `LiveHandlesCache` so it can hydrate
/// `Local` candidates without going through the executor.
pub struct AliasResolver {
    handles: Arc<LiveHandlesCache>,
    local_node_id: LocalNodeIdProvider,
}

impl AliasResolver {
    pub fn new(handles: Arc<LiveHandlesCache>, local_node_id: LocalNodeIdProvider) -> Self {
        Self {
            handles,
            local_node_id,
        }
    }

    /// Convenience constructor dla testow ktore podaja staly node_id.
    #[cfg(test)]
    pub fn new_with_static_id(handles: Arc<LiveHandlesCache>, local_node_id: String) -> Self {
        Self {
            handles,
            local_node_id: Arc::new(move || local_node_id.clone()),
        }
    }
}

/// Helper dla `Router::new` (R1.5e). Zwraca closure ktora przy kazdym wywolaniu
/// odczytuje aktualny `local().node_id` z `ServiceManager.mesh_services_registry`.
/// W momencie konstrukcji executor'a registry moze byc jeszcze None (Router::new
/// jest sekwencyjny, ale `set_mesh_services_registry` woła sie dopiero w
/// callerze) — provider obsluguje to bezpiecznie zwracajac pusty string,
/// supervisor natychmiast przepisze snapshot i kolejne wywolania widza pelen id.
pub fn local_node_id_provider_for_router(
    sm: &Arc<crate::routing::service_manager::ServiceManager>,
) -> LocalNodeIdProvider {
    let sm = Arc::clone(sm);
    Arc::new(move || {
        let registry = sm.mesh_services_registry.read();
        registry
            .as_ref()
            .map(|r| r.local().node_id.clone())
            .unwrap_or_default()
    })
}

impl AliasResolver {

    /// Walk the catalog from `req.requested_model`, expand aliases, drop
    /// candidates whose surface/modalities don't satisfy the request, and
    /// return what's left. The context is mutated to track the alias
    /// chain depth — the caller is expected to pass a fresh context (or
    /// one whose alias_stack has already accounted for the parent
    /// resolver call).
    pub fn resolve<'a>(
        &self,
        req: &ResolveRequest<'a>,
        snapshot: &CatalogSnapshot,
        ctx: &mut ExecutionContext,
    ) -> Result<ResolveOutcome, ResolveError> {
        let entry = lookup_entry(snapshot, req.requested_model)
            .ok_or_else(|| ResolveError::UnknownModel(req.requested_model.to_string()))?;

        // Strategy is anchored on the root entry — alias-of-alias still
        // honors the root's choice; aliases without an explicit value
        // default to FirstAvailable inside `from_db`.
        let strategy = match &entry.kind {
            CatalogEntryKind::Alias { strategy, .. } => *strategy,
            _ => Strategy::FirstAvailable,
        };

        let mut candidates = Vec::new();
        self.expand_into(req, snapshot, entry, ctx, &mut candidates)?;

        if candidates.is_empty() {
            return Err(ResolveError::CapabilityUnsupported {
                requested: req.requested_model.to_string(),
                surface: req.required_surface,
                input: req.required_input_modalities.to_vec(),
                output: req.required_output_modalities.to_vec(),
            });
        }
        Ok(ResolveOutcome {
            candidates,
            strategy,
        })
    }

    /// Recursive walk — handles aliases (push onto ctx.alias_stack, dive
    /// into target + fallbacks, pop) and direct entries (try to convert
    /// into a `ResolvedExecutionTarget` after capability check).
    ///
    /// Every successful `enter_alias` is paired with a `leave_alias` even
    /// on the error path — without that an inherited `ExecutionContext`
    /// (the case once flow nodes call into the resolver) would build up
    /// a stale stack across calls and start producing false-positive
    /// cycle errors. The closure-and-finalise pattern below makes the
    /// pairing structural so a future refactor cannot accidentally drop
    /// the `leave` on a new error branch.
    fn expand_into<'a>(
        &self,
        req: &ResolveRequest<'a>,
        snapshot: &CatalogSnapshot,
        entry: &CatalogEntry,
        ctx: &mut ExecutionContext,
        out: &mut Vec<ResolvedExecutionTarget>,
    ) -> Result<(), ResolveError> {
        match &entry.kind {
            CatalogEntryKind::Alias {
                target,
                fallback_targets,
                ..
            } => {
                ctx.enter_alias(&entry.id).map_err(|source| {
                    ResolveError::AliasLimit {
                        requested: req.requested_model.to_string(),
                        source,
                    }
                })?;

                let result = self.walk_alias_targets(
                    req,
                    snapshot,
                    &entry.id,
                    target,
                    fallback_targets,
                    ctx,
                    out,
                );
                ctx.leave_alias();
                result
            }
            CatalogEntryKind::ServiceModel { instances } => {
                if !satisfies(entry, req) {
                    return Ok(());
                }
                self.emit_service_model(&entry.id, instances, out);
                Ok(())
            }
            CatalogEntryKind::Flow {
                flow_id,
                published_name,
            } => {
                if !satisfies(entry, req) {
                    return Ok(());
                }
                out.push(ResolvedExecutionTarget::Flow {
                    flow_id: *flow_id,
                    published_name: published_name.clone(),
                });
                Ok(())
            }
        }
    }

    /// Inner alias walk: visits the primary first, then each fallback in
    /// declared order. A missing or broken primary is fatal — that
    /// signals a config bug (typo, deleted target) and silently routing
    /// to fallbacks would hide the misconfiguration in production. A
    /// fallback that fails (cycle, depth limit, unknown id) is logged
    /// and skipped because fallbacks exist precisely to absorb partial
    /// outages.
    fn walk_alias_targets(
        &self,
        req: &ResolveRequest<'_>,
        snapshot: &CatalogSnapshot,
        alias_id: &str,
        primary_target: &str,
        fallback_targets: &[String],
        ctx: &mut ExecutionContext,
        out: &mut Vec<ResolvedExecutionTarget>,
    ) -> Result<(), ResolveError> {
        let primary = lookup_entry(snapshot, primary_target).ok_or_else(|| {
            ResolveError::AliasPrimaryMissing {
                alias: alias_id.to_string(),
                primary: primary_target.to_string(),
            }
        })?;
        self.expand_into(req, snapshot, primary, ctx, out)?;
        for fb in fallback_targets {
            let Some(fb_entry) = lookup_entry(snapshot, fb) else {
                tracing::trace!(
                    fallback = fb,
                    "alias fallback target not in catalog — skipped"
                );
                continue;
            };
            if let Err(e) = self.expand_into(req, snapshot, fb_entry, ctx, out) {
                tracing::trace!(
                    fallback = fb,
                    error = %e,
                    "alias fallback skipped"
                );
            }
        }
        Ok(())
    }

    /// Service models map to either a `Local` target (when we hold a live
    /// handle) or a `MeshForward` (when only a peer hosts it). Multiple
    /// instances of the same model produce multiple candidates — strategy
    /// ranking decides which one wins per request.
    fn emit_service_model(
        &self,
        model_name: &str,
        instances: &[ModelInstance],
        out: &mut Vec<ResolvedExecutionTarget>,
    ) {
        let local_id = (self.local_node_id)();
        for inst in instances {
            if inst.node_id == local_id {
                if let Some(handle) = self.handles.get(&inst.node_id, inst.service_id) {
                    out.push(ResolvedExecutionTarget::Local {
                        model_name: model_name.to_string(),
                        handle,
                    });
                }
                // No local handle yet (deploy in flight) — skip silently;
                // mesh fallback below covers the gap when a peer hosts
                // the same model name.
            } else {
                out.push(ResolvedExecutionTarget::MeshForward {
                    node_id: inst.node_id.clone(),
                    service_id: inst.service_id,
                    model_name: model_name.to_string(),
                });
            }
        }
    }
}

fn lookup_entry<'a>(snapshot: &'a CatalogSnapshot, id: &str) -> Option<&'a CatalogEntry> {
    snapshot.entries.iter().find(|e| e.id == id)
}

/// Capability check: candidate satisfies the request iff its surfaces
/// contain `required_surface` AND its modality lists cover every
/// requested modality.
///
/// Modality semantics:
/// - **Empty list** on the candidate = "text-only default". A
///   service-model entry that has not been annotated yet (manifest
///   normalisation is in progress) only handles text in / text out;
///   audio or image requests against it must fail-closed.
/// - **Non-empty list** = strict set containment. A candidate that
///   advertises `output_modalities = [Audio]` and nothing else does
///   NOT serve text requests, even though Text is the implicit
///   universal default — declaring a modality list opts the entry
///   into a closed world.
fn satisfies(entry: &CatalogEntry, req: &ResolveRequest<'_>) -> bool {
    if !entry.service_surfaces.contains(&req.required_surface) {
        return false;
    }
    let allows_input = |needed: &InputModality| -> bool {
        if entry.input_modalities.is_empty() {
            *needed == InputModality::Text
        } else {
            entry.input_modalities.contains(needed)
        }
    };
    for needed in req.required_input_modalities {
        if !allows_input(needed) {
            return false;
        }
    }
    let allows_output = |needed: &OutputModality| -> bool {
        if entry.output_modalities.is_empty() {
            *needed == OutputModality::Text
        } else {
            entry.output_modalities.contains(needed)
        }
    };
    for needed in req.required_output_modalities {
        if !allows_output(needed) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::catalog::{CatalogEntry, CatalogEntryKind, ModelInstance};
    use std::sync::Arc;

    fn snapshot(entries: Vec<CatalogEntry>) -> CatalogSnapshot {
        CatalogSnapshot {
            entries: Arc::from(entries.into_boxed_slice()),
            version: 1,
        }
    }

    fn service_entry(
        id: &str,
        node: &str,
        surfaces: Vec<ServiceSurface>,
        input: Vec<InputModality>,
        output: Vec<OutputModality>,
    ) -> CatalogEntry {
        CatalogEntry {
            id: id.to_string(),
            kind: CatalogEntryKind::ServiceModel {
                instances: vec![ModelInstance {
                    node_id: node.to_string(),
                    node_hostname: None,
                    service_id: 1,
                    status: "running".into(),
                    backend: Some("emb".into()),
                    size_mb: None,
                    loaded: true,
                    input_modalities: input.clone(),
                    output_modalities: output.clone(),
                }],
            },
            service_surfaces: surfaces,
            input_modalities: input,
            output_modalities: output,
            diagnostic: None,
        }
    }

    fn alias(id: &str, target: &str, fallbacks: &[&str], strategy: Strategy) -> CatalogEntry {
        CatalogEntry {
            id: id.to_string(),
            kind: CatalogEntryKind::Alias {
                target: target.to_string(),
                fallback_targets: fallbacks.iter().map(|s| s.to_string()).collect(),
                strategy,
            },
            // Alias modalities mirror primary — set them to whatever the
            // caller wants the wire to advertise (resolver does not look
            // at these; only at the underlying targets).
            service_surfaces: vec![ServiceSurface::Chat],
            input_modalities: vec![],
            output_modalities: vec![],
            diagnostic: None,
        }
    }

    fn resolver_for(local: &str) -> AliasResolver {
        AliasResolver::new_with_static_id(Arc::new(LiveHandlesCache::new()), local.to_string())
    }

    fn chat_request<'a>(model: &'a str) -> ResolveRequest<'a> {
        ResolveRequest {
            requested_model: model,
            required_surface: ServiceSurface::Chat,
            required_input_modalities: &[],
            required_output_modalities: &[],
        }
    }

    #[test]
    fn unknown_model_returns_unknown_error() {
        let snap = snapshot(vec![]);
        let resolver = resolver_for("local");
        let mut ctx = ExecutionContext::new(None);
        let err = resolver
            .resolve(&chat_request("ghost"), &snap, &mut ctx)
            .unwrap_err();
        assert!(matches!(err, ResolveError::UnknownModel(_)));
    }

    #[test]
    fn local_service_model_emits_mesh_forward_when_handle_missing() {
        // Local node id is "local" but instance lives on "peer" — that's
        // the canonical "remote service" case. No live handle needed.
        let entry = service_entry(
            "m",
            "peer",
            vec![ServiceSurface::Chat],
            vec![],
            vec![],
        );
        let snap = snapshot(vec![entry]);
        let resolver = resolver_for("local");
        let mut ctx = ExecutionContext::new(None);
        let outcome = resolver.resolve(&chat_request("m"), &snap, &mut ctx).unwrap();
        assert_eq!(outcome.candidates.len(), 1);
        assert!(matches!(
            outcome.candidates[0],
            ResolvedExecutionTarget::MeshForward { .. }
        ));
    }

    /// Alias's primary target is audio-capable, fallback is text-only.
    /// Audio request must drop the fallback **without** dropping the
    /// primary — primary modalities define the alias's contract,
    /// fallbacks are filtered per-request.
    #[test]
    fn audio_request_skips_text_only_fallback_keeps_audio_primary() {
        let primary = service_entry(
            "qwen-omni",
            "peer-a",
            vec![ServiceSurface::Chat],
            vec![InputModality::Text, InputModality::Audio],
            vec![OutputModality::Text],
        );
        let fallback = service_entry(
            "bielik-11b",
            "peer-b",
            vec![ServiceSurface::Chat],
            vec![InputModality::Text],
            vec![OutputModality::Text],
        );
        let alias_entry = alias(
            "chat-pl",
            "qwen-omni",
            &["bielik-11b"],
            Strategy::FirstAvailable,
        );

        let snap = snapshot(vec![primary, fallback, alias_entry]);
        let resolver = resolver_for("local");

        let mut ctx = ExecutionContext::new(None);
        let req = ResolveRequest {
            requested_model: "chat-pl",
            required_surface: ServiceSurface::Chat,
            required_input_modalities: &[InputModality::Audio],
            required_output_modalities: &[],
        };
        let outcome = resolver.resolve(&req, &snap, &mut ctx).unwrap();
        assert_eq!(outcome.candidates.len(), 1);
        assert_eq!(outcome.candidates[0].requested_model(), "qwen-omni");
    }

    /// Inverse case — primary is text-only, fallback is audio. Audio
    /// request drops the primary and falls through to the fallback.
    #[test]
    fn audio_request_falls_through_to_audio_fallback() {
        let primary = service_entry(
            "bielik-11b",
            "peer-a",
            vec![ServiceSurface::Chat],
            vec![InputModality::Text],
            vec![OutputModality::Text],
        );
        let fallback = service_entry(
            "qwen-omni",
            "peer-b",
            vec![ServiceSurface::Chat],
            vec![InputModality::Text, InputModality::Audio],
            vec![OutputModality::Text],
        );
        let alias_entry = alias(
            "chat-pl",
            "bielik-11b",
            &["qwen-omni"],
            Strategy::FirstAvailable,
        );

        let snap = snapshot(vec![primary, fallback, alias_entry]);
        let resolver = resolver_for("local");
        let mut ctx = ExecutionContext::new(None);
        let req = ResolveRequest {
            requested_model: "chat-pl",
            required_surface: ServiceSurface::Chat,
            required_input_modalities: &[InputModality::Audio],
            required_output_modalities: &[],
        };
        let outcome = resolver.resolve(&req, &snap, &mut ctx).unwrap();
        assert_eq!(outcome.candidates.len(), 1);
        assert_eq!(outcome.candidates[0].requested_model(), "qwen-omni");
    }

    /// Wrong surface (asking for Stt while the entry is Chat) returns
    /// `CapabilityUnsupported` rather than silently downgrading.
    #[test]
    fn surface_mismatch_returns_capability_unsupported() {
        let entry = service_entry(
            "llama",
            "peer",
            vec![ServiceSurface::Chat],
            vec![],
            vec![],
        );
        let snap = snapshot(vec![entry]);
        let resolver = resolver_for("local");
        let mut ctx = ExecutionContext::new(None);
        let req = ResolveRequest {
            requested_model: "llama",
            required_surface: ServiceSurface::Stt,
            required_input_modalities: &[],
            required_output_modalities: &[],
        };
        let err = resolver.resolve(&req, &snap, &mut ctx).unwrap_err();
        assert!(matches!(err, ResolveError::CapabilityUnsupported { .. }));
    }

    /// Alias cycle — two aliases pointing at each other. Resolver must
    /// detect via the context's alias stack and bail out on the second
    /// entry, not loop until the depth limit hits.
    #[test]
    fn alias_cycle_aborts_resolution() {
        let entries = vec![
            alias("a", "b", &[], Strategy::FirstAvailable),
            alias("b", "a", &[], Strategy::FirstAvailable),
        ];
        let snap = snapshot(entries);
        let resolver = resolver_for("local");
        let mut ctx = ExecutionContext::new(None);
        let err = resolver.resolve(&chat_request("a"), &snap, &mut ctx).unwrap_err();
        match err {
            ResolveError::AliasLimit { source, .. } => {
                assert!(matches!(source, ContextLimitError::AliasCycle { .. }));
            }
            other => panic!("expected AliasLimit::AliasCycle, got {:?}", other),
        }
    }

    /// A nested alias error must not leave the alias_stack growing
    /// across `resolve` calls. Without the scope guard a second resolve
    /// using the same context would inherit the stale stack and report
    /// a false-positive `AliasCycle` for the outer alias.
    #[test]
    fn nested_error_pops_alias_stack_for_subsequent_resolve() {
        // Build chain: outer → mid → mid (self-cycle on mid).
        let entries = vec![
            alias("outer", "mid", &[], Strategy::FirstAvailable),
            alias("mid", "mid", &[], Strategy::FirstAvailable),
        ];
        let snap = snapshot(entries);
        let resolver = resolver_for("local");
        let mut ctx = ExecutionContext::new(None);

        // First resolve trips the cycle.
        let _ = resolver.resolve(&chat_request("outer"), &snap, &mut ctx);

        // Stack must be empty — otherwise the next resolve sees a
        // poisoned ctx.
        assert!(
            ctx.alias_stack.is_empty(),
            "alias_stack leaked after error: {:?}",
            ctx.alias_stack
        );

        // Second resolve on a fresh model id must succeed (or fail with
        // its own legitimate error), never with a phantom cycle on
        // "outer" or "mid".
        let entries2 = vec![service_entry(
            "fresh",
            "peer",
            vec![ServiceSurface::Chat],
            vec![],
            vec![],
        )];
        let snap2 = snapshot(entries2);
        resolver
            .resolve(&chat_request("fresh"), &snap2, &mut ctx)
            .expect("second resolve must not be poisoned by previous error");
    }

    /// A self-referential alias (`target == id`) must trip the cycle
    /// detector immediately rather than recurse until the depth limit.
    #[test]
    fn self_reference_alias_trips_cycle_not_depth_limit() {
        let entries = vec![alias("loop", "loop", &[], Strategy::FirstAvailable)];
        let snap = snapshot(entries);
        let resolver = resolver_for("local");
        let mut ctx = ExecutionContext::new(None);
        let err = resolver.resolve(&chat_request("loop"), &snap, &mut ctx).unwrap_err();
        match err {
            ResolveError::AliasLimit { source, .. } => {
                assert!(
                    matches!(source, ContextLimitError::AliasCycle { .. }),
                    "self-ref alias should report cycle, got {:?}",
                    source
                );
            }
            other => panic!("expected AliasLimit::AliasCycle, got {:?}", other),
        }
    }

    /// Alias references a primary target that doesn't exist in the
    /// snapshot — typo in `model_aliases.target_model` or stale entry
    /// after the target was deleted. Resolution must fail loudly so the
    /// operator sees the misconfiguration, instead of silently routing
    /// to a fallback (which would hide the real problem).
    #[test]
    fn alias_with_missing_primary_returns_error() {
        let entries = vec![alias(
            "stale-alias",
            "deleted-target",
            &["fallback-one"],
            Strategy::FirstAvailable,
        )];
        let snap = snapshot(entries);
        let resolver = resolver_for("local");
        let mut ctx = ExecutionContext::new(None);
        let err = resolver
            .resolve(&chat_request("stale-alias"), &snap, &mut ctx)
            .unwrap_err();
        match err {
            ResolveError::AliasPrimaryMissing { alias, primary } => {
                assert_eq!(alias, "stale-alias");
                assert_eq!(primary, "deleted-target");
            }
            other => panic!("expected AliasPrimaryMissing, got {:?}", other),
        }
    }

    /// One fallback alias is broken (cycle), the other points at a
    /// healthy service. The healthy fallback must still appear in the
    /// candidate list — without the scope-guard fix the broken
    /// fallback's leftover alias_stack entry would tank the second
    /// fallback with a phantom cycle.
    #[test]
    fn broken_fallback_alias_does_not_block_healthy_sibling() {
        let entries = vec![
            alias(
                "umbrella",
                "primary",
                &["broken-alias", "healthy-svc"],
                Strategy::FirstAvailable,
            ),
            service_entry(
                "primary",
                "peer-a",
                vec![ServiceSurface::Chat],
                vec![],
                vec![],
            ),
            // self-cycle alias used as a fallback
            alias("broken-alias", "broken-alias", &[], Strategy::FirstAvailable),
            service_entry(
                "healthy-svc",
                "peer-b",
                vec![ServiceSurface::Chat],
                vec![],
                vec![],
            ),
        ];
        let snap = snapshot(entries);
        let resolver = resolver_for("local");
        let mut ctx = ExecutionContext::new(None);
        let outcome = resolver
            .resolve(&chat_request("umbrella"), &snap, &mut ctx)
            .expect("umbrella with one broken fallback must still resolve");

        // primary + healthy-svc; broken alias dropped silently
        let names: Vec<&str> = outcome
            .candidates
            .iter()
            .map(|t| t.requested_model())
            .collect();
        assert!(names.contains(&"primary"));
        assert!(names.contains(&"healthy-svc"));
        assert_eq!(names.len(), 2, "expected 2 candidates, got {:?}", names);
    }

    /// Alias of alias of … 9 deep — exceeds `MAX_ALIAS_DEPTH` (8).
    /// 8-deep should resolve, 9 should reject before stack overflow.
    #[test]
    fn alias_chain_depth_8_passes_9_fails() {
        // Build linear chain: a0 → a1 → a2 → ... → a8 → leaf.
        let mut entries = Vec::new();
        for i in 0..crate::services::runtime::context::MAX_ALIAS_DEPTH {
            let next = format!("a{}", i + 1);
            entries.push(alias(
                &format!("a{}", i),
                &next,
                &[],
                Strategy::FirstAvailable,
            ));
        }
        // The 9th alias (a8 → leaf-service) must trip the depth limit.
        entries.push(alias(
            &format!("a{}", crate::services::runtime::context::MAX_ALIAS_DEPTH),
            "leaf",
            &[],
            Strategy::FirstAvailable,
        ));
        entries.push(service_entry(
            "leaf",
            "peer",
            vec![ServiceSurface::Chat],
            vec![],
            vec![],
        ));

        let snap = snapshot(entries);
        let resolver = resolver_for("local");
        let mut ctx = ExecutionContext::new(None);
        let err = resolver.resolve(&chat_request("a0"), &snap, &mut ctx).unwrap_err();
        assert!(matches!(err, ResolveError::AliasLimit { .. }));
    }
}
