// ===== File: tests/critic_code_studio.rs — adversarial tests for Code Studio =====
//
// Written by a reviewer, not by the author of the feature. Each test pins ONE
// invariant the plan states in words and the suite does not check in code.
// A test in here that fails is a defect in the production code, not in the test.

use std::collections::BTreeSet;

use tentaflow_core::agents::{AgentPrincipal, ToolCatalog};
use tentaflow_core::code_studio::models::{
    AutonomyMode, EgressEnforcement, ExecMode, NewWorkspace,
};
use tentaflow_core::code_studio::repository;
use tentaflow_core::dispatch::code_studio::code_studio_dispatch;
use tentaflow_core::dispatch::{AppState, HandlerContext};
use tentaflow_core::services::rbac::middleware::OrgContext;
use tentaflow_protocol::code_studio::CodeStudioPayload;
use tentaflow_protocol::{MessageBody, ProtocolErrorCode, SessionAuth};

const PERM_READ: &str = "code_studio.read";
const PERM_ADMIN: &str = "code_studio.admin";

// =============================================================================
// 1. Registration — derived from the protocol, not from a second hand list
// =============================================================================

/// `dispatch/code_studio.rs` keeps THREE hand-maintained lists of the same
/// thing: the match arms, the `register_code_studio_variant!` calls and the
/// `REGISTERED_VARIANTS` slice its own registration test iterates. Only the
/// first is compiler-checked. The in-crate test walks the third list, so it can
/// never notice a variant missing from all three — it is self-referential.
///
/// This one derives the truth from the protocol source itself, so forgetting an
/// `inventory::submit!` fails here instead of surfacing as NotImplemented in a
/// browser.
#[test]
fn critic_every_protocol_request_variant_resolves_to_a_handler() {
    const PROTOCOL_SRC: &str = include_str!("../../tentaflow-protocol/src/code_studio.rs");

    // Stream requests are answered by a subscription, not by the
    // request/response dispatcher (stream_handlers.rs).
    let stream_only: BTreeSet<&str> = [
        "SessionStreamRequest",
        "TerminalStreamRequest",
        "IndexStreamRequest",
    ]
    .into_iter()
    .collect();

    let body = PROTOCOL_SRC
        .split_once("pub enum CodeStudioPayload {")
        .expect("CodeStudioPayload enum")
        .1;
    let mut variants: Vec<&str> = Vec::new();
    for line in body.lines() {
        if line == "}" {
            break;
        }
        // A variant head is indented exactly four spaces and starts uppercase.
        let Some(rest) = line.strip_prefix("    ") else {
            continue;
        };
        if rest.starts_with(' ') || !rest.starts_with(char::is_uppercase) {
            continue;
        }
        let name: &str = rest
            .split(|c: char| !c.is_ascii_alphanumeric())
            .next()
            .unwrap_or_default();
        if name.ends_with("Request") {
            variants.push(name);
        }
    }
    assert!(
        variants.len() > 60,
        "the enum scan found only {} request variants — the parser drifted",
        variants.len()
    );

    let mut missing: Vec<String> = Vec::new();
    for variant in variants {
        if stream_only.contains(variant) {
            continue;
        }
        let registered = format!("CodeStudio{variant}");
        if tentaflow_core::dispatch::find(&registered).is_none() {
            missing.push(registered);
        }
    }
    assert!(
        missing.is_empty(),
        "request variants with no registered handler — the client sees NotImplemented: {missing:?}"
    );
}

// =============================================================================
// 2. Routing happens before authorization
// =============================================================================

