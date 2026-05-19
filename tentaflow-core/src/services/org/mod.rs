// ============ File: services/org/mod.rs — F2 P1.a multi-tenant org module ============
//
// Foundation for the F2 RBAC + org-isolation work. This chunk (P1.a) covers
// schema, public types and the CRUD repository only. Middleware that scopes
// incoming requests by org and the CLI surface land in P1.b / P1.c.
//
// The `org-default` row is seeded by migration v32 so every pre-existing
// row (cameras, addons, audit log, ...) backfills cleanly to a single
// tenant. New nodes start with the same single-tenant row; multi-tenant
// deployments add more via the CLI in P1.c.

pub mod error;
pub mod repo;

pub use error::{OrgError, Result};
pub use repo::{
    add_membership, create_organization, delete_organization, get_organization,
    get_user_role_in_org, list_memberships_for_org, list_memberships_for_user, list_organizations,
    list_roles, remove_membership, update_organization, OrgPatch,
};

/// Slug of the default organization created by migration v32. All historical
/// rows are backfilled to this org so an upgrade-in-place node keeps working
/// without operator action. The slug is also the bootstrap target for the
/// admin CLI in P1.c — `tentaflow-cli org create` requires a different slug.
pub const DEFAULT_ORG_ID: &str = "org-default";
pub const DEFAULT_ORG_SLUG: &str = "default";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Organization {
    pub org_id: String,
    pub name: String,
    pub slug: String,
    pub contact_email: Option<String>,
    pub dpo_contact: Option<String>,
    pub retention_policy_json: Option<String>,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Role {
    pub role_id: String,
    pub name: String,
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct OrgMembership {
    pub org_id: String,
    pub user_id: String,
    pub role_id: String,
    pub granted_at: String,
    pub granted_by: String,
}
