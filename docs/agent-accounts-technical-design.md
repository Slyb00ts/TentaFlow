# Agent accounts — technical design

Sources: `docs/agent-accounts-global-user-plan.md` (§2, §4; §5 decisions accepted), `mockups/agent-accounts-20260917/` (A01–A04, N01, U01, G01, C01, C02).

Pinned facts verified now: highest migration = **155** (`tentaflow-core/src/db/migrations.rs:963-967`) → new numbers start at **156**. `SCHEMA_VERSION` = **29** (`tentaflow-protocol/src/envelope.rs:256`) → **30**. Last `MessageBody` family = `TentaQuantBody` (`tentaflow-protocol/src/message_body.rs:8387`).

---

## A. Data model

### A.1 New tables — migration 156 `provider_accounts`

All in the main DB (`tentaflow.db`), because they must travel through the Sync Ledger. Style follows `CODING_AGENT_ACCOUNT_ACCESS` (`tentaflow-core/src/db/migrations.rs:1030`) and `sync_nodes` (`:5774`).

```sql
CREATE TABLE provider_accounts (
    account_id      TEXT PRIMARY KEY,
    org_id          TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    engine_id       TEXT NOT NULL,                 -- value of CREDENTIAL_ENGINES
    display_name    TEXT NOT NULL,
    scope           TEXT NOT NULL CHECK(scope IN ('global','user')),
    owner_user_id   TEXT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
    credential_kind TEXT NOT NULL CHECK(credential_kind IN ('api_key','provider_login')),
    provider_subject TEXT NULL,                    -- provider identity, set at first successful login
    plan_label      TEXT NULL,                     -- e.g. "Max 20x", shown in A01/A03
    home_node_id    TEXT NULL REFERENCES sync_nodes(node_id) ON DELETE SET NULL,
    status          TEXT NOT NULL DEFAULT 'pending'
                      CHECK(status IN ('pending','active','needs_login','disabled')),
    created_by      TEXT NOT NULL REFERENCES user_accounts(id) ON DELETE RESTRICT,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    CHECK ((scope = 'user') = (owner_user_id IS NOT NULL))
);
CREATE UNIQUE INDEX idx_provider_accounts_subject
    ON provider_accounts(org_id, engine_id, provider_subject)
    WHERE provider_subject IS NOT NULL;
CREATE INDEX idx_provider_accounts_owner ON provider_accounts(owner_user_id, engine_id);

CREATE TABLE provider_account_grants (
    account_id   TEXT NOT NULL REFERENCES provider_accounts(account_id) ON DELETE CASCADE,
    subject_type TEXT NOT NULL CHECK(subject_type IN ('user','group','org')),
    subject_id   TEXT NOT NULL,                    -- '' for subject_type='org'
    granted_by   TEXT NOT NULL,
    granted_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    PRIMARY KEY (account_id, subject_type, subject_id)
);
CREATE INDEX idx_provider_account_grants_subject ON provider_account_grants(subject_type, subject_id);

CREATE TABLE provider_account_credentials (
    account_id       TEXT PRIMARY KEY REFERENCES provider_accounts(account_id) ON DELETE CASCADE,
    revision         INTEGER NOT NULL DEFAULT 1,
    material_enc     TEXT NOT NULL,                -- SettingsCipher::encrypt_bound, context below
    material_sha256  TEXT NOT NULL,                -- of the PLAINTEXT; idempotence + CAS diagnostics
    provider_subject TEXT NULL,
    expires_at       TEXT NULL,
    refreshed_at     TEXT NOT NULL,
    refreshed_by_node TEXT NULL
);

CREATE TABLE provider_account_sessions (
    account_id        TEXT NOT NULL REFERENCES provider_accounts(account_id) ON DELETE CASCADE,
    session_id        TEXT NOT NULL,
    user_id           TEXT NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
    agent_id          TEXT NULL,
    workspace_id      TEXT NULL,
    node_id           TEXT NOT NULL,
    vendor_session_id TEXT NULL,
    started_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    last_used_at      TEXT NULL,
    PRIMARY KEY (account_id, session_id)
);
CREATE UNIQUE INDEX idx_provider_account_sessions_vendor
    ON provider_account_sessions(account_id, user_id, agent_id, workspace_id)
    WHERE vendor_session_id IS NOT NULL;
CREATE INDEX idx_provider_account_sessions_user ON provider_account_sessions(account_id, user_id);
```

Migration 161 replaces that index, which never fired: the vendor conversation is not
among its columns (`agent_id` was NULL for every row the session recorder wrote, and
SQLite treats NULLs as distinct). The index is now drawn on the conversation alone —
one vendor conversation is driven by one recorded session, and `upsert_session` clears
the row a conversation was recorded under before recording it for the session that has
it now (`db/migrations.rs:1395-1398`):

```sql
DROP INDEX IF EXISTS idx_provider_account_sessions_vendor;
CREATE UNIQUE INDEX idx_provider_account_sessions_vendor
    ON provider_account_sessions(vendor_session_id)
    WHERE vendor_session_id IS NOT NULL;
```

Node-local, **never synced** (it is per-node truth, like `provider_account_node_state.applied_revision`):

```sql
CREATE TABLE provider_account_node_state (
    account_id       TEXT NOT NULL REFERENCES provider_accounts(account_id) ON DELETE CASCADE,
    node_id          TEXT NOT NULL,
    applied_revision INTEGER NOT NULL DEFAULT 0,
    runtime_state    TEXT NOT NULL DEFAULT 'absent'
                       CHECK(runtime_state IN ('absent','materializing','ready','error')),
    last_error       TEXT NULL,
    updated_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    PRIMARY KEY (account_id, node_id)
);

CREATE TABLE agent_runtime_nodes (            -- N01 left column + "Otrzymuje konta"
    node_id           TEXT PRIMARY KEY REFERENCES sync_nodes(node_id) ON DELETE CASCADE,
    receives_accounts INTEGER NOT NULL DEFAULT 0 CHECK(receives_accounts IN (0,1)),
    updated_by        TEXT NULL,
    updated_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);

CREATE TABLE agent_runtime_engines (          -- N01 matrix cells
    node_id       TEXT NOT NULL REFERENCES agent_runtime_nodes(node_id) ON DELETE CASCADE,
    engine_id     TEXT NOT NULL,
    install_state TEXT NOT NULL CHECK(install_state IN ('absent','installing','installed','error')),
    version       TEXT NULL,
    installed_at  TEXT NULL,
    last_error    TEXT NULL,
    PRIMARY KEY (node_id, engine_id)
);
```

`agent_runtime_nodes` / `agent_runtime_engines` ARE synced (the admin edits them from any node; N01 is a fleet view) — see §C.1. `provider_account_node_state` is not.

Vault context string: `format!("provider-account:{account_id}")`, bound via `SettingsCipher::encrypt_bound` — the same bound-context mechanism the Code Studio vault uses (`tentaflow-core/src/code_studio/vault.rs:402`, `:627`); the cipher itself is in `tentaflow-core/src/crypto/mod.rs`.

### A.2 Migration 157 `retire_coding_agent_account_model` — `MigrationStep::Rust`

Data moved (all inside one transaction, in this order):

| From | To | Rule |
|---|---|---|
| `services` rows whose `config_json` has `account_id` (written by `tentaflow-core/src/services/coding_agent.rs:22` `ensure_account_config`) | one `provider_accounts` row per service | `account_id` kept verbatim as PK; `scope='global'`; `credential_kind='provider_login'`; `engine_id` from the service's engine; `display_name` = service name; `home_node_id` = the service's node; `status='needs_login'` unless a credential row lands below; `created_by` = service creator or the org admin fallback |
| `coding_agent_account_grants(service_id,user_id)` (`migrations.rs:1529`) | `provider_account_grants(account_id,'user',user_id,granted_by)` | joined through the service row |
| `coding_agent_session_owners(service_id,session_id,vendor_session_id,user_id)` (`migrations.rs:1535`) | `provider_account_sessions` | `node_id` = the service's node; `agent_id`/`workspace_id` NULL (not known in the old model) |
| `services.config_json.account_id`, `.account_dir` | stripped from the JSON | the service row survives only as an engine runtime, see §D.2 |

Dropped in the same step: `coding_agent_account_grants`, `coding_agent_session_owners`, `coding_agent_account_moves` (`migrations.rs:973`). The FK map at `migrations.rs:4330-4332` loses its three entries.

> **Corrected (2026-09-20).** Only ONE of the three was ever dropped, and not in
> this step. The ladder is append-only: rung **149** (`migrations.rs:972`,
> DDL const `CODING_AGENT_ACCOUNT_MOVES` at `:1259`) still CREATES
> `coding_agent_account_moves` exactly as it did, because a database that already
> ran it has it recorded as applied and editing that rung would desynchronise the
> ladder instead of removing anything.
> The table is dropped by the separate rung **163**
> (`drop_coding_agent_account_moves`, `migrations.rs:1040`, const
> `DROP_CODING_AGENT_ACCOUNT_MOVES` at `:1292`) — a fresh database creates
> it at 149 and drops it at 163.
>
> `coding_agent_account_grants` and `coding_agent_session_owners` were NOT
> dropped and are not dead: rung 160 (`adopt_coding_agent_accounts`,
> `migrations.rs:1025`) adopts their rows into `provider_account_grants` /
> `provider_account_sessions` and deliberately keeps the old tables and their
> rows — "nothing the running code still reads may move" — and
> `services/coding_agent.rs` still reads both (`:211`, `:221`, `:316`, `:342`).
> They are created at `migrations.rs:1529` and `:1535`. The FK map at
> `migrations.rs:4330-4332` therefore KEEPS its three entries, one line per
> `(table, column)` pair (`coding_agent_account_grants.user_id`,
> `coding_agent_account_grants.granted_by`,
> `coding_agent_session_owners.user_id`). The line numbers this paragraph
> originally carried (`:973`, `:3806-3808`) no longer point at any of it, and the
> citations this paragraph now carries were corrected on 2026-09-21.
>
> **Corrected (2026-09-22).** `coding_agent_account_grants` is now dropped by
> rung **165** (`drop_coding_agent_account_grants`). Its one reader,
> `services/coding_agent.rs::account_permission`, answered from it while the
> account screen wrote only `provider_account_grants`, so a grant revoked in A04
> kept working on the service console and a grant given in A04 never reached it.
> `account_permission` now calls `provider_accounts::repository::user_may_use_account`
> for the account the service row names. `coding_agent_session_owners` stays: it
> is still the session-ownership table of that console.

