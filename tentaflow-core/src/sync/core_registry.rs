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
    LegacyUser,
    UserGroup,
    GroupMember,
    Role,
    OrgMembership,
    SyncNode,
    UserIdentityKey,
    NodeUserAssignment,
    SyncUserOrgProfile,
    Flow,
    FlowVersion,
    FlowModelBinding,
    SyncPolicy,
    SyncResourceAcl,
    SyncExplicitShare,
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
        owner_user_id: Option<i64>,
    ) -> LedgerResult<PartitionId> {
        match self.scope {
            CoreSyncScope::Organization => {
                PartitionId::new(format!("core/org/{org_id}/{}", self.partition_suffix))
            }
            CoreSyncScope::User => {
                let user_id = owner_user_id.unwrap_or(0);
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
        kind: CoreSyncResourceKind::LegacyUser,
        table_name: "users",
        resource_type: "core.legacy_user",
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
        kind: CoreSyncResourceKind::FlowVersion,
        table_name: "flow_versions",
        resource_type: "core.flow_version",
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
        primary_key_column: "org_id,addon_id,resource_type,resource_id,subject_type,subject_id,action",
        scope: CoreSyncScope::Organization,
        retention: CoreSyncRetention::Durable,
        partition_suffix: "sync-control",
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
            descriptor_for_table("flow_versions").map(|descriptor| descriptor.resource_type),
            Some("core.flow_version")
        );
        assert_eq!(
            descriptor_for_table("flow_model_bindings").map(|descriptor| descriptor.resource_type),
            Some("core.flow_model_binding")
        );
    }

    #[test]
    fn registry_contains_identity_and_rbac_tables() {
        for table in [
            "organizations",
            "user_accounts",
            "users",
            "user_groups",
            "group_members",
            "roles",
            "org_memberships",
        ] {
            assert!(is_core_sync_table(table), "missing descriptor for {table}");
        }
    }

    #[test]
    fn runtime_tables_are_not_core_synced() {
        for table in ["flow_executions", "flow_invocations", "audit_log"] {
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
