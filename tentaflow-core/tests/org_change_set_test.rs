// ============ File: tests/org_change_set_test.rs ============
//
// History and planned reorganizations over the binary RPC, against a real
// database: a draft is validated by a dry run through the batch machinery, the
// author cannot approve it, a second administrator's approval applies it as
// dated writes in ONE transaction with the state change, a withdrawal leaves
// the live structure alone, a structure that moved on since the draft makes the
// approval fail as a whole, and the history shows every member the structure's
// changes but only an administrator (and the person) the people's.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{Days, NaiveDate};
use tentaflow_core::dispatch::org_structure::org_structure_dispatch;
use tentaflow_core::dispatch::state::AppState;
use tentaflow_core::dispatch::HandlerContext;
use tentaflow_core::services::org::{self, DEFAULT_ORG_ID};
use tentaflow_core::services::rbac::OrgContext;
use tentaflow_protocol::org_history::{OrgChangeSet, OrgDiffItem};
use tentaflow_protocol::org_structure::{
    OrgAssignmentType, OrgBatchOpResult, OrgFileFormat, OrgStructurePayload as P, OrgStructureView,
    OrgSubject, OrgWriteOp, OrgWriteResult,
};
use tentaflow_protocol::{MessageBody, ProtocolErrorCode, SessionAuth};

struct World {
    state: Arc<AppState>,
    admin: String,
    second: String,
    member: String,
    outsider: String,
}

fn add_user(state: &AppState, org_id: Option<&str>, name: &str) -> String {
    let id = tentaflow_core::db::repository::create_user_account(
        &state.db,
        name,
        "hash",
        name,
        &format!("{name}@example.test"),
    )
    .unwrap();
    if let Some(org_id) = org_id {
        let role_id: String = state
            .db
            .read()
            .unwrap()
            .query_row("SELECT role_id FROM roles LIMIT 1", [], |r| r.get(0))
            .unwrap();
        org::add_membership(&state.db, org_id, &id, &role_id, "test").unwrap();
    }
    id
}

fn world() -> World {
    let state = AppState::for_test();
    let w = World {
        admin: add_user(&state, Some(DEFAULT_ORG_ID), "first-admin"),
        second: add_user(&state, Some(DEFAULT_ORG_ID), "second-admin"),
        member: add_user(&state, Some(DEFAULT_ORG_ID), "plain-member"),
        outsider: add_user(&state, None, "outsider"),
        state,
    };
    // Two administrators: the author of a plan may not approve it.
    set_admins(&w, &[w.admin.as_str(), w.second.as_str()]);
    w
}

fn ctx_of(w: &World, user: &str, permissions: &[&str]) -> HandlerContext {
    HandlerContext {
        session: SessionAuth::UserSession {
            user_id: [7u8; 16],
            role: Some("user".to_string()),
        },
        correlation_id: 1,
        connection_id: 0,
        resume_secret: None,
        state: w.state.clone(),
        origin: tentaflow_core::dispatch::RequestOrigin::Local,
        org_context: Some(OrgContext {
            user_id: user.to_string(),
            org_id: DEFAULT_ORG_ID.to_string(),
            role_id: "role-test".to_string(),
            permissions: permissions
                .iter()
                .map(|p| p.to_string())
                .collect::<HashSet<_>>(),
        }),
    }
}

fn admin(w: &World) -> HandlerContext {
    ctx_of(w, &w.admin, &["org.admin"])
}

fn second(w: &World) -> HandlerContext {
    ctx_of(w, &w.second, &["org.admin"])
}

fn member(w: &World) -> HandlerContext {
    ctx_of(w, &w.member, &[])
}

async fn run(ctx: &HandlerContext, payload: P) -> Result<P, tentaflow_protocol::ProtocolError> {
    match org_structure_dispatch(&MessageBody::OrgStructureBody(payload), ctx).await? {
        MessageBody::OrgStructureBody(p) => Ok(p),
        other => panic!("expected an OrgStructureBody answer, got {other:?}"),
    }
}

fn day(offset: i64) -> String {
    let today: NaiveDate = chrono::Utc::now().date_naive();
    let shifted = if offset >= 0 {
        today + Days::new(offset as u64)
    } else {
        today - Days::new(offset.unsigned_abs())
    };
    shifted.format("%Y-%m-%d").to_string()
}

// ----- the live structure, built with ordinary writes ------------------------

async fn write(ctx: &HandlerContext, payload: P) -> OrgWriteResult {
    match run(ctx, payload).await.expect("a write is answered") {
        P::WriteResponse {
            ok: true,
            result: Some(result),
            ..
        } => result,
        other => panic!("write did not succeed: {other:?}"),
    }
}

