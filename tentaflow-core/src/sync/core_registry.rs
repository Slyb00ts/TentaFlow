// =============================================================================
// Plik: sync/core_registry.rs
// Opis: Rejestr danych platformowych TentaFlow, ktore moga byc zapisywane do
//       Sync Ledgera jako zasoby core zamiast zasobow addonow.
// =============================================================================

use super::ledger::{LedgerResult, PartitionId};

pub const CORE_SYNC_ADDON_ID: &str = "core";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreSyncResourceKind {
    Organization,
    UserAccount,
    UserGroup,
    GroupMember,
    Role,
    OrgMembership,
    SyncNode,
    UserIdentityKey,
    NodeUserAssignment,
    SyncUserOrgProfile,
    Flow,
    FlowModelBinding,
    Skill,
    SkillFile,
    Agent,
    SyncPolicy,
    SyncResourceAcl,
    SyncExplicitShare,
    SharedSettingSecret,
    AddonInstance,
    AddonConfig,
    AddonPermission,
    AddonPermissionDefault,
    AddonVisibility,
    ApiKey,
    ResourcePermission,
    FlowVersion,
    PiiRule,
    ComplianceDataCategory,
    ComplianceProcessingActivity,
    ComplianceLegalBasis,
    ComplianceRetentionPolicy,
    ComplianceProcessor,
    TokenUsageDaily,
    TokenQuota,
    TokenLease,
    ModelMetricsRollup,
    ModelPricing,
    CameraCvPipeline,
    VisionModel,
    CodeWorkspace,
    CodeWorkspaceMember,
    CodeWorkspaceCreatorGrant,
    CodeWorkspaceProjectLink,
    CodeWorkspaceAllowlist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreSyncScope {
    Organization,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreSyncRetention {
    Durable,
    LocalRuntimeOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreSyncDescriptor {
    pub kind: CoreSyncResourceKind,
    pub table_name: &'static str,
    pub resource_type: &'static str,
    pub primary_key_column: &'static str,
    pub scope: CoreSyncScope,
    pub retention: CoreSyncRetention,
    pub partition_suffix: &'static str,
}

impl CoreSyncDescriptor {
    pub fn partition_id(
        &self,
        org_id: &str,
        owner_user_id: Option<&str>,
    ) -> LedgerResult<PartitionId> {
        match self.scope {
            CoreSyncScope::Organization => {
                PartitionId::new(format!("core/org/{org_id}/{}", self.partition_suffix))
            }
            CoreSyncScope::User => {
                let user_id = owner_user_id.unwrap_or("system");
                PartitionId::new(format!(
                    "core/org/{org_id}/user/{user_id}/{}",
                    self.partition_suffix
                ))
            }
        }
    }
}

pub const CORE_SYNC_DESCRIPTORS: &[CoreSyncDescriptor] = &[
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::Organization,
        table_name: "organizations",
        resource_type: "core.organization",
        primary_key_column: "org_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "organizations",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::UserAccount,
        table_name: "user_accounts",
        resource_type: "core.user_account",
        primary_key_column: "id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "users",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::UserGroup,
        table_name: "user_groups",
        resource_type: "core.user_group",
        primary_key_column: "id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "groups",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::GroupMember,
        table_name: "group_members",
        resource_type: "core.group_member",
        primary_key_column: "group_id,user_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "groups",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::Role,
        table_name: "roles",
        resource_type: "core.role",
        primary_key_column: "role_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "roles",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::OrgMembership,
        table_name: "org_memberships",
        resource_type: "core.org_membership",
        primary_key_column: "org_id,user_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "roles",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::SyncNode,
        table_name: "sync_nodes",
        resource_type: "core.sync_node",
        primary_key_column: "node_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "identity",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::UserIdentityKey,
        table_name: "user_identity_keys",
        resource_type: "core.user_identity_key",
        primary_key_column: "key_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "identity",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::NodeUserAssignment,
        table_name: "node_user_assignments",
        resource_type: "core.node_user_assignment",
        primary_key_column: "node_id,user_id,assignment_mode",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "identity",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::SyncUserOrgProfile,
        table_name: "sync_user_org_profiles",
        resource_type: "core.sync_user_org_profile",
        primary_key_column: "org_id,user_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "identity",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::Flow,
        table_name: "flows",
        resource_type: "core.flow",
        primary_key_column: "id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "flows",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::FlowModelBinding,
        table_name: "flow_model_bindings",
        resource_type: "core.flow_model_binding",
        primary_key_column: "id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "flows",
    },
    // Skills registry (Harness §3.2) — replicates like flows. Node-local usage
    // stats (use_count, last_used_at) are intentionally NOT part of the synced
    // field set; see the capture builders in db/repository.rs.
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::Skill,
        table_name: "skills",
        resource_type: "core.skill",
        primary_key_column: "id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "skills",
    },
    // Markdown/text reference files of a skill. Composite key (skill_id, path)
    // travels as a length-prefixed composite resource_id (per-file LWW), the
    // same scheme addon_config uses for (addon_id, key).
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::SkillFile,
        table_name: "skill_files",
        resource_type: "core.skill_file",
        primary_key_column: "skill_id,path",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "skills",
    },
    // Agents registry (Harness §3.3) — replicates like flows/skills. Runtime
    // `agent_runs` are deliberately absent: like `flow_executions`, they are
    // node-local execution state and never travel through sync.
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::Agent,
        table_name: "agents",
        resource_type: "core.agent",
        primary_key_column: "id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "agents",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::SyncPolicy,
        table_name: "sync_policies",
        resource_type: "core.sync_policy",
        primary_key_column: "policy_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "sync-control",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::SyncResourceAcl,
        table_name: "sync_resource_acl",
        resource_type: "core.sync_resource_acl",
        primary_key_column: "org_id,addon_id,resource_type,resource_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "sync-control",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::SyncExplicitShare,
        table_name: "sync_explicit_shares",
        resource_type: "core.sync_explicit_share",
        primary_key_column:
            "org_id,addon_id,resource_type,resource_id,subject_type,subject_id,action",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "sync-control",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::SharedSettingSecret,
        table_name: "settings",
        resource_type: "core.shared_setting_secret",
        primary_key_column: "key",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "external-credentials",
    },
    // Installed addon instances (the `addons` row). Bundled-package instances
    // replicate fleet-wide; the receiver loads the wasm from its own (identical)
    // bundled package store via a post-apply runtime reconcile. Uploaded-package
    // instances are NOT captured until package-byte transport exists. Per-addon
    // config + secrets are separate (secrets stay node-local).
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::AddonInstance,
        table_name: "addons",
        resource_type: "core.addon_instance",
        primary_key_column: "addon_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "addons",
    },
    // Per-addon NON-secret config (`addon_config` rows with is_secret=0). Secret
    // rows (passwords, tokens) stay node-local by design. Same "addons" partition
    // as the instance so a row's config applies after its install. Per-key LWW.
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::AddonConfig,
        table_name: "addon_config",
        resource_type: "core.addon_config",
        primary_key_column: "addon_id,key",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "addons",
    },
    // Per-subject app permission matrix rows (allow/deny/inherit). Replicated so
    // an admin's grant made on any node applies fleet-wide — the executing node
    // re-validates the caller against ITS OWN matrix, so the matrix must be
    // global (T8). Same "addons" partition as the instance so grants apply after
    // the install row. `inherit` travels as an Update (it is a stored row), a
    // row removal travels as a Delete tombstone. Per-row LWW.
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::AddonPermission,
        table_name: "addon_permissions",
        resource_type: "core.addon_permission",
        primary_key_column: "addon_id,subject_type,subject_id,permission_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "addons",
    },
    // Admin-set per-permission default (allow|deny) that overrides the manifest
    // seed. The catalog itself (addon_permission_catalog) is NOT synced — every
    // node rebuilds it from the replicated manifest via sync_manifest_metadata.
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::AddonPermissionDefault,
        table_name: "addon_permission_defaults",
        resource_type: "core.addon_permission_default",
        primary_key_column: "addon_id,permission_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "addons",
    },
    // Per-group launcher visibility for an app/addon instance. Global for the
    // same reason as the matrix: what a user sees must not depend on which node
    // serves their dashboard.
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::AddonVisibility,
        table_name: "addon_visibility",
        resource_type: "core.addon_visibility",
        primary_key_column: "addon_id,group_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "addons",
    },
    // External-app API keys (Tier-2 /v1 surface). Replicated so a key issued on
    // one node verifies on every node — the verifier travels, NEVER the raw key.
    // Node-local `last_used_at` is deliberately excluded from the synced field
    // set (same scheme as skills usage stats); the materializer preserves it on
    // UPSERT so a synced edit cannot reset a node's local usage timestamp.
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::ApiKey,
        table_name: "api_keys",
        resource_type: "core.api_key",
        primary_key_column: "uid",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "security",
    },
    // Generic resource ACL (model/flow/addon allow|deny per subject). Composite
    // key (resource_type, resource_id, subject_type, subject_id) travels as a
    // length-prefixed composite resource_id; the materializer reads the four
    // components from the fields. `clear` replicates as a Delete tombstone so a
    // stale `allow` from another node cannot resurrect a cleared rule (LWW: the
    // newer clear wins over an older allow via core_resource_versions).
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::ResourcePermission,
        table_name: "resource_permissions",
        resource_type: "core.resource_permission",
        primary_key_column: "resource_type,resource_id,subject_type,subject_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "permissions",
    },
    // Append-only Flow Builder version history. Replicated so a flow's snapshot
    // trail survives on every node; the org is the flow's default org at the
    // write site. Not LWW: each row has a unique (flow_id, version_num) and is
    // never edited in place, so concurrent writers never collide on a row.
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::FlowVersion,
        table_name: "flow_versions",
        resource_type: "core.flow_version",
        primary_key_column: "id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "flows",
    },
    // PII redaction rules (org-scoped after the UUID identity redesign). Admins
    // may edit the same rule on different nodes concurrently, so it is LWW.
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::PiiRule,
        table_name: "pii_rules",
        resource_type: "core.pii_rule",
        primary_key_column: "id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "pii",
    },
    // Compliance config (GDPR/RODO catalog). Seeded per-org and editable by
    // org_admin/dpo on any node, so each table is LWW. Runtime AI-audit event
    // tables are deliberately NOT here — only the static config replicates.
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::ComplianceDataCategory,
        table_name: "compliance_data_categories",
        resource_type: "core.compliance_data_category",
        primary_key_column: "category_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "compliance",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::ComplianceProcessingActivity,
        table_name: "compliance_processing_activities",
        resource_type: "core.compliance_processing_activity",
        primary_key_column: "activity_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "compliance",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::ComplianceLegalBasis,
        table_name: "compliance_legal_basis",
        resource_type: "core.compliance_legal_basis",
        primary_key_column: "legal_basis_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "compliance",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::ComplianceRetentionPolicy,
        table_name: "compliance_retention_policies",
        resource_type: "core.compliance_retention_policy",
        primary_key_column: "retention_policy_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "compliance",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::ComplianceProcessor,
        table_name: "compliance_processors",
        resource_type: "core.compliance_processor",
        primary_key_column: "processor_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "compliance",
    },
    // Token accounting. `token_usage_daily` is single-writer-per-row (only the
    // owning node mutates its rows) but still replicates so any node can sum the
    // mesh-wide usage; quotas and leases are admin/coordinator edited (LWW).
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::TokenUsageDaily,
        table_name: "token_usage_daily",
        resource_type: "core.token_usage_daily",
        primary_key_column: "id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "tokens",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::TokenQuota,
        table_name: "token_quota",
        resource_type: "core.token_quota",
        primary_key_column: "id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "tokens",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::TokenLease,
        table_name: "token_lease",
        resource_type: "core.token_lease",
        primary_key_column: "id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "tokens",
    },
    // Model performance/usage metrics. `model_metrics_rollup` is
    // single-writer-per-row (each node accumulates only its own `id` rows; the
    // mesh-wide figure is the SUM across all node rows) but replicates so any node
    // can render fleet metrics; `model_pricing` is admin edited (LWW).
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::ModelMetricsRollup,
        table_name: "model_metrics_rollup",
        resource_type: "core.model_metrics_rollup",
        primary_key_column: "id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "metrics",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::ModelPricing,
        table_name: "model_pricing",
        resource_type: "core.model_pricing",
        primary_key_column: "id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "metrics",
    },
    // Configurable camera CV pipelines. Admin-edited on any node (LWW) and
    // replicated fleet-wide like flows; the per-camera assignment
    // (`cameras.cv_pipeline_id`) stays node-local with the `cameras` table.
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::CameraCvPipeline,
        table_name: "camera_cv_pipelines",
        resource_type: "core.camera_cv_pipeline",
        primary_key_column: "id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "cameras",
    },
    // Vision model registry (dynamic camera-CV ONNX models). Row METADATA
    // replicates fleet-wide (LWW, admin edited) so pipelines referencing a
    // model alias validate on every node; the model FILE does not sync — a
    // node without the ONNX serves nothing locally and inference reaches the
    // owning node through the existing mesh camera_cv forward (alias
    // resolution picks the node whose catalog advertises the model).
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::VisionModel,
        table_name: "vision_models",
        resource_type: "core.vision_model",
        primary_key_column: "model_name",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "cameras",
    },
    // Code Studio registry (plan §5.1) — the catalog of WHAT exists and who may
    // touch it, org-scoped and replicated so a workspace created on the desktop
    // is visible on the phone. Only its `node_id` may RUN it; every other node
    // renders it and says whose it is (`is_local` is computed against the local
    // node id, never assumed).
    //
    // The whole satellite set shares ONE partition with the workspace row (the
    // same grouping `addon_config` keeps with its `addons` install). A satellite
    // that still arrives before its workspace is answered with
    // `DeferredOrdering`, so the inbox retries it instead of burning it as a
    // conflict.
    //
    // NEVER add here, and the omission is a security boundary, not an oversight:
    //   * `code_workspace_secrets` / `code_agent_credentials` (§5.2) — key
    //     material encrypted with the PER-NODE SettingsCipher key. Replicated it
    //     would be undecryptable at the far end anyway, so shipping it would only
    //     widen the attack surface for no gain. Same decision as `addon_config`,
    //     which replicates `is_secret=0` rows only. Since migration 138 the vault
    //     is not even in this database: it lives in the Code Studio instance's
    //     content DB (`code_studio::db`), which the sync engine never opens.
    //   * `session_assertion_jti` (§12.1) — a replay guard is meaningful only on
    //     the node that verifies the assertion.
    //   * everything in `workspace.db` (sessions, events, operations, patch sets)
    //     — owner-node runtime; remote access is `RemoteProxy` over the mesh
    //     (§3, §12), not replication.
    //   * `code_workspace_saga_steps` — the provisioning run state of ONE node's
    //     saga. No other node can resume, retry or compensate it, and the durable
    //     outcome a remote UI needs (`status` + `status_detail`) already travels
    //     on the workspace row. Same class as `flow_executions` / `agent_runs`,
    //     and kept in the instance content DB next to the vault.
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::CodeWorkspace,
        table_name: "code_workspaces",
        resource_type: "core.code_workspace",
        primary_key_column: "id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "code-studio",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::CodeWorkspaceMember,
        table_name: "code_workspace_members",
        resource_type: "core.code_workspace_member",
        primary_key_column: "workspace_id,user_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "code-studio",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::CodeWorkspaceCreatorGrant,
        table_name: "code_workspace_creator_grants",
        resource_type: "core.code_workspace_creator_grant",
        primary_key_column: "org_id,user_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "code-studio",
    },
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::CodeWorkspaceProjectLink,
        table_name: "code_workspace_project_links",
        resource_type: "core.code_workspace_project_link",
        primary_key_column: "workspace_id,project_id",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "code-studio",
    },
    // Identity is the UNIQUE triple, NOT the table's `id INTEGER PRIMARY KEY
    // AUTOINCREMENT`: two nodes hand the same rowid to different rows, so a
    // rowid-keyed resource would make one node's `net_egress` grant overwrite
    // another's `git_push` grant. Nothing reads or writes an allowlist row by
    // `id` — every query in Code Studio already keys on the triple.
    CoreSyncDescriptor {
        kind: CoreSyncResourceKind::CodeWorkspaceAllowlist,
        table_name: "code_workspace_allowlist",
        resource_type: "core.code_workspace_allowlist",
        primary_key_column: "workspace_id,capability,pattern",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "code-studio",
    },
];