fn context(org: OrgContext) -> HandlerContext {
    let state = AppState::for_test();
    // The gates read the app-permission matrix now (P2.3): an enabled
    // code-studio instance is registered and every permission the fixture org
    // lists becomes a per-user MATRIX grant — a fixture with no permissions
    // is refused by the matrix exactly as org-RBAC used to refuse it. The row
    // carries the real manifest so it is an instance the handlers could open
    // a content database for, not a bare registry entry.
    {
        let conn = state.db.write().expect("test db");
        conn.execute(
            "INSERT OR IGNORE INTO addons \
               (addon_id, name, version, package_id, package_version, runtime, is_enabled, \
                manifest_json) \
             VALUES ('code-studio-testinst', 'code-studio', '1.0.0', 'code-studio', '1.0.0', \
                     'native', 1, ?1)",
            [include_str!("../src/code_studio/app-manifest.toml")],
        )
        .expect("test instance row");
        for perm in &org.permissions {
            conn.execute(
                "INSERT OR IGNORE INTO addon_permissions \
                   (addon_id, subject_type, subject_id, permission_id, granted, grant_mode) \
                 VALUES ('code-studio-testinst', 'user', ?1, ?2, 1, 'allow')",
                rusqlite::params![org.user_id, perm],
            )
            .expect("test grant row");
        }
    }
    state
        .permission_checker
        .as_ref()
        .expect("test state has a checker")
        .refresh_addon("code-studio-testinst");
    HandlerContext {
        session: SessionAuth::UserSession {
            user_id: [7u8; 16],
            role: None,
        },
        correlation_id: 1,
        connection_id: 0,
        resume_secret: None,
        state,
        org_context: Some(org),
    }
}

fn org(user_id: &str, permissions: &[&str]) -> OrgContext {
    OrgContext {
        user_id: user_id.to_string(),
        org_id: "org-1".to_string(),
        role_id: "role-1".to_string(),
        permissions: permissions.iter().map(|p| p.to_string()).collect(),
    }
}

/// §9.2 — creating a workspace needs `code_studio.read` plus a creator grant.
/// `code_studio_dispatch` runs `route_to_owner` FIRST, and that function reads
/// `node_id` straight off the wire and forwards to it. A caller who holds no
/// Code Studio permission at all therefore reaches the mesh layer, and the
/// error it gets back describes the mesh ("not running", "not a trusted peer")
/// instead of the refusal it should have received.
///
/// That is both an authorization-ordering defect and a probe: the answer for a
/// node id differs by whether that node is a trusted peer.
#[tokio::test]
async fn critic_a_caller_without_permission_is_refused_before_the_mesh_is_touched() {
    let ctx = context(org("u-nobody", &[]));
    let req = MessageBody::CodeStudioBody(CodeStudioPayload::WorkspaceCreateRequest {
        name: "probe".into(),
        node_id: "some-other-node".into(),
        exec_mode: "trusted_native".into(),
        container_image: None,
        repo_kind: "empty".into(),
        repo_url: None,
        repo_auth_kind: None,
        secret_material: None,
        ssh_host_fingerprint: None,
        default_branch: None,
        autonomy_ceiling: "normal".into(),
        egress_policy: "org_approved".into(),
        index_enabled: false,
        members: Vec::new(),
    });

    let err = code_studio_dispatch(&req, &ctx)
        .await
        .expect_err("a caller with no permission must be refused");
    assert_eq!(
        err.code,
        ProtocolErrorCode::PolicyDenied,
        "the permission gate must run before routing, but the caller got: {:?} / {}",
        err.code,
        err.message
    );
}

// =============================================================================
// 3. The administrator overlay is metadata and lifecycle, never policy
// =============================================================================

fn seed_workspace(ctx: &HandlerContext, id: &str, owner: &str) {
    repository::create_workspace(
        &ctx.state.db,
        &NewWorkspace {
            id: id.to_string(),
            org_id: "org-1".to_string(),
            owner_user_id: owner.to_string(),
            name: id.to_string(),
            slug: id.to_string(),
            node_id: ctx.state.local_node_id.to_string(),
            exec_mode: ExecMode::TrustedNative,
            container_image: None,
            egress_enforcement: EgressEnforcement::Unrestricted,
            repo_kind: "empty".to_string(),
            repo_url: None,
            repo_auth_kind: None,
            secret_ref: None,
            ssh_host_fingerprint: None,
            default_branch: Some("main".to_string()),
            target_branch: None,
            autonomy_ceiling: AutonomyMode::Normal,
            egress_policy: "org_approved".to_string(),
            index_enabled: false,
            quota_disk_bytes: None,
            quota_sessions: None,
        },
    )
    .expect("seed workspace");
}

