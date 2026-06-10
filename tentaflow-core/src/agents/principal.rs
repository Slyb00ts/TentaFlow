// ===== File: agents/principal.rs — AgentPrincipal: the identity a harness run
// acts under. Threads user_id/org_id into addon tool permission checks
// (Harness §3.3). A run ALWAYS has a principal; `user_id == None` is the
// unattended-call case where addon tools are denied (core.* still work). =====

/// Principal of an agent run. `user_id` gates every addon tool call through the
/// addon permission engine; `org_id` attributes the run for compliance audit.
/// Children (spawned agents) inherit the parent principal verbatim (§3.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentPrincipal {
    /// Owning user. `None` = unattended (external call without a session):
    /// addon tools are denied (permission_checker has no user to grant for),
    /// only core.* builtins run.
    pub user_id: Option<String>,
    /// Owning organization for compliance attribution (multi-tenant).
    pub org_id: Option<String>,
}

impl AgentPrincipal {
    pub fn new(user_id: Option<String>, org_id: Option<String>) -> Self {
        Self { user_id, org_id }
    }

    /// A principal with a concrete user — addon tools can be permission-checked.
    pub fn user(user_id: impl Into<String>) -> Self {
        Self {
            user_id: Some(user_id.into()),
            org_id: None,
        }
    }

    /// `Some(user_id)` when addon tool permission checks are possible. Used by
    /// the tool catalog to skip addon tools entirely for unattended runs.
    pub fn user_id(&self) -> Option<&str> {
        self.user_id.as_deref()
    }
}