> **Corrected (2026-09-22, later the same day).** The owner decided to remove
> the entire old "agent account = services row" path rather than keep it
> beside the new one. `coding_agent_session_owners` and its index are now
> dropped by rung **166** (`drop_coding_agent_legacy_service_path`), which also
> deletes every surviving `services` row with `deploy_method =
> 'native_managed_cli'` — the coding-agent-as-a-service-row itself.
> `services/coding_agent.rs` no longer has an `account_permission`,
> `execute_authorized`, `execute_chat`, `lock_account` or model-discovery
> cache: only the on-disk account layout (`account_root`,
> `prepare_account_root`, `purge_account_credentials`, …) and the bridge HTTP
> client (`call_bridge`, `route`) survive, shared with
> `services::agent_runtime`. `Transport::AgentRpc` and
> `DeployMethod::NativeManagedCli` are gone from the Rust enums;
> `ServicePayload::ReqAgent`/`ResAgent` and `MeshCommandResponsePayload::
> AgentRpcResult` are gone from the wire (`SCHEMA_VERSION` 31 → 32). Deploying
> a managed-CLI manifest as a service is now refused before a `DeployMethod`
> is even resolved (`dispatch::handlers::service_manifest_deploy`,
> `mesh::command_executor::handle_service_deploy_remote`), and the catalog
> tile for such an engine opens the "Konta agentów" screen instead of the
> deploy wizard.

**Credentials are NOT migrated.** `code_agent_credentials` lives in the Code Studio *content* DB (`tentaflow-core/src/code_studio/db.rs:243`), is keyed `(org_id,node_id,engine_id)` and is encrypted with the **per-node** `SettingsCipher` key — a platform migration has neither the pool nor the cipher, and a cross-DB adoption hook would be exactly the parallel path the rules forbid. The bridge-held provider logins (`accounts/<uuid>/` on disk) are likewise unreachable from SQL. Therefore:

- content-DB `STEPS` gains step `(2, "DROP TABLE code_agent_credentials;")` in `tentaflow-core/src/code_studio/db.rs:83`, and the idempotence test's table checks (`:103`, `:322`, `:427`) lose the entry;
- migration 157 sets every adopted account to `status='needs_login'`;
- the operator re-runs A02 once per account, and re-enters one API key per `api_key` account. Flagged in §H-1.

### A.3 Deleted in the same increment

> **Removed / did not land (2026-09-20).** This table is a PLAN inventory, not a
> description of the tree: every line number in it points at the code as it stood
> when the design was written. The manual "move an agent account between nodes"
> mechanism is the part that was removed, by WP8 — `services/account_move.rs`
> (and its `Manifest`, phases, `operate`/`start`/`receive`/`recover`), the
> bridge's `transfer.rs`, `MeshCommandType::AgentAccountMove` with its executor
> branch, `services::mod`'s `mod account_move;`, the `account.move` dispatch
> branch, the frontend's `mountAccountMove` and every `agent_accounts.move_*`
> key. `SCHEMA_VERSION` is now **31** (`tentaflow-protocol/src/envelope.rs`),
> which refuses a peer that still sends the variant at connect. The three rows
> naming `account_move` itself, plus the last row (the frontend window and its
> `move_*` keys), are WP8's; the rows between them belong to WP1–WP6 and are read
> the same way — as the plan, against line numbers that have since moved. Nothing
> here is a navigation aid today.

| Artifact | Path |
|---|---|
| whole file `account_move.rs` (Manifest, phases, `ensure_service_mutation_allowed:119`, `operate:243`, `start:268`, `receive:525`, `recover:741`) | `tentaflow-core/src/services/account_move.rs` |
| `mod account_move;` + `account.move` branch (`:11653`) | `tentaflow-core/src/services/mod.rs`, `tentaflow-core/src/dispatch/handlers.rs:11653` |
| `MeshCommandType::AgentAccountMove` (`:708`) + executor branch (`:576`) | `tentaflow-protocol/src/mesh.rs:708`, `tentaflow-core/src/mesh/command_executor.rs:576` |
| account commands inside `execute_authorized` (`account.access`, `account.rename`, `account.grants.set`, `account.list`, `auth.start` admin gate at `:291`), `account_permission:125`, `lock_account:176`, `prepare_account_directory:56`, `account_directory:41`, `ensure_account_config:20`, `monitor_session:374` | `tentaflow-core/src/services/coding_agent.rs:20,41,56,125,176,189,291,374,417` |
| vault agent-credential half: `put_agent_credential:592`, `get_agent_credential:660`, `AgentCredentialRecord:725`, `list_agent_credentials:757`, `get_agent_credential_record:778`, `delete_agent_credential:798`, `agent_credential_context:831` | `tentaflow-core/src/code_studio/vault.rs` |
| `agent_credentials_list_v1:2467`, `agent_credential_set_v1:2493`, `agent_credential_delete_v1:2548` | `tentaflow-core/src/dispatch/code_studio.rs` |
| `resolve_harness_flow:2994` synthetic-flow branch that injects `delegate_cli.service_id` | `tentaflow-core/src/dispatch/code_studio.rs:2994` |
| `SessionInfo.agent_service_id` (`:109`) and its producer/consumers | `tentaflow-protocol/src/code_studio.rs:109`, `tentaflow-core/src/dispatch/code_studio.rs:2864`, `tentaflow-core/src/flow_engine/node_adapters/delegate_cli.rs:1063` |
| `DelegationConfig.service_id` (mandatory today, `parse:114`) | `tentaflow-core/src/flow_engine/node_adapters/delegate_cli.rs:114`; seed block schema `tentaflow-core/src/db/seed.rs:752-759` and `code_harness_flow_json:2111` |
| bridge `account_busy` + per-session credential copy: `auth_status:811`, `auth_start:970-972`, `create_session` lease check `:1336`, `reconcile_session_credential:1720`, credential copy inside `prepare_session_profile:1649`, its rollback in `rollback_session_start:1772` | `tentaflow-containers/agents/native/coding-agent-bridge/src/main.rs` |
| frontend `mountAccountMove:253` and every `agent_accounts.move_*` i18n key | `tentaflow-core/www/js/modules/coding-agent.js:253`, `tentaflow-core/www/i18n/{pl,en,de,es,fr}.json` (pl `:8340-8371`) |

### A.4 Retiring the `AgentCredential*` wire variants

`MessageBody` is CBOR-tagged **by variant NAME** (proved by `events::tests::message_body_is_tagged_by_variant_name`), so deleting variants shifts nothing. The four variants at `tentaflow-protocol/src/code_studio.rs:1621-1657` plus `AgentCredentialInfo:614` and the golden-pin entry `("AgentCredentialInfo", 8, 0x8739_d280_a6ae_8d9f)` at `:1795` are **deleted outright**. The only exposure is a peer that still sends them; that is closed by:

1. `SCHEMA_VERSION` 29 → **30** (`tentaflow-protocol/src/envelope.rs:256`) — old/new binaries reject each other at handshake, which is the documented mesh rule;
2. the dashboard is served by the same binary, so no browser can hold an older encoder.

No deprecation window, no `#[serde(other)]` catch-all. `ServiceAgentRequest` (`tentaflow-core/src/dispatch/handlers.rs:11556-11665`) **stays** — it keeps carrying model-sync and chat commands; only its account branches die.

---

## B. Wire contract

New module `tentaflow-protocol/src/provider_account.rs`, new family appended **after** `TentaQuantBody` at `tentaflow-protocol/src/message_body.rs:8387`:

```rust
ProviderAccountBody(ProviderAccountPayload)
```

Handler family registered like `tentaflow-core/src/dispatch/tentanas.rs:4194`:
`#[handler(variant = "ProviderAccountBody", since = (1,0))] #[policy(UserSession)] #[observed] async fn provider_account_dispatch(req: ProviderAccountPayload, ctx: &HandlerContext)`. Admin-only variants take their own `PowerUser`/`org_admin` check inside, as `dispatch/ml_studio.rs` does.

### B.1 `ProviderAccountPayload` variants

| Variant | Fields | Actor |
|---|---|---|
| `AccountListRequest` | `engine_id: Option<String>`, `scope: Option<String>`, `query: Option<String>` | admin |
| `AccountListResponse` | `accounts: Vec<ProviderAccountInfo>`, `engines: Vec<EngineSummary>` | |
| `AccountGetRequest` | `account_id: String` | admin |
| `AccountGetResponse` | `account: ProviderAccountInfo`, `grants: Vec<GrantEntry>`, `sessions: Vec<AccountSessionInfo>`, `nodes: Vec<AccountNodeInfo>`, `agents: Vec<AccountAgentInfo>` | A03 three tabs |
| `AccountCreateRequest` | `engine_id`, `display_name`, `scope`, `owner_user_id: Option<String>`, `credential_kind` | admin; user-scope also from U01 |
| `AccountCreateResponse` | `account: ProviderAccountInfo` | |
| `AccountUpdateRequest` | `account_id`, `display_name: Option<String>`, `status: Option<String>` | admin |
| `AccountDeleteRequest` | `account_id` | admin / owner |
| `CredentialSetRequest` | `account_id`, `material: String` | `api_key` accounts only |
| `CredentialClearRequest` | `account_id` | |
| `LoginStartRequest` | `account_id`, `node_id: Option<String>` | A02 step 1 |
| `LoginStartResponse` | `login_id`, `node_id`, `verification_url`, `instruction_key`, `expires_at` | A02 step 2 |
| `LoginInputRequest` | `login_id`, `value: String` | A02 step 3 (pasted code) |
| `LoginStatusRequest` | `login_id` | polled |
| `LoginStatusResponse` | `state: String` (`awaiting_open`/`awaiting_input`/`verifying`/`succeeded`/`failed`), `provider_subject: Option<String>`, `plan_label: Option<String>`, `message_key: Option<String>` | A02 step 4 |
| `LoginCancelRequest` | `login_id` | |
| `GrantsSetRequest` | `account_id`, `grants: Vec<GrantEntry>` | A04 (full replace, one write) |
| `GrantsSetResponse` | `grants: Vec<GrantEntry>` | |
| `SessionListRequest` / `SessionListResponse` | `account_id` / `sessions: Vec<AccountSessionInfo>` | A03 |
| `SessionRevokeRequest` | `account_id`, `session_id` | A03 |
| `MyAccountListRequest` | `engine_id: Option<String>` | U01, any user |
| `MyAccountListResponse` | `accounts: Vec<MyAccountInfo>` | |
| `RuntimeListRequest` / `RuntimeListResponse` | — / `nodes: Vec<RuntimeNodeInfo>` | N01 |
| `RuntimeSetReceivesAccountsRequest` | `node_id`, `enabled: bool` | N01 toggle |
| `RuntimeInstallRequest` / `RuntimeUninstallRequest` | `node_id`, `engine_id` | N01 |
| `RuntimeStatusResponse` | `node: RuntimeNodeInfo` | reply to all three |
| `AccountOpAck` | `account_id: Option<String>`, `ok: bool`, `message_key: Option<String>` | generic ack |

Shared structs (every field added later must carry `#[serde(default)]`):