/// §9.2 puts `workspace_settings` in the OWNER column only; §25.4 limits the
/// administrator overlay to metadata, archive, delete, grants and quotas.
///
/// `workspace_settings_update_v1` gates all of it behind one `Access::Lifecycle`
/// check, so a non-member org administrator can rewrite `autonomy_ceiling`,
/// `egress_policy`, `target_branch` and `index_enabled` on somebody else's
/// workspace — the two knobs that decide how much an agent may do unattended
/// and where it may reach.
#[tokio::test]
async fn critic_an_org_admin_who_is_not_a_member_cannot_raise_the_autonomy_ceiling() {
    let ctx = context(org("u-admin", &[PERM_READ, PERM_ADMIN]));
    seed_workspace(&ctx, "ws-policy", "u-owner");

    let req = MessageBody::CodeStudioBody(CodeStudioPayload::WorkspaceSettingsUpdateRequest {
        workspace_id: "ws-policy".into(),
        name: "ws-policy".into(),
        autonomy_ceiling: "auto_edit".into(),
        egress_policy: "any".into(),
        target_branch: None,
        index_enabled: false,
        quota_disk_bytes: None,
        quota_sessions: None,
    });

    let outcome = code_studio_dispatch(&req, &ctx).await;
    let err = outcome.err().unwrap_or_else(|| {
        let record = repository::get_workspace(&ctx.state.db, "ws-policy")
            .expect("read back")
            .expect("row");
        panic!(
            "a non-member administrator rewrote the workspace policy: ceiling={} egress={}",
            record.autonomy_ceiling, record.egress_policy
        )
    });
    assert!(
        matches!(
            err.code,
            ProtocolErrorCode::PolicyDenied | ProtocolErrorCode::NotFound
        ),
        "unexpected refusal: {:?} / {}",
        err.code,
        err.message
    );
}

// =============================================================================
// 4. What `no_permission_grant_can_add_a_tool_the_allowlist_omits` really tests
// =============================================================================

/// `tests/code_harness_flow_e2e.rs::no_permission_grant_can_add_a_tool_the_allowlist_omits`
/// passes a maximally permissive closure (`|_| true`) to `ToolCatalog::resolve`
/// and concludes that "no permission grant can add a tool".
///
/// The closure is the ADDON permission checker; `resolve` never consults it for
/// `core.*` names (agents/catalog.rs, the `for core in CoreToolName::all()`
/// loop). The demonstration below flips the closure to `|_| false` and gets the
/// identical answer, so the original test defeats nothing: it would still pass
/// if `code_workspace_allowlist`, `session_grants` or an `approvals` decision
/// could widen the agent's tool surface, because it touches none of them.
#[test]
fn critic_the_permissive_grant_closure_never_reaches_a_core_tool() {
    let committer = r#"["core.fs_read","core.git_commit"]"#;
    let principal = AgentPrincipal::user("u1");

    let permissive: Vec<String> = ToolCatalog::resolve(committer, &principal, &[], true, |_| true)
        .into_iter()
        .map(|s| s.name)
        .collect();
    let denying: Vec<String> = ToolCatalog::resolve(committer, &principal, &[], true, |_| false)
        .into_iter()
        .map(|s| s.name)
        .collect();

    assert_eq!(
        permissive, denying,
        "if the grant closure mattered for core tools these would differ"
    );
    assert!(permissive.iter().any(|n| n == "core.git_commit"));
}