async fn make_unit(ctx: &HandlerContext, name: &str, parent: Option<&str>) -> String {
    match write(ctx, unit_create(name, parent, day(-30))).await {
        OrgWriteResult::Unit(u) => u.unit_id,
        other => panic!("expected a unit, got {other:?}"),
    }
}

async fn make_position(
    ctx: &HandlerContext,
    unit: &str,
    name: &str,
    parent: Option<&str>,
) -> String {
    match write(ctx, position_create(unit, name, parent, day(-30))).await {
        OrgWriteResult::Position(p) => p.position_id,
        other => panic!("expected a position, got {other:?}"),
    }
}

fn unit_create(name: &str, parent: Option<&str>, from: String) -> P {
    P::UnitCreateRequest {
        name: name.to_string(),
        code: None,
        type_id: None,
        parent_unit_id: parent.map(str::to_string),
        color: None,
        valid_from: from,
        valid_to: None,
        confirm_backdated: true,
    }
}

fn position_create(unit: &str, name: &str, parent: Option<&str>, from: String) -> P {
    P::PositionCreateRequest {
        unit_id: unit.to_string(),
        name: name.to_string(),
        code: None,
        role_id: None,
        is_manager: None,
        is_staff: false,
        parent_position_id: parent.map(str::to_string),
        valid_from: from,
        valid_to: None,
        confirm_backdated: true,
    }
}

fn op(request: P) -> OrgWriteOp {
    OrgWriteOp {
        temp_id: None,
        request,
    }
}

fn op_with(temp: &str, request: P) -> OrgWriteOp {
    OrgWriteOp {
        temp_id: Some(temp.to_string()),
        request,
    }
}

// ----- change sets -----------------------------------------------------------

struct Answer {
    ok: bool,
    code: Option<String>,
    set: Option<OrgChangeSet>,
    valid: bool,
    results: Vec<OrgBatchOpResult>,
}

async fn change_set(ctx: &HandlerContext, payload: P) -> Answer {
    match run(ctx, payload)
        .await
        .expect("a change set request is answered")
    {
        P::ChangeSetResponse {
            ok,
            error,
            change_set,
            valid,
            results,
            ..
        } => Answer {
            ok,
            code: error.map(|e| e.code),
            set: change_set,
            valid,
            results,
        },
        other => panic!("expected a ChangeSetResponse, got {other:?}"),
    }
}

async fn save(
    ctx: &HandlerContext,
    id: Option<&str>,
    effective: i64,
    ops: Vec<OrgWriteOp>,
) -> Answer {
    change_set(
        ctx,
        P::ChangeSetSaveRequest {
            id: id.map(str::to_string),
            name: "Reorganizacja Q4".to_string(),
            effective_date: day(effective),
            ops,
        },
    )
    .await
}

async fn submit(ctx: &HandlerContext, id: &str) -> Answer {
    change_set(ctx, P::ChangeSetSubmitRequest { id: id.to_string() }).await
}

async fn approve(ctx: &HandlerContext, id: &str) -> Answer {
    change_set(ctx, P::ChangeSetApproveRequest { id: id.to_string() }).await
}

async fn withdraw(ctx: &HandlerContext, id: &str) -> Answer {
    change_set(ctx, P::ChangeSetWithdrawRequest { id: id.to_string() }).await
}

async fn state_of(ctx: &HandlerContext, id: &str) -> String {
    change_set(ctx, P::ChangeSetGetRequest { id: id.to_string() })
        .await
        .set
        .expect("the change set exists")
        .state
}

async fn view_on(ctx: &HandlerContext, offset: i64) -> OrgStructureView {
    match run(
        ctx,
        P::StructureRequest {
            at: Some(day(offset)),
        },
    )
    .await
    .unwrap()
    {
        P::StructureResponse { view, .. } => view,
        other => panic!("expected a structure, got {other:?}"),
    }
}

fn unit_names(view: &OrgStructureView) -> Vec<String> {
    let mut names: Vec<String> = view.units.iter().map(|u| u.name.clone()).collect();
    names.sort();
    names
}

/// Board > IT, with a Lead position under a CTO in IT: the live structure a
/// reorganization is planned against. Returns (board, it, cto, lead).
async fn company(ctx: &HandlerContext) -> (String, String, String, String) {
    let board = make_unit(ctx, "Board", None).await;
    let it = make_unit(ctx, "IT", Some(&board)).await;
    let cto = make_position(ctx, &it, "CTO", None).await;
    let lead = make_position(ctx, &it, "Lead", Some(&cto)).await;
    (board, it, cto, lead)
}

