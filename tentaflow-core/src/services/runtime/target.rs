// =============================================================================
// File: services/runtime/target.rs
// Resolved dispatch target — what the runtime actually has to do once a
// model name is fully resolved through aliases. Distinct from
// `BackendHandle` (transport detail of a local service) because the
// resolver must also express "forward across mesh" and "execute as flow",
// neither of which fits a per-service handle.
// =============================================================================

use crate::services::handles_cache::BackendHandle;

/// Resolution outcome handed off to `ModelRuntimeExecutor::dispatch_one`.
/// Each variant carries exactly the data its branch needs; the executor
/// pattern-matches and routes accordingly. `BackendHandle` does not
/// implement `Debug` (it wraps reqwest/QUIC clients with no Debug bound),
/// so this enum derives only `Clone` and provides a manual Debug impl
/// that prints the variant + tag without descending into the handle.
#[derive(Clone)]
pub enum ResolvedExecutionTarget {
    /// Service backed by a live runtime handle on this node. The executor
    /// calls into HTTP/QUIC/Embedded depending on the underlying handle.
    Local {
        /// Original model name as seen by the client. Carried through so
        /// telemetry can report the requested id, not just the handle.
        model_name: String,
        /// Lokalny service_id z `services` tabeli — uzywany przez
        /// `SttRuntime::transcribe_for_service` zeby wybrac per-service
        /// backend (Local/Http) zarejestrowany przy deployu.
        service_id: i64,
        handle: BackendHandle,
    },
    /// Forward the request to a peer that owns the model. The executor
    /// bumps `ctx.hop_count` (rejecting loops) and dispatches over mesh
    /// QUIC. `service_id` lives in the **target node's** SQLite namespace
    /// — never confuse with a local id.
    MeshForward {
        node_id: String,
        service_id: i64,
        model_name: String,
    },
    /// Execute as a published flow. The executor hands off to
    /// `FlowDispatcher` which walks the DAG; modalities are already
    /// validated by the resolver so the flow is guaranteed compatible
    /// with the request shape.
    Flow {
        flow_id: i64,
        published_name: String,
    },
}

impl std::fmt::Debug for ResolvedExecutionTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local { model_name, .. } => f
                .debug_struct("Local")
                .field("model_name", model_name)
                .field("tag", &self.telemetry_tag())
                .finish(),
            Self::MeshForward {
                node_id,
                service_id,
                model_name,
            } => f
                .debug_struct("MeshForward")
                .field("node_id", node_id)
                .field("service_id", service_id)
                .field("model_name", model_name)
                .finish(),
            Self::Flow {
                flow_id,
                published_name,
            } => f
                .debug_struct("Flow")
                .field("flow_id", flow_id)
                .field("published_name", published_name)
                .finish(),
        }
    }
}

impl ResolvedExecutionTarget {
    /// Stable telemetry tag — slotted into `RouteMetadata.backend_type` so
    /// dashboards can chart dispatch outcomes without inspecting handles.
    pub fn telemetry_tag(&self) -> &'static str {
        match self {
            Self::Local { handle, .. } => match handle {
                BackendHandle::Http(_) => "http",
                BackendHandle::Quic(_) => "quic",
                BackendHandle::Embedded { .. } => "embedded",
            },
            Self::MeshForward { .. } => "mesh_forward",
            Self::Flow { .. } => "flow_engine",
        }
    }

    /// Quick liveness check — local handles consult `BackendHandle::is_alive`,
    /// mesh forwards always count as live (the actual connectivity check
    /// happens when the executor tries to forward), flows are live iff the
    /// flow row exists (the resolver already verified that).
    pub fn is_alive(&self) -> bool {
        match self {
            Self::Local { handle, .. } => handle.is_alive(),
            Self::MeshForward { .. } | Self::Flow { .. } => true,
        }
    }

    /// Lokalny service_id (gdy target=Local). `None` dla mesh forward i
    /// flow (te nie maja lokalnego service ownera).
    pub fn local_service_id(&self) -> Option<i64> {
        match self {
            Self::Local { service_id, .. } => Some(*service_id),
            _ => None,
        }
    }

    /// Originally requested model id. Useful for telemetry + error
    /// messages where we want to report the user-facing name, not the
    /// resolved target's internal identity.
    pub fn requested_model(&self) -> &str {
        match self {
            Self::Local { model_name, .. } | Self::MeshForward { model_name, .. } => model_name,
            Self::Flow { published_name, .. } => published_name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::handles_cache::BackendHandle;

    fn embedded(name: &str) -> BackendHandle {
        BackendHandle::Embedded {
            model_name: name.to_string(),
            node_id: "node".to_string(),
            engine_id: "test-engine".to_string(),
        }
    }

    #[test]
    fn telemetry_tags_cover_every_variant() {
        let local = ResolvedExecutionTarget::Local { service_id: 1,
            model_name: "m".into(),
            handle: embedded("m"),
        };
        assert_eq!(local.telemetry_tag(), "embedded");

        let mesh = ResolvedExecutionTarget::MeshForward {
            node_id: "peer".into(),
            service_id: 1,
            model_name: "m".into(),
        };
        assert_eq!(mesh.telemetry_tag(), "mesh_forward");

        let flow = ResolvedExecutionTarget::Flow {
            flow_id: 7,
            published_name: "chat-pl".into(),
        };
        assert_eq!(flow.telemetry_tag(), "flow_engine");
    }

    #[test]
    fn requested_model_returns_user_facing_name() {
        let flow = ResolvedExecutionTarget::Flow {
            flow_id: 7,
            published_name: "chat-pl".into(),
        };
        assert_eq!(flow.requested_model(), "chat-pl");
    }

    #[test]
    fn embedded_local_is_always_alive() {
        let t = ResolvedExecutionTarget::Local { service_id: 1,
            model_name: "m".into(),
            handle: embedded("m"),
        };
        assert!(t.is_alive());
    }
}
