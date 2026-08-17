// =============================================================================
// File: code_studio.rs
// Purpose: Binary CBOR protocol for Code Studio — the workspace registry
//          (create, list, get, members, creator grants) and work sessions
//          (open, list, close). Sessions are private per user: the server
//          filters every session query by the authenticated caller, so the
//          wire never carries another person's unfinished work.
// Example: MessageBody::CodeStudioBody(CodeStudioPayload::WorkspacesListRequest {})
// =============================================================================

use serde::{Deserialize, Serialize};

/// Workspace row for the list and detail views.
///
/// `secret_ref` is deliberately ABSENT from the wire: it is a handle into the
/// node-local vault and means nothing on another node. `has_secret` is what the
/// UI actually needs — whether credentials are stored at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    pub name: String,
    pub slug: String,
    /// Node that OWNS the workspace. Only that node can run it; the others
    /// show it and say so.
    pub node_id: String,
    pub node_name: String,
    /// True when this Core is the owner node.
    pub is_local: bool,
    /// 'container' | 'trusted_native'. The native mode promises NO isolation
    /// from the host, so the UI marks it permanently.
    pub exec_mode: String,
    /// 'namespace' | 'firewall' | 'unrestricted' — how network policy is
    /// REALLY enforced, not what was requested.
    pub egress_enforcement: String,
    /// 'empty' | 'git'.
    pub repo_kind: String,
    pub repo_url: Option<String>,
    /// 'none' | 'token' | 'ssh_key'.
    pub repo_auth_kind: Option<String>,
    pub has_secret: bool,
    pub default_branch: Option<String>,
    pub target_branch: Option<String>,
    pub autonomy_ceiling: String,
    pub egress_policy: String,
    pub index_enabled: bool,
    /// 'provisioning' | 'active' | 'error' | 'archived'.
    pub status: String,
    /// Reason, present only for `error`.
    pub status_detail: Option<String>,
    /// Role of the calling user in this workspace.
    pub my_role: String,
    pub member_count: u32,
    pub open_sessions: u32,
    pub disk_used_bytes: u64,
    pub quota_disk_bytes: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

/// Workspace member with display data resolved server-side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceMemberInfo {
    pub user_id: String,
    pub display_name: String,
    /// 'owner' | 'editor' | 'viewer'.
    pub role: String,
    pub added_by: String,
    pub added_at: String,
}

/// Member entry as sent by the creation wizard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceMemberInput {
    pub user_id: String,
    pub role: String,
}

/// One provisioning step, so a failed workspace can show WHERE it stopped
/// instead of a bare error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvisionStepInfo {
    pub step: String,
    /// 'pending' | 'done' | 'failed' | 'compensated'.
    pub status: String,
    pub detail: Option<String>,
    pub updated_at: String,
}

/// A work session on a branch of a workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub workspace_id: String,
    pub title: String,
    pub branch: String,
    pub autonomy_mode: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

/// Node that can host a workspace, for the wizard's node picker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceNodeInfo {
    pub node_id: String,
    pub name: String,
    pub is_local: bool,
    /// Whether this node can run a container-isolated workspace at all.
    pub supports_container: bool,
    /// How this node would enforce egress policy.
    pub egress_enforcement: String,
}

