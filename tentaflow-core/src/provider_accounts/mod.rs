// ===== File: provider_accounts/mod.rs — agent provider accounts, the model =====
//
// An "account" here is the identity a CLI agent runs as at its provider
// (Claude Code, Codex, Grok Build, Muse Code): either a provider LOGIN whose
// token TentaFlow stores, or an organisation API KEY. Migration 156 gives it
// its own org-scoped, replicated tables, which is the whole point — a
// `services` row is node-local, so an account written into one can only ever
// exist on the node that holds it.
//
// This module holds the shapes and the engine catalog; `repository.rs` is the
// only writer, and `sync_capture.rs` the only place a write becomes a ledger
// operation.

pub mod repository;
pub mod sync_capture;

/// One CLI engine an account can belong to.
pub struct EngineDescriptor {
    pub engine_id: &'static str,
    /// Product name as the provider spells it. DATA, not prose: there is
    /// nothing here to translate.
    pub display_name: &'static str,
    /// The engine has a device-code / browser login flow.
    pub supports_login: bool,
    /// The engine can run on a plain organisation API key through the
    /// delegation adapter. Grok and Muse have no such mode: their CLIs
    /// authenticate through their own login only.
    pub supports_api_key: bool,
}

/// The four CLI engines. The ids are the ones `services/coding_agent.rs` and
/// `services/deploy/managed_cli.rs` already use; spelling one differently here
/// would create an account nothing can run.
///
/// `supports_api_key` names the same two engines as the org-key path in
/// `dispatch/code_studio.rs` (`CREDENTIAL_ENGINES`). That constant belongs to
/// the vault path this feature replaces and goes away with it; until the
/// consumers switch, the two lists are read by two different code paths and
/// must stay in step.
pub const AGENT_ENGINES: &[EngineDescriptor] = &[
    EngineDescriptor {
        engine_id: "claude-code",
        display_name: "Claude Code",
        supports_login: true,
        supports_api_key: true,
    },
    EngineDescriptor {
        engine_id: "codex",
        display_name: "Codex",
        supports_login: true,
        supports_api_key: true,
    },
    EngineDescriptor {
        engine_id: "grok-build",
        display_name: "Grok Build",
        supports_login: true,
        supports_api_key: false,
    },
    EngineDescriptor {
        engine_id: "muse-code",
        display_name: "Muse Code",
        supports_login: true,
        supports_api_key: false,
    },
];

pub fn engine(engine_id: &str) -> Option<&'static EngineDescriptor> {
    AGENT_ENGINES.iter().find(|e| e.engine_id == engine_id)
}

/// Admissible `provider_accounts.scope` values (mirrors the column CHECK).
pub const ACCOUNT_SCOPES: &[&str] = &["global", "user"];

/// Admissible `provider_accounts.credential_kind` values.
pub const CREDENTIAL_KINDS: &[&str] = &["api_key", "provider_login"];

/// Admissible `provider_accounts.status` values.
pub const ACCOUNT_STATUSES: &[&str] = &["pending", "active", "needs_login", "disabled"];

/// Admissible `provider_account_grants.subject_type` values.
pub const GRANT_SUBJECT_TYPES: &[&str] = &["user", "group", "org"];

/// Admissible `agent_runtime_engines.install_state` values.
pub const INSTALL_STATES: &[&str] = &["absent", "installing", "installed", "error"];

/// Admissible `provider_account_node_state.runtime_state` values.
pub const RUNTIME_STATES: &[&str] = &["absent", "materializing", "ready", "error"];

/// What a stored credential is bound to, as additional authenticated data.
///
/// AES-GCM authenticates the ciphertext, not its location: without this,
/// anyone with write access to the database could move account A's credential
/// row onto account B and have it decrypt there. Mirrors
/// `code_studio::vault`'s context for the same reason.
pub fn credential_context(account_id: &str) -> Vec<u8> {
    format!("provider-account:{account_id}").into_bytes()
}

/// One `provider_accounts` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRecord {
    pub account_id: String,
    pub org_id: String,
    pub engine_id: String,
    pub display_name: String,
    pub scope: String,
    pub owner_user_id: Option<String>,
    pub credential_kind: String,
    pub provider_subject: Option<String>,
    pub plan_label: Option<String>,
    pub home_node_id: Option<String>,
    pub status: String,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
}