/// The plan of the tests: a new unit "Quality" under Board with a Tester in it.
fn quality_ops(board: &str, from: i64) -> Vec<OrgWriteOp> {
    vec![
        op_with(
            "tmp:quality",
            unit_create("Quality", Some(board), day(from)),
        ),
        op_with(
            "tmp:tester",
            position_create("tmp:quality", "Tester", None, day(from)),
        ),
    ]
}

#[tokio::test]
async fn a_draft_is_validated_by_a_dry_run_and_the_live_structure_is_not_touched() {
    let w = world();
    let a = admin(&w);
    let (board, ..) = company(&a).await;

    let saved = save(&a, None, 30, quality_ops(&board, 30)).await;
    assert!(saved.ok && saved.valid, "{:?}", saved.results);
    let set = saved.set.expect("the draft");
    assert_eq!((set.state.as_str(), set.op_count), ("draft", 2));
    assert_eq!(
        set.ops.len(),
        2,
        "the answer to a save carries the operations"
    );
    assert_eq!(set.author_user_id, w.admin);

    assert_eq!(
        unit_names(&view_on(&a, 30).await),
        ["Board", "IT"],
        "a dry run leaves nothing"
    );
}

#[tokio::test]
async fn a_draft_is_kept_with_the_operations_that_do_not_pass_and_cannot_be_submitted() {
    let w = world();
    let a = admin(&w);
    let (_, _, cto, _) = company(&a).await;
    let bad = vec![
        op(P::PositionMoveRequest {
            position_id: "no-such-position".into(),
            new_parent_position_id: Some(cto),
            from: day(30),
            confirm_backdated: false,
        }),
        op(unit_create("Fine", None, day(30))),
    ];
    let saved = save(&a, None, 30, bad).await;
    assert!(saved.ok, "the draft is stored so work is not lost");
    assert!(!saved.valid);
    assert_eq!(
        saved.results.iter().map(|r| r.ok).collect::<Vec<_>>(),
        [false, true]
    );
    assert_eq!(saved.results[0].error.as_ref().unwrap().code, "not_found");

    let id = saved.set.unwrap().id;
    let refused = submit(&a, &id).await;
    assert_eq!(refused.code.as_deref(), Some("change_set_invalid"));
    assert_eq!(state_of(&a, &id).await, "draft");
}

#[tokio::test]
async fn an_operation_dated_before_the_plan_is_reported() {
    let w = world();
    let a = admin(&w);
    let (board, ..) = company(&a).await;
    let saved = save(&a, None, 30, quality_ops(&board, 10)).await;
    assert!(!saved.valid);
    assert_eq!(
        saved.results[0].error.as_ref().map(|e| e.code.as_str()),
        Some("op_before_effective_date")
    );
}

#[tokio::test]
async fn the_author_cannot_approve_and_a_second_administrator_applies_the_plan() {
    let w = world();
    let (a, b) = (admin(&w), second(&w));
    let (board, ..) = company(&a).await;
    let id = save(&a, None, 30, quality_ops(&board, 30))
        .await
        .set
        .unwrap()
        .id;

    // A draft is not approvable.
    assert_eq!(
        approve(&b, &id).await.code.as_deref(),
        Some("change_set_state")
    );
    let submitted = submit(&a, &id).await;
    assert_eq!(submitted.set.unwrap().state, "pending");

    let own = approve(&a, &id).await;
    assert_eq!(own.code.as_deref(), Some("self_approval"));
    assert!(!own.ok);
    assert_eq!(
        state_of(&a, &id).await,
        "pending",
        "the refusal changes nothing"
    );
    assert_eq!(unit_names(&view_on(&a, 30).await), ["Board", "IT"]);

    let approved = approve(&b, &id).await;
    assert!(approved.ok, "{:?} {:?}", approved.code, approved.results);
    let set = approved.set.unwrap();
    assert_eq!(
        (set.state.as_str(), set.approver_user_id.as_deref()),
        ("applied", Some(w.second.as_str()))
    );
    assert_eq!(
        unit_names(&view_on(&a, 30).await),
        ["Board", "IT", "Quality"]
    );
    assert_eq!(
        unit_names(&view_on(&a, 0).await),
        ["Board", "IT"],
        "the change takes effect on its day, not before"
    );

    let again = approve(&b, &id).await;
    assert_eq!(again.code.as_deref(), Some("change_set_state"));
}

