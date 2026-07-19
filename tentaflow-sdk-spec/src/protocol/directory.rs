// =============================================================================
// File: protocol/directory.rs — directory host-function ABI payloads
// Purpose: single source of truth for the CBOR response structs of the
// read-only directory host functions (`directory_users_v1`,
// `directory_groups_v1`, `directory_roles_v1`, `directory_org_v1`). Shared
// verbatim by the core host (encode output) and the addon SDK (decode output)
// so the wire format cannot drift. All four functions are output-only (no
// input payload) and are scoped by the host to the calling instance's org.
// =============================================================================

use minicbor::{Decode, Encode};

// -----------------------------------------------------------------------------
// directory_users_v1
// -----------------------------------------------------------------------------

/// One user row returned by `directory_users_v1`. Only active members of the
/// caller's organization are returned; credential / SSO fields never cross
/// the ABI. `groups` carries group IDs (`user_groups.id`), not names.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DirectoryUserOut {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub username: String,
    #[n(2)]
    pub display_name: String,
    #[n(3)]
    pub email: Option<String>,
    #[n(4)]
    pub groups: Vec<String>,
    #[n(5)]
    pub is_active: bool,
    /// Organization RBAC role (`user_accounts.role`): `user` | `power_user` |
    /// `admin`. Backs the sharing UI role chip; permission lists stay host-side.
    #[n(6)]
    pub role: String,
}

/// Output of `directory_users_v1`.
#[derive(Debug, Clone, PartialEq, Default, Encode, Decode)]
#[cbor(map)]
pub struct DirectoryUsersOutput {
    #[n(0)]
    pub users: Vec<DirectoryUserOut>,
}

// -----------------------------------------------------------------------------
// directory_groups_v1
// -----------------------------------------------------------------------------

/// One group row returned by `directory_groups_v1`. `member_count` counts
/// only active users that are members of the caller's organization.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DirectoryGroupOut {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub name: String,
    #[n(2)]
    pub description: String,
    #[n(3)]
    pub member_count: u64,
}

/// Output of `directory_groups_v1`.
#[derive(Debug, Clone, PartialEq, Default, Encode, Decode)]
#[cbor(map)]
pub struct DirectoryGroupsOutput {
    #[n(0)]
    pub groups: Vec<DirectoryGroupOut>,
}

// -----------------------------------------------------------------------------
// directory_roles_v1
// -----------------------------------------------------------------------------

/// One RBAC role returned by `directory_roles_v1` (preseed + custom roles).
/// The role's permission list is deliberately NOT exposed to addons.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DirectoryRoleOut {
    #[n(0)]
    pub role_id: String,
    #[n(1)]
    pub name: String,
}

/// Output of `directory_roles_v1`.
#[derive(Debug, Clone, PartialEq, Default, Encode, Decode)]
#[cbor(map)]
pub struct DirectoryRolesOutput {
    #[n(0)]
    pub roles: Vec<DirectoryRoleOut>,
}

// -----------------------------------------------------------------------------
// directory_org_v1
// -----------------------------------------------------------------------------

/// Output of `directory_org_v1` — the calling instance's organization.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DirectoryOrgOutput {
    #[n(0)]
    pub org_id: String,
    #[n(1)]
    pub name: String,
    #[n(2)]
    pub slug: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: &T)
    where
        T: Encode<()> + for<'b> Decode<'b, ()> + PartialEq + std::fmt::Debug,
    {
        let bytes = minicbor::to_vec(value).expect("encode");
        let decoded: T = minicbor::decode(&bytes).expect("decode");
        assert_eq!(&decoded, value);
    }

    #[test]
    fn directory_users_roundtrip() {
        roundtrip(&DirectoryUsersOutput {
            users: vec![DirectoryUserOut {
                id: "u-1".into(),
                username: "jan".into(),
                display_name: "Jan Kowalski".into(),
                email: Some("jan@example.com".into()),
                groups: vec!["g-1".into(), "g-2".into()],
                is_active: true,
                role: "admin".into(),
            }],
        });
        roundtrip(&DirectoryUsersOutput::default());
    }

    #[test]
    fn directory_groups_roundtrip() {
        roundtrip(&DirectoryGroupsOutput {
            groups: vec![DirectoryGroupOut {
                id: "g-1".into(),
                name: "developers".into(),
                description: "Dev team".into(),
                member_count: 7,
            }],
        });
    }

    #[test]
    fn directory_roles_roundtrip() {
        roundtrip(&DirectoryRolesOutput {
            roles: vec![DirectoryRoleOut {
                role_id: "role-org-admin".into(),
                name: "org_admin".into(),
            }],
        });
    }

    #[test]
    fn directory_org_roundtrip() {
        roundtrip(&DirectoryOrgOutput {
            org_id: "org-default".into(),
            name: "Default Organization".into(),
            slug: "default".into(),
        });
    }
}
