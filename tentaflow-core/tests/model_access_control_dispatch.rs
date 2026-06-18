// =============================================================================
// File: tests/model_access_control_dispatch.rs
// Purpose: Handler-level coverage for the F1a §6.6 model/alias access control
//          RPCs (visibility set, consumer grant/revoke, addon access view +
//          decision). Drives the generic binary dispatch with an Admin session
//          and asserts the wire responses plus the reconciled DB state.
// =============================================================================

use std::collections::HashSet;
use std::sync::Arc;

use tentaflow_core::dispatch::state::AppState;
use tentaflow_core::dispatch::{dispatch, HandlerContext};
use tentaflow_core::services::rbac::OrgContext;
use tentaflow_protocol::{
    AddonAccessDecisionRequest, AddonAccessListRequest, MessageBody, ModelConsumerGrantRequest,
    ModelConsumerListRequest, ModelVisibilitySetRequest, SessionAuth,
};

fn admin_ctx(state: Arc<AppState>) -> HandlerContext {
    let mut user_id_bytes = [0u8; 16];
    user_id_bytes[0] = 0xAB;
    HandlerContext {
        session: SessionAuth::UserSession {
            user_id: user_id_bytes,
            role: Some("admin".to_string()),
        },
        correlation_id: 1,
        connection_id: 0,
        resume_secret: None,
        state,
        org_context: Some(OrgContext {
            user_id: "user-admin".to_string(),
            org_id: "org-test".to_string(),
            role_id: "role-admin".to_string(),
            permissions: HashSet::new(),
        }),
    }
}

fn non_admin_ctx(state: Arc<AppState>) -> HandlerContext {
    HandlerContext {
        session: SessionAuth::UserSession {
            user_id: [0u8; 16],
            role: Some("user".to_string()),
        },
        correlation_id: 2,
        connection_id: 0,
        resume_secret: None,
        state,
        org_context: None,
    }
}

#[tokio::test]
async fn non_admin_is_policy_denied() {
    let state = AppState::for_test();
    let ctx = non_admin_ctx(state.clone());
    let req = MessageBody::ModelVisibilitySetRequestBody(ModelVisibilitySetRequest {
        model_id: "m1".into(),
        visibility: "public".into(),
    });
    let (resp, is_err) = dispatch(&req, &ctx).await;
    assert!(is_err);
    assert!(matches!(resp, MessageBody::Error(_)));
}

#[tokio::test]
async fn visibility_set_and_grant_reconcile_round_trip() {
    let state = AppState::for_test();
    let ctx = admin_ctx(state.clone());

    // Seed a consumer-side declaration so reconciliation has something to flip.
    {
        let conn = state.db.lock().unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        tentaflow_core::db::repository::upsert_uses_model_within_tx(
            &tx, "addon-a", "m1", true, "needs it",
        )
        .unwrap();
        tx.commit().unwrap();
    }

    // Admin grants addon-a as a consumer of restricted model m1 → pending→granted.
    let grant = MessageBody::ModelConsumerGrantRequestBody(ModelConsumerGrantRequest {
        model_id: "m1".into(),
        addon_id: "addon-a".into(),
    });
    let (resp, is_err) = dispatch(&grant, &ctx).await;
    assert!(!is_err, "grant failed: {:?}", resp);
    match resp {
        MessageBody::AccessMutationResponseBody(r) => {
            assert!(r.ok);
            assert_eq!(r.transitions.len(), 1);
            assert_eq!(r.transitions[0].addon_id, "addon-a");
            assert_eq!(r.transitions[0].before, "pending");
            assert_eq!(r.transitions[0].after, "granted");
        }
        other => panic!("unexpected response: {:?}", other),
    }

    // Consumer list reflects the active grant.
    let list = MessageBody::ModelConsumerListRequestBody(ModelConsumerListRequest {
        model_id: "m1".into(),
    });
    let (resp, _) = dispatch(&list, &ctx).await;
    match resp {
        MessageBody::ModelConsumerListResponseBody(r) => {
            assert_eq!(r.consumers.len(), 1);
            assert_eq!(r.consumers[0].addon_id, "addon-a");
            assert!(r.consumers[0].revoked_at.is_none());
        }
        other => panic!("unexpected response: {:?}", other),
    }

    // Flipping m1 public auto-grants without consumer rows.
    let vis = MessageBody::ModelVisibilitySetRequestBody(ModelVisibilitySetRequest {
        model_id: "m1".into(),
        visibility: "public".into(),
    });
    let (resp, is_err) = dispatch(&vis, &ctx).await;
    assert!(!is_err);
    match resp {
        MessageBody::AccessMutationResponseBody(r) => {
            assert_eq!(r.transitions.len(), 1);
            assert_eq!(r.transitions[0].after, "auto_granted");
        }
        other => panic!("unexpected response: {:?}", other),
    }
}

#[tokio::test]
async fn addon_access_view_and_decision() {
    let state = AppState::for_test();
    let ctx = admin_ctx(state.clone());

    {
        let conn = state.db.lock().unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        tentaflow_core::db::repository::upsert_uses_model_within_tx(
            &tx, "addon-b", "m2", true, "needs it",
        )
        .unwrap();
        tx.commit().unwrap();
    }

    // Access view shows the pending model use with restricted owner visibility.
    let view = MessageBody::AddonAccessListRequestBody(AddonAccessListRequest {
        addon_id: "addon-b".into(),
    });
    let (resp, is_err) = dispatch(&view, &ctx).await;
    assert!(!is_err, "view failed: {:?}", resp);
    match resp {
        MessageBody::AddonAccessListResponseBody(r) => {
            assert_eq!(r.uses_model.len(), 1);
            assert_eq!(r.uses_model[0].target, "m2");
            assert_eq!(r.uses_model[0].grant_status, "pending");
            assert_eq!(r.uses_model[0].owner_visibility, "restricted");
            assert!(r.uses_alias.is_empty());
        }
        other => panic!("unexpected response: {:?}", other),
    }

    // Approve via the unified decision endpoint → pending→granted.
    let approve = MessageBody::AddonAccessDecisionRequestBody(AddonAccessDecisionRequest {
        addon_id: "addon-b".into(),
        kind: "model".into(),
        target: "m2".into(),
        decision: "approve".into(),
    });
    let (resp, is_err) = dispatch(&approve, &ctx).await;
    assert!(!is_err);
    match resp {
        MessageBody::AccessMutationResponseBody(r) => {
            assert_eq!(r.transitions.len(), 1);
            assert_eq!(r.transitions[0].after, "granted");
        }
        other => panic!("unexpected response: {:?}", other),
    }

    // Deny revokes it back to pending.
    let deny = MessageBody::AddonAccessDecisionRequestBody(AddonAccessDecisionRequest {
        addon_id: "addon-b".into(),
        kind: "model".into(),
        target: "m2".into(),
        decision: "deny".into(),
    });
    let (resp, is_err) = dispatch(&deny, &ctx).await;
    assert!(!is_err);
    match resp {
        MessageBody::AccessMutationResponseBody(r) => {
            assert_eq!(r.transitions[0].after, "pending");
        }
        other => panic!("unexpected response: {:?}", other),
    }
}