#[tokio::test]
async fn editing_a_plan_makes_the_editor_its_author() {
    let w = world();
    let (a, b) = (admin(&w), second(&w));
    let (board, ..) = company(&a).await;
    let id = save(&a, None, 30, quality_ops(&board, 30))
        .await
        .set
        .unwrap()
        .id;
    submit(&a, &id).await;

    let edited = save(&b, Some(&id), 30, quality_ops(&board, 30)).await;
    let set = edited.set.unwrap();
    assert_eq!(
        (set.state.as_str(), set.author_user_id.as_str()),
        ("draft", w.second.as_str())
    );
    submit(&b, &id).await;
    assert_eq!(
        approve(&b, &id).await.code.as_deref(),
        Some("self_approval"),
        "the one who last changed it cannot approve it"
    );
    assert!(approve(&a, &id).await.ok);
}

#[tokio::test]
async fn withdrawing_leaves_the_live_structure_untouched() {
    let w = world();
    let (a, b) = (admin(&w), second(&w));
    let (board, ..) = company(&a).await;
    let before = view_on(&a, 30).await;
    let id = save(&a, None, 30, quality_ops(&board, 30))
        .await
        .set
        .unwrap()
        .id;
    submit(&a, &id).await;

    let withdrawn = withdraw(&b, &id).await;
    assert!(withdrawn.ok);
    assert_eq!(withdrawn.set.unwrap().state, "withdrawn");
    assert_eq!(view_on(&a, 30).await, before);
    assert_eq!(
        approve(&b, &id).await.code.as_deref(),
        Some("change_set_state")
    );
    assert_eq!(
        withdraw(&a, &id).await.code.as_deref(),
        Some("change_set_state"),
        "a withdrawn plan is final"
    );
}

/// Every day the tests look at: the structure must be identical on all of them after a take-back.
const DAYS: [i64; 6] = [-60, -10, 0, 10, 40, 90];

async fn all_views(ctx: &HandlerContext) -> Vec<OrgStructureView> {
    let mut out = Vec::new();
    for offset in DAYS {
        out.push(view_on(ctx, offset).await);
    }
    out
}

#[tokio::test]
async fn an_approved_plan_withdrawn_before_its_day_leaves_no_trace_on_any_day() {
    let w = world();
    let (a, b) = (admin(&w), second(&w));
    let (board, _, _, lead) = company(&a).await;
    // Besides new things the plan closes an existing line and renames a position.
    let mut ops = quality_ops(&board, 30);
    ops.push(op(P::PositionMoveRequest {
        position_id: lead.clone(),
        new_parent_position_id: None,
        from: day(30),
        confirm_backdated: false,
    }));
    ops.push(op(P::PositionUpdateRequest {
        position_id: lead.clone(),
        name: Some("Team lead".into()),
        code: None,
        role_id: None,
        is_manager: None,
        is_staff: None,
        clear: vec![],
        from: day(30),
        confirm_backdated: false,
    }));
    let before = all_views(&a).await;
    let id = save(&a, None, 30, ops).await.set.unwrap().id;
    submit(&a, &id).await;
    assert!(approve(&b, &id).await.ok);
    assert_ne!(
        all_views(&a).await,
        before,
        "the plan is in the structure once approved"
    );

    let withdrawn = withdraw(&a, &id).await;
    assert!(withdrawn.ok, "{:?}", withdrawn.code);
    assert_eq!(withdrawn.set.unwrap().state, "withdrawn");
    assert_eq!(
        all_views(&a).await,
        before,
        "the structure is what it was before the approval, on every day"
    );
    assert_eq!(
        approve(&b, &id).await.code.as_deref(),
        Some("change_set_state")
    );
    assert_eq!(
        withdraw(&b, &id).await.code.as_deref(),
        Some("change_set_state")
    );
}

#[tokio::test]
async fn a_later_change_that_leans_on_the_plan_refuses_the_withdrawal() {
    let w = world();
    let (a, b) = (admin(&w), second(&w));
    let (board, ..) = company(&a).await;
    let id = save(&a, None, 30, quality_ops(&board, 30))
        .await
        .set
        .unwrap()
        .id;
    submit(&a, &id).await;
    assert!(approve(&b, &id).await.ok);

    let tester = view_on(&a, 40)
        .await
        .positions
        .into_iter()
        .find(|p| p.name == "Tester")
        .expect("the plan made the position")
        .position_id;
    write(
        &a,
        P::AssignRequest {
            position_id: tester,
            subject: OrgSubject::User(w.member.clone()),
            assignment_type: OrgAssignmentType::Permanent,
            share: 1.0,
            is_primary: None,
            valid_from: day(40),
            valid_to: None,
            confirm_backdated: false,
        },
    )
    .await;
    let with_person = all_views(&a).await;

    let refused = withdraw(&a, &id).await;
    assert!(!refused.ok);
    assert_eq!(refused.code.as_deref(), Some("change_set_dependents"));
    assert_eq!(refused.set.unwrap().state, "applied");
    assert_eq!(all_views(&a).await, with_person, "nothing was taken back");
}

