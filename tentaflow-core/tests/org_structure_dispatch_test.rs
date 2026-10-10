// ============ File: tests/org_structure_dispatch_test.rs ============
//
// The org-structure binary RPC (`dispatch::org_structure::org_structure_dispatch`)
// against a real database: an administrator can make every write, a plain
// member is refused every one with PolicyDenied, someone outside the
// organization gets NotFound for reads AND writes, another organization's ids
// are never confirmed, every write leaves an audit row on an intact chain and
// publishes an `org.*` event carrying ids only.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{Days, NaiveDate};
use tentaflow_core::addon::event_bus::EventBus;
use tentaflow_core::addon::event_publish;
use tentaflow_core::audit::verify::verify_chain;
use tentaflow_core::dispatch::org_structure::org_structure_dispatch;
use tentaflow_core::dispatch::state::AppState;
use tentaflow_core::dispatch::HandlerContext;
use tentaflow_core::services::org::{self, DEFAULT_ORG_ID};
use tentaflow_core::services::rbac::OrgContext;
use tentaflow_protocol::org_structure::{
    OrgAssignmentType, OrgDirection, OrgFileFormat, OrgImportAction, OrgImportMode,
    OrgImportReport, OrgImportResolution, OrgLineKind, OrgSeatScope, OrgStructurePayload as P,
    OrgSubject, OrgTarget, OrgWarning, OrgWriteResult,
};
use tentaflow_protocol::{MessageBody, ProtocolErrorCode, SessionAuth};

fn event_bus() -> Arc<EventBus> {
    // The global handle is first-caller-wins, so every test in this binary
    // reads the same bus; each one filters by ids only it created.
    if let Some(bus) = event_publish::global() {
        return bus;
    }
    event_publish::init_global(Arc::new(EventBus::new()));
    event_publish::global().expect("bus installed")
}

struct World {
    state: Arc<AppState>,
    admin: String,
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
    event_bus();
    let state = AppState::for_test();
    let admin = add_user(&state, Some(DEFAULT_ORG_ID), "org-admin");
    let member = add_user(&state, Some(DEFAULT_ORG_ID), "org-member");
    let outsider = add_user(&state, None, "outsider");
    World {
        state,
        admin,
        member,
        outsider,
    }
}

fn ctx(state: &Arc<AppState>, org_id: &str, user_id: &str, permissions: &[&str]) -> HandlerContext {
    HandlerContext {
        session: SessionAuth::UserSession {
            user_id: [7u8; 16],
            role: Some("user".to_string()),
        },
        correlation_id: 1,
        connection_id: 0,
        resume_secret: None,
        state: state.clone(),
        origin: tentaflow_core::dispatch::RequestOrigin::Local,
        org_context: Some(OrgContext {
            user_id: user_id.to_string(),
            org_id: org_id.to_string(),
            role_id: "role-test".to_string(),
            permissions: permissions
                .iter()
                .map(|p| p.to_string())
                .collect::<HashSet<_>>(),
        }),
    }
}

fn admin_ctx(w: &World) -> HandlerContext {
    ctx(&w.state, DEFAULT_ORG_ID, &w.admin, &["org.admin"])
}

fn member_ctx(w: &World) -> HandlerContext {
    ctx(&w.state, DEFAULT_ORG_ID, &w.member, &[])
}

async fn run(ctx: &HandlerContext, payload: P) -> Result<P, tentaflow_protocol::ProtocolError> {
    match org_structure_dispatch(&MessageBody::OrgStructureBody(payload), ctx).await? {
        MessageBody::OrgStructureBody(p) => Ok(p),
        other => panic!("expected an OrgStructureBody answer, got {other:?}"),
    }
}

/// A write that must succeed: returns its result and warnings.
async fn write_ok(ctx: &HandlerContext, payload: P) -> (OrgWriteResult, Vec<OrgWarning>) {
    let label = format!("{payload:?}");
    match run(ctx, payload)
        .await
        .unwrap_or_else(|e| panic!("{label} refused: {e:?}"))
    {
        P::WriteResponse {
            ok: true,
            result: Some(result),
            warnings,
            ..
        } => (result, warnings),
        other => panic!("{label} did not succeed: {other:?}"),
    }
}

