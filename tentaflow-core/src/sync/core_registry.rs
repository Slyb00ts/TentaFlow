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
    ApiKey,
    ResourcePermission,
    FlowVersion,
    PiiRule,
    ComplianceDataCategory,
    ComplianceProcessingActivity,
    ComplianceLegalBasis,
    ComplianceRetentionPolicy,
    ComplianceProcessor,
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