#[tokio::test]
async fn a_plan_whose_day_came_is_changed_with_ordinary_edits() {
    let w = world();
    let (a, b) = (admin(&w), second(&w));
    let id = save(&a, None, 0, vec![op(unit_create("Today", None, day(0)))])
        .await
        .set
        .unwrap()
        .id;
    submit(&a, &id).await;
    assert!(approve(&b, &id).await.ok);
    assert_eq!(
        withdraw(&a, &id).await.code.as_deref(),
        Some("change_set_started")
    );
    assert_eq!(state_of(&a, &id).await, "applied");
}

/// (unit name, parent name) and (position name, unit name, parent name) — ids
/// are made fresh on every run, so what the two runs must agree on is names.
fn shape(
    view: &OrgStructureView,
) -> (
    Vec<(String, Option<String>)>,
    Vec<(String, String, Option<String>)>,
) {
    let unit = |id: &str| {
        view.units
            .iter()
            .find(|u| u.unit_id == id)
            .map(|u| u.name.clone())
    };
    let position = |id: &str| {
        view.positions
            .iter()
            .find(|p| p.position_id == id)
            .map(|p| p.name.clone())
    };
    let mut units: Vec<_> = view
        .units
        .iter()
        .map(|u| (u.name.clone(), u.parent_unit_id.as_deref().and_then(unit)))
        .collect();
    let mut positions: Vec<_> = view
        .positions
        .iter()
        .map(|p| {
            (
                p.name.clone(),
                unit(&p.unit_id).unwrap_or_default(),
                p.primary_parent_position_id.as_deref().and_then(position),
            )
        })
        .collect();
    units.sort();
    positions.sort();
    (units, positions)
}

#[tokio::test]
async fn approving_does_what_the_dry_run_showed() {
    let w = world();
    let (a, b) = (admin(&w), second(&w));
    let (board, it, cto, lead) = company(&a).await;
    let ops = vec![
        op_with("tmp:quality", unit_create("Quality", Some(&board), day(30))),
        op_with(
            "tmp:tester",
            position_create("tmp:quality", "Tester", None, day(30)),
        ),
        op(P::HeadSetRequest {
            unit_id: "tmp:quality".into(),
            head_position_id: Some("tmp:tester".into()),
            from: day(30),
            confirm_backdated: false,
        }),
        op(P::PositionMoveRequest {
            position_id: lead.clone(),
            new_parent_position_id: Some("tmp:tester".into()),
            from: day(30),
            confirm_backdated: false,
        }),
        op(P::UnitMoveRequest {
            unit_id: it.clone(),
            new_parent_unit_id: Some("tmp:quality".into()),
            from: day(30),
            confirm_backdated: false,
        }),
    ];
    let _ = cto;
    let id = save(&a, None, 30, ops).await.set.unwrap().id;
    submit(&a, &id).await;

    let preview = match run(
        &a,
        P::ChangeSetPreviewRequest {
            id: id.clone(),
            unit_id: None,
        },
    )
    .await
    .unwrap()
    {
        P::ChangeSetPreviewResponse {
            ok,
            valid,
            preview: Some(preview),
            live: Some(live),
            items,
            at,
            ..
        } => {
            assert!(ok && valid);
            assert_eq!(at, day(30));
            assert_eq!(
                live,
                view_on(&a, 30).await,
                "the live side is the live structure"
            );
            assert!(items
                .iter()
                .any(|i| i.entity == "unit" && i.name == "Quality" && i.change == "added"));
            assert!(items
                .iter()
                .any(|i| i.id == it && i.field.as_deref() == Some("parent_unit_id")));
            preview
        }
        other => panic!("expected a preview, got {other:?}"),
    };
    assert_eq!(
        view_on(&a, 30).await.units.len(),
        2,
        "the dry run left nothing behind"
    );

    assert!(approve(&b, &id).await.ok);
    assert_eq!(
        shape(&view_on(&a, 30).await),
        shape(&preview),
        "the applied structure is the previewed one"
    );
    assert_eq!(view_on(&a, 30).await.units.len(), 3);
}