/// Code Studio message family (request + response). ciborium encodes variants
/// external-tagged by variant NAME, so never rename variants or fields without
/// updating the frontend and the golden test (`code_studio_wire_golden`).
/// Variant order is the wire contract: append-only, never insert or reorder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CodeStudioPayload {
    // ---- Registry ----
    WorkspacesListRequest {
        #[serde(default)]
        include_archived: bool,
    },
    WorkspacesListResponse {
        workspaces: Vec<WorkspaceInfo>,
        /// Whether the caller holds the per-user grant needed to create one.
        can_create: bool,
        nodes: Vec<WorkspaceNodeInfo>,
    },
    WorkspaceCreateRequest {
        name: String,
        node_id: String,
        /// 'container' | 'trusted_native'.
        exec_mode: String,
        container_image: Option<String>,
        /// 'empty' | 'git'.
        repo_kind: String,
        repo_url: Option<String>,
        /// 'none' | 'token' | 'ssh_key'.
        repo_auth_kind: Option<String>,
        /// Credential material. Travels once, is stored encrypted in the
        /// node-local vault and is never sent back.
        secret_material: Option<String>,
        /// Pinned SSH host key line, shown to the user on first contact.
        ssh_host_fingerprint: Option<String>,
        default_branch: Option<String>,
        autonomy_ceiling: String,
        egress_policy: String,
        index_enabled: bool,
        members: Vec<WorkspaceMemberInput>,
    },
    WorkspaceCreateResponse {
        workspace_id: String,
        /// The workspace starts `provisioning`; the UI follows it with
        /// `WorkspaceGetRequest` rather than assuming success.
        status: String,
    },
    WorkspaceGetRequest {
        workspace_id: String,
    },
    WorkspaceGetResponse {
        workspace: WorkspaceInfo,
        members: Vec<WorkspaceMemberInfo>,
        provisioning: Vec<ProvisionStepInfo>,
    },
    /// Re-runs provisioning of a workspace left in `error`. Completed steps are
    /// skipped, so this is a resume rather than a rebuild.
    WorkspaceRetryRequest {
        workspace_id: String,
    },
    WorkspaceRetryResponse {
        workspace_id: String,
        status: String,
        status_detail: Option<String>,
    },
    WorkspaceArchiveRequest {
        workspace_id: String,
        archived: bool,
    },
    WorkspaceArchiveResponse {
        workspace_id: String,
        status: String,
    },

    // ---- Members and the create grant ----
    WorkspaceMemberSetRequest {
        workspace_id: String,
        user_id: String,
        role: String,
    },
    WorkspaceMemberRemoveRequest {
        workspace_id: String,
        user_id: String,
    },
    WorkspaceMembersResponse {
        workspace_id: String,
        members: Vec<WorkspaceMemberInfo>,
    },
    WorkspaceCreatorGrantSetRequest {
        user_id: String,
        granted: bool,
    },
    WorkspaceCreatorGrantResponse {
        user_id: String,
        granted: bool,
    },

    // ---- Sessions ----
    SessionsListRequest {
        workspace_id: String,
    },
    SessionsListResponse {
        workspace_id: String,
        sessions: Vec<SessionInfo>,
    },
    SessionOpenRequest {
        workspace_id: String,
        title: String,
        autonomy_mode: String,
    },
    SessionOpenResponse {
        session: SessionInfo,
    },
    SessionCloseRequest {
        workspace_id: String,
        session_id: String,
    },
    SessionCloseResponse {
        session_id: String,
        status: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;

    fn hex_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
            .collect()
    }

    /// Golden wire snapshot: ciborium encodes enum variants as a 1-element map
    /// keyed by the variant NAME (external tagging). Pinning exact bytes turns
    /// any accidental rename of a variant, field or the
    /// `MessageBody::CodeStudioBody` tag into a test failure.
    #[test]
    fn code_studio_wire_golden() {
        let list = CodeStudioPayload::WorkspacesListRequest {
            include_archived: false,
        };
        let bytes = crate::cbor::encode(&list).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a175576f726b7370616365734c69737452657175657374a170696e636c7564655f6172636869766564f4"
            ),
            "WorkspacesListRequest wire drift"
        );

        let body = MessageBody::CodeStudioBody(list);
        let bytes = crate::cbor::encode(&body).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a16e436f646553747564696f426f6479a175576f726b7370616365734c69737452657175657374a170696e636c7564655f6172636869766564f4"
            ),
            "MessageBody::CodeStudioBody wire drift"
        );

        let close = CodeStudioPayload::SessionCloseRequest {
            workspace_id: "w1".to_string(),
            session_id: "s1".to_string(),
        };
        let bytes = crate::cbor::encode(&close).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a17353657373696f6e436c6f736552657175657374a26c776f726b73706163655f69646277316a73657373696f6e5f6964627331"
            ),
            "SessionCloseRequest wire drift"
        );
    }

    #[test]
    fn a_workspace_round_trips_without_losing_a_field() {
        let info = WorkspaceInfo {
            workspace_id: "w1".into(),
            name: "Core".into(),
            slug: "core".into(),
            node_id: "n1".into(),
            node_name: "dev-ryzen".into(),
            is_local: true,
            exec_mode: "trusted_native".into(),
            egress_enforcement: "unrestricted".into(),
            repo_kind: "git".into(),
            repo_url: Some("https://example.invalid/r.git".into()),
            repo_auth_kind: Some("token".into()),
            has_secret: true,
            default_branch: Some("main".into()),
            target_branch: Some("main".into()),
            autonomy_ceiling: "normal".into(),
            egress_policy: "org_approved".into(),
            index_enabled: false,
            status: "active".into(),
            status_detail: None,
            my_role: "owner".into(),
            member_count: 1,
            open_sessions: 0,
            disk_used_bytes: 0,
            quota_disk_bytes: None,
            created_at: "2026-08-14T10:00:00Z".into(),
            updated_at: "2026-08-14T10:00:00Z".into(),
        };
        let payload = CodeStudioPayload::WorkspaceGetResponse {
            workspace: info.clone(),
            members: vec![WorkspaceMemberInfo {
                user_id: "u1".into(),
                display_name: "Piotr".into(),
                role: "owner".into(),
                added_by: "u1".into(),
                added_at: "2026-08-14T10:00:00Z".into(),
            }],
            provisioning: vec![ProvisionStepInfo {
                step: "repository".into(),
                status: "done".into(),
                detail: None,
                updated_at: "2026-08-14T10:00:00Z".into(),
            }],
        };
        let bytes = crate::cbor::encode(&payload).expect("encode");
        let decoded: CodeStudioPayload = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn the_wire_has_no_place_to_put_secret_material_in_a_response() {
        // The create request carries material once; nothing sent BACK may.
        let json = serde_json::to_string(&CodeStudioPayload::WorkspaceGetResponse {
            workspace: WorkspaceInfo {
                workspace_id: "w1".into(),
                name: "Core".into(),
                slug: "core".into(),
                node_id: "n1".into(),
                node_name: "dev".into(),
                is_local: true,
                exec_mode: "trusted_native".into(),
                egress_enforcement: "unrestricted".into(),
                repo_kind: "git".into(),
                repo_url: None,
                repo_auth_kind: Some("token".into()),
                has_secret: true,
                default_branch: None,
                target_branch: None,
                autonomy_ceiling: "normal".into(),
                egress_policy: "org_approved".into(),
                index_enabled: false,
                status: "active".into(),
                status_detail: None,
                my_role: "owner".into(),
                member_count: 1,
                open_sessions: 0,
                disk_used_bytes: 0,
                quota_disk_bytes: None,
                created_at: String::new(),
                updated_at: String::new(),
            },
            members: Vec::new(),
            provisioning: Vec::new(),
        })
        .expect("json");
        assert!(!json.contains("secret_material"));
        assert!(!json.contains("secret_ref"));
    }
}
