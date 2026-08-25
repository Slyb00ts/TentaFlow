// ===== File: agents/service.rs — AgentService: thin harness service wiring
// registry access, tool catalog resolution and core.* builtin execution. It
// does NOT run the agent loop — the loop is a Flow Builder flow (Harness §3.4).
// Constructed in main.rs and pinned into FlowDispatcher's AgentServiceSlot so
// the phase-3 blocks (agent_context, tool_exec) read it. =====

use std::sync::Arc;

use anyhow::Result;

use crate::addon::tool_dispatch::{ToolCallResult, ToolDispatcher};
use crate::addon::AddonManager;
use crate::compliance::ai_gateway::{AiGateway, ToolExecution};
use crate::db::{models::DbAgent, repository, DbPool};
use crate::flow_engine::dispatchers::LlmToolSpec;
use crate::flow_engine::envelope::LlmToolCall;

use super::builtins::{execute_core_tool, is_core_tool, CoreToolName};
use super::catalog::{tool_in_allowlist, ToolCatalog};
use super::principal::AgentPrincipal;

/// Agent skill selection (`agents.skills_json`): explicit names plus tag
/// queries. Both default to empty so a `{}` selection yields no skills.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct SkillSelection {
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
}

/// The harness service. Owns its own dependencies (no ServiceManager / god
/// object): the DB pool for registry + skill reads, and the AddonManager that
/// backs both the tool catalog (live tool list + permission checks) and the
/// ToolDispatcher used to execute addon tool calls.
pub struct AgentService {
    db: DbPool,
    addon_manager: Arc<AddonManager>,
    tool_dispatcher: ToolDispatcher,
}

impl AgentService {
    pub fn new(db: DbPool, addon_manager: Arc<AddonManager>) -> Self {
        let tool_dispatcher = ToolDispatcher::new(addon_manager.clone());
        Self {
            db,
            addon_manager,
            tool_dispatcher,
        }
    }

    // ------------------------------------------------------------------
    // Registry access
    // ------------------------------------------------------------------

    /// Loads an agent definition by id. `None` when the id is unknown.
    pub fn get_agent(&self, id: &str) -> Result<Option<DbAgent>> {
        repository::get_agent(&self.db, id)
    }

    /// Loads an agent definition by name (soft-unique — oldest row wins).
    pub fn get_agent_by_name(&self, name: &str) -> Result<Option<DbAgent>> {
        repository::get_agent_by_name(&self.db, name)
    }

    /// The model to use when an agent definition names none. A fresh install
    /// seeds every agent with `model = NULL`, so without this the first turn of
    /// a new deployment dies in the llm block with "no model" — a message that
    /// names the node config and the envelope, neither of which the operator
    /// set. See `services_repo::models::default_llm_model` for what counts.
    pub fn default_llm_model(&self) -> Option<String> {
        let conn = self.db.read().ok()?;
        crate::services_repo::models::default_llm_model(&conn)
            .ok()
            .flatten()
    }

    /// Direct DB handle — the agent_context block uses it to read the skills
    /// repository (skills index) and create the `agent_runs` row. Sharing the
    /// pool keeps the service the single owner of agent-domain persistence.
    pub fn db(&self) -> &DbPool {
        &self.db
    }

    /// The node's settings key. `delegate_cli` needs it to open the Code Studio
    /// vault row holding the provider credential (§5.2) — the key is per node,
    /// which is what makes that credential node-local.
    pub fn settings_cipher(&self) -> &Arc<crate::crypto::SettingsCipher> {
        self.addon_manager.settings_cipher()
    }

    // ------------------------------------------------------------------
    // Skills index
    // ------------------------------------------------------------------

    /// Resolves the skill index a model sees for one agent (Hermes-style
    /// `<available_skills>`): every active skill admitted by the agent's
    /// `skills_json` allowlist (`{"names":[...],"tags":[...]}`), as `(name,
    /// description)` pairs. The agent_context block renders these into the
    /// system prompt with the "load with core.skill_view" directive (§3.2,
    /// §3.4). Deduplicated, name-ordered, so the index is deterministic.
    pub fn skill_index(&self, skills_json: &str) -> Result<Vec<(String, String)>> {
        use std::collections::BTreeMap;

        let sel: SkillSelection = serde_json::from_str(skills_json).unwrap_or_default();
        let mut out: BTreeMap<String, String> = BTreeMap::new();

        for name in &sel.names {
            if let Some(skill) = repository::get_skill_by_name(&self.db, name)? {
                if skill.status == "active" {
                    out.insert(skill.name, skill.description);
                }
            }
        }
        for tag in &sel.tags {
            let filter = crate::db::models::SkillListFilter {
                source: None,
                status: Some("active"),
                tag: Some(tag.as_str()),
            };
            for skill in repository::list_skills(&self.db, &filter)? {
                out.insert(skill.name, skill.description);
            }
        }
        Ok(out.into_iter().collect())
    }