#[tokio::test]
async fn a_structure_that_moved_on_makes_the_approval_fail_as_a_whole() {
    let w = world();
    let (a, b) = (admin(&w), second(&w));
    let (board, _, cto, lead) = company(&a).await;
    let mut ops = quality_ops(&board, 30);
    ops.push(op(P::PositionMoveRequest {
        position_id: lead.clone(),
        new_parent_position_id: None,
        from: day(30),
        confirm_backdated: false,
    }));
    let id = save(&a, None, 30, ops).await.set.unwrap().id;
    assert!(submit(&a, &id).await.ok);

    // Somebody ends the Lead position before the plan takes effect.
    write(
        &a,
        P::PositionEndRequest {
            position_id: lead.clone(),
            from: day(10),
            confirm_backdated: false,
        },
    )
    .await;
    let _ = cto;

    let refused = approve(&b, &id).await;
    assert!(!refused.ok);
    assert_eq!(refused.code.as_deref(), Some("change_set_conflict"));
    let failed: Vec<usize> = refused
        .results
        .iter()
        .filter(|r| !r.ok)
        .map(|r| r.index as usize)
        .collect();
    assert_eq!(failed, [2], "the operation that no longer fits is named");
    assert_eq!(
        refused.set.unwrap().state,
        "pending",
        "nothing was recorded"
    );
    assert_eq!(
        unit_names(&view_on(&a, 30).await),
        ["Board", "IT"],
        "the operations that would have passed were not kept either"
    );
}

#[tokio::test]
async fn a_plan_dated_in_the_past_is_refused() {
    let w = world();
    let a = admin(&w);
    let id = save(&a, None, -3, vec![]).await.set.unwrap().id;
    assert_eq!(
        submit(&a, &id).await.code.as_deref(),
        Some("effective_date_passed")
    );
}

#[tokio::test]
async fn change_sets_are_an_administrators_business() {
    let w = world();
    let (a, m) = (admin(&w), member(&w));
    let id = save(&a, None, 30, vec![]).await.set.unwrap().id;
    let requests = vec![
        P::ChangeSetListRequest {},
        P::ChangeSetGetRequest { id: id.clone() },
        P::ChangeSetSaveRequest {
            id: None,
            name: "x".into(),
            effective_date: day(30),
            ops: vec![],
        },
        P::ChangeSetSubmitRequest { id: id.clone() },
        P::ChangeSetApproveRequest { id: id.clone() },
        P::ChangeSetWithdrawRequest { id: id.clone() },
        P::ChangeSetPreviewRequest {
            id: id.clone(),
            unit_id: None,
        },
    ];
    for request in requests {
        let label = format!("{request:?}");
        let error = run(&m, request.clone())
            .await
            .expect_err(&format!("{label} must be refused"));
        assert_eq!(error.code, ProtocolErrorCode::PolicyDenied, "{label}");

        let mut outsider = member(&w);
        outsider.org_context.as_mut().unwrap().user_id = w.outsider.clone();
        let error = run(&outsider, request).await.expect_err(&label);
        assert_eq!(error.code, ProtocolErrorCode::NotFound, "{label}");
    }
    assert_eq!(state_of(&a, &id).await, "draft");
}

#[tokio::test]
async fn the_list_shows_every_plan_with_its_state_and_people() {
    let w = world();
    let (a, b) = (admin(&w), second(&w));
    let (board, ..) = company(&a).await;
    let one = save(&a, None, 30, quality_ops(&board, 30))
        .await
        .set
        .unwrap()
        .id;
    let two = save(&a, None, 60, vec![]).await.set.unwrap().id;
    submit(&a, &one).await;
    withdraw(&b, &two).await;

    match run(&b, P::ChangeSetListRequest {}).await.unwrap() {
        P::ChangeSetListResponse { items, today, .. } => {
            assert!(!today.is_empty());
            let mut states: Vec<(&str, &str)> = items
                .iter()
                .map(|s| (s.id.as_str(), s.state.as_str()))
                .collect();
            states.sort();
            let mut expected = vec![(one.as_str(), "pending"), (two.as_str(), "withdrawn")];
            expected.sort();
            assert_eq!(states, expected);
            assert!(items
                .iter()
                .all(|s| s.ops.is_empty() && s.author_name.is_some()));
            assert_eq!(items.iter().find(|s| s.id == one).unwrap().op_count, 2);
        }
        other => panic!("expected a list, got {other:?}"),
    }
}

// ----- history ---------------------------------------------------------------

async fn history(
    ctx: &HandlerContext,
) -> (Vec<tentaflow_protocol::org_history::OrgHistoryEntry>, bool) {
    match run(
        ctx,
        P::HistoryListRequest {
            from: None,
            to: None,
            unit_id: None,
            offset: 0,
            limit: 200,
        },
    )
    .await
    .unwrap()
    {
        P::HistoryListResponse {
            entries,
            personal_visible,
            ..
        } => (entries, personal_visible),
        other => panic!("expected a history, got {other:?}"),
    }
}