```rust
pub struct ProviderAccountInfo {
    pub account_id: String, pub engine_id: String, pub display_name: String,
    pub scope: String, pub owner_user_id: Option<String>, pub owner_display_name: Option<String>,
    pub credential_kind: String, pub provider_subject: Option<String>, pub plan_label: Option<String>,
    pub status: String, pub home_node_id: Option<String>, pub home_node_name: Option<String>,
    pub credential_revision: i64, pub expires_at: Option<String>,
    pub grant_count: u32, pub session_count: u32, pub agent_count: u32,
    pub updated_at: String,
}
pub struct GrantEntry { pub subject_type: String, pub subject_id: String,
                        pub display_name: String, pub member_count: Option<u32> }
pub struct AccountSessionInfo { pub session_id: String, pub user_id: String,
    pub user_display_name: String, pub agent_id: Option<String>, pub agent_name: Option<String>,
    pub workspace_id: Option<String>, pub workspace_name: Option<String>,
    pub node_id: String, pub node_name: String, pub started_at: String, pub last_used_at: Option<String> }
pub struct AccountNodeInfo { pub node_id: String, pub node_name: String,
    pub receives_accounts: bool, pub applied_revision: i64, pub runtime_state: String,
    pub last_error: Option<String> }
pub struct AccountAgentInfo { pub agent_id: String, pub agent_name: String, pub bind_mode: String }
pub struct MyAccountInfo { pub account_id: String, pub engine_id: String, pub display_name: String,
    pub scope: String, pub status: String, pub provider_subject: Option<String>,
    pub plan_label: Option<String>, pub can_login: bool, pub can_delete: bool,
    pub last_used_at: Option<String> }
pub struct RuntimeNodeInfo { pub node_id: String, pub node_name: String, pub online: bool,
    pub sandbox_capable: bool, pub receives_accounts: bool,
    pub engines: Vec<RuntimeEngineInfo>, pub account_count: u32 }
pub struct RuntimeEngineInfo { pub engine_id: String, pub install_state: String,
    pub version: Option<String>, pub last_error: Option<String> }
pub struct EngineSummary { pub engine_id: String, pub display_name: String,
    pub supports_login: bool, pub supports_api_key: bool }
```

Names follow `tentaflow-core/www/js/modules/analytics.js` conventions: Core resolves `display_name` / `node_name`; the UI never renders a bare UUID.

### B.2 Mesh

A login and an install always execute **on the target node**, on behalf of a user who is not that node's session. Replacing the unsigned `MeshCommandType::AgentRpc{..., user_id}` (`tentaflow-protocol/src/mesh.rs:617`, forwarded at `tentaflow-core/src/dispatch/handlers.rs:11629`) for these paths:

```rust
// tentaflow-protocol/src/mesh.rs — appended after the last MeshCommandType variant
ProviderAccountOp {
    assertion: SessionAssertion,    // reuse of the type at mesh.rs:727
    op: String,                     // "login.start" | "login.input" | "login.status" | "login.cancel"
                                    // | "runtime.install" | "runtime.uninstall" | "session.revoke"
    payload_cbor: Vec<u8>,          // a ProviderAccountPayload variant
},
ProviderCredentialSubmit {
    assertion: SessionAssertion,
    account_id: String,
    base_revision: i64,             // CAS — the revision the satellite started from
    material: Vec<u8>,              // plaintext under the mesh channel, same trust model as
                                    // the shared-secret ledger op (core_materializer.rs:426)
    provider_subject: Option<String>,
    expires_at: Option<String>,
},
```

Both modelled on `MeshCommandType::CodeStudioOp` (`mesh.rs:628`) and executed next to `handle_agent_rpc` (`tentaflow-core/src/mesh/command_executor.rs:1318`).

Actor authentication: the calling node mints a `SessionAssertion` exactly as `tentaflow-core/src/code_studio/remote_proxy.rs:249` (`forward`) does via `assertion::issue` (`tentaflow-core/src/code_studio/assertion.rs:452`); the target verifies with `assertion::verify` (`:756`), which already checks `aud`, `org`, `rbac_rev`, `jti` replay and `args_digest`. For an account op there is no workspace, so `workspace` carries the `WORKSPACE_PENDING` sentinel (`remote_proxy.rs:231`) and `sub` is the account id; `caps` carries `provider_account.login` / `provider_account.admin`. `args_digest` binds the CBOR payload. **`AgentRpc` keeps its `user_id` for the model/chat commands it still serves; no account decision is ever made from it again.**

---

## C. Sync

### C.1 Core sync descriptors

Added to `CORE_SYNC_DESCRIPTORS` in `tentaflow-core/src/sync/core_registry.rs` (kinds appended to `CoreSyncResourceKind:12-112`, descriptors appended after `CodeWorkspaceMember:636`, which is the composite-PK precedent):

| Kind | `table_name` | `resource_type` | `primary_key_column` | `scope` | `partition_suffix` |
|---|---|---|---|---|---|
| `ProviderAccount` | `provider_accounts` | `core.provider_account` | `account_id` | `Organization` | `agent-accounts` |
| `ProviderAccountGrant` | `provider_account_grants` | `core.provider_account_grant` | `account_id,subject_type,subject_id` | `Organization` | `agent-accounts` |
| `ProviderAccountCredential` | `provider_account_credentials` | `core.provider_account_credential` | `account_id` | `Organization` | `agent-accounts` |
| `AgentRuntimeNode` | `agent_runtime_nodes` | `core.agent_runtime_node` | `node_id` | `Organization` | `agent-accounts` |
| `AgentRuntimeEngine` | `agent_runtime_engines` | `core.agent_runtime_engine` | `node_id,engine_id` | `Organization` | `agent-accounts` |

`provider_account_sessions` is **runtime state** — like `flow_executions` it stays out of the ledger; A03's session list on a remote account is read through `ProviderAccountOp` (`session.revoke`) / a read forwarded the same way. `provider_account_node_state` is node-local by definition.

`ensure_default_core_sync_policies` (`tentaflow-core/src/db/repository.rs:13991`) picks all five up automatically (it iterates `CORE_SYNC_DESCRIPTORS`), giving each `mode='replicated_by_permission'`.

### C.2 Per-node re-encryption

`ProviderAccountCredential` is the second member of the class that `SharedSettingSecret` already defines:

- **capture (donor):** the op carries the credential **decrypted** — the pattern at `tentaflow-core/src/sync/core_baseline.rs:952-958` (snapshot decrypt) and `tentaflow-core/src/sync/runtime.rs:13176-13182` (`FieldValue::String` for `value`). New capture helper next to it emits fields `account_id`, `revision`, `material`, `material_sha256`, `provider_subject`, `expires_at`, `refreshed_by_node`.
- **materialize (receiver):** new `apply_provider_account_credential` beside `apply_shared_setting_secret` (`tentaflow-core/src/sync/core_materializer.rs:407-448`), re-encrypting with the local `SettingsCipher` (`encrypt_bound`, context `provider-account:<account_id>`), then `upsert_resource_version` (`:384`). It is **not** added to the "no special handling" list at `core_materializer.rs:36-40`.
- **baseline import:** donor-wins, mirroring `core_baseline.rs:2115-2121`.
- the allowlist analogue of `SHARED_SECRET_SETTING_KEYS` (`tentaflow-core/src/db/repository.rs:940`) is not needed — the resource kind itself is the allowlist; anything else in that table is a bug, and the materializer refuses an unknown field set the same way.

### C.3 Restricting credentials to agent-runtime nodes

Mechanism verified in `can_node_receive_sync_resource_with_conn` (`tentaflow-core/src/db/repository.rs:15108-15150`): a core resource is blanket-replicated to every trusted node **only while it has no `sync_resource_acl` row** (`:15126-15137`). So:

1. On credential create, write a `sync_resource_acl` row for `('core.provider_account_credential', account_id)` via `upsert_sync_resource_acl` (`repository.rs:14717`) with `visibility_scope='restricted'` — this *removes* the blanket allow.
2. For every node with `agent_runtime_nodes.receives_accounts = 1`, `grant_sync_explicit_share(..., subject_type="node", subject_id=node_id, action="sync_receive", granted_by)` (`repository.rs:14851`).
3. Toggling the flag off calls `revoke_sync_explicit_share` (`repository.rs:14915`).

Both writers already `bump_sync_permission_epoch_with_conn`, so `cached_sync_targets_for_resource` (`tentaflow-core/src/sync/runtime.rs:1574`) invalidates and `backfill_outbox_for_permission_grants` (`:1645`) re-enqueues the credential to a node that is flagged **after** the op was minted. No new sync-side code. `provider_accounts` / `_grants` / `agent_runtime_*` stay on the blanket allow: the metadata is needed everywhere the UI runs; only the secret is restricted.

### C.4 Conflict rules

| Resource | Rule |
|---|---|
| `provider_accounts` | HLC last-writer-wins per row (the default for core kinds). `provider_subject` is **immutable once non-NULL**: a materialized op whose `provider_subject` differs from a stored non-NULL one is refused and logged (a different provider identity under the same account id is a mistake, never a rename). |
| `provider_account_grants` | row-level LWW on a composite PK — the same shape as `CodeWorkspaceMember` (`core_registry.rs:636`). `GrantsSetRequest` diffs and emits per-row inserts/deletes, never a truncate. |
| `provider_account_credentials` | **revision CAS**, not HLC: materialization applies the op only when `op.revision > local.revision`; equal revision with a different `material_sha256` is a conflict → the row is left untouched, `status` moves to `needs_login` and an audit entry is written. The home node is the only minting authority in the steady state (§C.5). |
| `agent_runtime_engines` | LWW; `install_state` is per-node truth reported by the owning node, so only that node writes its own rows (enforced in the handler, not the materializer). |

### C.5 Refresh, per engine

`home_node_id` is the single refresher. Satellites never rotate.

| Engine | Credential | Refresh mechanics | Bridge change (`tentaflow-containers/agents/native/coding-agent-bridge/src/main.rs`) |
|---|---|---|---|
| `claude` | static OAuth token (`CLAUDE_CODE_OAUTH_TOKEN`, set in `prepare_session_profile:1649`) | none — revision changes only on a new login | credential dir becomes read-only for the CLI; `reconcile_session_credential:1720` deleted |
| `codex`, `muse` | refresh token that **rotates on use** | only the home node runs a session that may rotate; it mints `revision+1` and the ledger fans out. A satellite that nevertheless observes a changed credential file does **not** write locally: it calls `ProviderCredentialSubmit{base_revision}`; the home node applies CAS and re-mints. A rejected CAS makes the satellite refetch and restart the instance. | credential dir shared per account; a post-turn watcher compares the file hash and, on change, POSTs the new material to Core (new bridge route `POST /account/{id}/credential-observed`) instead of writing `pending-credential.json`; the `credential-review-required` quarantine is deleted |
| `grok` | short-lived token from `auth_provider_command` | the command is a **Core broker call**: the bridge exports `GROK_AUTH_PROVIDER_COMMAND` pointing at a tiny exec that hits the bridge's loopback `GET /account/{id}/token`, which asks Core, which mints from the home node's credential. Nothing is persisted in the profile. | new route + env export in `prepare_session_profile:1649` |
| any `api_key` account | admin-entered key | never refreshed; `CredentialSetRequest` bumps the revision | adapter path (`DelegationAuth::OrgCredential`) unchanged apart from where the key comes from |

