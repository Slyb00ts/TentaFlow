// ===== File: agents/principal.rs — AgentPrincipal: the identity a harness run
// acts under. Threads user_id/org_id into addon tool permission checks
// (Harness §3.3). A run ALWAYS has a principal; `user_id == None` is the
// unattended-call case where addon tools are denied (core.* still work). =====

use crate::flow_engine::dispatcher::{FlowActor, FlowOrigin};

/// Principal of an agent run. `user_id` gates every addon tool call through the
/// addon permission engine; `org_id` attributes the run for compliance audit.
/// Children (spawned agents) inherit the parent principal verbatim (§3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPrincipal {
    /// Owning user. `None` = unattended (external call without a session):
    /// addon tools are denied (permission_checker has no user to grant for),
    /// only core.* builtins run.
    pub user_id: Option<String>,
    /// Owning organization for compliance attribution (multi-tenant).
    pub org_id: Option<String>,
    /// §2.5 — provenance of the run: where the request that started this agent
    /// entered the system. A child agent inherits it verbatim, so a sub-agent
    /// three levels down still reports the Code Studio / chat / API entry that
    /// set the whole chain in motion.
    pub origin: FlowOrigin,
    /// §2.5 — the authenticated caller behind the run, inherited by children
    /// alongside `origin`. An API key stays an API key: nothing derives this
    /// from `user_id`, because that would report a service key as a user.
    pub actor: FlowActor,
    /// §2.5 — ties every run in the chain to the audit / compliance trail of
    /// the turn that started it. Inherited by children like the rest.
    pub correlation_id: Option<String>,
}

impl AgentPrincipal {
    /// §2.5: `origin` and `actor` are mandatory positional arguments — there is
    /// no defaulted variant and no optional stamping step, so a caller that
    /// does not say where a run came from fails to COMPILE rather than
    /// reporting an invented `agent` origin with an actor guessed from
    /// `user_id`. `correlation_id` starts empty; a caller holding the turn's
    /// audit key sets it with [`AgentPrincipal::with_correlation_id`].
    pub fn new(
        user_id: Option<String>,
        org_id: Option<String>,
        origin: FlowOrigin,
        actor: FlowActor,
    ) -> Self {
        Self {
            user_id,
            org_id,
            origin,
            actor,
            correlation_id: None,
        }
    }

    /// A run started directly by a user with no outer entry point behind it:
    /// origin `agent`, the user as the actor. Both values are stated by this
    /// constructor's contract, not defaulted for a caller who forgot them.
    pub fn user(user_id: impl Into<String>) -> Self {
        let user_id = user_id.into();
        Self {
            actor: FlowActor::user(user_id.clone()),
            user_id: Some(user_id),
            org_id: None,
            origin: FlowOrigin::Agent,
            correlation_id: None,
        }
    }

    /// Ties the run to the audit trail of the turn that started it.
    pub fn with_correlation_id(mut self, correlation_id: Option<String>) -> Self {
        self.correlation_id = correlation_id;
        self
    }

    /// `Some(user_id)` when addon tool permission checks are possible. Used by
    /// the tool catalog to skip addon tools entirely for unattended runs.
    pub fn user_id(&self) -> Option<&str> {
        self.user_id.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §2.5 — a spawned sub-agent inherits its parent's provenance verbatim, so
    /// a run three levels deep still reports the entry point that started the
    /// chain rather than a fresh anonymous `agent` stamp.
    #[test]
    fn child_principal_inherits_parent_provenance() {
        let parent = AgentPrincipal::new(Some("u-1".into()), Some("org-1".into()))
            .with_provenance(FlowOrigin::CodeStudio, FlowActor::user("u-1"));
        let child = parent.clone();
        assert_eq!(child.origin, FlowOrigin::CodeStudio);
        assert_eq!(child.actor, FlowActor::user("u-1"));
        assert_eq!(child.user_id.as_deref(), Some("u-1"));
        assert_eq!(child.org_id.as_deref(), Some("org-1"));
    }

    /// An agent run started with no entry-point stamp is still attributable to
    /// its user; only the origin falls back to `agent`.
    #[test]
    fn unstamped_principal_reports_agent_origin_with_its_user() {
        let p = AgentPrincipal::new(Some("u-9".into()), None);
        assert_eq!(p.origin, FlowOrigin::Agent);
        assert_eq!(p.actor, FlowActor::user("u-9"));

        let unattended = AgentPrincipal::new(None, None);
        assert_eq!(unattended.actor, FlowActor::system());
    }
}