impl AccountRecord {
    /// A user account is usable by its owner and nobody else — not even an
    /// administrator (plan §2.2). A global account is decided by its grants.
    pub fn is_owned_by(&self, user_id: &str) -> bool {
        self.owner_user_id.as_deref() == Some(user_id)
    }
}

/// A new account. `account_id` is caller-supplied (a fresh UUIDv4) so the row
/// and its ledger capture share one identity from the first write.
#[derive(Debug, Clone)]
pub struct NewAccount {
    pub account_id: String,
    pub org_id: String,
    pub engine_id: String,
    pub display_name: String,
    pub scope: String,
    pub owner_user_id: Option<String>,
    pub credential_kind: String,
    pub created_by: String,
}

/// The fields an administrator may edit after creation. `None` leaves the
/// stored value alone; `engine_id`, `scope` and `credential_kind` are absent on
/// purpose — changing any of them would make the stored credential meaningless
/// without deleting it, and deleting an account is the honest way to say that.
#[derive(Debug, Clone, Default)]
pub struct AccountUpdate {
    pub display_name: Option<String>,
    pub status: Option<String>,
    pub plan_label: Option<String>,
    pub home_node_id: Option<String>,
}

/// One `provider_account_grants` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRecord {
    pub account_id: String,
    pub subject_type: String,
    pub subject_id: String,
    pub granted_by: String,
    pub granted_at: String,
}

/// One grant as the caller asks for it (A04 sends the whole desired set).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantInput {
    pub subject_type: String,
    pub subject_id: String,
}

/// The credential row WITHOUT its material. Everything a reader needs to know
/// whether a node is current, and nothing it needs a key for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialSummary {
    pub account_id: String,
    pub revision: i64,
    pub material_sha256: String,
    pub provider_subject: Option<String>,
    pub expires_at: Option<String>,
    pub refreshed_at: String,
    pub refreshed_by_node: Option<String>,
}

/// What a credential write carries besides the material itself.
#[derive(Debug, Clone, Default)]
pub struct CredentialMeta {
    pub provider_subject: Option<String>,
    pub expires_at: Option<String>,
    pub refreshed_by_node: Option<String>,
    /// Who caused this write, for the audit row. `None` is a write nobody
    /// requested — a refresh the home node performed on its own schedule.
    pub actor: Option<String>,
}

/// The outcome of a revision-CAS credential write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialWrite {
    /// Stored; the row now carries this revision.
    Applied { revision: i64 },
    /// The same material at the same revision — a replay, not a change.
    Unchanged { revision: i64 },
    /// The same revision claimed for DIFFERENT material. The row is left
    /// exactly as it was and the account moves to `needs_login`: two writers
    /// minted one revision, and guessing which one is current is how a rotated
    /// token gets burned.
    Conflict { revision: i64 },
    /// An older revision than the stored one. Nothing is written.
    Stale { revision: i64 },
}

/// One `provider_account_sessions` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub account_id: String,
    pub session_id: String,
    pub user_id: String,
    pub agent_id: Option<String>,
    pub workspace_id: Option<String>,
    pub node_id: String,
    pub vendor_session_id: Option<String>,
    pub started_at: String,
    pub last_used_at: Option<String>,
}

/// One `provider_account_node_state` row — what THIS node did with an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeStateRecord {
    pub account_id: String,
    pub node_id: String,
    pub applied_revision: i64,
    pub runtime_state: String,
    pub last_error: Option<String>,
    pub updated_at: String,
}

/// One `agent_runtime_nodes` row with the engines installed on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeNodeRecord {
    pub node_id: String,
    pub receives_accounts: bool,
    pub updated_by: Option<String>,
    pub updated_at: String,
    pub engines: Vec<RuntimeEngineRecord>,
}

/// One `agent_runtime_engines` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEngineRecord {
    pub node_id: String,
    pub engine_id: String,
    pub install_state: String,
    pub version: Option<String>,
    pub installed_at: Option<String>,
    pub last_error: Option<String>,
}

/// Filters for the admin list (A01).
#[derive(Debug, Clone, Default)]
pub struct AccountFilter {
    pub engine_id: Option<String>,
    pub scope: Option<String>,
    /// Case-insensitive substring of the display name or the provider subject.
    pub query: Option<String>,
}