    // ------------------------------------------------------------------
    // Addon index
    // ------------------------------------------------------------------

    /// The addons a model may actually use this turn, derived from the RESOLVED
    /// tool catalog — the allowlist ∩ permission intersection is computed once,
    /// in `tool_catalog_from_allowlist`, and this reads its result. An addon the
    /// principal cannot call therefore never appears in the prompt.
    ///
    /// Order follows first appearance in `specs` (i.e. the registry order the
    /// tools were advertised in), deduplicated. `core.*` builtins carry no addon
    /// and are skipped; an instance whose row disappeared between catalog
    /// resolution and this read contributes nothing to describe.
    pub fn addon_index(&self, specs: &[LlmToolSpec]) -> Result<Vec<repository::AddonPromptInfo>> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for spec in specs {
            let Some((addon_id, _)) = spec.name.split_once('.') else {
                continue;
            };
            if addon_id == super::builtins::CORE_ADDON_ID || !seen.insert(addon_id.to_string()) {
                continue;
            }
            if let Some(info) = repository::get_addon_prompt_info(&self.db, addon_id)? {
                out.push(info);
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------------
    // Tool catalog
    // ------------------------------------------------------------------

    /// Resolves the LlmToolSpec list a model sees for `agent` under
    /// `principal`: addon tools admitted by the agent's allowlist AND granted
    /// to the principal, plus the allowed core.* builtins (§3.1, §3.3). Reads
    /// the live addon tool list and per-addon "llm" permission for the
    /// principal's user. The sub-agent control builtins surface only when the
    /// agent may spawn (`max_subagents > 0`, §3.6).
    pub fn tool_catalog(&self, agent: &DbAgent, principal: &AgentPrincipal) -> Vec<LlmToolSpec> {
        self.tool_catalog_from_allowlist(&agent.tools_json, principal, agent.max_subagents > 0)
    }

    /// Same as `tool_catalog` but driven by a raw allowlist JSON + an explicit
    /// `can_spawn` flag — the harness passes the agent's `tools_json` through
    /// flow variables, so the block can resolve the catalog without re-loading
    /// the agent row.
    pub fn tool_catalog_from_allowlist(
        &self,
        tools_json: &str,
        principal: &AgentPrincipal,
        can_spawn: bool,
    ) -> Vec<LlmToolSpec> {
        let addon_tools = self.addon_manager.list_tools();
        let checker = self.addon_manager.permission_checker();
        let user_id = principal.user_id().map(|s| s.to_string());
        ToolCatalog::resolve(tools_json, principal, &addon_tools, can_spawn, |addon_id| {
            // No user → no addon tool admission (the catalog already short-
            // circuits, but keep the check explicit). Admin bypass and grant
            // logic live entirely in the permission engine.
            match user_id.as_deref() {
                Some(uid) => checker
                    .check(
                        addon_id,
                        uid,
                        crate::addon::permissions::LLM_PERMISSION_ID,
                        None,
                    )
                    .is_granted(),
                None => false,
            }
        })
    }

    // ------------------------------------------------------------------
    // Tool dispatch
    // ------------------------------------------------------------------

    /// True when `name` is a core.* builtin (dispatched in Core, not WASM).
    pub fn is_core_tool(&self, name: &str) -> bool {
        is_core_tool(name)
    }

    /// LLM-facing specs of every core builtin — used to advertise builtins
    /// independent of an agent allowlist (e.g. the tools-catalog protocol).
    pub fn core_tool_specs(&self) -> Vec<LlmToolSpec> {
        CoreToolName::all().iter().map(|c| c.spec()).collect()
    }

    /// Executes one core.* builtin call. Synchronous (DB read + use_count bump);
    /// the tool_exec block runs it on a blocking thread. Returns the JSON result
    /// handed back to the model. Unknown / malformed core calls return an error
    /// the block shapes into a `[TOOL_ERROR]` tool result (model recovers).
    pub fn execute_core_tool(
        &self,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        execute_core_tool(&self.db, name, arguments)
    }

    /// Records one executed tool call against an agent run's latest AI event
    /// (§3.10). Best-effort: a missing run event or an audit write error is
    /// logged and swallowed — the tool loop must never abort on audit failure.
    pub fn record_tool_execution(&self, agent_run_id: &str, execution: &ToolExecution<'_>) {
        let gateway = AiGateway::new(
            self.db.clone(),
            crate::mesh::node_info_collector::local_hostname(),
            crate::compliance::ai_gateway::token_quota_enabled(),
        );
        if let Err(e) = gateway.record_run_tool_execution(agent_run_id, execution) {
            tracing::warn!("agent tool-execution audit failed (skipping): {e}");
        }
    }

    /// Executes one addon tool call through the revived ToolDispatcher.
    /// Permission enforcement happens inside `AddonManager::call_tool`; this is
    /// the synchronous wasmtime path the tool_exec block wraps in
    /// `spawn_blocking` (§2.12).
    /// Every registered addon tool definition, for callers that need a tool's
    /// declared properties rather than its result.
    pub fn addon_tool_catalog(&self) -> Vec<crate::addon::ToolDefinition> {
        self.tool_dispatcher.tool_catalog()
    }

    pub fn dispatch_addon_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
        user_id: &str,
    ) -> Result<serde_json::Value> {
        self.tool_dispatcher
            .dispatch_tool_call(tool_name, arguments, user_id)
    }

    /// Executes one addon tool call skipping the addon permission check — the
    /// harness already adjudicated a grant (§3.13 B, AllowOnce / AllowForRun
    /// retries). NEVER call without first gating permission via
    /// `permission_for_tool`.
    pub fn dispatch_addon_tool_preauthorized(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
        user_id: &str,
    ) -> Result<serde_json::Value> {
        self.tool_dispatcher
            .dispatch_tool_call_preauthorized(tool_name, arguments, user_id)
    }

    /// Resolves the live `"llm"` permission of `user_id` for the addon owning
    /// `tool_name` (the per-tool permission the harness gates on, §3.13 B).
    /// Returns the three-state result so the caller can distinguish an explicit
    /// deny from a NotConfigured deny (only the latter raises a grant card).
    pub fn permission_for_tool(
        &self,
        tool_name: &str,
        user_id: &str,
    ) -> crate::addon::permissions::PermissionResult {
        let addon_id = tool_name
            .split_once('.')
            .map(|(a, _)| a)
            .unwrap_or(tool_name);
        self.addon_manager
            .permission_checker()
            .check(addon_id, user_id, "llm", None)
    }

    /// Persists a principal-scoped `"llm"` grant for the addon owning
    /// `tool_name` (the `Always` decision, §3.13 B) and refreshes the permission
    /// cache so a retry passes immediately. `global` (admin-only, decided by the
    /// handler) writes the addon default instead of a per-user grant.
    pub fn persist_tool_grant(
        &self,
        tool_name: &str,
        user_id: &str,
        global: bool,
        actor_user_id: Option<&str>,
    ) -> Result<()> {
        let addon_id = tool_name
            .split_once('.')
            .map(|(a, _)| a)
            .unwrap_or(tool_name);
        if global {
            repository::upsert_permission_default(
                &self.db,
                addon_id,
                "llm",
                "allow",
                actor_user_id,
            )?;
        } else {
            repository::upsert_permission(
                &self.db,
                addon_id,
                "user",
                user_id,
                "llm",
                "allow",
                actor_user_id,
            )?;
        }
        // Refresh the proactive cache so the retried call sees the grant.
        self.addon_manager
            .permission_checker()
            .refresh_addon(addon_id);
        Ok(())
    }

    /// Whether a model-issued tool name is inside the agent's allowlist — the
    /// tool_exec block rejects out-of-surface calls before dispatch (§3.3).
    /// The package behind the called instance comes from the SAME live registry
    /// the catalog resolves against, so a package-level allowlist entry admits
    /// the call here exactly when it advertised the tool to the model.
    pub fn tool_allowed(&self, tools_json: &str, name: &str) -> bool {
        let package_id = name.split_once('.').and_then(|(addon_id, tool_name)| {
            self.addon_manager.tool_package_id(addon_id, tool_name)
        });
        tool_in_allowlist(tools_json, name, package_id.as_deref())
    }

    /// The allowlist of the agent the harness pinned on the run
    /// (`envelope.meta["agent_id"]`). THE one loader: `tool_exec` and every
    /// deterministic block that runs a core verb read the surface here, so the
    /// graph can never out-rank the agent definition.
    ///
    /// No agent id, or an id that no longer resolves, yields `"[]"` — an empty
    /// surface rejects everything, which is the right answer for a
    /// misconfigured flow.
    pub fn agent_tools_json(&self, agent_id: Option<&str>) -> String {
        agent_id
            .and_then(|id| self.get_agent(id).ok().flatten())
            .map(|agent| agent.tools_json)
            .unwrap_or_else(|| "[]".to_string())
    }

    /// Fail-closed gate for a GRAPH block that runs a core verb on the agent's
    /// behalf (§10: `agents.tools_json` is the first sieve, before the PEP).
    ///
    /// A flow author dropping an `exec_command` block into the harness of a
    /// read-only agent must get the agent's answer, not the graph's: the block
    /// therefore passes exactly the sieve a model-issued call passes.
    pub fn require_core_tool(&self, agent_id: Option<&str>, tool: CoreToolName) -> Result<()> {
        let name = tool.public_name();
        if self.tool_allowed(&self.agent_tools_json(agent_id), name) {
            return Ok(());
        }
        let who = match agent_id {
            Some(id) => format!("agent '{id}'"),
            None => "this run (no agent pinned in meta.agent_id)".to_string(),
        };
        Err(anyhow::anyhow!(
            "'{name}' is not in the tool allowlist of {who}: a flow block runs the verb on the \
             agent's behalf, so it passes the same first sieve (agents.tools_json) as a call the \
             model issues"
        ))
    }

    /// Executes every call of one assistant turn for a run principal: core.*
    /// builtins run in Core, addon tools go through the ToolDispatcher. A call
    /// outside the agent allowlist, or to an unknown tool, becomes an error
    /// result (never an aborted batch — the model expects a reply per call id).
    /// An unattended principal (no user) can only run core builtins.
    pub fn process_tool_calls(
        &self,
        tools_json: &str,
        tool_calls: &[LlmToolCall],
        principal: &AgentPrincipal,
    ) -> Vec<ToolCallResult> {
        let mut results = Vec::with_capacity(tool_calls.len());
        for call in tool_calls {
            results.push(self.process_one_call(tools_json, call, principal));
        }
        results
    }

    fn process_one_call(
        &self,
        tools_json: &str,
        call: &LlmToolCall,
        principal: &AgentPrincipal,
    ) -> ToolCallResult {
        if !self.tool_allowed(tools_json, &call.name) {
            return error_result(call, format!("tool '{}' not in agent allowlist", call.name));
        }
        if is_core_tool(&call.name) {
            return self.run_core_call(call);
        }
        let Some(user_id) = principal.user_id() else {
            return error_result(
                call,
                format!("tool '{}' requires a user principal", call.name),
            );
        };
        let arguments = match serde_json::from_str::<serde_json::Value>(&call.arguments) {
            Ok(v) => v,
            Err(e) => return error_result(call, format!("invalid arguments JSON: {e}")),
        };
        match self.dispatch_addon_tool(&call.name, arguments, user_id) {
            Ok(output) => ToolCallResult {
                tool_call_id: call.id.clone(),
                name: call.name.clone(),
                content: serde_json::to_string(&output).unwrap_or_default(),
                success: true,
            },
            Err(e) => error_result(call, e.to_string()),
        }
    }

    fn run_core_call(&self, call: &LlmToolCall) -> ToolCallResult {
        let arguments = match serde_json::from_str::<serde_json::Value>(&call.arguments) {
            Ok(v) => v,
            Err(e) => return error_result(call, format!("invalid arguments JSON: {e}")),
        };
        match self.execute_core_tool(&call.name, &arguments) {
            Ok(output) => ToolCallResult {
                tool_call_id: call.id.clone(),
                name: call.name.clone(),
                content: serde_json::to_string(&output).unwrap_or_default(),
                success: true,
            },
            Err(e) => error_result(call, e.to_string()),
        }
    }
}

fn error_result(call: &LlmToolCall, error: String) -> ToolCallResult {
    ToolCallResult {
        tool_call_id: call.id.clone(),
        name: call.name.clone(),
        content: serde_json::json!({ "error": error }).to_string(),
        success: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use crate::db::models::SkillParams;

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn addon_manager(pool: DbPool) -> Arc<crate::addon::AddonManager> {
        let cipher = Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32]));
        Arc::new(crate::addon::AddonManager::new(pool, cipher).expect("addon manager"))
    }

    fn spec(name: &str) -> LlmToolSpec {
        LlmToolSpec {
            name: name.to_string(),
            description: format!("{name} description"),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    /// One installed instance row: `addon_id` is the instance, `package_id` the
    /// package it was created from.
    fn seed_addon_row(
        pool: &DbPool,
        addon_id: &str,
        package_id: &str,
        display_name: &str,
        description: &str,
    ) {
        pool.write()
            .unwrap()
            .execute(
                "INSERT INTO addons (addon_id, name, display_name, version, package_id, \
                 package_version, description, platforms, manifest_json) \
                 VALUES (?1, ?2, ?3, '1.0.0', ?4, '1.0.0', ?5, 'linux', '{}')",
                rusqlite::params![addon_id, package_id, display_name, package_id, description],
            )
            .expect("seed addon row");
    }

    /// A skill materialized from an addon's SKILL.md (source='addon').
    fn seed_addon_skill(pool: &DbPool, id: &str, name: &str, addon_id: &str) {
        repository::upsert_skill(
            pool,
            &SkillParams {
                id,
                name,
                display_name: None,
                description: "Addon skill",
                content: "# How to use this addon",
                tags_json: "[]",
                category: None,
                source: "addon",
                source_ref: Some(addon_id),
                status: "active",
                created_by: None,
                actor_user_id: None,
            },
        )
        .expect("seed addon skill");
    }

    fn seed_skill(pool: &DbPool, id: &str, name: &str) {
        seed_skill_full(pool, id, name, "A test skill", "[]", "active");
    }

    #[allow(clippy::too_many_arguments)]
    fn seed_skill_full(
        pool: &DbPool,
        id: &str,
        name: &str,
        description: &str,
        tags_json: &str,
        status: &str,
    ) {
        repository::upsert_skill(
            pool,
            &SkillParams {
                id,
                name,
                display_name: Some("Test Skill"),
                description,
                content: "# Test\nDo the thing.",
                tags_json,
                category: None,
                source: "user",
                source_ref: None,
                status,
                created_by: None,
                actor_user_id: None,
            },
        )
        .expect("seed skill");
    }

    fn service(pool: DbPool) -> AgentService {
        let cipher = Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32]));
        let addon_manager =
            Arc::new(crate::addon::AddonManager::new(pool.clone(), cipher).expect("addon manager"));
        AgentService::new(pool, addon_manager)
    }

    #[tokio::test]
    async fn skill_index_resolves_names_and_tags_deduplicated_and_sorted() {
        let pool = db();
        seed_skill_full(
            &pool,
            "id-a",
            "alpha",
            "Alpha skill",
            r#"["research"]"#,
            "active",
        );
        seed_skill_full(
            &pool,
            "id-b",
            "beta",
            "Beta skill",
            r#"["research"]"#,
            "active",
        );
        seed_skill_full(&pool, "id-c", "gamma", "Gamma skill", "[]", "disabled");
        let svc = service(pool);

        // names picks alpha (active); tags picks the research-tagged alpha+beta.
        // alpha appears in both selectors but is deduplicated.
        let index = svc
            .skill_index(r#"{"names":["alpha","gamma"],"tags":["research"]}"#)
            .expect("skill index");
        let names: Vec<&str> = index.iter().map(|(n, _)| n.as_str()).collect();
        // gamma is disabled → excluded; result is name-sorted.
        assert_eq!(names, vec!["alpha", "beta"]);
        assert_eq!(index[0].1, "Alpha skill");
    }

    #[tokio::test]
    async fn skill_index_empty_selection_is_empty() {
        let pool = db();
        seed_skill(&pool, "id-x", "x");
        let svc = service(pool);
        assert!(svc.skill_index("{}").expect("index").is_empty());
    }

    #[test]
    fn execute_core_skill_view_loads_skill_and_bumps_use_count() {
        let pool = db();
        let skill_id = "00000000-0000-0000-0000-0000000000aa";
        seed_skill(&pool, skill_id, "test-skill");

        let result = execute_core_tool(
            &pool,
            "core.skill_view",
            &serde_json::json!({"name": "test-skill"}),
        )
        .expect("skill_view ok");
        assert_eq!(result["skill"], "test-skill");
        assert!(result["content"].as_str().unwrap().contains("Do the thing"));

        let use_count: i64 = repository::get_skill(&pool, skill_id)
            .expect("get skill")
            .expect("skill exists")
            .use_count;
        assert_eq!(use_count, 1, "skill_view must bump use_count");

        // A second view bumps again.
        execute_core_tool(
            &pool,
            "core.skill_view",
            &serde_json::json!({"name": "test-skill"}),
        )
        .expect("skill_view ok 2");
        let use_count: i64 = repository::get_skill(&pool, skill_id)
            .expect("get skill")
            .expect("skill exists")
            .use_count;
        assert_eq!(use_count, 2);
    }

    #[test]
    fn execute_core_skill_view_missing_skill_is_error() {
        let pool = db();
        let err = execute_core_tool(
            &pool,
            "core.skill_view",
            &serde_json::json!({"name": "nope"}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("nope"));
    }

    /// The addon index is DERIVED from the resolved catalog: one entry per addon
    /// that actually contributed a tool, in first-appearance order, with the
    /// display name/description an operator sees and the addon's skill when it
    /// has one. `core.*` never yields an addon.
    // `AddonManager::new` starts the permission-cache refresh task, so the
    // service needs a reactor.
    #[tokio::test]
    async fn addon_index_describes_only_addons_present_in_the_resolved_catalog() {
        let pool = db();
        seed_addon_row(
            &pool,
            "deep-research-043b6b64",
            "deep-research",
            "Deep Research",
            "Searches the public web.",
        );
        seed_addon_row(&pool, "memory-aa11bb22", "memory", "", "Remembers facts.");
        seed_addon_skill(
            &pool,
            "33333333-0000-0000-0000-000000000001",
            "deep-research",
            "deep-research-043b6b64",
        );

        let specs = vec![
            spec("deep-research-043b6b64.search_web"),
            spec("deep-research-043b6b64.read_url"),
            spec("memory-aa11bb22.memory_store"),
            spec("core.skill_view"),
        ];
        let service = AgentService::new(pool.clone(), addon_manager(pool));
        let index = service.addon_index(&specs).expect("addon index");

        assert_eq!(index.len(), 2, "one entry per addon, deduplicated");
        assert_eq!(index[0].addon_id, "deep-research-043b6b64");
        assert_eq!(index[0].display_name, "Deep Research");
        assert_eq!(index[0].description, "Searches the public web.");
        assert_eq!(index[0].skill_name.as_deref(), Some("deep-research"));
        // No display_name → the manifest name carries the label; no skill → None.
        assert_eq!(index[1].addon_id, "memory-aa11bb22");
        assert_eq!(index[1].display_name, "memory");
        assert_eq!(index[1].skill_name, None);
    }

    /// An addon the catalog did NOT admit contributes no line — the prompt can
    /// never advertise an addon whose tools the principal may not call.
    // `AddonManager::new` starts the permission-cache refresh task, so the
    // service needs a reactor.
    #[tokio::test]
    async fn addon_index_omits_an_addon_with_no_admitted_tool() {
        let pool = db();
        seed_addon_row(
            &pool,
            "memory-aa11bb22",
            "memory",
            "Memory",
            "Remembers facts.",
        );
        seed_addon_row(
            &pool,
            "contacts-cc33dd44",
            "contacts",
            "Contacts",
            "CRM source of truth.",
        );
        let specs = vec![spec("memory-aa11bb22.memory_store")];
        let service = AgentService::new(pool.clone(), addon_manager(pool));
        let index = service.addon_index(&specs).expect("addon index");
        let ids: Vec<&str> = index.iter().map(|a| a.addon_id.as_str()).collect();
        assert_eq!(ids, vec!["memory-aa11bb22"]);
    }
}