async fn diff(ctx: &HandlerContext, from: i64, to: i64) -> (Vec<OrgDiffItem>, bool) {
    match run(
        ctx,
        P::HistoryDiffRequest {
            from: day(from),
            to: day(to),
            unit_id: None,
        },
    )
    .await
    .unwrap()
    {
        P::HistoryDiffResponse {
            items,
            personal_visible,
            ..
        } => (items, personal_visible),
        other => panic!("expected a diff, got {other:?}"),
    }
}

async fn assign_past(ctx: &HandlerContext, position: &str, user: &str, from: i64) {
    write(
        ctx,
        P::AssignRequest {
            position_id: position.to_string(),
            subject: OrgSubject::User(user.to_string()),
            assignment_type: OrgAssignmentType::Permanent,
            share: 1.0,
            is_primary: None,
            valid_from: day(from),
            valid_to: None,
            confirm_backdated: true,
        },
    )
    .await;
}

#[tokio::test]
async fn as_of_snapshots_of_the_past_and_the_future_differ_by_what_was_planned() {
    let w = world();
    let (a, b) = (admin(&w), second(&w));
    let (board, ..) = company(&a).await;
    let id = save(&a, None, 30, quality_ops(&board, 30))
        .await
        .set
        .unwrap()
        .id;
    submit(&a, &id).await;
    assert!(approve(&b, &id).await.ok);

    assert_eq!(
        unit_names(&view_on(&a, -60).await),
        Vec::<String>::new(),
        "before it existed"
    );
    assert_eq!(unit_names(&view_on(&a, 0).await), ["Board", "IT"]);
    assert_eq!(
        unit_names(&view_on(&a, 60).await),
        ["Board", "IT", "Quality"]
    );

    let (forward, _) = diff(&a, 0, 30).await;
    assert!(forward
        .iter()
        .any(|i| i.entity == "unit" && i.name == "Quality" && i.change == "added"));
    assert!(forward
        .iter()
        .any(|i| i.entity == "position" && i.name == "Tester" && i.change == "added"));
    let (backward, _) = diff(&a, 30, 0).await;
    assert!(backward
        .iter()
        .any(|i| i.entity == "unit" && i.name == "Quality" && i.change == "removed"));
    assert!(diff(&a, 0, 0).await.0.is_empty());

    // The change is in the history on its own day, planned ones on top.
    let (entries, _) = history(&a).await;
    let planned: Vec<_> = entries
        .iter()
        .filter(|e| e.effective_date.as_deref() == Some(day(30).as_str()))
        .collect();
    assert!(
        planned.iter().any(|e| e.action == "org.structure.batch"),
        "{entries:#?}"
    );
    assert_eq!(
        entries[0].effective_date.as_deref(),
        Some(day(30).as_str()),
        "the planned change comes first"
    );
    assert!(entries.iter().any(|e| e.action == "org.change_set.approve"));
}

#[tokio::test]
async fn history_shows_every_member_the_structure_but_only_an_administrator_the_people() {
    let w = world();
    let (a, m) = (admin(&w), member(&w));
    let (_, it, cto, lead) = company(&a).await;
    assign_past(&a, &cto, &w.admin, -20).await;
    assign_past(&a, &lead, &w.second, -20).await;
    let _ = it;

    let (all, personal) = history(&a).await;
    assert!(personal);
    assert_eq!(
        all.iter().filter(|e| e.target_kind == "assignment").count(),
        2
    );

    let (seen, personal) = history(&m).await;
    assert!(!personal, "the answer says what is left out");
    assert!(seen.iter().all(|e| e.target_kind != "assignment"));
    assert!(
        seen.iter().any(|e| e.target_kind == "unit"),
        "the structural changes stay"
    );
    assert!(seen.iter().all(|e| !e.action.starts_with("org.change_set")));

    // A person's own seat is theirs to see.
    let mine = ctx_of(&w, &w.second, &[]);
    let (own, _) = history(&mine).await;
    let assignments: Vec<_> = own
        .iter()
        .filter(|e| e.target_kind == "assignment")
        .collect();
    assert_eq!(assignments.len(), 1);
    assert_eq!(
        assignments[0].subject,
        Some(OrgSubject::User(w.second.clone()))
    );
}