/// A write the rules refuse: returns the typed error code.
async fn write_refused(ctx: &HandlerContext, payload: P) -> String {
    match run(ctx, payload)
        .await
        .expect("a rule refusal is an answer, not a protocol error")
    {
        P::WriteResponse {
            ok: false,
            error: Some(error),
            result: None,
            ..
        } => error.code,
        other => panic!("expected a typed refusal, got {other:?}"),
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

fn unit_id(result: &OrgWriteResult) -> String {
    match result {
        OrgWriteResult::Unit(u) => u.unit_id.clone(),
        other => panic!("expected a unit, got {other:?}"),
    }
}

fn position_id(result: &OrgWriteResult) -> String {
    match result {
        OrgWriteResult::Position(p) => p.position_id.clone(),
        other => panic!("expected a position, got {other:?}"),
    }
}

fn unit_create(name: &str, parent: Option<&str>, from: String, confirm: bool) -> P {
    P::UnitCreateRequest {
        name: name.to_string(),
        code: None,
        type_id: None,
        parent_unit_id: parent.map(str::to_string),
        color: None,
        valid_from: from,
        valid_to: None,
        confirm_backdated: confirm,
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
        confirm_backdated: false,
    }
}

fn assign(position: &str, subject: OrgSubject, share: f64, from: String) -> P {
    P::AssignRequest {
        position_id: position.to_string(),
        subject,
        assignment_type: OrgAssignmentType::Permanent,
        share,
        is_primary: None,
        valid_from: from,
        valid_to: None,
        confirm_backdated: false,
    }
}

/// One request of every write variant, aimed at ids that need not exist:
/// authorization is decided before any of them is looked at.
const FILE: &str = "kod jednostki;nazwa jednostki;kod stanowiska;stanowisko;kierownik jednostki;login/e-mail osoby\n\
IT;Dział IT;IT-1;Dyrektor;tak;org-admin\n\
IT;;IT-2;Analityk;;org-member\n";

fn import_dry_run(file: &str, resolutions: Vec<OrgImportResolution>) -> P {
    P::ImportDryRunRequest {
        format: OrgFileFormat::Csv,
        bytes: file.as_bytes().to_vec(),
        mode: OrgImportMode::Upsert,
        as_of: None,
        confirm_backdated: false,
        confirm_ended: false,
        resolutions,
    }
}

fn import_apply(file: &str, resolutions: Vec<OrgImportResolution>) -> P {
    P::ImportApplyRequest {
        format: OrgFileFormat::Csv,
        bytes: file.as_bytes().to_vec(),
        mode: OrgImportMode::Upsert,
        as_of: None,
        confirm_backdated: false,
        confirm_ended: false,
        resolutions,
    }
}

async fn report_of(ctx: &HandlerContext, payload: P) -> OrgImportReport {
    match run(ctx, payload)
        .await
        .expect("an import is answered with a report")
    {
        P::ImportReportResponse { report } => report,
        other => panic!("expected a report, got {other:?}"),
    }
}

async fn file_of(ctx: &HandlerContext, payload: P) -> (String, String, Vec<u8>) {
    match run(ctx, payload)
        .await
        .expect("an export is answered with a file")
    {
        P::ExportResponse {
            file_name,
            mime,
            bytes,
        } => (file_name, mime, bytes),
        other => panic!("expected a file, got {other:?}"),
    }
}

fn unit_count(w: &World) -> i64 {
    w.state
        .db
        .read()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM org_units", [], |r| r.get(0))
        .unwrap()
}

fn every_write() -> Vec<P> {
    let d = day(3);
    vec![
        // A deputy or an absence is the covered person's own arrangement, so only creating one for somebody
        // else is refused to a plain member here; changing and ending rows are tested with real rows.
        P::DeputySetRequest {
            user_id: "u".into(),
            deputy_user_id: "v".into(),
            scope: "all".into(),
            valid_from: d.clone(),
            valid_to: None,
            confirm_backdated: false,
        },
        P::UnitTypeCreateRequest {
            name: "x".into(),
            color: None,
            icon: None,
        },
        P::UnitTypeUpdateRequest {
            id: "t".into(),
            name: Some("y".into()),
            color: None,
            icon: None,
            clear: vec![],
        },
        P::UnitTypeDeleteRequest { id: "t".into() },
        unit_create("x", None, d.clone(), false),
        P::UnitUpdateRequest {
            unit_id: "u".into(),
            name: Some("y".into()),
            code: None,
            type_id: None,
            color: None,
            clear: vec![],
            from: d.clone(),
            confirm_backdated: false,
        },
        P::UnitMoveRequest {
            unit_id: "u".into(),
            new_parent_unit_id: None,
            from: d.clone(),
            confirm_backdated: false,
        },
        P::UnitEndRequest {
            unit_id: "u".into(),
            from: d.clone(),
            confirm_backdated: false,
        },
        P::HeadSetRequest {
            unit_id: "u".into(),
            head_position_id: None,
            from: d.clone(),
            confirm_backdated: false,
        },
        P::DeputyHeadsSetRequest {
            unit_id: "u".into(),
            position_ids: vec![],
            from: d.clone(),
            confirm_backdated: false,
        },
        position_create("u", "x", None, d.clone()),
        P::PositionUpdateRequest {
            position_id: "p".into(),
            name: Some("y".into()),
            code: None,
            role_id: None,
            is_manager: None,
            is_staff: None,
            clear: vec![],
            from: d.clone(),
            confirm_backdated: false,
        },
        P::PositionMoveRequest {
            position_id: "p".into(),
            new_parent_position_id: None,
            from: d.clone(),
            confirm_backdated: false,
        },
        P::PositionEndRequest {
            position_id: "p".into(),
            from: d.clone(),
            confirm_backdated: false,
        },
        P::ReportingLineSetRequest {
            position_id: "p".into(),
            parent_position_id: "q".into(),
            kind: OrgLineKind::Primary,
            priority: 0,
            valid_from: d.clone(),
            valid_to: None,
            confirm_backdated: false,
        },
        P::ExternalPersonCreateRequest {
            display_name: "x".into(),
            email: None,
            note: None,
        },
        assign("p", OrgSubject::User("u".into()), 1.0, d.clone()),
        P::AssignmentUpdateRequest {
            assignment_id: "a".into(),
            assignment_type: None,
            share: Some(0.5),
            is_primary: None,
            from: d.clone(),
            confirm_backdated: false,
        },
        P::AssignmentEndRequest {
            assignment_id: "a".into(),
            from: d,
            confirm_backdated: false,
        },
        P::TimezoneSetRequest {
            timezone: "Europe/Warsaw".into(),
        },
        P::RecomputeRequest {},
        // A file import writes the whole structure: it is an administrator's,
        // and so is the report of its errors (they quote the file's logins).
        import_dry_run(FILE, vec![]),
        import_apply(FILE, vec![]),
        P::ExportErrorsRequest {
            format: OrgFileFormat::Csv,
            bytes: FILE.as_bytes().to_vec(),
            mode: OrgImportMode::Upsert,
            as_of: None,
            confirm_backdated: false,
            confirm_ended: false,
            resolutions: vec![],
        },
    ]
}

fn every_read() -> Vec<P> {
    vec![
        P::StructureRequest { at: None },
        P::ReportsChainRequest {
            target: OrgTarget::User("u".into()),
            direction: OrgDirection::Up,
            seat_scope: OrgSeatScope::Primary,
            at: None,
        },
        P::SubordinatesRequest {
            target: OrgTarget::Position("p".into()),
            transitive: true,
            seat_scope: OrgSeatScope::Primary,
            at: None,
        },
        P::ManagerRequest {
            user_id: "u".into(),
            at: None,
        },
        P::AssignmentRequest {
            user_id: "u".into(),
            at: None,
        },
        P::IntegrityReportRequest { at: None },
        P::ExportRequest {
            format: OrgFileFormat::Csv,
            at: None,
        },
        P::CoverRequest {
            user_id: None,
            at: None,
            include_past: false,
        },
        P::AvailabilityRequest { at: None },
        P::EscalationChainRequest {
            user_id: "u".into(),
            scope: None,
            at: None,
        },
        P::IsAvailableRequest {
            user_id: "u".into(),
            at: None,
        },
        P::CanViewPersonDataRequest {
            viewer_user_id: None,
            subject_user_id: "u".into(),
            kind: "absence_dates".into(),
            at: None,
        },
        P::VisibilityRequest {
            user_id: None,
            at: None,
        },
        P::WhoSeesRequest {
            subject_user_id: None,
            at: None,
        },
    ]
}

fn expect_code(
    result: Result<P, tentaflow_protocol::ProtocolError>,
    code: ProtocolErrorCode,
    what: &str,
) {
    match result {
        Err(e) => assert_eq!(e.code, code, "{what}: {e:?}"),
        Ok(answer) => panic!("{what} was answered: {answer:?}"),
    }
}

#[tokio::test]
async fn an_admin_makes_every_write_and_the_reads_see_the_result() {
    let w = world();
    let admin = admin_ctx(&w);
    let d = day(2);

    let (result, _) = write_ok(
        &admin,
        P::UnitTypeCreateRequest {
            name: "Dział".into(),
            color: None,
            icon: Some("building".into()),
        },
    )
    .await;
    let type_id = match result {
        OrgWriteResult::UnitType(t) => t.id,
        other => panic!("{other:?}"),
    };
    let (result, _) = write_ok(
        &admin,
        P::UnitTypeUpdateRequest {
            id: type_id.clone(),
            name: Some("Pion".into()),
            color: Some("#123456".into()),
            icon: None,
            clear: vec!["icon".into()],
        },
    )
    .await;
    match result {
        OrgWriteResult::UnitType(t) => {
            assert_eq!(
                (t.name.as_str(), t.icon),
                ("Pion", None),
                "the icon was cleared by name"
            );
        }
        other => panic!("{other:?}"),
    }

    let (root, _) = write_ok(&admin, unit_create("Firma", None, d.clone(), false)).await;
    let root = unit_id(&root);
    let (it, _) = write_ok(&admin, unit_create("IT", Some(&root), d.clone(), false)).await;
    let it = unit_id(&it);
    let (ceo, _) = write_ok(&admin, position_create(&root, "Prezes", None, d.clone())).await;
    let ceo = position_id(&ceo);
    let (lead, _) = write_ok(
        &admin,
        position_create(&it, "Kierownik IT", Some(&ceo), d.clone()),
    )
    .await;
    let lead = position_id(&lead);
    let (dev, _) = write_ok(
        &admin,
        position_create(&it, "Developer", Some(&lead), d.clone()),
    )
    .await;
    let dev = position_id(&dev);

    write_ok(
        &admin,
        P::HeadSetRequest {
            unit_id: root.clone(),
            head_position_id: Some(ceo.clone()),
            from: d.clone(),
            confirm_backdated: false,
        },
    )
    .await;
    write_ok(
        &admin,
        P::HeadSetRequest {
            unit_id: it.clone(),
            head_position_id: Some(lead.clone()),
            from: d.clone(),
            confirm_backdated: false,
        },
    )
    .await;
    let (deputies, _) = write_ok(
        &admin,
        P::DeputyHeadsSetRequest {
            unit_id: it.clone(),
            position_ids: vec![dev.clone()],
            from: d.clone(),
            confirm_backdated: false,
        },
    )
    .await;
    match deputies {
        OrgWriteResult::DeputyHeads(rows) => assert_eq!(rows.len(), 1),
        other => panic!("{other:?}"),
    }

    let (person, _) = write_ok(
        &admin,
        P::ExternalPersonCreateRequest {
            display_name: "Jan Kowalski".into(),
            email: None,
            note: Some("kontraktor".into()),
        },
    )
    .await;
    let external = match person {
        OrgWriteResult::ExternalPerson(p) => p.id,
        other => panic!("{other:?}"),
    };
    let (held, _) = write_ok(
        &admin,
        assign(&ceo, OrgSubject::User(w.admin.clone()), 1.0, d.clone()),
    )
    .await;
    let ceo_assignment = match held {
        OrgWriteResult::Assignment(a) => a,
        other => panic!("{other:?}"),
    };
    assert_eq!(ceo_assignment.assignment_type, OrgAssignmentType::Permanent);
    let (_, warnings) = write_ok(
        &admin,
        assign(&lead, OrgSubject::User(w.admin.clone()), 1.0, d.clone()),
    )
    .await;
    assert!(
        warnings
            .iter()
            .any(|w| matches!(w, OrgWarning::ShareOverbooked { .. })),
        "two full-time positions are answered with a warning, not a refusal: {warnings:?}"
    );
    let (held, _) = write_ok(
        &admin,
        assign(&dev, OrgSubject::External(external.clone()), 0.5, d.clone()),
    )
    .await;
    let dev_assignment = match held {
        OrgWriteResult::Assignment(a) => a,
        other => panic!("{other:?}"),
    };
    write_ok(
        &admin,
        P::AssignmentUpdateRequest {
            assignment_id: dev_assignment.id.clone(),
            assignment_type: Some(OrgAssignmentType::Contractor),
            share: Some(0.75),
            is_primary: None,
            from: day(4),
            confirm_backdated: false,
        },
    )
    .await;

    write_ok(
        &admin,
        P::UnitUpdateRequest {
            unit_id: it.clone(),
            name: Some("Informatyka".into()),
            code: Some("IT".into()),
            type_id: Some(type_id.clone()),
            color: None,
            clear: vec![],
            from: day(4),
            confirm_backdated: false,
        },
    )
    .await;
    write_ok(
        &admin,
        P::PositionUpdateRequest {
            position_id: dev.clone(),
            name: Some("Starszy developer".into()),
            code: None,
            role_id: None,
            is_manager: Some(false),
            is_staff: None,
            clear: vec![],
            from: day(4),
            confirm_backdated: false,
        },
    )
    .await;
    let (line, _) = write_ok(
        &admin,
        P::ReportingLineSetRequest {
            position_id: dev.clone(),
            parent_position_id: ceo.clone(),
            kind: OrgLineKind::Functional,
            priority: 1,
            valid_from: d.clone(),
            valid_to: None,
            confirm_backdated: false,
        },
    )
    .await;
    assert!(matches!(line, OrgWriteResult::ReportingLine(Some(_))));
    write_ok(
        &admin,
        P::PositionMoveRequest {
            position_id: dev.clone(),
            new_parent_position_id: Some(ceo.clone()),
            from: day(5),
            confirm_backdated: false,
        },
    )
    .await;
    write_ok(
        &admin,
        P::UnitMoveRequest {
            unit_id: it.clone(),
            new_parent_unit_id: None,
            from: day(5),
            confirm_backdated: false,
        },
    )
    .await;
    write_ok(
        &admin,
        P::TimezoneSetRequest {
            timezone: "Europe/Warsaw".into(),
        },
    )
    .await;

    // The reads see what the writes made.
    match run(&admin, P::StructureRequest { at: Some(day(6)) })
        .await
        .unwrap()
    {
        P::StructureResponse {
            view,
            unit_types,
            my_permissions,
            import_max_file_bytes,
            import_max_rows,
            batch_max_ops,
        } => {
            assert_eq!(
                import_max_file_bytes,
                900 * 1024,
                "the limit the screen checks before sending"
            );
            assert!(import_max_rows > 0);
            assert_eq!(batch_max_ops, 500);
            assert_eq!(view.units.len(), 2);
            assert_eq!(view.positions.len(), 3);
            assert_eq!(my_permissions, vec!["org.admin".to_string()]);
            assert_eq!(unit_types.len(), 1);
            let dev_view = view
                .positions
                .iter()
                .find(|p| p.position_id == dev)
                .unwrap();
            assert_eq!(dev_view.name, "Starszy developer");
            assert_eq!(
                dev_view.primary_parent_position_id.as_deref(),
                Some(ceo.as_str())
            );
            let holder = view
                .assignments
                .iter()
                .find(|a| a.position_id == dev)
                .unwrap();
            assert_eq!(holder.display_name, "Jan Kowalski");
            assert_eq!(holder.share, 0.75);
        }
        other => panic!("{other:?}"),
    }
    match run(
        &admin,
        P::ReportsChainRequest {
            target: OrgTarget::Position(dev.clone()),
            direction: OrgDirection::Up,
            seat_scope: OrgSeatScope::Primary,
            at: Some(day(6)),
        },
    )
    .await
    .unwrap()
    {
        P::ReportsChainResponse { links } => {
            assert_eq!(
                links
                    .iter()
                    .map(|l| l.position_id.as_str())
                    .collect::<Vec<_>>(),
                vec![ceo.as_str()]
            );
        }
        other => panic!("{other:?}"),
    }
    match run(
        &admin,
        P::SubordinatesRequest {
            target: OrgTarget::Position(ceo.clone()),
            transitive: true,
            seat_scope: OrgSeatScope::Primary,
            at: Some(day(6)),
        },
    )
    .await
    .unwrap()
    {
        P::SubordinatesResponse { links } => assert!(links.iter().any(|l| l.position_id == dev)),
        other => panic!("{other:?}"),
    }
    match run(
        &admin,
        P::AssignmentRequest {
            user_id: w.admin.clone(),
            at: Some(day(6)),
        },
    )
    .await
    .unwrap()
    {
        P::AssignmentResponse { primary, others } => {
            assert!(primary.is_some());
            assert_eq!(others.len(), 1);
        }
        other => panic!("{other:?}"),
    }
    match run(
        &admin,
        P::ManagerRequest {
            user_id: w.admin.clone(),
            at: Some(day(6)),
        },
    )
    .await
    .unwrap()
    {
        P::ManagerResponse { manager } => {
            assert!(manager.is_none(), "the top of the line has no manager")
        }
        other => panic!("{other:?}"),
    }
    match run(&admin, P::IntegrityReportRequest { at: None })
        .await
        .unwrap()
    {
        P::IntegrityReportResponse { violations } => {
            assert!(violations.is_empty(), "{violations:?}")
        }
        other => panic!("{other:?}"),
    }

    // A rule refusal is an answer with a typed error the screen can key on.
    assert_eq!(
        write_refused(
            &admin,
            P::UnitEndRequest {
                unit_id: it.clone(),
                from: day(8),
                confirm_backdated: false
            }
        )
        .await,
        "unit_not_empty"
    );
    assert_eq!(
        write_refused(&admin, unit_create("Wczoraj", None, day(-3), false)).await,
        "backdated_confirmation_required"
    );
    write_ok(&admin, unit_create("Wczoraj", None, day(-3), true)).await;
    assert_eq!(
        write_refused(
            &admin,
            unit_create("Zła data", None, "1.10.2026".into(), false)
        )
        .await,
        "invalid_date"
    );

    // Ending things and the closing writes.
    let (ended, _) = write_ok(
        &admin,
        P::PositionEndRequest {
            position_id: dev.clone(),
            from: day(9),
            confirm_backdated: false,
        },
    )
    .await;
    match ended {
        OrgWriteResult::Ended(e) => {
            assert!(!e.assignments.is_empty() || !e.reporting_lines.is_empty())
        }
        other => panic!("{other:?}"),
    }
    write_ok(
        &admin,
        P::AssignmentEndRequest {
            assignment_id: ceo_assignment.id,
            from: day(9),
            confirm_backdated: false,
        },
    )
    .await;
    assert_eq!(
        write_refused(&admin, P::UnitTypeDeleteRequest { id: type_id }).await,
        "unit_type_in_use"
    );
    let (spare, _) = write_ok(
        &admin,
        P::UnitTypeCreateRequest {
            name: "Zbędny".into(),
            color: None,
            icon: None,
        },
    )
    .await;
    let spare = match spare {
        OrgWriteResult::UnitType(t) => t.id,
        other => panic!("{other:?}"),
    };
    let (deleted, _) = write_ok(&admin, P::UnitTypeDeleteRequest { id: spare }).await;
    assert_eq!(deleted, OrgWriteResult::Done);

    match run(&admin, P::RecomputeRequest {}).await.unwrap() {
        P::RecomputeResponse {
            written, removed, ..
        } => {
            // Every write already projected inside its own transaction, so a
            // recompute right after finds nothing to change.
            assert_eq!((written, removed), (0, 0));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_plain_member_may_read_and_is_refused_every_write() {
    let w = world();
    let member = member_ctx(&w);

    for read in every_read() {
        let label = format!("{read:?}");
        match run(&member, read).await {
            Ok(_) => {}
            // The ids are dummies: an unknown one is NotFound, never a permission problem.
            Err(e) => assert_eq!(e.code, ProtocolErrorCode::NotFound, "{label}: {e:?}"),
        }
    }
    for write in every_write() {
        let label = format!("{write:?}");
        expect_code(
            run(&member, write).await,
            ProtocolErrorCode::PolicyDenied,
            &label,
        );
    }

    let rows: i64 = w
        .state
        .db
        .read()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM org_units", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0, "a refused write changed nothing");
}

#[tokio::test]
async fn someone_outside_the_organization_gets_not_found_for_reads_and_writes() {
    let w = world();
    let outsider = ctx(&w.state, DEFAULT_ORG_ID, &w.outsider, &["org.admin"]);

    for read in every_read() {
        let label = format!("{read:?}");
        expect_code(
            run(&outsider, read).await,
            ProtocolErrorCode::NotFound,
            &label,
        );
    }
    // Even holding org.admin: the organization is not theirs, so it does not exist for them.
    for write in every_write() {
        let label = format!("{write:?}");
        expect_code(
            run(&outsider, write).await,
            ProtocolErrorCode::NotFound,
            &label,
        );
    }
    let mut no_context = admin_ctx(&w);
    no_context.org_context = None;
    expect_code(
        run(&no_context, P::StructureRequest { at: None }).await,
        ProtocolErrorCode::NotFound,
        "no organization in the session",
    );
}

#[tokio::test]
async fn another_organizations_ids_are_neither_readable_nor_confirmed() {
    let w = world();
    let admin = admin_ctx(&w);
    let (unit, _) = write_ok(&admin, unit_create("Tajna", None, day(2), false)).await;
    let (secret, _) = write_ok(
        &admin,
        position_create(&unit_id(&unit), "Szef", None, day(2)),
    )
    .await;
    let secret = position_id(&secret);

    let other =
        org::create_organization(&w.state.db, "Other", "other", None, None, None, None).unwrap();
    let other_admin_id = add_user(&w.state, Some(&other.org_id), "other-admin");
    let other_admin = ctx(&w.state, &other.org_id, &other_admin_id, &["org.admin"]);

    match run(&other_admin, P::StructureRequest { at: Some(day(3)) })
        .await
        .unwrap()
    {
        P::StructureResponse { view, .. } => {
            assert!(
                view.units.is_empty() && view.positions.is_empty(),
                "no leakage into the other organization"
            );
        }
        other => panic!("{other:?}"),
    }
    expect_code(
        run(
            &other_admin,
            P::SubordinatesRequest {
                target: OrgTarget::Position(secret.clone()),
                transitive: true,
                seat_scope: OrgSeatScope::Primary,
                at: Some(day(3)),
            },
        )
        .await,
        ProtocolErrorCode::NotFound,
        "reading below a position of another organization",
    );
    // A write that names the other organization's id is refused exactly like a missing one.
    let code = write_refused(
        &other_admin,
        position_create(&unit_id(&unit), "Podrzędny", None, day(2)),
    )
    .await;
    assert_eq!(
        code, "not_found",
        "the answer does not confirm that the id exists elsewhere"
    );
    let code = write_refused(
        &other_admin,
        P::PositionMoveRequest {
            position_id: secret,
            new_parent_position_id: None,
            from: day(3),
            confirm_backdated: false,
        },
    )
    .await;
    assert_eq!(code, "not_found");
}

#[tokio::test]
async fn every_write_leaves_an_audit_row_on_an_intact_chain_and_publishes_an_event() {
    let w = world();
    let admin = admin_ctx(&w);
    let bus = event_bus();

    let (unit, _) = write_ok(&admin, unit_create("Audyt", None, day(2), false)).await;
    let unit = unit_id(&unit);
    let (position, _) = write_ok(&admin, position_create(&unit, "Audytor", None, day(2))).await;
    let position = position_id(&position);
    let (moved, _) = write_ok(
        &admin,
        P::PositionMoveRequest {
            position_id: position.clone(),
            new_parent_position_id: None,
            from: day(3),
            confirm_backdated: false,
        },
    )
    .await;
    assert!(matches!(moved, OrgWriteResult::ReportingLine(None)));
    write_refused(
        &admin,
        P::UnitEndRequest {
            unit_id: unit.clone(),
            from: day(4),
            confirm_backdated: false,
        },
    )
    .await;

    let conn = w.state.db.read().unwrap();
    let actions: Vec<String> = conn
        .prepare("SELECT action FROM audit_log WHERE action LIKE 'org.%' ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        actions
            .iter()
            .filter(|a| a.as_str() == "org.unit.create")
            .count()
            + actions
                .iter()
                .filter(|a| a.as_str() == "org.position.create")
                .count(),
        2,
        "one audit row per successful write, none for the refused one: {actions:?}"
    );
    assert!(
        !actions.iter().any(|a| a == "org.unit.end"),
        "a refused write is not audited as done"
    );
    let report = verify_chain(&conn).expect("the chain verifies");
    assert!(report.is_clean(), "tampered: {:?}", report.tampered);
    assert!(report.chained_ok >= actions.len());

    let events = bus.recent_events(4096);
    let mine = |name: &str, field: &str, id: &str| {
        events.iter().any(|e| {
            e.event_type == name
                && e.payload.get(field).and_then(|v| v.as_str()) == Some(id)
                && e.payload.get("org_id").and_then(|v| v.as_str()) == Some(DEFAULT_ORG_ID)
                && e.source_user.as_deref() == Some(w.admin.as_str())
        })
    };
    assert!(
        mine("org.unit_created", "unit_id", &unit),
        "unit_created was published"
    );
    assert!(
        mine("org.position_created", "position_id", &position),
        "position_created was published"
    );
    assert!(
        mine("org.reporting_line_changed", "position_id", &position),
        "reporting_line_changed was published"
    );
    let subtree = events
        .iter()
        .find(|e| {
            e.event_type == "org.reporting_line_changed"
                && e.payload["position_id"] == position.as_str()
        })
        .unwrap();
    assert_eq!(
        subtree.payload["affected_position_ids"],
        serde_json::json!([position]),
        "the affected subtree is named by ids"
    );
    assert!(
        !events
            .iter()
            .any(|e| e.event_type == "org.unit_ended" && e.payload["unit_id"] == unit.as_str()),
        "a refused write publishes nothing"
    );
}

#[tokio::test]
async fn a_reply_variant_sent_as_a_request_is_a_bad_request() {
    let w = world();
    let admin = admin_ctx(&w);
    expect_code(
        run(
            &admin,
            P::RecomputeResponse {
                written: 0,
                removed: 0,
                unchanged: 0,
                cycles_broken: vec![],
            },
        )
        .await,
        ProtocolErrorCode::BadRequest,
        "a reply",
    );
}

#[tokio::test]
async fn a_clear_of_a_field_that_cannot_be_cleared_is_a_typed_refusal() {
    let w = world();
    let admin = admin_ctx(&w);
    let (unit, _) = write_ok(&admin, unit_create("Nazwa", None, day(2), false)).await;
    let code = write_refused(
        &admin,
        P::UnitUpdateRequest {
            unit_id: unit_id(&unit),
            name: None,
            code: None,
            type_id: None,
            color: None,
            clear: vec!["name".into()],
            from: day(3),
            confirm_backdated: false,
        },
    )
    .await;
    assert_eq!(code, "invalid_value");
}

#[tokio::test]
async fn the_seat_scope_decides_whether_a_secondary_seat_is_followed() {
    let w = world();
    let admin = admin_ctx(&w);
    let d = day(2);

    let (unit, _) = write_ok(&admin, unit_create("Zespoly", None, d.clone(), false)).await;
    let unit = unit_id(&unit);
    let (lead_a, _) = write_ok(
        &admin,
        position_create(&unit, "Kierownik A", None, d.clone()),
    )
    .await;
    let lead_a = position_id(&lead_a);
    let (lead_b, _) = write_ok(
        &admin,
        position_create(&unit, "Kierownik B", None, d.clone()),
    )
    .await;
    let lead_b = position_id(&lead_b);
    let (analyst_a, _) = write_ok(
        &admin,
        position_create(&unit, "Analityk A", Some(&lead_a), d.clone()),
    )
    .await;
    let analyst_a = position_id(&analyst_a);
    let (analyst_b, _) = write_ok(
        &admin,
        position_create(&unit, "Analityk B", Some(&lead_b), d.clone()),
    )
    .await;
    let analyst_b = position_id(&analyst_b);
    // One person leads two teams: the first seat is primary, the second is not.
    let person = OrgSubject::User(w.member.clone());
    write_ok(&admin, assign(&lead_a, person.clone(), 0.5, d.clone())).await;
    write_ok(&admin, assign(&lead_b, person, 0.5, d.clone())).await;

    let below = |scope| P::SubordinatesRequest {
        target: OrgTarget::User(w.member.clone()),
        transitive: true,
        seat_scope: scope,
        at: Some(day(3)),
    };
    let positions = |answer: P| match answer {
        P::SubordinatesResponse { links } => {
            let mut ids: Vec<String> = links.into_iter().map(|l| l.position_id).collect();
            ids.sort();
            ids
        }
        other => panic!("{other:?}"),
    };
    let primary = positions(run(&admin, below(OrgSeatScope::Primary)).await.unwrap());
    assert_eq!(
        primary,
        vec![analyst_a.clone()],
        "the permission checks follow the primary seat only"
    );
    let all = positions(run(&admin, below(OrgSeatScope::All)).await.unwrap());
    let mut both = vec![analyst_a, analyst_b];
    both.sort();
    assert_eq!(all, both, "the tree asks for every seat");
    let default_scope = positions(
        run(
            &admin,
            serde_json::from_value(serde_json::json!({
                "SubordinatesRequest": {
                    "target": { "kind": "user", "id": w.member },
                    "transitive": true,
                    "at": day(3),
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        default_scope, primary,
        "a request that names no scope gets the primary one"
    );
}

#[tokio::test]
async fn oversized_names_and_deputy_lists_are_typed_refusals() {
    let w = world();
    let admin = admin_ctx(&w);
    let long = "x".repeat(201);
    let code = write_refused(&admin, unit_create(&long, None, day(2), false)).await;
    assert_eq!(code, "invalid_value");
    let (unit, _) = write_ok(&admin, unit_create("Ok", None, day(2), false)).await;
    let refused = write_refused(
        &admin,
        P::DeputyHeadsSetRequest {
            unit_id: unit_id(&unit),
            position_ids: (0..21).map(|i| format!("p-{i}")).collect(),
            from: day(3),
            confirm_backdated: false,
        },
    )
    .await;
    assert_eq!(refused, "invalid_value");
}

#[tokio::test]
async fn an_admin_dry_runs_then_applies_a_file_and_a_member_can_do_neither() {
    let w = world();
    let admin = admin_ctx(&w);
    let member = member_ctx(&w);
    let bus = event_bus();

    let dry = report_of(&admin, import_dry_run(FILE, vec![])).await;
    assert!(
        dry.file_error.is_none() && dry.errors.is_empty(),
        "{:?}",
        dry.errors
    );
    assert!(!dry.applied);
    assert_eq!(
        (dry.counts.rows, dry.counts.added, dry.counts.units_added),
        (2, 2, 1)
    );
    assert_eq!(dry.preview.as_ref().map(|p| p.positions.len()), Some(2));
    assert_eq!(unit_count(&w), 0, "a dry run writes nothing");

    for payload in [import_dry_run(FILE, vec![]), import_apply(FILE, vec![])] {
        expect_code(
            run(&member, payload).await,
            ProtocolErrorCode::PolicyDenied,
            "a member's import",
        );
    }
    assert_eq!(unit_count(&w), 0);

    let applied = report_of(&admin, import_apply(FILE, vec![])).await;
    assert!(applied.applied, "{:?}", applied.errors);
    assert_eq!(unit_count(&w), 1);

    // One event for the whole file, none of the per-entity ones.
    let events = bus.recent_events(4096);
    let mine: Vec<_> = events
        .iter()
        .filter(|e| e.source_user.as_deref() == Some(w.admin.as_str()))
        .collect();
    let imported: Vec<_> = mine
        .iter()
        .filter(|e| e.event_type == "org.structure_imported")
        .collect();
    assert_eq!(
        imported.len(),
        1,
        "{:?}",
        mine.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );
    assert_eq!(imported[0].payload["counts"]["units_added"], 1);
    assert_eq!(imported[0].payload["counts"]["positions_added"], 2);
    assert_eq!(imported[0].payload["org_id"], DEFAULT_ORG_ID);
    assert!(
        !mine
            .iter()
            .any(|e| e.event_type == "org.unit_created" || e.event_type == "org.position_created"),
        "no event per row"
    );

    // Audit: every operation with the source, one summary, and the chain holds.
    let conn = w.state.db.read().unwrap();
    let imported_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE action LIKE 'org.%' AND details LIKE '%\"source\":\"import\"%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(imported_rows, 1, "one entry for the whole file");
    assert!(verify_chain(&conn).expect("the chain verifies").is_clean());
}

#[tokio::test]
async fn the_answer_to_a_broken_file_is_a_report_with_row_numbers_and_the_decisions_apply() {
    let w = world();
    let admin = admin_ctx(&w);
    let broken = FILE.replace("org-member", "org-membr");

    let report = report_of(&admin, import_apply(&broken, vec![])).await;
    assert!(!report.applied);
    assert_eq!(report.errors.len(), 1);
    assert_eq!(
        (
            report.errors[0].row,
            report.errors[0].kind.as_str(),
            report.errors[0].suggestion.as_deref()
        ),
        (3, "unknown_person", Some("org-member"))
    );
    assert_eq!(unit_count(&w), 0);

    let decisions = vec![OrgImportResolution {
        row: 3,
        action: OrgImportAction::UseSuggestedLogin,
        login: Some("org-member".into()),
    }];
    let applied = report_of(&admin, import_apply(&broken, decisions)).await;
    assert!(applied.applied, "{:?}", applied.errors);
    assert_eq!(unit_count(&w), 1);

    // The error report is a CSV of the same run.
    let (name, mime, bytes) = file_of(
        &admin,
        P::ExportErrorsRequest {
            format: OrgFileFormat::Csv,
            bytes: broken.into_bytes(),
            mode: OrgImportMode::Upsert,
            as_of: None,
            confirm_backdated: false,
            confirm_ended: false,
            resolutions: vec![],
        },
    )
    .await;
    assert!(name.starts_with("org-structure-import-errors-") && name.ends_with(".csv"));
    assert!(mime.starts_with("text/csv"));
    let text = String::from_utf8(bytes).unwrap();
    assert!(
        text.contains("unknown_person") && text.contains("org-membr"),
        "{text}"
    );
}

#[tokio::test]
async fn an_unusable_file_is_a_typed_error_in_the_report_not_a_protocol_error() {
    let w = world();
    let admin = admin_ctx(&w);
    let too_big = P::ImportDryRunRequest {
        format: OrgFileFormat::Csv,
        bytes: vec![b'a'; 4 * 1024 * 1024 + 1],
        mode: OrgImportMode::Upsert,
        as_of: None,
        confirm_backdated: false,
        confirm_ended: false,
        resolutions: vec![],
    };
    for (payload, code) in [
        (too_big, "file_too_large"),
        (
            import_dry_run("nazwa jednostki\nIT\n", vec![]),
            "missing_column",
        ),
        (import_dry_run("", vec![]), "empty_file"),
        (
            P::ImportDryRunRequest {
                format: OrgFileFormat::Xlsx,
                bytes: b"not a workbook".to_vec(),
                mode: OrgImportMode::Upsert,
                as_of: None,
                confirm_backdated: false,
                confirm_ended: false,
                resolutions: vec![],
            },
            "unreadable_file",
        ),
    ] {
        let report = report_of(&admin, payload).await;
        assert_eq!(
            report.file_error.as_ref().map(|e| e.code.as_str()),
            Some(code)
        );
        assert!(!report.applied && report.errors.is_empty() && report.preview.is_none());
    }
    expect_code(
        run(
            &admin,
            P::ImportDryRunRequest {
                format: OrgFileFormat::Csv,
                bytes: FILE.as_bytes().to_vec(),
                mode: OrgImportMode::Upsert,
                as_of: Some("not-a-date".into()),
                confirm_backdated: false,
                confirm_ended: false,
                resolutions: vec![],
            },
        )
        .await,
        ProtocolErrorCode::BadRequest,
        "a malformed day",
    );
}

#[tokio::test]
async fn a_member_exports_the_structure_without_logins_and_emails_and_an_admin_with_them() {
    let w = world();
    let admin = admin_ctx(&w);
    let member = member_ctx(&w);
    assert!(report_of(&admin, import_apply(FILE, vec![])).await.applied);

    for format in [OrgFileFormat::Csv, OrgFileFormat::Xlsx] {
        let request = P::ExportRequest { format, at: None };
        let (name, _, admin_bytes) = file_of(&admin, request.clone()).await;
        let (_, _, member_bytes) = file_of(&member, request).await;
        assert!(name.ends_with(match format {
            OrgFileFormat::Csv => ".csv",
            OrgFileFormat::Xlsx => ".xlsx",
        }));
        if format == OrgFileFormat::Csv {
            let admin_text = String::from_utf8(admin_bytes.clone()).unwrap();
            let member_text = String::from_utf8(member_bytes).unwrap();
            assert!(
                admin_text.contains("org-admin@example.test")
                    && admin_text.contains("login/e-mail osoby")
            );
            // In this fixture a display name equals the login, so what tells the
            // member's file apart is what only an administrator's has: the columns.
            let header = member_text
                .trim_start_matches('\u{feff}')
                .lines()
                .next()
                .unwrap();
            assert!(
                !member_text.contains('@')
                    && !header.contains("login")
                    && !header.split(';').any(|h| h == "e-mail"),
                "{member_text}"
            );
            assert!(
                member_text.contains("Dyrektor") && member_text.contains("IT-1"),
                "the structure itself is theirs to see"
            );
            // An admin's export imports as no change.
            let report = report_of(
                &admin,
                P::ImportDryRunRequest {
                    format,
                    bytes: admin_bytes,
                    mode: OrgImportMode::Upsert,
                    as_of: None,
                    confirm_backdated: false,
                    confirm_ended: false,
                    resolutions: vec![],
                },
            )
            .await;
            assert!(report.errors.is_empty(), "{:?}", report.errors);
            assert_eq!(report.counts.unchanged, report.counts.rows);
        } else {
            assert!(admin_bytes.starts_with(b"PK"), "an xlsx is a zip");
            assert!(member_bytes.starts_with(b"PK"));
            assert_ne!(admin_bytes, member_bytes);
        }
    }
    expect_code(
        run(
            &member,
            P::ExportRequest {
                format: OrgFileFormat::Csv,
                at: Some("someday".into()),
            },
        )
        .await,
        ProtocolErrorCode::BadRequest,
        "a malformed day",
    );
}

// ---------------------------------------------------------------------------
// Batch of edits
// ---------------------------------------------------------------------------

use tentaflow_protocol::org_structure::{OrgBatchOpResult, OrgWriteOp};

fn op(request: P) -> OrgWriteOp {
    OrgWriteOp {
        temp_id: None,
        request,
    }
}

fn op_tmp(temp_id: &str, request: P) -> OrgWriteOp {
    OrgWriteOp {
        temp_id: Some(temp_id.to_string()),
        request,
    }
}

fn batch(ops: Vec<OrgWriteOp>, dry_run: bool) -> P {
    P::BatchRequest {
        ops,
        dry_run,
        confirm_backdated: false,
    }
}

struct BatchAnswer {
    ok: bool,
    applied: bool,
    error: Option<String>,
    results: Vec<OrgBatchOpResult>,
    warnings: Vec<OrgWarning>,
    preview: Option<tentaflow_protocol::org_structure::OrgStructureView>,
    max_ops: u32,
}

async fn run_batch(ctx: &HandlerContext, request: P) -> BatchAnswer {
    match run(ctx, request).await.expect("a batch is answered") {
        P::BatchResponse {
            ok,
            applied,
            error,
            results,
            warnings,
            preview,
            max_ops,
            ..
        } => BatchAnswer {
            ok,
            applied,
            error: error.map(|e| e.code),
            results,
            warnings,
            preview,
            max_ops,
        },
        other => panic!("expected a BatchResponse, got {other:?}"),
    }
}

fn codes(answer: &BatchAnswer) -> Vec<Option<String>> {
    answer
        .results
        .iter()
        .map(|r| r.error.as_ref().map(|e| e.code.clone()))
        .collect()
}

fn audit_actions(w: &World) -> Vec<String> {
    let conn = w.state.db.read().unwrap();
    let mut stmt = conn
        .prepare("SELECT action FROM audit_log WHERE action LIKE 'org.%' ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

async fn structure(ctx: &HandlerContext) -> tentaflow_protocol::org_structure::OrgStructureView {
    match run(ctx, P::StructureRequest { at: Some(day(3)) })
        .await
        .unwrap()
    {
        P::StructureResponse { view, .. } => view,
        other => panic!("expected the structure, got {other:?}"),
    }
}

/// A draft that needs temporary ids to be expressed at all: a type, a unit of
/// that type, a child unit, two positions where the second reports to the
/// first, a head, and two people (one an account, one made in the same batch).
fn draft(member: &str) -> Vec<OrgWriteOp> {
    vec![
        op_tmp(
            "tmp:type",
            P::UnitTypeCreateRequest {
                name: "Dział".into(),
                color: None,
                icon: None,
            },
        ),
        op_tmp(
            "tmp:board",
            P::UnitCreateRequest {
                name: "Zarząd".into(),
                code: None,
                type_id: Some("tmp:type".into()),
                parent_unit_id: None,
                color: None,
                valid_from: day(1),
                valid_to: None,
                confirm_backdated: false,
            },
        ),
        op_tmp(
            "tmp:it",
            unit_create("IT", Some("tmp:board"), day(1), false),
        ),
        op_tmp(
            "tmp:ceo",
            position_create("tmp:board", "Prezes", None, day(1)),
        ),
        op_tmp(
            "tmp:cto",
            position_create("tmp:it", "CTO", Some("tmp:ceo"), day(1)),
        ),
        op(P::HeadSetRequest {
            unit_id: "tmp:board".into(),
            head_position_id: Some("tmp:ceo".into()),
            from: day(1),
            confirm_backdated: false,
        }),
        op(P::HeadSetRequest {
            unit_id: "tmp:it".into(),
            head_position_id: Some("tmp:cto".into()),
            from: day(1),
            confirm_backdated: false,
        }),
        op_tmp(
            "tmp:a1",
            assign("tmp:ceo", OrgSubject::User(member.to_string()), 1.0, day(1)),
        ),
        op_tmp(
            "tmp:ext",
            P::ExternalPersonCreateRequest {
                display_name: "Jan Zewnętrzny".into(),
                email: None,
                note: None,
            },
        ),
        op(assign(
            "tmp:cto",
            OrgSubject::External("tmp:ext".into()),
            1.0,
            day(1),
        )),
    ]
}

#[tokio::test]
async fn a_draft_is_saved_in_one_call_and_its_temporary_ids_become_real_ones() {
    let w = world();
    let admin = admin_ctx(&w);
    let bus = event_bus();
    let audit_before = audit_actions(&w).len();

    let answer = run_batch(&admin, batch(draft(&w.member), false)).await;
    assert!(answer.ok && answer.applied, "{:?}", codes(&answer));
    assert!(answer.error.is_none());
    assert_eq!(answer.max_ops, 500);
    assert_eq!(answer.results.len(), 10);
    assert!(answer.results.iter().all(|r| r.ok));
    assert!(
        answer.preview.is_none(),
        "only a dry run carries the preview"
    );

    let made: Vec<(Option<String>, Option<String>)> = answer
        .results
        .iter()
        .map(|r| (r.temp_id.clone(), r.created_id.clone()))
        .collect();
    let real = |temp: &str| -> String {
        made.iter()
            .find(|(t, _)| t.as_deref() == Some(temp))
            .and_then(|(_, id)| id.clone())
            .unwrap_or_else(|| panic!("{temp} was made"))
    };
    for (temp, id) in made
        .iter()
        .filter_map(|(t, i)| Some((t.as_ref()?, i.as_ref()?)))
    {
        assert!(!id.starts_with("tmp:"), "{temp} -> {id}");
    }
    assert!(
        answer.results[5].created_id.is_none(),
        "a head change makes nothing"
    );

    // What the batch made is what the structure now says.
    let view = structure(&admin).await;
    let unit = |id: &str| view.units.iter().find(|u| u.unit_id == id).unwrap().clone();
    let board = unit(&real("tmp:board"));
    let it = unit(&real("tmp:it"));
    assert_eq!(board.type_id.as_deref(), Some(real("tmp:type").as_str()));
    assert_eq!(it.parent_unit_id.as_deref(), Some(board.unit_id.as_str()));
    assert_eq!(
        board.head_position_id.as_deref(),
        Some(real("tmp:ceo").as_str())
    );
    assert_eq!(
        it.head_position_id.as_deref(),
        Some(real("tmp:cto").as_str())
    );
    let cto = view
        .positions
        .iter()
        .find(|p| p.position_id == real("tmp:cto"))
        .unwrap();
    assert_eq!(
        cto.primary_parent_position_id.as_deref(),
        Some(real("tmp:ceo").as_str())
    );
    assert_eq!(view.assignments.len(), 2);
    assert!(view
        .assignments
        .iter()
        .any(|a| a.subject == OrgSubject::External(real("tmp:ext"))));
    assert!(view.vacancies.is_empty());

    // One audit entry for the whole draft, listing what it did.
    let after = audit_actions(&w);
    let new: Vec<&String> = after.iter().skip(audit_before).collect();
    assert_eq!(new, vec!["org.structure.batch"], "{after:?}");
    let conn = w.state.db.read().unwrap();
    let details: String = conn
        .query_row(
            "SELECT details FROM audit_log WHERE action = 'org.structure.batch'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let details: serde_json::Value = serde_json::from_str(&details).unwrap();
    assert_eq!(details["source"], "batch");
    assert_eq!(details["operations"].as_array().unwrap().len(), 10);
    assert_eq!(details["counts"]["org.unit.create"], 2);
    assert!(verify_chain(&conn).expect("the chain verifies").is_clean());
    drop(conn);

    // One event for the batch, none per operation.
    let events = bus.recent_events(4096);
    let mine = events
        .iter()
        .filter(|e| {
            e.event_type == "org.structure_batch_applied"
                && e.payload["affected"]["unit_ids"]
                    .as_array()
                    .is_some_and(|ids| ids.iter().any(|i| i == real("tmp:board").as_str()))
        })
        .collect::<Vec<_>>();
    assert_eq!(mine.len(), 1);
    let event = mine[0];
    assert_eq!(event.payload["ops"], 10);
    assert_eq!(event.payload["org_id"], DEFAULT_ORG_ID);
    assert!(event.payload["affected"]["position_ids"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i == real("tmp:cto").as_str()));
    assert!(
        !events.iter().any(|e| e.event_type == "org.unit_created"
            && e.payload.get("unit_id").and_then(|v| v.as_str())
                == Some(real("tmp:board").as_str())),
        "a batch does not repeat itself as one event per operation"
    );
}

#[tokio::test]
async fn a_dry_run_reports_and_previews_the_draft_and_changes_nothing() {
    let w = world();
    let admin = admin_ctx(&w);
    let audit_before = audit_actions(&w).len();

    let answer = run_batch(&admin, batch(draft(&w.member), true)).await;
    assert!(answer.ok, "{:?}", codes(&answer));
    assert!(!answer.applied, "a dry run never saves");
    let answer_ids: Vec<String> = answer
        .results
        .iter()
        .filter_map(|r| r.created_id.clone())
        .collect();
    let preview = answer.preview.expect("a dry run returns the preview");
    assert_eq!(preview.units.len(), 2);
    assert_eq!(preview.positions.len(), 2);
    assert_eq!(preview.assignments.len(), 2);
    assert!(answer.warnings.is_empty(), "{:?}", answer.warnings);

    assert!(
        structure(&admin).await.units.is_empty(),
        "nothing was saved"
    );
    assert_eq!(audit_actions(&w).len(), audit_before);
    // Tests of this binary share the bus, so look for the ids of THIS run.
    let dry_ids: Vec<String> = answer_ids;
    assert!(
        !event_bus().recent_events(4096).iter().any(|e| {
            e.event_type == "org.structure_batch_applied"
                && e.payload["affected"]["unit_ids"]
                    .as_array()
                    .is_some_and(|ids| ids.iter().any(|i| dry_ids.iter().any(|d| i == d.as_str())))
        }),
        "no batch event for a dry run"
    );
    let count_of = |w: &World, table: &str| -> i64 {
        w.state
            .db
            .read()
            .unwrap()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    };
    for table in [
        "org_units",
        "org_positions",
        "org_assignments",
        "org_external_persons",
        "org_unit_types",
    ] {
        assert_eq!(count_of(&w, table), 0, "{table}");
    }

    // The same draft then saves and agrees with what the dry run said.
    let saved = run_batch(&admin, batch(draft(&w.member), false)).await;
    assert!(saved.applied);
    let view = structure(&admin).await;
    assert_eq!(
        (
            view.units.len(),
            view.positions.len(),
            view.assignments.len()
        ),
        (2, 2, 2)
    );
}

#[tokio::test]
async fn one_bad_operation_is_named_by_its_index_and_takes_the_whole_batch_with_it() {
    let w = world();
    let admin = admin_ctx(&w);
    let audit_before = audit_actions(&w).len();

    let ops = vec![
        op_tmp("tmp:good", unit_create("Good", None, day(1), false)),
        op_tmp("tmp:bad", unit_create("", None, day(1), false)),
        op(unit_create("Child of bad", Some("tmp:bad"), day(1), false)),
        op(unit_create(
            "Child of good",
            Some("tmp:good"),
            day(1),
            false,
        )),
    ];
    let answer = run_batch(&admin, batch(ops, false)).await;
    assert!(!answer.ok && !answer.applied);
    assert!(answer.error.is_none(), "the batch itself was fine");
    assert_eq!(
        codes(&answer),
        vec![
            None,
            Some("empty_field".to_string()),
            Some("temp_id_not_made".to_string()),
            None
        ]
    );
    assert_eq!(
        answer.results.iter().map(|r| r.index).collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(
        answer.results[1].error.as_ref().unwrap().field.as_deref(),
        Some("name")
    );
    assert!(
        structure(&admin).await.units.is_empty(),
        "nothing of it was saved"
    );
    assert_eq!(
        audit_actions(&w).len(),
        audit_before,
        "and nothing was audited"
    );

    // A rule the repository enforces across operations is enforced in a batch:
    // a second unit under the first that would make a cycle.
    let ops = vec![
        op_tmp("tmp:a", unit_create("A", None, day(1), false)),
        op_tmp("tmp:b", unit_create("B", Some("tmp:a"), day(1), false)),
        op(P::UnitMoveRequest {
            unit_id: "tmp:a".into(),
            new_parent_unit_id: Some("tmp:b".into()),
            from: day(1),
            confirm_backdated: false,
        }),
    ];
    let answer = run_batch(&admin, batch(ops, false)).await;
    assert_eq!(codes(&answer)[2].as_deref(), Some("unit_cycle"));
    assert!(!answer.applied);
    assert!(structure(&admin).await.units.is_empty());
}

#[tokio::test]
async fn temporary_ids_are_checked_where_they_are_defined_and_where_they_are_used() {
    let w = world();
    let admin = admin_ctx(&w);
    let unit = |name: &str| unit_create(name, None, day(1), false);
    let ops = vec![
        op_tmp("tmp:1", unit("One")),
        op_tmp("tmp:1", unit("Again")),
        op_tmp("nope", unit("No prefix")),
        op_tmp("tmp:", unit("Empty name")),
        op_tmp(
            "tmp:end",
            P::UnitEndRequest {
                unit_id: "x".into(),
                from: day(1),
                confirm_backdated: false,
            },
        ),
        op(unit_create(
            "Uses a later id",
            Some("tmp:later"),
            day(1),
            false,
        )),
        op_tmp("tmp:later", unit("Later")),
        op(position_create("tmp:later", "P", None, day(1))),
        op(position_create(
            "tmp:1",
            "Wrong kind",
            Some("tmp:1"),
            day(1),
        )),
        op(unit_create(
            "Real id passes through",
            Some("not-a-temp-id"),
            day(1),
            false,
        )),
    ];
    let answer = run_batch(&admin, batch(ops, true)).await;
    assert_eq!(
        codes(&answer),
        vec![
            None,
            Some("duplicate_temp_id".to_string()),
            Some("invalid_temp_id".to_string()),
            Some("invalid_temp_id".to_string()),
            Some("temp_id_not_allowed".to_string()),
            Some("unknown_temp_id".to_string()),
            None,
            None,
            Some("temp_id_wrong_kind".to_string()),
            Some("not_found".to_string()),
        ]
    );
    assert_eq!(
        answer.results[5].error.as_ref().unwrap().id.as_deref(),
        Some("tmp:later"),
        "an id is not usable before the operation that makes it"
    );
}

#[tokio::test]
async fn only_writes_of_the_structure_are_operations_of_a_batch() {
    let w = world();
    let admin = admin_ctx(&w);
    let ops = vec![
        op(P::StructureRequest { at: None }),
        op(P::TimezoneSetRequest {
            timezone: "UTC".into(),
        }),
        op(batch(vec![], false)),
        op(P::RecomputeRequest {}),
        op(unit_create("Fine", None, day(1), false)),
    ];
    let answer = run_batch(&admin, batch(ops, true)).await;
    assert_eq!(
        codes(&answer),
        vec![
            Some("not_a_batch_op".to_string()),
            Some("not_a_batch_op".to_string()),
            Some("not_a_batch_op".to_string()),
            Some("not_a_batch_op".to_string()),
            None
        ]
    );
}

#[tokio::test]
async fn a_batch_over_the_limit_is_refused_before_anything_runs() {
    let w = world();
    let admin = admin_ctx(&w);
    let unit = || op(unit_create("U", None, day(1), false));
    let at_limit = run_batch(&admin, batch((0..500).map(|_| unit()).collect(), true)).await;
    assert!(
        at_limit.ok && at_limit.error.is_none(),
        "{:?}",
        at_limit.error
    );
    assert_eq!(at_limit.results.len(), 500);

    let over = run_batch(&admin, batch((0..501).map(|_| unit()).collect(), false)).await;
    assert!(!over.ok && !over.applied);
    assert_eq!(over.error.as_deref(), Some("too_many_ops"));
    assert!(over.results.is_empty());
    assert_eq!(over.max_ops, 500);
    assert!(structure(&admin).await.units.is_empty());
}

#[tokio::test]
async fn rewriting_history_in_a_batch_needs_the_confirmation() {
    let w = world();
    let admin = admin_ctx(&w);
    let backdated = || vec![op(unit_create("Old news", None, day(-5), false))];
    let refused = run_batch(&admin, batch(backdated(), false)).await;
    assert_eq!(
        codes(&refused),
        vec![Some("backdated_confirmation_required".to_string())]
    );
    assert!(!refused.applied);

    let confirmed = run_batch(
        &admin,
        P::BatchRequest {
            ops: backdated(),
            dry_run: false,
            confirm_backdated: true,
        },
    )
    .await;
    assert!(confirmed.applied, "{:?}", codes(&confirmed));

    // An operation may carry its own confirmation.
    let own = run_batch(
        &admin,
        batch(vec![op(unit_create("Own", None, day(-2), true))], false),
    )
    .await;
    assert!(own.applied);
}

#[tokio::test]
async fn warnings_are_the_ones_that_still_hold_after_the_whole_draft() {
    let w = world();
    let admin = admin_ctx(&w);
    let answer = run_batch(
        &admin,
        batch(
            vec![
                op_tmp("tmp:u", unit_create("Headless", None, day(1), false)),
                op_tmp("tmp:p", position_create("tmp:u", "Boss", None, day(1))),
                op_tmp("tmp:v", unit_create("Still headless", None, day(1), false)),
                op(P::HeadSetRequest {
                    unit_id: "tmp:u".into(),
                    head_position_id: Some("tmp:p".into()),
                    from: day(1),
                    confirm_backdated: false,
                }),
            ],
            true,
        ),
    )
    .await;
    assert!(answer.ok, "{:?}", codes(&answer));
    let headless: Vec<&String> = answer
        .warnings
        .iter()
        .filter_map(|w| match w {
            OrgWarning::UnitWithoutHead { unit_id, .. } => Some(unit_id),
            _ => None,
        })
        .collect();
    assert_eq!(headless.len(), 1, "{:?}", answer.warnings);
    assert_eq!(
        Some(headless[0]),
        answer.results[2].created_id.as_ref(),
        "the unit given a head later is no longer listed"
    );
}

#[tokio::test]
async fn a_member_may_not_send_a_batch_and_a_stranger_does_not_learn_the_organization_exists() {
    let w = world();
    let request = || batch(vec![op(unit_create("X", None, day(1), false))], false);
    for dry_run in [false, true] {
        let request = P::BatchRequest {
            ops: vec![op(unit_create("X", None, day(1), false))],
            dry_run,
            confirm_backdated: false,
        };
        expect_code(
            run(&member_ctx(&w), request.clone()).await,
            ProtocolErrorCode::PolicyDenied,
            "a member",
        );
    }
    expect_code(
        run(
            &ctx(&w.state, DEFAULT_ORG_ID, &w.outsider, &["org.admin"]),
            request(),
        )
        .await,
        ProtocolErrorCode::NotFound,
        "someone who is not in the organization",
    );
    assert!(structure(&admin_ctx(&w)).await.units.is_empty());
}

// ---------------------------------------------------------------------------
// Deputies, absences, escalation and visibility (WP9)
// ---------------------------------------------------------------------------

/// Board { Boss (admin) }, Team { Worker (member) -> Boss, Peer -> Boss }.
struct Team {
    boss: String,
    worker: String,
    peer: String,
}

async fn team(w: &World) -> Team {
    let admin = admin_ctx(w);
    let peer = add_user(&w.state, Some(DEFAULT_ORG_ID), "org-peer");
    let (unit, _) = write_ok(&admin, unit_create("Zespół", None, day(1), false)).await;
    let unit = unit_id(&unit);
    let (boss, _) = write_ok(&admin, position_create(&unit, "Szef", None, day(1))).await;
    let boss = position_id(&boss);
    let (worker, _) = write_ok(
        &admin,
        position_create(&unit, "Analityk", Some(&boss), day(1)),
    )
    .await;
    let worker = position_id(&worker);
    let (peer_position, _) = write_ok(
        &admin,
        position_create(&unit, "Kolega", Some(&boss), day(1)),
    )
    .await;
    let peer_position = position_id(&peer_position);
    for (position, user) in [
        (&boss, &w.admin),
        (&worker, &w.member),
        (&peer_position, &peer),
    ] {
        write_ok(
            &admin,
            assign(position, OrgSubject::User(user.clone()), 1.0, day(1)),
        )
        .await;
    }
    // The helper hands every account the first role there is, which may carry org.admin; the visibility
    // answers ask the permission matrix about people other than the caller, so these two must not.
    w.state
        .db
        .write()
        .unwrap()
        .execute(
            "UPDATE org_memberships SET role_id = \
             (SELECT role_id FROM roles WHERE role_id <> 'role-org-admin' LIMIT 1) \
             WHERE org_id = ?1 AND user_id IN (?2, ?3)",
            [DEFAULT_ORG_ID, &w.member, &peer],
        )
        .unwrap();
    Team {
        boss: w.admin.clone(),
        worker: w.member.clone(),
        peer,
    }
}

fn peer_ctx(w: &World, t: &Team) -> HandlerContext {
    ctx(&w.state, DEFAULT_ORG_ID, &t.peer, &[])
}

fn absence_add(user: Option<&str>, from: String, to: Option<String>, reason: Option<&str>) -> P {
    P::AbsenceAddRequest {
        user_id: user.map(str::to_string),
        valid_from: from,
        valid_to: to,
        kind: tentaflow_protocol::org_structure_cover::OrgAbsenceKind::Leave,
        reason: reason.map(str::to_string),
        confirm_backdated: false,
    }
}

fn cover_request(user: Option<&str>, at: String) -> P {
    P::CoverRequest {
        user_id: user.map(str::to_string),
        at: Some(at),
        include_past: false,
    }
}

async fn cover_of(ctx: &HandlerContext, payload: P) -> P {
    run(ctx, payload).await.expect("a cover read is answered")
}

#[tokio::test]
async fn a_person_writes_only_their_own_absences_and_others_learn_nothing_of_their_dates() {
    let w = world();
    let t = team(&w).await;
    let member = member_ctx(&w);

    let (added, _) = write_ok(&member, absence_add(None, day(1), Some(day(4)), None)).await;
    let absence = match added {
        OrgWriteResult::Absence(a) => a,
        other => panic!("expected an absence, got {other:?}"),
    };
    assert_eq!(absence.user_id, t.worker);
    assert_eq!(absence.reason, None, "an absence has no reason");
    assert_eq!(absence.source, "manual");

    // The person and an administrator: the dates. A peer: nothing about another day.
    for ctx in [&member, &admin_ctx(&w)] {
        match cover_of(ctx, cover_request(Some(&t.worker), day(2))).await {
            P::CoverResponse {
                available,
                absences,
                can_see_absences,
                can_see_reason,
                ..
            } => {
                assert!(!available, "away on day(2)");
                assert!(can_see_absences);
                assert!(!can_see_reason, "deprecated: there is no reason to see");
                assert_eq!(absences.len(), 1);
                assert_eq!(absences[0].reason, None);
            }
            other => panic!("{other:?}"),
        }
    }
    match cover_of(&peer_ctx(&w, &t), cover_request(Some(&t.worker), day(2))).await {
        P::CoverResponse {
            available,
            absences,
            can_see_absences,
            can_see_reason,
            can_edit_absences,
            ..
        } => {
            assert!(
                available,
                "the peer is not told that the person is away on another day"
            );
            assert!(absences.is_empty() && !can_see_absences && !can_see_reason);
            assert!(!can_edit_absences);
        }
        other => panic!("{other:?}"),
    }

    // Nobody else's absence: not to add, not to change, not to delete.
    let peer = peer_ctx(&w, &t);
    expect_code(
        run(
            &peer,
            absence_add(Some(&t.worker), day(6), Some(day(7)), None),
        )
        .await,
        ProtocolErrorCode::PolicyDenied,
        "an absence for another person",
    );
    expect_code(
        run(
            &peer,
            P::AbsenceDeleteRequest {
                id: absence.id.clone(),
                confirm_backdated: false,
            },
        )
        .await,
        ProtocolErrorCode::PolicyDenied,
        "delete another person's absence",
    );
    expect_code(
        run(
            &peer,
            P::AbsenceUpdateRequest {
                id: absence.id.clone(),
                valid_from: None,
                valid_to: None,
                kind: None,
                reason: None,
                clear: vec![],
                confirm_backdated: false,
            },
        )
        .await,
        ProtocolErrorCode::PolicyDenied,
        "change another person's absence",
    );

    // The person changes and deletes their own; an administrator may write anybody's.
    write_ok(
        &member,
        P::AbsenceUpdateRequest {
            id: absence.id.clone(),
            valid_from: None,
            valid_to: Some(day(3)),
            kind: None,
            reason: None,
            clear: vec![],
            confirm_backdated: false,
        },
    )
    .await;
    write_ok(
        &admin_ctx(&w),
        absence_add(Some(&t.peer), day(8), Some(day(9)), None),
    )
    .await;
    write_ok(
        &member,
        P::AbsenceDeleteRequest {
            id: absence.id,
            confirm_backdated: false,
        },
    )
    .await;
    match cover_of(&member, cover_request(None, day(2))).await {
        P::CoverResponse {
            available,
            absences,
            ..
        } => assert!(available && absences.is_empty()),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn an_absence_has_no_reason_a_client_that_sends_one_is_told_and_nothing_is_stored_or_published(
) {
    let w = world();
    team(&w).await;
    let member = member_ctx(&w);
    assert_eq!(
        write_refused(
            &member,
            absence_add(None, day(20), Some(day(21)), Some("secret-reason-xyz")),
        )
        .await,
        "invalid_value"
    );
    // A blank reason is the same as none; and an old client clearing a reason is told it is gone.
    write_ok(
        &member,
        absence_add(None, day(20), Some(day(21)), Some("  ")),
    )
    .await;
    let events = event_bus().recent_events(4096);
    assert!(
        events
            .iter()
            .all(|e| !e.payload.to_string().contains("secret-reason-xyz")),
        "no event carries the reason"
    );
    let stored: i64 = w
        .state
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM org_absences WHERE user_id = ?1",
            [&w.member],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, 1, "only the request without a reason was stored");
}

#[tokio::test]
async fn only_an_administrator_enters_an_absence_or_a_deputy_dated_before_today() {
    let w = world();
    let t = team(&w).await;
    let member = member_ctx(&w);
    let absence = |from: i64, confirm: bool| P::AbsenceAddRequest {
        user_id: None,
        valid_from: day(from),
        valid_to: Some(day(from + 2)),
        kind: tentaflow_protocol::org_structure_cover::OrgAbsenceKind::Leave,
        reason: None,
        confirm_backdated: confirm,
    };
    for confirm in [false, true] {
        assert_eq!(
            write_refused(&member, absence(-4, confirm)).await,
            "backdating_admin_only",
            "confirm_backdated={confirm}"
        );
    }
    write_ok(&member, absence(0, false)).await;
    let deputy = |from: i64, confirm: bool| P::DeputySetRequest {
        user_id: t.worker.clone(),
        deputy_user_id: t.peer.clone(),
        scope: "all".into(),
        valid_from: day(from),
        valid_to: None,
        confirm_backdated: confirm,
    };
    for confirm in [false, true] {
        assert_eq!(
            write_refused(&member, deputy(-4, confirm)).await,
            "backdating_admin_only",
            "confirm_backdated={confirm}"
        );
    }
    write_ok(&member, deputy(0, false)).await;
}

#[tokio::test]
async fn deputies_are_the_administrators_and_they_change_who_the_chain_asks() {
    let w = world();
    let t = team(&w).await;
    let admin = admin_ctx(&w);
    let member = member_ctx(&w);

    write_ok(
        &admin,
        absence_add(Some(&t.boss), day(1), Some(day(10)), None),
    )
    .await;
    let deputy = |scope: &str| P::DeputySetRequest {
        user_id: t.boss.clone(),
        deputy_user_id: t.peer.clone(),
        scope: scope.into(),
        valid_from: day(1),
        valid_to: Some(day(10)),
        confirm_backdated: false,
    };
    expect_code(
        run(&member, deputy("escalations")).await,
        ProtocolErrorCode::PolicyDenied,
        "a member sets a deputy",
    );
    assert_eq!(
        write_refused(
            &admin,
            P::DeputySetRequest {
                user_id: t.boss.clone(),
                deputy_user_id: t.boss.clone(),
                scope: "all".into(),
                valid_from: day(1),
                valid_to: None,
                confirm_backdated: false,
            }
        )
        .await,
        "invalid_value"
    );
    write_ok(&admin, deputy("escalations")).await;
    assert_eq!(
        write_refused(&admin, deputy("escalations")).await,
        "duplicate"
    );

    let chain = |scope: Option<&str>| P::EscalationChainRequest {
        user_id: t.worker.clone(),
        scope: scope.map(str::to_string),
        at: Some(day(2)),
    };
    match run(&admin, chain(None)).await.unwrap() {
        P::EscalationChainResponse {
            steps,
            skipped,
            problem,
        } => {
            assert_eq!(steps.len(), 1, "{steps:?}");
            assert_eq!(steps[0].user_id, t.peer);
            assert_eq!(steps[0].via, "deputy");
            assert_eq!(steps[0].covering_user_id.as_deref(), Some(t.boss.as_str()));
            assert!(skipped.is_empty() && problem.is_none());
        }
        other => panic!("{other:?}"),
    }
    // An escalations deputy does not stand in for approvals.
    match run(&admin, chain(Some("approvals"))).await.unwrap() {
        P::EscalationChainResponse { steps, skipped, .. } => {
            assert!(steps.is_empty());
            assert_eq!(skipped[0].reason, "unavailable");
        }
        other => panic!("{other:?}"),
    }
    match run(&admin, chain(Some("nonsense"))).await {
        Err(e) => assert_eq!(e.code, ProtocolErrorCode::BadRequest, "{e:?}"),
        Ok(answer) => panic!("a bad scope was answered: {answer:?}"),
    }

    // The manager is the boss until a deputy for everything exists.
    let manager = |at: String| P::ManagerRequest {
        user_id: t.worker.clone(),
        at: Some(at),
    };
    match run(&admin, manager(day(2))).await.unwrap() {
        P::ManagerResponse { manager: Some(m) } => {
            assert_eq!(
                (m.user_id.as_str(), m.source.as_str()),
                (t.boss.as_str(), "primary_holder")
            );
        }
        other => panic!("{other:?}"),
    }
    write_ok(&admin, deputy("all")).await;
    match run(&admin, manager(day(2))).await.unwrap() {
        P::ManagerResponse { manager: Some(m) } => {
            assert_eq!(
                (m.user_id.as_str(), m.source.as_str()),
                (t.peer.as_str(), "deputy")
            );
        }
        other => panic!("{other:?}"),
    }
    match run(&admin, P::AvailabilityRequest { at: Some(day(2)) })
        .await
        .unwrap()
    {
        P::AvailabilityResponse {
            absent_user_ids,
            deputies,
            ..
        } => {
            assert_eq!(absent_user_ids, vec![t.boss.clone()]);
            assert_eq!(deputies.len(), 2);
            assert_eq!(deputies[0].deputy_name, "org-peer");
        }
        other => panic!("{other:?}"),
    }
    match run(
        &admin,
        P::IsAvailableRequest {
            user_id: t.boss.clone(),
            at: Some(day(2)),
        },
    )
    .await
    .unwrap()
    {
        P::IsAvailableResponse { available } => assert!(!available),
        other => panic!("{other:?}"),
    }

    // Ending a deputy from a later day keeps the days before it.
    let listed = match cover_of(&admin, cover_request(Some(&t.boss), day(2))).await {
        P::CoverResponse {
            covered_by,
            can_edit_deputies,
            ..
        } => {
            assert!(can_edit_deputies);
            covered_by
        }
        other => panic!("{other:?}"),
    };
    let first = &listed[0];
    write_ok(
        &admin,
        P::DeputyEndRequest {
            id: first.id.clone(),
            from: day(5),
            confirm_backdated: false,
        },
    )
    .await;
    match run(&admin, P::AvailabilityRequest { at: Some(day(6)) })
        .await
        .unwrap()
    {
        P::AvailabilityResponse { deputies, .. } => assert_eq!(deputies.len(), 1),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn visibility_answers_are_the_callers_own_unless_an_administrator_asks() {
    let w = world();
    let t = team(&w).await;
    let member = member_ctx(&w);
    let admin = admin_ctx(&w);

    match run(
        &member,
        P::VisibilityRequest {
            user_id: None,
            at: Some(day(2)),
        },
    )
    .await
    .unwrap()
    {
        P::VisibilityResponse {
            user,
            manager,
            subtree,
            rows,
            ..
        } => {
            assert_eq!(user.user_id, t.worker);
            assert_eq!(manager.unwrap().user_id, t.boss);
            assert!(subtree.is_empty());
            let dates = rows.iter().find(|r| r.area == "absence_dates").unwrap();
            assert_eq!(
                (dates.verdict.as_str(), dates.rule.as_str()),
                ("own", "owner")
            );
            assert!(
                !rows.iter().any(|r| r.area == "absence_reasons"),
                "an absence has no reason to be seen"
            );
            let structure = rows.iter().find(|r| r.area == "structure").unwrap();
            assert_eq!(structure.rule, "every_member");
        }
        other => panic!("{other:?}"),
    }
    expect_code(
        run(
            &member,
            P::VisibilityRequest {
                user_id: Some(t.boss.clone()),
                at: None,
            },
        )
        .await,
        ProtocolErrorCode::PolicyDenied,
        "a member inspects somebody else",
    );
    expect_code(
        run(
            &member,
            P::WhoSeesRequest {
                subject_user_id: Some(t.boss.clone()),
                at: None,
            },
        )
        .await,
        ProtocolErrorCode::PolicyDenied,
        "a member asks who sees somebody else",
    );
    match run(
        &admin,
        P::VisibilityRequest {
            user_id: Some(t.boss.clone()),
            at: Some(day(2)),
        },
    )
    .await
    .unwrap()
    {
        P::VisibilityResponse {
            subtree, direct, ..
        } => {
            let ids: HashSet<String> = subtree.into_iter().map(|p| p.user_id).collect();
            assert_eq!(ids, HashSet::from([t.worker.clone(), t.peer.clone()]));
            assert_eq!(direct.len(), 2);
        }
        other => panic!("{other:?}"),
    }
    match run(
        &member,
        P::WhoSeesRequest {
            subject_user_id: None,
            at: Some(day(2)),
        },
    )
    .await
    .unwrap()
    {
        P::WhoSeesResponse { subject, viewers } => {
            assert_eq!(subject.user_id, t.worker);
            let rule_of = |id: &str| {
                viewers
                    .iter()
                    .find(|v| v.user_id == id)
                    .map(|v| v.rule.clone())
            };
            assert_eq!(rule_of(&t.worker).as_deref(), Some("owner"));
            assert_eq!(rule_of(&t.boss).as_deref(), Some("primary_manager"));
            assert_eq!(rule_of(&t.peer), None, "a peer sees nothing of the person");
        }
        other => panic!("{other:?}"),
    }

    let can_view = |viewer: Option<&str>, subject: &str, kind: &str| P::CanViewPersonDataRequest {
        viewer_user_id: viewer.map(str::to_string),
        subject_user_id: subject.to_string(),
        kind: kind.to_string(),
        at: Some(day(2)),
    };
    match run(&member, can_view(None, &t.boss, "absence_dates"))
        .await
        .unwrap()
    {
        P::CanViewPersonDataResponse { allowed, rule } => assert!(!allowed && rule == "none"),
        other => panic!("{other:?}"),
    }
    expect_code(
        run(&member, can_view(None, &t.boss, "absence_reason")).await,
        ProtocolErrorCode::BadRequest,
        "the kind of data that no longer exists",
    );
    match run(&admin, can_view(Some(&t.worker), &t.boss, "absence_dates"))
        .await
        .unwrap()
    {
        P::CanViewPersonDataResponse { allowed, rule } => assert!(!allowed, "{rule}"),
        other => panic!("{other:?}"),
    }
    match run(
        &admin,
        can_view(Some(&t.boss), &t.worker, "time_utilization"),
    )
    .await
    .unwrap()
    {
        P::CanViewPersonDataResponse { allowed, rule } => {
            assert!(allowed);
            assert_eq!(rule, "primary_manager");
        }
        other => panic!("{other:?}"),
    }
    expect_code(
        run(
            &member,
            can_view(Some(&t.boss), &t.worker, "time_utilization"),
        )
        .await,
        ProtocolErrorCode::PolicyDenied,
        "a member asks as somebody else",
    );
    expect_code(
        run(&member, can_view(None, &t.worker, "shoe_size")).await,
        ProtocolErrorCode::BadRequest,
        "an unknown kind",
    );
}

#[tokio::test]
async fn a_person_arranges_their_own_deputies_and_nobody_else_but_an_administrator_does() {
    let w = world();
    let t = team(&w).await;
    let worker = member_ctx(&w);
    let manager = ctx(&w.state, DEFAULT_ORG_ID, &t.boss, &[]);
    let admin = admin_ctx(&w);
    let set = |user: &str, deputy: &str| P::DeputySetRequest {
        user_id: user.to_string(),
        deputy_user_id: deputy.to_string(),
        scope: "approvals".into(),
        valid_from: day(2),
        valid_to: Some(day(9)),
        confirm_backdated: false,
    };

    // The covered person: create, change, end.
    let (made, _) = write_ok(&worker, set(&t.worker, &t.peer)).await;
    let own = match made {
        OrgWriteResult::Deputy(d) => d,
        other => panic!("{other:?}"),
    };
    assert_eq!(own.user_id, t.worker);
    write_ok(
        &worker,
        P::DeputyUpdateRequest {
            id: own.id.clone(),
            scope: Some("all".into()),
            valid_from: None,
            valid_to: None,
            clear: vec!["valid_to".into()],
            confirm_backdated: false,
        },
    )
    .await;
    // The deputy must be a member and not the person themselves.
    assert_eq!(
        write_refused(&worker, set(&t.worker, &t.worker)).await,
        "invalid_value"
    );
    assert_eq!(
        write_refused(&worker, set(&t.worker, &w.outsider)).await,
        "not_found"
    );

    // Somebody else's row: the person cannot create, change or end it.
    let (theirs, _) = write_ok(&admin, set(&t.peer, &t.worker)).await;
    let theirs = match theirs {
        OrgWriteResult::Deputy(d) => d,
        other => panic!("{other:?}"),
    };
    expect_code(
        run(&worker, set(&t.peer, &t.boss)).await,
        ProtocolErrorCode::PolicyDenied,
        "create for another",
    );
    expect_code(
        run(
            &worker,
            P::DeputyUpdateRequest {
                id: theirs.id.clone(),
                scope: Some("all".into()),
                valid_from: None,
                valid_to: None,
                clear: vec![],
                confirm_backdated: false,
            },
        )
        .await,
        ProtocolErrorCode::PolicyDenied,
        "change another's row",
    );
    expect_code(
        run(
            &worker,
            P::DeputyEndRequest {
                id: theirs.id.clone(),
                from: day(3),
                confirm_backdated: false,
            },
        )
        .await,
        ProtocolErrorCode::PolicyDenied,
        "end another's row",
    );
    // A manager has no such right over the people below them.
    expect_code(
        run(&manager, set(&t.worker, &t.boss)).await,
        ProtocolErrorCode::PolicyDenied,
        "manager creates",
    );
    expect_code(
        run(
            &manager,
            P::DeputyEndRequest {
                id: own.id.clone(),
                from: day(3),
                confirm_backdated: false,
            },
        )
        .await,
        ProtocolErrorCode::PolicyDenied,
        "manager ends",
    );

    // The covered person ends their own, an administrator changes anyone's.
    write_ok(
        &worker,
        P::DeputyEndRequest {
            id: own.id.clone(),
            from: day(5),
            confirm_backdated: false,
        },
    )
    .await;
    write_ok(
        &admin,
        P::DeputyEndRequest {
            id: theirs.id,
            from: day(4),
            confirm_backdated: false,
        },
    )
    .await;

    // The cover response says who may edit; the member list is open to every member.
    match cover_of(&worker, cover_request(None, day(2))).await {
        P::CoverResponse {
            can_edit_deputies,
            is_admin,
            ..
        } => assert!(can_edit_deputies && !is_admin),
        other => panic!("{other:?}"),
    }
    match cover_of(&manager, cover_request(Some(&t.worker), day(2))).await {
        P::CoverResponse {
            can_edit_deputies, ..
        } => assert!(!can_edit_deputies, "a manager may not"),
        other => panic!("{other:?}"),
    }
    match run(&worker, P::MemberListRequest {}).await.unwrap() {
        P::MemberListResponse { members } => {
            assert!(members.iter().any(|m| m.user_id == t.peer));
            assert!(!members.iter().any(|m| m.user_id == w.outsider));
        }
        other => panic!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Past days and other days: what a member may read
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_past_day_names_who_held_each_position_then_and_is_the_administrators() {
    let w = world();
    let t = team(&w).await;
    let admin = admin_ctx(&w);
    let member = member_ctx(&w);
    let past = Some(day(-3));
    let reads_at = |at: Option<String>| -> Vec<(&'static str, P)> {
        vec![
            (
                "reports chain",
                P::ReportsChainRequest {
                    target: OrgTarget::User(t.worker.clone()),
                    direction: OrgDirection::Up,
                    seat_scope: OrgSeatScope::Primary,
                    at: at.clone(),
                },
            ),
            (
                "subordinates",
                P::SubordinatesRequest {
                    target: OrgTarget::User(t.boss.clone()),
                    transitive: true,
                    seat_scope: OrgSeatScope::Primary,
                    at: at.clone(),
                },
            ),
            (
                "integrity report",
                P::IntegrityReportRequest { at: at.clone() },
            ),
            (
                "manager of somebody else",
                P::ManagerRequest {
                    user_id: t.boss.clone(),
                    at: at.clone(),
                },
            ),
            (
                "assignment of somebody else",
                P::AssignmentRequest {
                    user_id: t.boss.clone(),
                    at: at.clone(),
                },
            ),
        ]
    };
    for (what, request) in reads_at(past.clone()) {
        expect_code(
            run(&member, request.clone()).await,
            ProtocolErrorCode::PolicyDenied,
            &format!("a member reads the {what} of a past day"),
        );
        run(&admin, request)
            .await
            .unwrap_or_else(|e| panic!("an administrator reads the {what} of a past day: {e:?}"));
    }
    // Today and the future are the structure everybody sees.
    for at in [None, Some(day(0)), Some(day(5))] {
        for (what, request) in reads_at(at) {
            run(&member, request)
                .await
                .unwrap_or_else(|e| panic!("a member reads the {what} of today or later: {e:?}"));
        }
    }
    // A person's own line of a past day is theirs to read.
    for request in [
        P::ManagerRequest {
            user_id: t.worker.clone(),
            at: past.clone(),
        },
        P::AssignmentRequest {
            user_id: t.worker.clone(),
            at: past,
        },
    ] {
        run(&member, request).await.expect("own past line");
    }
}

#[tokio::test]
async fn presence_on_another_day_does_not_give_away_the_dates_of_a_person_the_asker_may_not_see() {
    let w = world();
    let t = team(&w).await;
    let admin = admin_ctx(&w);
    let member = member_ctx(&w);
    write_ok(
        &admin,
        absence_add(Some(&t.boss), day(1), Some(day(10)), None),
    )
    .await;
    write_ok(
        &admin,
        P::DeputySetRequest {
            user_id: t.boss.clone(),
            deputy_user_id: t.peer.clone(),
            scope: "approvals".into(),
            valid_from: day(0),
            valid_to: Some(day(10)),
            confirm_backdated: false,
        },
    )
    .await;

    let availability = |ctx: &HandlerContext| {
        let ctx = ctx.clone();
        async move {
            match run(&ctx, P::AvailabilityRequest { at: Some(day(2)) })
                .await
                .unwrap()
            {
                P::AvailabilityResponse {
                    absent_user_ids,
                    deputies,
                    ..
                } => (absent_user_ids, deputies),
                other => panic!("{other:?}"),
            }
        }
    };
    let (absent, deputies) = availability(&admin).await;
    assert_eq!(absent, vec![t.boss.clone()]);
    assert_eq!(deputies[0].valid_to.as_deref(), Some(day(10).as_str()));

    // The worker is below the boss: no absence on day 2, a cover that is in force today and has no dates.
    let (absent, deputies) = availability(&member).await;
    assert!(absent.is_empty(), "{absent:?}");
    assert_eq!(deputies.len(), 1);
    assert_eq!(deputies[0].valid_to, None);

    let is_available = |ctx: &HandlerContext| {
        let request = P::IsAvailableRequest {
            user_id: t.boss.clone(),
            at: Some(day(2)),
        };
        let ctx = ctx.clone();
        async move {
            match run(&ctx, request).await.unwrap() {
                P::IsAvailableResponse { available } => available,
                other => panic!("{other:?}"),
            }
        }
    };
    assert!(!is_available(&admin).await);
    assert!(is_available(&member).await);

    match run(
        &member,
        P::EscalationChainRequest {
            user_id: t.worker.clone(),
            scope: None,
            at: Some(day(2)),
        },
    )
    .await
    .unwrap()
    {
        P::EscalationChainResponse { steps, skipped, .. } => {
            assert_eq!(steps[0].user_id, t.boss);
            assert_eq!(steps[0].via, "holder");
            assert!(skipped.is_empty());
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_past_day_of_the_chain_the_visibility_and_the_view_rules_is_the_administrators() {
    let w = world();
    let t = team(&w).await;
    let member = member_ctx(&w);
    let admin = admin_ctx(&w);
    let past = day(-3);

    let reads = [
        P::EscalationChainRequest {
            user_id: t.boss.clone(),
            scope: None,
            at: Some(past.clone()),
        },
        P::VisibilityRequest {
            user_id: None,
            at: Some(past.clone()),
        },
        P::CanViewPersonDataRequest {
            viewer_user_id: None,
            subject_user_id: t.boss.clone(),
            kind: "absence_dates".into(),
            at: Some(past.clone()),
        },
        P::WhoSeesRequest {
            subject_user_id: None,
            at: Some(past.clone()),
        },
    ];
    for read in reads {
        let label = format!("{read:?}");
        expect_code(
            run(&member, read.clone()).await,
            ProtocolErrorCode::PolicyDenied,
            &label,
        );
        run(&admin, read)
            .await
            .unwrap_or_else(|e| panic!("{label} refused to the administrator: {e:?}"));
    }
    // Today and the future stay open to the member.
    run(
        &member,
        P::VisibilityRequest {
            user_id: None,
            at: Some(day(2)),
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn an_absence_event_on_the_bus_carries_ids_and_no_dates() {
    let w = world();
    let t = team(&w).await;
    let bus = event_bus();
    let (added, _) = write_ok(
        &member_ctx(&w),
        absence_add(None, day(1), Some(day(4)), None),
    )
    .await;
    let id = match added {
        OrgWriteResult::Absence(a) => a.id,
        other => panic!("expected an absence, got {other:?}"),
    };
    write_ok(
        &member_ctx(&w),
        P::AbsenceUpdateRequest {
            id: id.clone(),
            valid_from: None,
            valid_to: Some(day(5)),
            kind: None,
            reason: None,
            clear: vec![],
            confirm_backdated: false,
        },
    )
    .await;
    let events = bus.recent_events(4096);
    for name in ["org.absence_added", "org.absence_updated"] {
        let payload = &events
            .iter()
            .find(|e| e.event_type == name && e.payload["id"] == id.as_str())
            .unwrap_or_else(|| panic!("{name} was not published"))
            .payload;
        assert_eq!(payload["user_id"], t.worker.as_str());
        for private in ["valid_from", "valid_to", "kind", "reason"] {
            assert!(
                payload.get(private).is_none(),
                "{name} leaks {private}: {payload}"
            );
        }
    }
}