pub fn descriptor_for_kind(kind: CoreSyncResourceKind) -> &'static CoreSyncDescriptor {
    CORE_SYNC_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.kind == kind)
        .expect("core sync descriptor must exist for every resource kind")
}

pub fn descriptor_for_table(table_name: &str) -> Option<&'static CoreSyncDescriptor> {
    CORE_SYNC_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.table_name.eq_ignore_ascii_case(table_name))
}

pub fn descriptor_for_resource_type(resource_type: &str) -> Option<&'static CoreSyncDescriptor> {
    CORE_SYNC_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.resource_type == resource_type)
}

pub fn is_core_sync_table(table_name: &str) -> bool {
    descriptor_for_table(table_name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_contains_flowbuilder_tables() {
        assert_eq!(
            descriptor_for_table("flows").map(|descriptor| descriptor.resource_type),
            Some("core.flow")
        );
        assert_eq!(
            descriptor_for_table("flow_model_bindings").map(|descriptor| descriptor.resource_type),
            Some("core.flow_model_binding")
        );
        // flow_versions snapshot history replicates fleet-wide as an append-only
        // core sync resource (its UUID id is the resource id).
        assert_eq!(
            descriptor_for_table("flow_versions").map(|descriptor| descriptor.resource_type),
            Some("core.flow_version")
        );
    }

    #[test]
    fn registry_contains_skills_tables() {
        assert_eq!(
            descriptor_for_table("skills").map(|descriptor| descriptor.resource_type),
            Some("core.skill")
        );
        assert_eq!(
            descriptor_for_table("skill_files").map(|descriptor| descriptor.resource_type),
            Some("core.skill_file")
        );
        let skill = descriptor_for_kind(CoreSyncResourceKind::Skill);
        assert_eq!(
            skill.partition_id("org-default", None).unwrap().as_str(),
            "core/org/org-default/skills"
        );
    }

    #[test]
    fn registry_contains_agents_table() {
        assert_eq!(
            descriptor_for_table("agents").map(|descriptor| descriptor.resource_type),
            Some("core.agent")
        );
        let agent = descriptor_for_kind(CoreSyncResourceKind::Agent);
        assert_eq!(
            agent.partition_id("org-default", None).unwrap().as_str(),
            "core/org/org-default/agents"
        );
        // agent_runs is runtime state and must never become a sync resource.
        assert!(descriptor_for_table("agent_runs").is_none());
    }

    #[test]
    fn registry_contains_identity_and_rbac_tables() {
        for table in [
            "organizations",
            "user_accounts",
            "user_groups",
            "group_members",
            "roles",
            "org_memberships",
        ] {
            assert!(is_core_sync_table(table), "missing descriptor for {table}");
        }
        // The legacy `users` table is no longer a sync resource.
        assert!(!is_core_sync_table("users"));
    }

    #[test]
    fn registry_contains_model_metrics_tables() {
        // Both metrics tables ARE core sync resources (mesh-wide replication):
        // rollup is single-writer-per-row, pricing is LWW.
        assert_eq!(
            descriptor_for_table("model_metrics_rollup").map(|descriptor| descriptor.resource_type),
            Some("core.model_metrics_rollup")
        );
        assert_eq!(
            descriptor_for_table("model_pricing").map(|descriptor| descriptor.resource_type),
            Some("core.model_pricing")
        );
        let rollup = descriptor_for_kind(CoreSyncResourceKind::ModelMetricsRollup);
        assert_eq!(
            rollup.partition_id("org-default", None).unwrap().as_str(),
            "core/org/org-default/metrics"
        );
    }

    #[test]
    fn registry_contains_code_studio_tables() {
        for (table, resource_type, primary_key) in [
            ("code_workspaces", "core.code_workspace", "id"),
            (
                "code_workspace_members",
                "core.code_workspace_member",
                "workspace_id,user_id",
            ),
            (
                "code_workspace_creator_grants",
                "core.code_workspace_creator_grant",
                "org_id,user_id",
            ),
            (
                "code_workspace_project_links",
                "core.code_workspace_project_link",
                "workspace_id,project_id",
            ),
            (
                "code_workspace_allowlist",
                "core.code_workspace_allowlist",
                "workspace_id,capability,pattern",
            ),
        ] {
            let descriptor =
                descriptor_for_table(table).unwrap_or_else(|| panic!("missing descriptor {table}"));
            assert_eq!(descriptor.resource_type, resource_type);
            assert_eq!(descriptor.primary_key_column, primary_key);
            assert_eq!(
                descriptor.scope,
                CoreSyncScope::Organization,
                "{table} is org-wide (plan §5.1)"
            );
            assert_eq!(descriptor.retention, CoreSyncRetention::Durable);
            // One partition for the whole set: a satellite row must be ordered
            // after the workspace it references.
            assert_eq!(
                descriptor
                    .partition_id("org-default", None)
                    .unwrap()
                    .as_str(),
                "core/org/org-default/code-studio"
            );
        }
    }

    #[test]
    fn code_studio_allowlist_is_not_keyed_by_its_autoincrement_id() {
        // Two nodes assign the same rowid to different rows, so `id` can never be
        // the replicated identity of an allowlist entry.
        let allowlist = descriptor_for_kind(CoreSyncResourceKind::CodeWorkspaceAllowlist);
        assert_ne!(allowlist.primary_key_column, "id");
        assert_eq!(
            allowlist.primary_key_column,
            "workspace_id,capability,pattern"
        );
    }

    /// The tables that must never replicate: the ones in the instance content
    /// DB and in `workspace.db` are out of the main database entirely, and the
    /// one that stays there (`session_assertion_jti`) is not a descriptor.
    #[test]
    fn code_studio_vault_and_runtime_tables_are_not_core_synced() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        let in_main_db = |table: &str| -> bool {
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
                > 0
        };
        // §5.2 — key material encrypted with the per-node SettingsCipher key —
        // and the provisioning run state of one node's saga, both in the Code
        // Studio instance's content DB (`code_studio::db`).
        for table in [
            "code_workspace_secrets",
            "code_agent_credentials",
            "code_workspace_saga_steps",
        ] {
            assert!(
                !is_core_sync_table(table),
                "{table} must stay out of core sync"
            );
            assert!(
                !in_main_db(table),
                "{table} belongs to the instance content DB, not the main database"
            );
        }
        // Replay protection is local by definition (§12.1) and stays in the
        // main database because assertion verification has no instance in hand.
        assert!(in_main_db("session_assertion_jti"));
        assert!(!is_core_sync_table("session_assertion_jti"));
        // §5.3 — owner-node runtime (`workspace.db`), reached over the mesh.
        for table in [
            "sessions",
            "events",
            "operations",
            "patch_sets",
            "approvals",
        ] {
            assert!(
                !is_core_sync_table(table),
                "{table} must stay out of core sync"
            );
        }
    }

    #[test]
    fn runtime_tables_are_not_core_synced() {
        for table in [
            "flow_executions",
            "flow_invocations",
            "audit_log",
            "agent_runs",
        ] {
            assert!(
                !is_core_sync_table(table),
                "{table} must stay out of core sync"
            );
        }
    }

    #[test]
    fn organization_scoped_partitions_are_stable() {
        let flow = descriptor_for_kind(CoreSyncResourceKind::Flow);
        let role = descriptor_for_kind(CoreSyncResourceKind::Role);

        assert_eq!(
            flow.partition_id("org-default", None).unwrap().as_str(),
            "core/org/org-default/flows"
        );
        assert_eq!(
            role.partition_id("org-default", None).unwrap().as_str(),
            "core/org/org-default/roles"
        );
    }
}