A satellite is only eligible to run a `codex`/`muse` session while `provider_account_node_state.applied_revision == provider_account_credentials.revision`; a stale node refuses with `AccountRefusal::CredentialMissing` rather than burning a rotated token.

---

## D. Runtime

### D.1 What leaves the `services` row

Today an account **is** a `services` row: `ensure_account_config` (`tentaflow-core/src/services/coding_agent.rs:20`) stamps `account_id` into `config_json`, `prepare_account_directory` (`:56`) creates its dir, `deploy/mod.rs` (`:418,481,549,635,776,804,859`) carries it through `NativeManagedCli`, and `delegate_cli` addresses the account by `service_id` (`tentaflow-core/src/flow_engine/node_adapters/delegate_cli.rs:114`).

After the change:

| Concern | Owner |
|---|---|
| account identity, grants, credential, status | `provider_account*` tables |
| CLI binary present on a node, version, install state (`tentaflow-core/src/services/deploy/managed_cli.rs`) | `agent_runtime_engines`, installed by the existing `NativeManagedCli` deploy path, which resolves the vendor's newest release at install time |
| the bridge process | one `services` row **per (engine, node)**, not per account — started on demand |
| egress allowlist per engine (`provider_config:121`) | unchanged, `tentaflow-core/src/services/coding_agent_proxy.rs` |
| model catalogue sync (`sync_coding_agent_models:929-1002`) | unchanged, keyed by engine |

`coding_agent_proxy::start(engine_id, account_id)` (`:97`) keeps both arguments — the proxy is still per account for egress attribution — but is called by the runtime manager, not by a deploy of an account service.

> **Corrected (2026-09-22).** This section describes the transition PLAN, not
> the tree any more: the "Today" half is gone, not merely superseded. The
> owner decided to delete the whole old "agent account = services row" path
> instead of keeping it beside the new one, so `ensure_account_config`,
> `prepare_account_directory`, `sync_coding_agent_models` and the
> `NativeManagedCli` branches of `deploy/mod.rs`/`deploy/binary.rs` this
> section cites no longer exist — `binary.rs` no longer has a
> `prepare_managed_cli_env` either, and a coding-agent bridge process is
> started exclusively by `agent_runtime.rs` (§D.2), never by a `services`
> deploy. `DeployMethod::NativeManagedCli` and `Transport::AgentRpc` are gone
> from the Rust enums (`NativeRuntime::ManagedCli` in the MANIFEST schema
> stays — it is what `agent_runtime::engine_source_hash` still reads).
> Deploying a managed-CLI manifest as a `services` row is refused server-side
> before a `DeployMethod` is even resolved. `coding_agent_proxy::start` is
> unaffected — it was already called only from `agent_runtime.rs`, never from
> a services deploy strategy.

### D.2 On-demand bridge start

New `tentaflow-core/src/services/agent_runtime.rs`:

```rust
pub async fn ensure_runtime(db: &DbPool, node_id: &str, engine_id: &str) -> Result<BridgeHandle>;
pub async fn ensure_account_materialized(db: &DbPool, account_id: &str) -> Result<()>;
pub async fn release_idle(db: &DbPool) -> Result<usize>;   // periodic, closes bridges with no session
```

`ensure_account_materialized` decrypts the local credential row, writes it into `accounts/<account_id>/` under `TENTAFLOW_CODING_AGENT_DATA_DIR` (the env contract already built by `prepare_managed_cli_env` at `tentaflow-core/src/services/deploy/binary.rs:110`), and sets `provider_account_node_state.applied_revision`. Called before the first session on a node and after every credential revision bump.

### D.3 The single resolver

`tentaflow-core/src/services/agent_account.rs`:

```rust
pub struct ResolvedAccount {
    pub account_id: String,
    pub engine_id: String,
    pub credential_kind: CredentialKind,   // ApiKey | ProviderLogin
    pub node_id: String,                   // where the bridge will run
    pub revision: i64,
}

pub enum AccountRefusal {
    NoRuntimeNode      { engine_id: String },
    EngineNotInstalled { engine_id: String, node_id: String },
    NoAccountForUser   { engine_id: String },                 // C01 ask
    NotGranted         { account_id: String },
    CredentialMissing  { account_id: String },                // status='needs_login'
    AccountDisabled    { account_id: String },
    StaleCredential    { account_id: String, node_id: String },
}

pub fn resolve_run_account(
    db: &DbPool,
    agent: &DbAgent,
    principal: &AgentPrincipal,
    preferred_node: Option<&str>,
) -> Result<ResolvedAccount, AccountRefusal>;
```