#[tokio::test]
async fn a_past_day_hides_who_held_what_from_everyone_but_an_administrator_and_the_person() {
    let w = world();
    let (a, m) = (admin(&w), member(&w));
    let (_, _, cto, lead) = company(&a).await;
    assign_past(&a, &cto, &w.admin, -20).await;
    assign_past(&a, &lead, &w.member, -20).await;

    let holders = |view: &OrgStructureView| -> Vec<String> {
        let mut names: Vec<String> = view
            .assignments
            .iter()
            .map(|x| x.display_name.clone())
            .collect();
        names.sort();
        names
    };
    let as_admin = holders(&view_on(&a, -10).await);
    assert!(
        as_admin.iter().all(|n| !n.starts_with("Osoba #")),
        "{as_admin:?}"
    );

    let as_member = view_on(&m, -10).await;
    let names = holders(&as_member);
    assert_eq!(
        names.iter().filter(|n| n.starts_with("Osoba #")).count(),
        1,
        "{names:?}"
    );
    assert_eq!(as_member.positions.len(), 2, "the structure itself is open");
    assert!(
        as_member
            .assignments
            .iter()
            .any(|x| x.subject == OrgSubject::User(w.member.clone())),
        "the member's own seat is theirs"
    );

    // Today is not history.
    assert!(holders(&view_on(&m, 0).await)
        .iter()
        .all(|n| !n.starts_with("Osoba #")));

    // Nor is the diff of two past days.
    let (items, personal) = diff(&m, -10, 0).await;
    assert!(!personal);
    assert!(
        items
            .iter()
            .all(|i| i.entity != "assignment"
                || i.subject == Some(OrgSubject::User(w.member.clone())))
    );

    // A file of a past day lists the people: an administrator's.
    let export_past = run(
        &m,
        P::ExportRequest {
            format: OrgFileFormat::Csv,
            at: Some(day(-10)),
        },
    )
    .await
    .expect_err("refused");
    assert_eq!(export_past.code, ProtocolErrorCode::PolicyDenied);
    assert!(run(
        &m,
        P::ExportRequest {
            format: OrgFileFormat::Csv,
            at: None
        }
    )
    .await
    .is_ok());
    assert!(run(
        &a,
        P::ExportRequest {
            format: OrgFileFormat::Csv,
            at: Some(day(-10))
        }
    )
    .await
    .is_ok());
}

/// Sets which members of the organization hold `org.admin` in the database (the contexts
/// of these tests carry their own permissions; the sole-administrator rule reads the roles).
fn set_admins(w: &World, admins: &[&str]) {
    let conn = w.state.db.write().unwrap();
    let admin_role: String = conn
        .query_row(
            "SELECT role_id FROM roles WHERE name = 'org_admin'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let plain_role: String = conn
        .query_row(
            "SELECT role_id FROM roles WHERE permissions_json NOT LIKE '%org.admin%' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    // Everybody else is a plain member; the bootstrap account cannot be demoted, so it is deactivated.
    conn.execute("UPDATE org_memberships SET role_id = ?1", [&plain_role])
        .unwrap();
    conn.execute(
        "UPDATE user_accounts SET is_active = 0 WHERE id = '00000000-0000-4000-8000-000000000002'",
        [],
    )
    .unwrap();
    for user in admins {
        conn.execute(
            "UPDATE org_memberships SET role_id = ?1 WHERE user_id = ?2",
            [&admin_role, &user.to_string()],
        )
        .unwrap();
    }
}

async fn sole_admin_flag(ctx: &HandlerContext) -> bool {
    match run(ctx, P::ChangeSetListRequest {}).await.unwrap() {
        P::ChangeSetListResponse { sole_admin, .. } => sole_admin,
        other => panic!("expected a list, got {other:?}"),
    }
}

#[tokio::test]
async fn the_only_administrator_may_approve_their_own_plan_until_a_second_one_appears() {
    let w = world();
    let (a, b) = (admin(&w), second(&w));
    let (board, ..) = company(&a).await;
    set_admins(&w, &[w.admin.as_str()]);
    assert!(sole_admin_flag(&a).await);

    let own = save(&a, None, 30, quality_ops(&board, 30))
        .await
        .set
        .unwrap()
        .id;
    submit(&a, &own).await;
    let approved = approve(&a, &own).await;
    assert!(approved.ok, "{:?}", approved.code);
    assert_eq!(approved.set.unwrap().state, "applied");
    let flagged: i64 = w
        .state
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'org.change_set.approve' \
             AND details LIKE '%\"self_approval\":true%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(flagged, 1, "the audit entry says the author approved");

    // A second administrator appears: the author is denied again.
    set_admins(&w, &[w.admin.as_str(), w.second.as_str()]);
    assert!(!sole_admin_flag(&a).await);
    let another = save(&a, None, 60, vec![op(unit_create("Later", None, day(60)))])
        .await
        .set
        .unwrap()
        .id;
    submit(&a, &another).await;
    assert_eq!(
        approve(&a, &another).await.code.as_deref(),
        Some("self_approval")
    );
    assert!(approve(&b, &another).await.ok);
}