Order: `agent.runtime_json.account` (§E) decides the candidate set → `mode="fixed"` takes that account and checks the grant; `mode="user"` takes the principal's own `scope='user'` account for the engine, else a granted global one; `mode="engine"` takes any granted account of that engine, preferring the user's own. Then: status `active`, credential revision present, a node with `receives_accounts=1` and the engine installed, `applied_revision == revision`. Every failure is one of the typed refusals; **no silent fallback to another account** (that is what makes C01's ask honest).

Every caller goes through this one function: `delegate_cli::execute` (`tentaflow-core/src/flow_engine/node_adapters/delegate_cli.rs:1001`, replacing the `bound.session.agent_service_id` override at `:1063` and the `account_permission` check at `:1083`), `session_open_v1` (`tentaflow-core/src/dispatch/code_studio.rs:2864`), and `resolve_bridge` (`delegate_cli.rs:1265`) which now takes `ResolvedAccount` instead of a `service_id`. `resolve_delegation_auth` (`tentaflow-core/src/code_studio/cli_adapter.rs:265`) loses its vault probe and its `selected_account: bool` parameter and becomes a total function of `ResolvedAccount.credential_kind`; `DelegationAuth` (`:230`) and its budget/ticket machinery stay intact.

### D.4 Multi-session per account in the bridge

| Today | New |
|---|---|
| exclusive `account.lock` (`main.rs:371-378`) + in-process `lease` → `account_busy` (`:1336`, `:811`, `:972`) | the lock is kept **only** for credential-file mutation (login and materialization); sessions do not take it |
| each session gets a **copy** of the credential in its private profile (`prepare_session_profile:1649`) | one shared, read-only credential dir per account: `accounts/<id>/credentials/`; the per-instance profile (`session_profile_root:1625`) keeps HOME/CODEX_HOME/CLAUDE_CONFIG_DIR/GROK_HOME/XDG_*/TMPDIR and **symlinks/binds** the credential dir |
| `reconcile_session_credential:1720` writes `pending-credential.json` and quarantines with `credential-review-required` | deleted; replaced by the per-engine mechanics in §C.5 |
| `rollback_session_start:1772` undoes the credential copy | reduced to profile teardown |

History, sessions and caches stay private per instance — that is what makes two agents in one workspace (C02) independent.

`vendor_session_id` is bound to the **conversation alone** by the unique index in §A.1 as migration 161 replaced it: one vendor conversation is driven by one recorded session, so the same user resuming that conversation reattaches the session that recorded it — whatever agent or workspace asks — and a second live session cannot claim a conversation one already holds, because `upsert_session` clears the previous row first. Sessions of one account in one workspace stay free to run at the same time; that concurrency is what the shared credential directory exists for (§2.5). `close_session_instances` (`tentaflow-core/src/code_studio/cli_bridge.rs:1515`) clears the runtime instance but leaves the binding row, so resume works after a restart.

### D.5 Paused turn and resume (C01)

Reuse `tentaflow-core/src/agents/interaction.rs`:

- `InteractionKind` (`:36`) gains `AccountLogin` with `as_str() = "account_login"` (the kind travels as a string inside the `Serialize` `PendingInteraction` at `:121`, so this is additive);
- on `AccountRefusal::{NoAccountForUser, CredentialMissing}` the delegation node registers a pending interaction (`register:226`) carrying `{engine_id, account_id?, candidate_accounts}` and awaits the reply (same `oneshot` + `InteractionOutcome::TimedOut` semantics);
- the console renders it with the existing ask machinery (`state.ask` at `tentaflow-core/www/js/modules/code-studio-session.js:157`, `shell.dataset.ask` `:445`/`:915`, `.cs-answer` `:523`, `ev-askmark`→`focusAsk()` `:1081`, answer actions `:1123-1134`);
- the "Zaloguj" action opens A02; `LoginStatusResponse.state == "succeeded"` answers the interaction, and the turn continues from the same point. Nothing is re-planned and no tokens are re-spent.

---

## E. Agents

| Item | Change |
|---|---|
| schema | migration 156 also runs `ALTER TABLE agents ADD COLUMN runtime_json TEXT NOT NULL DEFAULT '{}'` — precedent `agents_add_delegation_roster` (`tentaflow-core/src/db/migrations.rs:1146`); base table `AGENTS_REGISTRY:3037` |
| shape | `{"account":{"mode":"user"\|"fixed"\|"engine","account_id":"…","engine_id":"…"}}` — `account_id` required iff `mode="fixed"`, `engine_id` required iff `mode="engine"` |
| model | `DbAgent.runtime_json: String`, `AgentParams.runtime: AgentRuntimeParams` in `tentaflow-core/src/agents/mod.rs` |
| validation | `validate_agent_params` (called from `agents_upsert` at `tentaflow-core/src/dispatch/handlers.rs:8292`): mode enum, referenced account exists in the org and its engine matches, `engine_id ∈ CREDENTIAL_ENGINES` (`tentaflow-core/src/dispatch/code_studio.rs:2410`). It does **not** check grants — a grant may be added later; the refusal is a run-time decision. |
| wire | **none**. The agent travels as the opaque `agent_json` string (`AgentsPayload`, `tentaflow-protocol/src/message_body.rs:3769`); `runtime` is one more key in that JSON. |
| sync | `CoreSyncResourceKind::Agent` (`tentaflow-core/src/sync/core_registry.rs:306`) already replicates the row; `apply_agent` (`core_materializer.rs:198`) and the baseline row builder gain the column. A `mode="fixed"` account id that does not exist on the receiver leaves the agent valid and the run refuses with `NotGranted` — an agent is not invalidated by a missing grant. |
| UI | only the **"Model i generowanie"** section of `renderConfigTab` (`tentaflow-core/www/js/modules/agents.js:1222`, model block `:1259-1279`), exactly as G01: one `tf-select` "Konto agenta CLI" with the three modes plus a dependent `tf-select` of accounts; wired in `wireConfigInputs:1330` next to `refreshReasoningOptionsFor:1170`. No other section moves. |

---

## F. Frontend

| Mockup | Module / function to extend | New pieces (`tf-*` only) | i18n | Dies |
|---|---|---|---|---|
| A01 | `tentaflow-core/www/js/modules/services.js` — existing tab wiring `:293-302`, `codingAgentServices:720` | `tf-segmented` (Konta / Aplikacje na nodach); `tf-table` with columns engine / nazwa / zakres / właściciel / plan / status / dostęp / sesje, `row-click` → A03; `.tf-toolbar` + `tf-searchbox` + `tf-select` (silnik, zakres) + right-aligned `tf-button` "Dodaj konto" via `.tf-toolbar-spacer` | `agent_accounts.list.*` | the old per-service account list in `codingAgentServices:720` |
| A02 | `tentaflow-core/www/js/modules/coding-agent.js` — `openAgentLogin:41` | 4-step stepper inside the existing `tf-window`: node `tf-select` → link (`tf-button` "Otwórz" + copy) → `tf-input` for the code → result; polls `LoginStatusRequest` | `agent_accounts.login.*` | `agentRequest:8` `auth.*` calls |
| A03 | `coding-agent.js` — `openAgentAccount:141` | `tf-tabs` Przegląd / Dostęp / Agenci; Przegląd holds a `tf-stat-card` row, the per-node `tf-table` (węzeł / otrzymuje konta / rewizja / stan) and the sessions `tf-table` with a per-row "Zakończ" | `agent_accounts.detail.*`, `agent_accounts.sessions.*` | the "Przenieś" action and `mountAccountMove:253` |
| A04 | `coding-agent.js`, same window's Dostęp tab | `tf-combobox` for users **and groups**, a "Cała organizacja" `tf-toggle`, `tf-chip` list of grants, one save → `GrantsSetRequest` | `agent_accounts.access.*` | `account.grants.set` over `ServiceAgentRequest` |
| N01 | `services.js`, second segment of the same tab | `tf-table selectable="none"` matrix: row = node, first cell two-line (name + short id), a `tf-toggle` "Otrzymuje konta", one cell per engine with state + `tf-menu` (Zainstaluj / Odinstaluj) | `agent_accounts.runtime.*` | — |
| U01 | `tentaflow-core/www/js/modules/my-accounts.js` — `loadAll:113`, `renderCard:212`, `renderBody:232`, `wireCardActions:286` | new "Aplikacje agentowe" card section built from the same `renderCard` shape; per account: status chip, "Zaloguj ponownie" → A02, "Usuń" for own accounts | `agent_accounts.my.*` | — |
| G01 | `agents.js` (see §E) | two `tf-select`s | `agents.runtime_account.*` | — |
| C01 | `tentaflow-core/www/js/modules/code-studio-session.js` — ask machinery `:157`, `:445`, `:523`, `:1081`, `:1123-1134`, routing `:1297-1332` | one more ask card kind `account_login` with actions `answer-login` (opens A02) and `answer-deny` | `code_studio.ask.account.*` | — |
| C02 | `code-studio-session.js` — `openSubagentTab:1210` | `tf-chip` with the account name on each sub-agent tab/row | `code_studio.session.account_chip` | — |
| — | `tentaflow-core/www/js/modules/code-studio.js` | `#cs-sess-account` (`:1663`) now lists resolved accounts, `agentServiceId` (`:1694`) → `accountId`, the `account.access` probe (`:1714`) → `MyAccountListRequest` | | the `agentServiceId` field |

i18n: `agent_accounts.*` must stay key-identical across `tentaflow-core/www/i18n/{pl,en,de,es,fr}.json`; the `agent_accounts.move_*` block (pl `:8340-8371`) is deleted in all five. Counts in the UI use the `{count|forma1|forma2|forma3}` plural form, never concatenation.

---

## G. Ordered work packages

Each package compiles, passes its tests, and leaves no old path behind it. WP1→WP8 is sequential; WP4 and WP7 may run in parallel with WP5/WP6 once WP3 lands.

| # | Scope | Files | Tests | Acceptance |
|---|---|---|---|---|
| **WP1** | Migrations 156 + 157, repository CRUD, vault credential store | `db/migrations.rs` (new consts + `MigrationStep::Rust`), `db/repository.rs` (`provider_account_*` CRUD, grant resolution incl. groups/org), `code_studio/vault.rs` (delete the agent-credential half, keep `encrypt_material`/`decrypt_material`), `code_studio/db.rs` STEP 2 | migration round-trip from a seeded old DB (services+grants+session owners → accounts+grants+sessions); grant resolution user/group/org; `encrypt_bound` context binding refuses a foreign `account_id` | `cargo test -p tentaflow-core --lib db::migrations` + new `provider_account` module tests green; old tables gone |
| **WP2** | Wire family + admin dispatch (list/get/create/update/delete/grants) + `SCHEMA_VERSION` 29→30 | `tentaflow-protocol/src/provider_account.rs` (new), `message_body.rs:8387`, `envelope.rs:256`, `dispatch/provider_account.rs` (new), `dispatch/mod.rs` | wire golden round-trip for the new family; handler ACL tests (non-admin sees only own+granted) | dashboard A01/A03/A04 read paths work against a live core |
| **WP3** | Credential store + login: `LoginStart/Input/Status/Cancel`, `CredentialSet/Clear`, signed mesh `ProviderAccountOp` + `ProviderCredentialSubmit`; **delete** `AgentCredential*` variants + golden pin, `agent_credential_*_v1` handlers, `code_agent_credentials` | `dispatch/provider_account.rs`, `mesh/command_executor.rs`, `code_studio/assertion.rs` (caps), `tentaflow-protocol/src/{mesh.rs,code_studio.rs}` | assertion verify/replay on the new op; CAS conflict test (equal revision, different sha → `needs_login`, row untouched); local login happy path against a stub provider | **needs a real provider account** for the end-to-end A02 run |
| **WP4** | N01: `agent_runtime_nodes`/`_engines`, install/uninstall through the existing `NativeManagedCli` deploy, `services/agent_runtime.rs` on-demand bridge start | `services/agent_runtime.rs` (new), `services/deploy/{mod.rs,binary.rs,managed_cli.rs}` (drop the account plumbing at `mod.rs:418,481,549,635,776,804,859`), `services/coding_agent.rs` (delete `:20,41,56,125,176,291,374` and the account branches of `:189`/`:417`) | bridge starts on demand and is released when idle; refusal when the engine is not installed | N01 toggles and installs a CLI on a second node |
| **WP5** | Resolver + consumers + agents `runtime_json` + G01 | `services/agent_account.rs` (new), `flow_engine/node_adapters/delegate_cli.rs` (`:114`, `:1001`, `:1063`, `:1083`, `:1265`, `:1335`), `code_studio/cli_adapter.rs:265`, `dispatch/code_studio.rs:2864,2994`, `db/seed.rs:752-759,2111`, `agents/mod.rs`, `dispatch/handlers.rs:8292`, `www/js/modules/agents.js` | one table-driven test per `AccountRefusal`; seed/flow test that `delegate_cli` blocks carry no `service_id`; agent validation rejects a foreign-engine account | a flow runs end to end with an account chosen by `runtime_json` |
| **WP6** | Bridge multi-session: shared credential dir, remove `account_busy`/`lease`/`reconcile_session_credential`, per-engine refresh hooks, `vendor_session_id` binding | `tentaflow-containers/agents/native/coding-agent-bridge/src/{main.rs,transfer.rs}`, `services/agent_runtime.rs`, `code_studio/cli_bridge.rs:1515` | two concurrent sessions on one account with isolated histories; codex rotation submits CAS and a stale satellite refuses; resume reattaches `vendor_session_id` | **needs a real codex/muse account** to prove rotation |
| **WP7** | C01 paused turn + C02 chips + U01 | `agents/interaction.rs:36`, delegation refusal path, `www/js/modules/{code-studio-session.js,code-studio.js,my-accounts.js}`, i18n ×5 | interaction registers, times out, and resumes after a successful login | C01 flow demonstrated live |
| **WP8** | Removal sweep: `services/account_move.rs`, `MeshCommandType::AgentAccountMove` + executor branch, `mountAccountMove`, `agent_accounts.move_*` keys, dead i18n/CSS, `cargo check` unused warnings to zero | as listed in §A.3 | key-parity check across the five locales; `cargo check` clean | no reference to "move"/`account_move` remains outside the migration ladder |

> **Removed (2026-09-20).** WP8 ran. The sweep is complete on every code path it
> owned: `services/account_move.rs` and the bridge's `transfer.rs` are deleted,
> no `mod account_move;`, no `account.move` dispatch branch, no
> `MeshCommandType::AgentAccountMove` and no executor branch for it, no
> `mountAccountMove` in `www/js/modules/coding-agent.js`, and no
> `agent_accounts.move_*` key in any of the five locales. `SCHEMA_VERSION` is
> **31**.
>
> What a `grep` still finds is deliberate, and the original acceptance sentence
> ("no reference to … remains") was false against it: the migration ladder keeps
> rung 149 and its DDL const plus the rung-163 `DROP` (§A.3), and
> `tentaflow-protocol/src/envelope.rs` carries the comment that documents the v31
> variant removal. Those are the record of a removal, not a live path — the
> ladder must not be rewritten, and the envelope comment is what tells a mixed
> fleet why its handshake fails.

> **E2E coverage of C01, C02 and `cli_model` (2026-09-21).** What WP7's "C01 flow
> demonstrated live" has behind it, and what it does not.
>
> `tests/e2e/agent-accounts-code-studio.spec.js` (project
> `agent-accounts-code-studio`) runs a real node with nothing stubbed below the
> browser. A session is pinned to the harness flow (`resolve_harness_flow`), so
> the turn runs the graph for real: the orchestrator's turn, the plan loop
> (planner + critic), then the build loop — and because `code-implementer`
> carries a `mode="user"` CLI runtime, the graph's own implementer spawn is the
> one that cannot resolve an account for the person driving the session. It
> asserts the ask card the console really paints (both rows and their order, the
> account-less wording, the `who` line), the pending `account_login` row the
> server wrote (field by field, `mandatory_interactive = false`), that every
> standing grant decision is refused at the write with the remedy named while the
> row stays pending, and that the deny settles the parked run as `failed` with
> the C01 note — read back from the run list and from the workspace database
> (`session_runs`, `approvals` including the exact `account_json`).
>
> **The resume half and C02's positive chip now run too**, on a node that has no
> vendor CLI and no provider account. `tests/e2e/helpers/stand-in-cli.{c,js}`
> compiles a small program into the cache tree the node's own managed-CLI
> installation check reads (`<cache>/coding-agents/<engine>/<version>/
> {installation-complete,bin/<exe>}`) and writes the `agent_runtime_nodes` /
> `agent_runtime_engines` rows, so the node reaches it exactly the way it reaches
> the vendor's binary — `services/agent_runtime.rs::start_bridge` puts the
> engine's `bin/` first on the PATH the bridge runs with. There is no test branch
> in the product. Two tests then drive the console:
>
> - the second describe signs in through A02 (the URL is read from the DOM the
>   console paints, the code is typed back), and asserts the wake-up on the
>   observable that means it: `delegate_cli` opens the `kind='cli'` run row only
>   AFTER the account resolves, so the row appears with no click on the account
>   card, and the `account_login` row settles `allow_once` with `decided_by` set.
>   The resumed run then asks its OWN question — `cli_delegate` goes past the PEP
>   in both delegation modes (`delegate_cli.rs` step 5) and, with no standing
>   grant for the engine, `pep.rs` rule 10 answers `AskUser` — so the operator
>   allows it once and the CLI turn completes: `session_runs` `completed` with
>   the delegated model, the `cli_instances` row naming the account, and the
>   credential byte-identical to the one the CLI wrote.
> - the third asserts C02 positively: `RunInfo.account` on the CLI run (engine,
>   `user` scope, the account name) read from the wire, and the chip in the
>   agents dock rendered as one string — while the root run, which never started
>   a CLI, names no account.
>
> Both also prove the work is not a bypass: the account's `processes/` records
> (written by `coding-agent-bridge/src/process.rs`) are watched while the
> children live, and `supervisor_root` must match
> `/private/tmp/tfp-<24 hex>` — the macOS supervisor's temporary root — for the
> `cli-login` terminal AND for the `codex-app-server` process that ran the turn.
>
> The seventh test drives the same account into its LAST state — gone. It signs in
> through the console, asserts the two files the bridge owns (canonical and login
> home), disconnects through U01's own control and its confirmation, and then
> reads the filesystem: both are absent. That is the half a store-only purge
> cannot give — the login home is a copy the bridge made, so nothing else can
> know it is there — and the third assertion is the one that would catch a
> regression: the account row is gone from the console, and the next turn parks
> on a fresh `account_login` row with no `cli` run instead of delegating on the
> credential the operator removed. `needs_login` is NOT what this path produces
> (a deleted account has no row to carry it); that state is pinned in
> `agent-accounts.spec.js`, where the key-clear leaves the account in place.
>
> The eighth purges the same account with NO bridge running, which is the state a
> purge normally meets: it signs in, stops the node, restarts it on the same
> database — `IDLE_GRACE` and the restart between them leave no bridge process —
> and only then disconnects. The subject is the two files again, asserted from the
> filesystem after a purge that had no bridge to tell, so a fix that removed them
> only through a running bridge fails here and passes in the seventh.
>
> The ninth is the partial outcome, and it is the only place the operator's view of
> a failed removal is pinned. It signs in with a bridge RUNNING and makes ONE tree
> refuse the walk (a `0500` engine directory: the walk reads the directory and the
> kernel refuses the `unlink`), then disconnects through U01. The bridge's arm
> fails, Core's arm is attempted after it and fails on the same directory, and what
> is asserted is the whole report: `ok = false` with `purge_incomplete` rendered as
> an error toast, the login home gone (the second tree was attempted by both arms),
> the canonical file still there and still byte-identical to what the sign-in
> published, the store rows gone, and the node's warning naming the tree it could
> not empty. That last assertion is the one that makes the acknowledgement
> actionable — `AccountOpAck` carries no path, so the log is the only channel that
> does. (The two arms' reports are NOT interchangeable here: the bridge names its
> own path in the error body and `call_bridge` embeds it, and the assertion is a
> substring of the account root, so it holds for either arm's wording.)
>
> The tenth is the case the ninth cannot be: a sign-in IN FLIGHT while the account
> goes. The bridge refuses `DELETE /account/credential` WHOLE in that state
> (`login_in_progress` — it will not promise a removal the sign-in would undo), so
> no path is attempted on its side at all, and the node has to remove the trees
> itself once it has stopped the bridge. The test signs in, opens a SECOND sign-in
> and leaves it blocked on its stdin (the node's side of it is detached from the
> page, so a reload — which is also how the disconnect control becomes reachable,
> the login window being modal — leaves it running), then disconnects through U01.
> Asserted: both trees absent, the three store tables empty, the success sentence
> rendered instead of `purge_incomplete`, and the sign-in process the purge met
> gone — its `cli-login` record, which the bridge deletes only after verifying the
> pid is gone, does not survive. Without the fallback that test fails on exactly
> the files the sign-in writes during the bridge's own shutdown.
>
> Still NOT covered on a machine that never signs in to a real vendor, and what
> each would need:
>
> - **A real sign-in at a real provider**, a credential the provider would
>   accept, and a vendor-side token ROTATION. The stand-in writes a well-formed
>   credential file, prints a verification URL, exits 0 and answers `login
>   status` with "Logged in" — the shape the product reads, and nothing more.
>   Anything the product claims about the account beyond "a credential exists and
>   the CLI exits 0" — a provider-reported plan, a subject, a rotation — needs the
>   vendor's own CLI and a real account (WP6's acceptance).
> - **`cli_model`'s option list.** The options are the `<engine_id>/<model>` rows
>   of the model catalogue, written only by `services::coding_agent::sync_models`,
>   which `services/supervisor.rs` reaches only after the bridge reports
>   `auth.status.authenticated = true`. The stand-in DOES answer `model/list`, so
>   the path is reachable, but what a real Codex reports there is not what the
>   stand-in reports, so the resulting catalogue is not asserted. The G01 e2e
>   covers the stored-value half — the select must keep the model the agent was
>   saved with while the catalogue offers nothing for the engine.
> - **Windows/Linux sandbox proofs.** `supervisor_root` is the macOS supervisor's
>   root (`macos_supervisor::new_root`); the same tests on another platform would
>   need that platform's equivalent.
> - **What the ninth and tenth purge tests do NOT prove.** They read what the node
>   can see: the two trees, the store rows, the acknowledgement, the log line, and
>   the bridge's own process record. They do not prove that no writer survived the
>   stop. The node waits for the bridge process and the bridge reaps the CLI
>   children it tracked before it answers `/runtime/shutdown`, so the ordinary case
>   is closed — but a shutdown the bridge itself could not confirm (a session that
>   will not close) is only LOGGED by the node, and an orphan it left is reaped at
>   the bridge's NEXT start. Core cannot see either, and neither case is
>   reproducible on this machine, so neither test may be read as proving the trees
>   cannot come back.
>
> The card and the chip logic itself is pinned at the unit level in
> `tentaflow-core/www/js/modules/code-studio-session.account.test.js`.

---

## H. Risks and open questions that block a package

1. **Credential loss at migration (blocks WP1 sign-off).** `code_agent_credentials` and the on-disk bridge logins are not migrated (§A.2). Every adopted account starts `needs_login`. Required: an explicit release note and a log line at drop time with the pre-drop row count per `(node_id, engine_id)`. Alternative if unacceptable: a pre-upgrade CLI export step — an extra deliverable, not in the packages above.
2. **`DelegationAuth::OrgCredential` survives as `credential_kind='api_key'`** (§A.2, §D.3). Confirm this is the intended reading of the plan: the adapter/ticket/budget machinery in `cli_adapter.rs:230-297` is retained unchanged, only its credential source moves. If the intent was to drop API-key delegation, WP5 shrinks and `credential_kind` becomes a constant — decide before WP1 writes the CHECK constraint.
3. **Plaintext credential on the ledger wire (WP3).** §C.2 copies the accepted `SharedSettingSecret` model, in which the operation body carries the decrypted secret and the receiver re-encrypts (`core_materializer.rs:426`, `core_baseline.rs:2116-2119`). That is a deliberate, already-shipped trust decision about the mesh channel; re-confirm it explicitly for a provider OAuth token, whose blast radius is larger than `hf_token`.
4. **A03 sessions for a remote account.** `provider_account_sessions` is not synced (§C.1), so a remote account's session list needs a forwarded read. The design routes it through `ProviderAccountOp`; if the node is offline the tab must render "brak danych", not an empty list. Needs a decision on which one A03 shows — the mockup does not distinguish them.
5. **Prerequisites carried over from the plan's §3, unverified here and outside these packages:** Linux sandbox networking, the `with_proxy` gate at `process_sandbox.rs:182`, the bridge binary in the release archives, and grok/muse Linux artifacts. WP4 and WP6 cannot be accepted operationally on Linux until those land; a background check of the rust targets was still running when this design was written and its result should be read before WP4 starts.
6. **`InteractionKind` string on the wire (WP7).** Adding `AccountLogin` is additive only if every existing consumer of `PendingInteraction.kind` ignores unknown values. Verify the dashboard's pending-interaction list (outside the files read for this design) before WP7, otherwise an older tab could render a blank card.

---

## Binding corrections (override the sections above)

1. **§E shape follows the accepted plan and mockup G01**, not the three-mode variant above:
   `runtime_json` = `{"kind":"llm"}` or
   `{"kind":"cli","engine":"<engine_id>","model":"…","reasoning":"…","account":{"mode":"global","account_id":"…"}}` or
   `{…,"account":{"mode":"user"}}`. The UI is the G01 section: segmented "Model LLM / Aplikacja CLI",
   application, model, reasoning level, account mode (global → account select, user → no select).
2. **§D.3 never switches mode.** `mode="user"` resolves ONLY the principal's own `scope='user'` account
   for the engine; when it is missing the run refuses with `NoAccountForUser` (C01). It never falls back
   to a granted global account. `mode="global"` takes exactly the configured account and checks the grant.
3. **Destructive steps move to the package that switches the consumers.** Migration 157, the vault
   agent-credential removal and the old-table drops land together with WP5/WP8, so every package compiles.
   WP1 is additive (migration 156 + repository + wire + admin dispatch).
4. H-1 accepted: adopted accounts start `needs_login`; H-2: `api_key` stays as the second method of the
   same A02 wizard; H-3 accepted (same trust model as shared secrets, restricted to agent-runtime nodes).
5. **"Administrator" is `handlers::session_is_admin`**, not the `org_admin` / PowerUser wording of §B.
   One definition of administrator for the whole dashboard; a second one in this family could drift from
   the Services and Nodes screens, and an ACL that disagrees with the screen next to it is an ACL nobody
   can reason about. Every account handler stays `#[policy(UserSession)]` and carries its own check, in
   the Project Studio pattern.
   Within that: a **personal** account's credential and display name answer to its OWNER alone
   (`CredentialSet`, `CredentialClear`, rename). An administrator keeps `status` (disable) and delete —
   taking a personal account out of service is a different power from putting a credential of one's
   choosing behind somebody else's name and having runs attributed to it (plan §2.2).
6. **`SCHEMA_VERSION` stays 29 in this additive package.** Nothing on the wire is removed here, and a
   bump would refuse the handshake of every node still on the old binary — including the ones this
   package is supposed to leave working. The consequence is stated rather than hidden: a node that has
   not been upgraded dead-letters the `core.provider_account*` operations it cannot materialize, and
   after its upgrade the state reaches it through `reseed_core_state_from_current_rows`, not through a
   replay of the dead-lettered ones. The bump lands with the package that REMOVES the `AgentCredential*`
   variants, where old and new binaries genuinely cannot talk.
7. **§C.5 and §D.4 credential sharing is replaced: a session never reaches the canonical credential.**
   The shared-writable credential directory those sections describe was measured on a real `bwrap`
   sandbox and failed: a session could overwrite, unlink or symlink-swap the account's canonical
   credential (planting a foreign provider identity for every other user of the account, or denying
   it to all of them), and by replacing `credentials/<engine>` with a directory symlink it could make
   the UNSANDBOXED bridge read, hash, publish and write arbitrary host paths — including the Claude
   sign-in writing a fresh token into a directory of the session's choosing. What replaces it:
   - `accounts/<id>/credentials/` as a DIRECTORY is in no session's sandbox policy. Of the account's
     own credential store that ONE FILE is the only path any session reaches — the profile
     names the source and a destination inside itself, and `ProcessSandbox::with_credential`
     (`process_sandbox.rs:717`) calls `CredentialExposure::install` (`:586-635`) → on Linux a `--bind` mount, so nothing inside the sandbox can
     replace, rename over or write around the mount point; on macOS a symlink with the source allowed
     literally, and a spawn that finds anything else at the destination refuses rather than run the
     engine against a credential nobody else can see. The engine therefore rotates the ACCOUNT's own
     file, which every other instance on the node reads; there is no per-session copy
     (`main.rs:2393-2399`). Claude Code gets no file at all: its token travels in the process
     environment (`main.rs:2358-2364`).
   - Sign-in, `login status` and discovery run in a separate bridge-private home
     `accounts/<id>/login/`, which no session can write either. A working copy of the canonical
     credential is placed there before the CLI runs and whatever the CLI leaves is extracted back.
   - A change to that file is a REQUEST, not a fact. The bridge polls it (`credentials::observe`),
     takes it only if it is a bounded regular non-symlink opened `O_NOFOLLOW` and stat'ed through the
     descriptor it is read from, and announces the digest; Core owns the revision CAS and returns
     early on a digest it already stores, so a late announcement moves nothing and a rotation this
     bridge never saw before a restart is still announced instead of staying on one node. The
     identity comparison never suppresses the REPORT — every change is announced, as a publication or
     as a refusal — but for codex it does decide whether the change is PUBLISHED: material
     (`tokens.account_id`, else the `sub` claim of `tokens.id_token`) naming a different
     account is a `Foreign` refusal, and codex material naming nobody where the bridge had announced
     an account is `Unverifiable`. Neither becomes a revision, because material is fetched only on the
     publication path. An engine whose format carries no stable subject (muse, grok) has nothing to
     compare on either side and its rotation is announced as `Moved`, because
     inventing a refusal from silence would disable accounts whose engine we cannot read
     (`credentials.rs:686-707`).
   - A refusal is reported as `credential_rejected {engine, reason, sha256}` carrying one of three
     reasons — `identity_mismatch`, `identity_unverifiable`, `unsafe_credential_file` — and a
     publication as `credential_changed {engine, sha256}`; material never travels on either. The
     bridge names two further reasons it never reports, because it settles both on its own login
     home before anything is published: `unusable_credential`, when what the CLI left is not a
     non-empty JSON object (`credentials.rs:745-753`), and `stale_baseline`, when the canonical
     credential moved under a probe that started from another version (`credentials.rs:761-765`).
     Both are written to the bridge's stderr and no event is queued for either
     (`main.rs:2641-2643`), so neither can reach Core at all.
   - After a publication the bridge fans the new material out to every live session whose copy is
     still the old one, so a rotation reaches the sessions running beside it; a session that changed
     its own copy is left alone.
   - Two live sessions may share an account but never one vendor profile (`profile_in_use`).
   The identity comparison refuses a credential that NAMES a different account. It is not
   authentication: the material is not authenticated at all, the `id_token` signature is not verified,
   and a session that forges the name defeats the check. It therefore stops a careless or
   differently-signed-in session from replacing the account's credential, and does not stop a hostile
   one. Open item: verify the `id_token` signature against the provider's published JWKS, which would
   turn the name into a claim the provider stands behind. A grant remains a delegation of the
   credential.
8. **§B.2's `MeshCommandType::ProviderAccountOp` / `ProviderCredentialSubmit` were not added; the
   family travels on the existing `AppRouteOp`.** Both designed variants are "a signed assertion plus
   a CBOR payload routed to one node and dispatched there", which is exactly what `AppRouteOp` already
   is, and a second variant of it would have to be kept in step with the first for the rest of its
   life — including `SCHEMA_VERSION`, which correction 6 keeps at 29 precisely to avoid a handshake
   break in this package. The guarantees §B.2 asked for are the ones `AppRouteOp` gives: the assertion
   is verified on the far node, the actor is RE-DERIVED there rather than taken from the message, the
   payload is bound by `args_digest`, `jti` is burned against replay and `exp`/`nbf` are enforced, and
   the far node re-runs the same dispatch — so every ACL in `dispatch/provider_account.rs` applies
   again on arrival. `ProviderCredentialSubmit` has no substitute because nothing needs it: a login
   stores the credential on the node that ran it, and the other nodes receive it through the ledger
   (§C.2), not through a mesh message carrying plaintext material.
   The caveat is inherited and stated rather than hidden: `mesh/command_policy.rs:66` classifies
   `AppRouteOp` as `ActorAsserted`, so a COMPROMISED trusted peer can name any subject in an assertion
   it mints. Credential minting (`login.*`) and runtime install now sit behind that classification.
   It is the same trust model the ledger already gives a trusted peer for this data (correction 4,
   H-3) — a peer that can mint assertions can also write the synced rows — but the blast radius grew
   from "reads what it is allowed to read" to "starts a sign-in as somebody else". Tightening it means
   a stricter class for this op (and a reason for every other `AppRouteOp` user to follow), which is
   a mesh-policy change, not an account change, and is therefore open rather than done.
9. **The fleet-wide-looking counts are node-local and the wire says so.** `used_on`,
   `MyAccountInfo.session_count`, `RuntimeNodeInfo.account_count` and `RuntimeNodeInfo.os` are derived
   from `provider_account_sessions` and `provider_account_node_state`, which §C.1 deliberately keeps
   OUT of the ledger. They are therefore a subset — what the answering node itself measured — and the
   doc comments in `tentaflow-protocol/src/provider_account.rs` say that instead of promising a fleet
   view. Making them fleet-wide is a forwarded read per node (§H.4), not a field rename, and is not
   part of this package; a screen must not print them as if they covered the mesh.
10. **`agent_runtime_nodes.receives_accounts` is a GATE, not a label.** A node with the flag off
    starts no bridge and materializes no credential: `services::agent_runtime::ensure_runtime` and
    `ensure_account_materialized` refuse with `NOT_RECEIVING_ACCOUNTS`, and `LoginStartRequest`
    refuses earlier, at the dispatch boundary, with `PolicyDenied`. Without the gate the flag was
    advisory: the target node of a sign-in is named by the caller (including a non-admin owner of a
    personal account), so a node an administrator had explicitly taken out of the account fleet still
    installed a CLI and wrote a credential onto its disk. The refusal carries the message key
    `agent_accounts.login.node_not_receiving` (`NOT_RECEIVING_ACCOUNTS_KEY`) in all five locales;
    `ProtocolError` has no key field, so the text on the wire is English and the key is what the
    dashboard will render once the A02 wizard shows this refusal.
11. **The two directions of `/account/credential` answer differently while a sign-in runs.**
    The PUT (Core installing what the ledger delivered) is REFUSED with `login_in_progress`, and
    the `login_flow` guard is held across the write rather than only read: releasing it after the
    test leaves a window in which `auth_start` takes it and this handler then overwrites the file
    the sign-in is about to be settled onto. The GET (Core collecting what a sign-in or a rotation
    produced) deliberately does NOT refuse: it is how a rotation is picked up at all, the canonical
    file is replaced by rename so the answer is whole whichever side of a settle it lands on, and
    refusing would leave a rotation unread for as long as somebody is at the provider's device
    page — minutes, during which every node in the fleet keeps the retired token. The same
    ordering rule binds the end of a sign-in: `close_session` takes the login-home lease BEFORE it
    clears `login_flow` and holds both across the publication, in the one order every other holder
    uses (flag, then home). Clearing first left a window in which a probe saw "no sign-in running",
    leased the home, materialized the OLD canonical credential over what the CLI had just written,
    and the sign-in then published nothing.
12. **One `identity_mismatch` from a VERIFIED identity puts the account in `needs_login`.** The
    two-in-24-hours rule exists for a refusal that measured nothing, and among the reasons Core can
    actually receive that is `identity_unverifiable` alone — `unsafe_credential_file` never moves the
    status at all, and `stale_baseline`, which would not move it either, never reaches Core to try
    (`repository.rs:1437-1500`, `main.rs:2641-2643`). A single refusal can be noise — a session closing late, a copy that never matched
    — but `identity_mismatch` is not noise:
    the bridge compared two identities it could both read and they named different provider accounts,
    which means the credential in front of the sessions is not the one the account is supposed to
    hold. Leaving it `active` would keep every session on a token whose owner has already changed.
    `identity_unverifiable` keeps the two-in-24-hours rule: it is the answer for CODEX material that
    names nobody while this bridge had announced an account, so it says nothing about the credential
    at all. Muse and Grok cannot raise it — their format never announces an identity, so there is no
    announced account for the comparison to lose.
13. **§C.2's credential-on-the-ledger is replaced: the material travels sealed per recipient on the
    mesh, and only the account's METADATA travels on the ledger.** §C.2 was written against a
    reading of `SharedSettingSecret` that the code no longer supports: a fleet secret does NOT
    ride the ledger re-encrypted by the receiver — `sync/runtime.rs::carries_shared_secret` turns
    an operation naming one into a REDACTED chain position whose body is never stored, served or
    materialized, and the value reaches its peers through `MESH_MSG_SHARED_SECRETS_SYNC`
    (`mesh/shared_secrets.rs`), sealed for exactly one recipient. The reason is stated in
    `repository.rs:1036-1039`: a ledger operation body is stored and relayed in plaintext by every
    node on the path, and migration 158 exists only to scrub the secrets an earlier build left in
    the capture journal. A provider OAuth token has a larger blast radius than `hf_token`, so it
    takes the same route, not the one that was abandoned:
    - new frame `MESH_MSG_PROVIDER_CREDENTIALS_SYNC` (0x29) carrying
      `ProviderCredentialsSyncPayload`, built by `mesh/provider_credentials.rs::build_for_peer` and
      taken in by `::ingest`. Each entry is sealed with `MeshSecurity::seal_for_peer` bound to
      `provider-credential|<account>|<revision>|<sha256>`, so it opens on ONE peer and cannot be
      replanted under another account or revision. The receiver re-computes the digest of what it
      opened before storing, and stores it with the local `SettingsCipher::encrypt_bound`;
    - no `MeshCommandType` is added (correction 8 stands) and `SCHEMA_VERSION` stays 29 (correction
      6): the frame is a new mesh discriminator, and an older peer that does not know it drops it.
      The value is a free slot INSIDE the Mesh channel kind range
      (`tentaflow-sdk-spec/src/protocol/frame/channel.rs::valid_kind_range`, 0x10..=0x51) because
      `validate_channel_kind` runs before dispatch and rejects anything above it as `UnknownKind` —
      the first allocation (0x54) sat one past the bound and every fan-out was dropped in silence;
    - the ledger keeps `provider_accounts`, `provider_account_grants`, `agent_runtime_nodes` and
      `agent_runtime_engines` exactly as §C.1 registered them. `provider_account_credentials` is
      still NOT a sync resource and `sync_resource_acl` is not used for it: with the material off
      the ledger there is no ledger row to restrict, and the gate is applied where the material is
      read out of the store instead (`repository::publishable_credentials`).
    Three gates decide every crossing, all three in the store rather than in the transport:
    1. **fleet** — the peer must have `agent_runtime_nodes.receives_accounts = 1`. The sender builds
       no entry for any other node, and the receiver refuses the whole frame when its own flag is
       off. That flag is replicated, so both ends answer from the same administrator decision;
    2. **home** — `credential_exchange_allowed(local, peer, home_node_id)`: one of the two ends must
       be `provider_accounts.home_node_id`. The home node FANS OUT to its satellites, and a
       satellite whose CLI rotated the token SUBMITS it to the home, which applies the revision CAS
       and fans the result out. Two satellites never exchange credentials, so they cannot take turns
       overwriting each other's revision. A refusal is audited as
       `provider_account.credential_refused {reason: "not_home_node"}` — nothing in the resulting
       state records it, so the refusal IS the record. An account with NO home is closed in both
       directions (an account that never had one is in the same state, and either way the honest
       answer is that a new home must be named — by a sign-in at the provider, or by pasting a key
       on the chosen node);
    3. **revocation** — the revision must be above `provider_accounts.credential_revoked_revision`.
       A submission BELOW the revision the receiver already holds is dropped — the home is the
       single refresher and the material it holds is the newer one — and, like a `not_home_node`
       refusal, it is the record: `provider_account.credential_refused {reason: "stale_revision",
       revision, held_revision}`, naming the node that offered, the revision it offered and the
       revision held when the offer was dropped. Without that row a satellite whose copy trails
       the home loses its rotation to the next fan-out with no trace on the home.
    **The home node is the node where the credential was DELIBERATELY PLACED.** `write_credential`
    records it for a local mint (no expected revision), and a mint by an ADMINISTRATOR PASTING AN API
    KEY takes over a home that already exists: that key exists only where it was pasted, so pasting
    it on another node is the ONLY operation that can move such an account, and without this the
    refusal below would name a remedy the account does not have. Everything else is stricter: a
    subscription's mint only fills a NULL home (holding a copy is not placing one), a revision
    APPLIED from the mesh never places anything (its `expected_revision` is set), and a SIGN-IN moves
    the home explicitly (`login.rs::adopt_credential` → `update_account.home_node_id`, unchanged).
    That is what makes the rule stable: a rotation cannot promote its node, only a placement or a
    sign-in can. `AccountUpdate.home_node_id` exists in the store but no wire variant carries it, so
    there is no "move home" button — the operator moves it by pasting the key or signing in on the
    target node; exposing a separate control would let an administrator name a node that never
    minted anything, and is not part of this package.
    **Revocation is metadata, because a removal has no material to travel.** Migration 159 adds
    `provider_accounts.credential_revoked_revision` (0 = nothing revoked). `clear_credential` raises
    it to the revision it dropped, the materializer applies the incoming value as
    `MAX(stored, incoming)` (HLC-LWW decides which EDIT is newer; a revocation must not be undone by
    an older row arriving late), and every node purges its own copy on the reconcile that the
    materializer starts after applying a `ProviderAccount` or `AgentRuntimeNode` row
    (`provider_accounts/credential_sync.rs`, also run at startup). A purge is BOTH halves: the store
    row and EVERY file the node holds — the canonical credential and the private login home the
    bridge keeps for its own sign-in, probe and discovery runs, each of the two trees whole. (That
    scope is the node's own view
    of the two account trees, measured on macOS with the codex bridge; on Linux they are the same
    two trees, but a session reaches the canonical file through a bubblewrap `--bind` mount point
    and no purge in this package was exercised against a live one.) The file half is done by
    whichever of the two can do it: a RUNNING bridge is told to drop both (bridge route `DELETE
    /account/credential`, refused while a sign-in is running, exactly as the PUT is — correction 11)
    and is then stopped, while with NO bridge running Core removes both trees from the account root
    itself (`services::coding_agent::purge_account_credentials`, called from
    `agent_runtime::drop_account_credential`). The second case is the ordinary one, not an edge:
    `IDLE_GRACE` releases a bridge fifteen minutes after its last turn and a node restart leaves
    none at all, so a purge reaches a live bridge rarely and a purge that needed one would leave
    the plaintext credential on disk exactly when nothing was there to remove it.
    **The two arms remove the same thing, and it is the trees WHOLE.** Not the running engine's file
    inside each of them: the account's engine is a row that can move under a live account while the
    files of the previous one stay where they were (`sync/core_materializer.rs` writes `engine_id`
    from the incoming row), so a purge that named a path would leave exactly the copies it was
    written to remove. Neither arm names one — Core walks the two directories, and so does the
    bridge (`credentials::remove_account_credentials`). What goes with a tree is a credential and
    not state a later run needs: the per-engine directories exist before any engine uses them
    (`credentials::prepare`), the bridge writes only a provider credential into the canonical one,
    and the login home holds, beside its copy of that credential, whatever a sign-in, a probe or a
    discovery run left in the CLI's own home — the scratch of a run that has ended, re-materialized
    from the canonical file by the next lease. A session's own home is a different root and no part
    of this.
    That removal reaches BOTH trees even when one of them refuses — a `uchg` file on macOS, an
    unreadable subtree, a `0500` parent directory: unlinking an entry needs write permission on its
    PARENT, so the walk still reads the directory and is refused at `unlink` (EACCES). A file held
    open by a running process is NOT one of those: POSIX unlinks it and the data leaves with the
    last descriptor. The one held-open refusal that exists is a busy DIRECTORY's final `rmdir`,
    which macOS answers with `EBUSY`. A sign-in in flight is not a partial failure either — the
    bridge refuses that call whole (`login_in_progress`, so nothing is attempted on its side), and
    Core removes the two trees itself once the bridge has been stopped. A failure in `credentials/`
    used to return before `login/` was looked at, and the copy left behind is a retired token no row
    names any more, so no reconcile can ever come back for it. Both arms attempt the second tree
    regardless and report the failure; the acknowledgement an operator sees carries `ok = false`
    with message key `agent_accounts.purge_incomplete` instead of a clean success, and the paths
    that could not be emptied travel in the node's warning on EITHER arm (`AccountOpAck` has no
    field for them): Core's `purge_account_credentials` names every tree it failed on, and a bridge
    names its own in the error body that `call_bridge` embeds in the warning. The reconcile returns
    an error for the same reason, after every stale account has been reached rather than instead of
    reaching them — a store failure no longer stops the walk either, because the accounts it has not
    reached are the only names left for their own files.
    **What the walk does NOT protect against, stated because the opposite reads as a guarantee.** Both
    arms remove a link as the link it is — neither descends into it — so a link planted where
    `credentials/` or `login/` should be cannot aim the purge at a host path of somebody else's
    choosing. The ACCOUNT DIRECTORY is not covered by that rule: it is resolved by joining
    `<keys>/coding-agents/accounts` with the account id, so only the FINAL component of the path the
    walk starts from is protected. An account directory that is ITSELF a symlink is followed by the
    kernel when the two names are unlinked, and the purge then removes `credentials` and `login`
    inside whatever it points at. The blast radius is exactly those two literal names — the walk
    never goes deeper than the two roots and never names a file inside them — but a purge is not a
    proof that the trees it removed were the account's own. Nothing in the product creates such a
    link: the account root is made by `coding_agent::prepare_account_root`, which creates it as a
    directory.
    The login home is never a second source of truth: it is a working copy of the account's ONE file,
    so the lease that hands it out clears it when there is no canonical credential to put there
    rather than leaving the copy behind, and a publication from it cannot CREATE a credential the
    account no longer has. Without that, a copy that outlived its file is what a removed credential
    comes back from — the next probe reads it, the vendor CLI answers that the account is signed in,
    and the publication after it puts the retired material back in front of every session on the
    node. A sign-in is unaffected by either rule: it materializes BEFORE its CLI writes anything, so
    a clear can only ever reach a copy of an earlier run. A sign-in after a clear mints ABOVE
    the mark, or every node that saw the revocation would refuse the new credential as covered by it.
    The same reconcile enforces the fleet flag: a node whose `receives_accounts` goes to 0 purges the
    material it already holds, and an account deleted anywhere drops the bridge copy from the arm
    that observed the delete (the FK cascade has already taken the row, so the reconcile cannot see
    it). Which of the two removes the files depends on whether a bridge is running; THAT a purge
    removes them does not, and that is why `set_receives_accounts` REFUSES to turn the flag off
    for a node that is some account's home: there the purge would drop the one copy every other
    node's is fanned out from, and for an account whose material exists only there, the credential
    itself. The refusal names the count and the remedy for the kinds
    actually homed there — a pasted key moves by being pasted on another node, a subscription by
    being signed in there, and both sentences appear when both kinds are present — and is returned
    to the dashboard as a bad request, not swallowed as an internal error.
    What this does NOT do, stated rather than hidden: the material is only re-sent on a reconnect or
    a local write, so a node that is offline through BOTH the mint and every later push stays without
    the credential until it reconnects (anti-entropy is a full rebuild per peer on connect, which is
    what makes that self-healing rather than lost). A node outside the fleet never relays credentials,
    so two account nodes that can only reach each other THROUGH a non-account node converge only when
    one of them reaches the other directly. And a revocation reaches a node that never comes back
    online never at all — the material on its disk is exactly as retired as the token itself, which
    only the provider can invalidate.
    **A lost home is RECORDED, never inferred.** Deleting the home node NULLs
    `provider_accounts.home_node_id`, which is the same state an account that never had a home is in
    — so migration rung 164 `provider_account_home_lost` (`db/migrations.rs:1045`) adds
    `home_lost_node_id TEXT NULL`. It is set by the one UPDATE that NULLs the home
    (`forget_node_tx`, `provider_accounts/repository.rs:697`) before the capture that publishes the
    row, so the record cannot be a second write that gets lost; it is cleared by every path that
    leaves the local node as the home — `claim_home_tx` including its early return for a home
    already here (`:134`, `:143`), and `update_account` in the same statement when it names one —
    and nothing ever sets it at creation. It is account metadata: it travels with the row through
    `sync_capture` and the core materializer, so a peer receives the record of the loss together
    with the absent home it explains, and a copy can therefore arrive carrying a loss this node
    never recorded. That is why
    the clearing sits on the claim itself rather than on this node's memory of having deleted
    something. It is a node ID and not a name, because the node is gone from the registry and no
    node can resolve one. A03 renders that state as its own — "Brak danych." plus the explanation
    naming the deleted node by its short id (`www/js/modules/agent-accounts-window.js:240-247`) —
    never as "no sessions", which is what an account that never had a home shows.
