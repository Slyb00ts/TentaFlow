// =============================================================================
// File: dispatch/events_browser.rs
// Purpose: Binary RPCs for Zarzadzanie -> Zdarzenia (§2.10) — the admin browser
//          over the run event log. Two questions, one `EventsBody` family: a
//          page of events across runs (`events::browse`) and the timeline of
//          one run (`events::read_run`).
//
// VISIBILITY is the load-bearing part of this file, so it is stated once here
// and enforced in exactly two places below.
//
//   `events.read_all` -> every run on the node.
//   `events.read`     -> ONLY runs whose `actor_user_id` is the calling
//                        principal. `actor_user_id`, not `actor_id`: a run made
//                        by an API key bound to this user is this user's run,
//                        and `actor_id` would carry the key's uid instead.
//   neither           -> PolicyDenied.
//
// Two permissions rather than a role-name check because migration v133 created
// them for exactly this split (`db/migrations.rs`), and a role string would put
// the policy in a place no grant can change.
//
// The narrowing is pushed into SQL (`EventFilter::restrict_to_actor_user_id`),
// never applied to a result set that already came back. And a caller asking for
// ONE run they do not own gets `not_found`, not `PolicyDenied` — same as
// `agent_run_detail`: a denial would confirm that somebody else's run with that
// id exists, which is the fact being withheld.
//
// Provenance fields are read, never accepted: there is no request field naming
// an origin, an actor kind or an `actor_user_id` for a row, so nothing a client
// sends can relabel what the writer stamped.
// =============================================================================

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    EventRowWire, EventsBrowseRequest, EventsBrowseResponse, EventsCursor, EventsPayload,
    EventsRunRequest, EventsRunResponse, EventsStepIoRequest, EventsStepIoResponse, MessageBody,
    ProtocolError, ProtocolErrorCode, SessionAuth,
};

use super::handlers::user_id_to_uuid;
use super::HandlerContext;
use crate::events::{self, EventCursor, EventFilter, StoredEvent, MAX_READ_LIMIT};
use crate::flow_engine::dispatcher::FlowOrigin;

/// Reading one's own runs.
const PERM_READ: &str = "events.read";
/// Reading every run on the node.
const PERM_READ_ALL: &str = "events.read_all";

/// Page size for a request that names none. Not a clamp — the clamp is
/// `MAX_READ_LIMIT` in the store — just the size the browser gets when the
/// field is absent, which is what a peer predating it sends.
const DEFAULT_PAGE: usize = 100;

/// How much of the log the caller may see.
enum Scope {
    /// `events.read_all`.
    Node,
    /// `events.read`: only runs whose `actor_user_id` is this principal.
    OwnRuns(String),
}

/// Resolves the scope from the permission snapshot AND the session.
///
/// The principal comes from `ctx.session` rather than from `OrgContext.user_id`
/// deliberately: the session is the authenticated fact, the org context is a
/// snapshot derived from it. Taking identity from the authenticated half means
/// no future change to how the org context is assembled can widen who a page
/// belongs to.
///
/// `events.read_all` is accepted on its own: it is strictly the wider
/// capability, and requiring both would make a grant of only the wider one deny
/// everything.
fn resolve_scope(ctx: &HandlerContext) -> Result<Scope, ProtocolError> {
    let org = ctx.org_context.as_ref().ok_or_else(|| {
        ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required")
    })?;
    if org.has(PERM_READ_ALL) {
        return Ok(Scope::Node);
    }
    if !org.has(PERM_READ) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "events.read permission required",
        ));
    }
    match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => Ok(Scope::OwnRuns(user_id_to_uuid(user_id))),
        // Only a user session has a principal to scope to, and `events.read`
        // without `events.read_all` is defined entirely in terms of one.
        _ => Err(ProtocolError::new(
            ProtocolErrorCode::AuthRequired,
            "this operation requires a logged-in user session",
        )),
    }
}

fn db_error(scope: &str, error: anyhow::Error) -> ProtocolError {
    tracing::warn!(scope, error = %error, "events browser database error");
    ProtocolError::internal("events database error")
}

/// Names for the ids a page of rows carries, looked up once per distinct id.
/// A row that names nobody the lookup can find keeps `None`: the screen says the
/// name is unknown rather than printing an id nobody can read.
struct Names<'a> {
    ctx: &'a HandlerContext,
    actors: std::collections::HashMap<(String, String), Option<String>>,
    sessions: std::collections::HashMap<String, Option<String>>,
    flows: std::collections::HashMap<String, Option<String>>,
}

impl<'a> Names<'a> {
    fn new(ctx: &'a HandlerContext) -> Self {
        Self {
            ctx,
            actors: Default::default(),
            sessions: Default::default(),
            flows: Default::default(),
        }
    }

    fn query_one(&self, sql: &str, id: &str) -> Option<String> {
        let conn = self.ctx.state.db.read().ok()?;
        conn.query_row(sql, rusqlite::params![id], |row| row.get::<_, Option<String>>(0))
            .ok()
            .flatten()
            .filter(|name| !name.is_empty())
    }

    fn actor(&mut self, kind: &str, id: Option<&str>) -> Option<String> {
        let id = id?;
        let key = (kind.to_string(), id.to_string());
        if let Some(hit) = self.actors.get(&key) {
            return hit.clone();
        }
        let name = match kind {
            "user" => self.query_one(
                "SELECT COALESCE(NULLIF(display_name, ''), username) FROM user_accounts WHERE id = ?1",
                id,
            ),
            "api_key" => self.query_one("SELECT name FROM api_keys WHERE uid = ?1", id),
            "addon" => self
                .query_one(
                    "SELECT instance_name FROM addon_instances WHERE instance_id = ?1",
                    id,
                )
                // An addon actor is usually the addon's own slug, which is
                // already a name.
                .or_else(|| uuid::Uuid::parse_str(id).is_err().then(|| id.to_string())),
            // System components are named by what they are (`scheduler`, …).
            _ => uuid::Uuid::parse_str(id).is_err().then(|| id.to_string()),
        };
        self.actors.insert(key, name.clone());
        name
    }

    fn session(&mut self, origin: &str, session_id: Option<&str>) -> Option<String> {
        let id = session_id?;
        if origin != "project" {
            return None;
        }
        if let Some(hit) = self.sessions.get(id) {
            return hit.clone();
        }
        let label = crate::project_studio::db::pool().ok().and_then(|pool| {
            let conn = pool.read().ok()?;
            conn.query_row(
                "SELECT p.name, c.title FROM project_chats c \
                 JOIN projects p ON p.project_id = c.project_id WHERE c.session_id = ?1",
                rusqlite::params![id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .ok()
            .map(|(project, title)| match title.filter(|t| !t.is_empty()) {
                Some(title) => format!("{project} · {title}"),
                None => project,
            })
        });
        self.sessions.insert(id.to_string(), label.clone());
        label
    }

    fn flow(&mut self, payload: &crate::events::EventPayload) -> Option<String> {
        let crate::events::EventPayload::RequestStarted {
            flow_id: Some(flow_id),
            ..
        } = payload
        else {
            return None;
        };
        if let Some(hit) = self.flows.get(flow_id) {
            return hit.clone();
        }
        let name = self.query_one("SELECT name FROM flows WHERE id = ?1", flow_id);
        self.flows.insert(flow_id.clone(), name.clone());
        name
    }
}

/// One stored row on the wire. `payload_json` is re-serialised from the decoded
/// payload rather than passed through as a string: the decode already happened
/// (it is how an unreadable row is refused), and re-encoding the typed value
/// guarantees the browser never receives a payload this build could not parse.
/// The content is unchanged — including an `assistant_message` whose body the
/// writer omitted, which stays an omission marker here.
fn to_wire(event: StoredEvent, names: &mut Names) -> Result<EventRowWire, ProtocolError> {
    // A page of rows must not carry every step's prompt: the body of a
    // `step_io` row is fetched on its own (`step_io`), the page keeps the sizes.
    let mut payload = event.payload;
    if let crate::events::EventPayload::StepIo { z, .. } = &mut payload {
        z.clear();
    }
    let payload_json =
        crate::events::store::to_json(&payload).map_err(|e| db_error("payload_encode", e))?;
    let actor_name = names.actor(event.actor_kind.as_str(), event.actor_id.as_deref());
    let session_label = names.session(event.origin.as_str(), event.session_id.as_deref());
    let flow_name = names.flow(&payload);
    let actor_user_name = names.actor("user", event.actor_user_id.as_deref());
    Ok(EventRowWire {
        run_id: event.run_id,
        seq: event.seq,
        at_ms: event.at_ms,
        kind: event.kind.slug().to_string(),
        origin: event.origin,
        actor_kind: event.actor_kind,
        actor_id: event.actor_id,
        actor_user_id: event.actor_user_id,
        org_id: event.org_id,
        correlation_id: event.correlation_id,
        session_id: event.session_id,
        node_id: event.node_id,
        call_id: event.call_id,
        payload_json,
        actor_name,
        session_label,
        flow_name,
        actor_user_name,
    })
}

/// Turns the wire slugs into the closed origin enum. An unknown slug is a bad
/// request, not an empty page: a stale client filtering on an origin this build
/// dropped would otherwise be told the node has no such traffic.
fn parse_origins(raw: &Option<Vec<String>>) -> Result<Option<Vec<FlowOrigin>>, ProtocolError> {
    let Some(slugs) = raw else {
        return Ok(None);
    };
    slugs
        .iter()
        .map(|slug| {
            FlowOrigin::parse(slug)
                .ok_or_else(|| ProtocolError::bad_request(format!("unknown origin '{slug}'")))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn browse(ctx: &HandlerContext, req: &EventsBrowseRequest) -> Result<MessageBody, ProtocolError> {
    let scope = resolve_scope(ctx)?;
    let origins = parse_origins(&req.origins)?;
    let pool = events::db::pool().map_err(|e| db_error("pool", e))?;

    let cursor = req.cursor.as_ref().map(|c| EventCursor {
        at_ms: c.at_ms,
        run_id: c.run_id.clone(),
        seq: c.seq,
    });
    let restrict_to_actor_user_id = match &scope {
        Scope::Node => None,
        Scope::OwnRuns(user_id) => Some(user_id.as_str()),
    };
    let filter = EventFilter {
        origins: origins.as_deref(),
        actor_id: req.actor_id.as_deref().filter(|s| !s.is_empty()),
        org_id: req.org_id.as_deref().filter(|s| !s.is_empty()),
        session_id: req.session_id.as_deref().filter(|s| !s.is_empty()),
        correlation_id: req.correlation_id.as_deref().filter(|s| !s.is_empty()),
        from_ms: req.from_ms,
        to_ms: req.to_ms,
        search: req.search.as_deref().filter(|s| !s.is_empty()),
        restrict_to_actor_user_id,
        cursor: cursor.as_ref(),
    };

    let limit = if req.limit == 0 {
        DEFAULT_PAGE
    } else {
        req.limit as usize
    };
    let page = events::browse(&pool, &filter, limit).map_err(|e| db_error("browse", e))?;
    let mut names = Names::new(ctx);
    let rows = page
        .events
        .into_iter()
        .map(|event| to_wire(event, &mut names))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(MessageBody::EventsBody(EventsPayload::BrowseResponse(
        EventsBrowseResponse {
            rows,
            next_cursor: page.next_cursor.map(|c| EventsCursor {
                at_ms: c.at_ms,
                run_id: c.run_id,
                seq: c.seq,
            }),
            scoped_to_self: restrict_to_actor_user_id.is_some(),
        },
    )))
}

fn run_timeline(
    ctx: &HandlerContext,
    req: &EventsRunRequest,
) -> Result<MessageBody, ProtocolError> {
    if req.run_id.is_empty() {
        return Err(ProtocolError::bad_request("run_id is required"));
    }
    let scope = resolve_scope(ctx)?;
    let pool = events::db::pool().map_err(|e| db_error("pool", e))?;

    // Identical message for "no such run" and "not yours": the difference
    // between them is exactly the fact a non-owner may not learn.
    let not_found = || ProtocolError::not_found(format!("run not found: {}", req.run_id));

    if let Scope::OwnRuns(user_id) = &scope {
        let owner = events::run_actor_user_id(&pool, &req.run_id)
            .map_err(|e| db_error("run_owner", e))?
            .ok_or_else(not_found)?;
        // A run naming no user (camera, scheduler, unbound service key) belongs
        // to nobody, so it belongs to no caller either — `events.read_all` only.
        if owner.as_deref() != Some(user_id.as_str()) {
            return Err(not_found());
        }
    }

    let limit = if req.limit == 0 {
        DEFAULT_PAGE
    } else {
        req.limit as usize
    };
    let limit = limit.clamp(1, MAX_READ_LIMIT);
    let events = events::read_run(&pool, &req.run_id, req.after_seq, limit)
        .map_err(|e| db_error("read_run", e))?;
    // A short page is the only evidence the run's log ended here; a full page
    // yields a resume point even when nothing follows it.
    let next_after_seq = (events.len() == limit)
        .then(|| events.last().map(|e| e.seq))
        .flatten();
    let mut names = Names::new(ctx);
    let events = events
        .into_iter()
        .map(|event| to_wire(event, &mut names))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(MessageBody::EventsBody(EventsPayload::RunResponse(
        EventsRunResponse {
            run_id: req.run_id.clone(),
            events,
            next_after_seq,
        },
    )))
}

/// The unpacked content of one `step_io` row, under the same visibility rule as
/// the timeline of its run.
fn step_io(ctx: &HandlerContext, req: &EventsStepIoRequest) -> Result<MessageBody, ProtocolError> {
    if req.run_id.is_empty() {
        return Err(ProtocolError::bad_request("run_id is required"));
    }
    let scope = resolve_scope(ctx)?;
    let pool = events::db::pool().map_err(|e| db_error("pool", e))?;
    let not_found = || ProtocolError::not_found(format!("step content not found: {}", req.run_id));

    if let Scope::OwnRuns(user_id) = &scope {
        let owner = events::run_actor_user_id(&pool, &req.run_id)
            .map_err(|e| db_error("run_owner", e))?
            .ok_or_else(not_found)?;
        if owner.as_deref() != Some(user_id.as_str()) {
            return Err(not_found());
        }
    }

    let row = events::read_run(&pool, &req.run_id, req.seq - 1, 1)
        .map_err(|e| db_error("read_run", e))?
        .into_iter()
        .find(|e| e.seq == req.seq)
        .ok_or_else(not_found)?;
    let crate::events::EventPayload::StepIo {
        z,
        input_bytes,
        output_bytes,
        truncated,
        ..
    } = row.payload
    else {
        return Err(not_found());
    };
    let body = events::step_io::unpack(&z).map_err(|e| db_error("step_io_unpack", e))?;
    Ok(MessageBody::EventsBody(EventsPayload::StepIoResponse(
        EventsStepIoResponse {
            input: body.input,
            output: body.output,
            input_bytes,
            output_bytes,
            truncated,
        },
    )))
}

#[handler(variant = "EventsBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn events_browser_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::EventsBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected EventsBody")),
    };
    match payload {
        EventsPayload::BrowseRequest(r) => browse(ctx, r),
        EventsPayload::RunRequest(r) => run_timeline(ctx, r),
        EventsPayload::StepIoRequest(r) => step_io(ctx, r),
        EventsPayload::BrowseResponse(_)
        | EventsPayload::RunResponse(_)
        | EventsPayload::StepIoResponse(_) => Err(ProtocolError::bad_request(
            "response variant cannot be sent as a request",
        )),
    }
}

/// `dispatch::find` keys the registry by SUB-variant name, so each request
/// variant needs its own inventory entry — without one the router answers
/// `NotImplemented` even though the handler above exists.
///
/// `UserSession` is the tier: the permission split (`events.read` vs
/// `events.read_all`) is finer than any session tier can express, so the real
/// gate is `resolve_scope`, and a role-based `Admin` tier here would lock out
/// the very users `events.read` was seeded for.
macro_rules! register_events_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::UserSession,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_events_browser_dispatch,
            }
        }
    };
}

register_events_variant!("EventsBrowseRequest", "tentaflow_ws_handler_events_browse");
register_events_variant!("EventsRunRequest", "tentaflow_ws_handler_events_run");
register_events_variant!("EventsStepIoRequest", "tentaflow_ws_handler_events_step_io");

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    use crate::db::DbPool;
    use crate::events::store::{append, RunEvent};
    use crate::events::EventPayload;
    use crate::flow_engine::dispatcher::FlowActor;
    use crate::services::rbac::OrgContext;

    /// The event log is reached through a process-global pool, so the tests
    /// share ONE temporary database — kept alive here for the length of the
    /// process. Every test therefore scopes its assertions to a session id of
    /// its own instead of assuming an empty table.
    static EVENTS_DIR: OnceLock<tempfile::TempDir> = OnceLock::new();

    fn events_pool() -> DbPool {
        let dir = EVENTS_DIR.get_or_init(|| tempfile::tempdir().expect("tempdir"));
        let _ = crate::events::db::init(&dir.path().join("events.db"));
        crate::events::db::pool().expect("events pool")
    }

    fn request_started() -> EventPayload {
        EventPayload::RequestStarted {
            model: Some("qwen3".into()),
            flow_id: None,
            service_type: Some("llm".into()),
            modality: Some("text".into()),
        }
    }

    /// A principal: the session's 16 raw bytes and the uuid string the writer
    /// stamps into `actor_user_id`, which is what the ACL compares.
    struct Principal {
        bytes: [u8; 16],
        uuid: String,
    }

    fn principal() -> Principal {
        let id = uuid::Uuid::new_v4();
        Principal {
            bytes: *id.as_bytes(),
            uuid: id.to_string(),
        }
    }

    fn ctx_with(who: &Principal, permissions: &[&str]) -> HandlerContext {
        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: who.bytes,
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: crate::dispatch::state::AppState::for_test(),
            origin: crate::dispatch::RequestOrigin::Local,
            org_context: Some(OrgContext {
                user_id: who.uuid.clone(),
                org_id: "org-test".to_string(),
                role_id: "role-test".to_string(),
                permissions: permissions.iter().map(|p| p.to_string()).collect(),
            }),
        };
        crate::dispatch::seed_session_account(&ctx);
        ctx
    }

    /// Seeds one run owned by `who`, inside `session_id` so the test can find
    /// its own rows in the shared log.
    fn seed_run(ctx: &HandlerContext, run_id: &str, session_id: &str, who: &str, at_ms: i64) {
        let pool = events_pool();
        append(
            &pool,
            &ctx.state.db,
            RunEvent::new(
                run_id,
                at_ms,
                FlowOrigin::Chat,
                &FlowActor::user(who),
                request_started(),
            )
            .with_session(session_id),
        )
        .expect("append");
    }

    fn browse_body(session_id: &str) -> MessageBody {
        MessageBody::EventsBody(EventsPayload::BrowseRequest(EventsBrowseRequest {
            session_id: Some(session_id.to_string()),
            ..EventsBrowseRequest::default()
        }))
    }

    fn expect_browse(body: MessageBody) -> EventsBrowseResponse {
        match body {
            MessageBody::EventsBody(EventsPayload::BrowseResponse(r)) => r,
            other => panic!("unexpected response: {other:?}"),
        }
    }

    /// The deliverable that must not be got wrong: user B's session does not
    /// receive user A's rows. Routed through `dispatch::dispatch` rather than
    /// calling the function directly, so it also proves the inventory
    /// registration resolves (without it the router answers NotImplemented).
    #[tokio::test]
    async fn a_browse_never_returns_another_principals_rows() {
        let anna = principal();
        let marek = principal();
        let session = format!("sess-{}", uuid::Uuid::new_v4());
        let anna_ctx = ctx_with(&anna, &[PERM_READ]);
        let marek_ctx = ctx_with(&marek, &[PERM_READ]);

        seed_run(&anna_ctx, "run-anna", &session, &anna.uuid, 100);
        seed_run(&marek_ctx, "run-marek", &session, &marek.uuid, 200);

        let (body, is_err) = crate::dispatch::dispatch(&browse_body(&session), &marek_ctx).await;
        assert!(!is_err, "unexpected error body: {body:?}");
        let resp = expect_browse(body);
        let runs: Vec<&str> = resp.rows.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(runs, vec!["run-marek"]);
        assert!(
            resp.rows
                .iter()
                .all(|r| r.actor_user_id.as_deref() == Some(marek.uuid.as_str())),
            "every row on a scoped page belongs to the caller"
        );
        assert!(resp.scoped_to_self, "the page was narrowed and says so");
    }

    /// The other half: `events.read_all` is what widens the same query.
    #[tokio::test]
    async fn read_all_sees_every_principals_rows() {
        let anna = principal();
        let marek = principal();
        let admin = principal();
        let session = format!("sess-{}", uuid::Uuid::new_v4());
        let anna_ctx = ctx_with(&anna, &[PERM_READ]);
        let admin_ctx = ctx_with(&admin, &[PERM_READ, PERM_READ_ALL]);

        seed_run(&anna_ctx, "run-anna", &session, &anna.uuid, 100);
        seed_run(&anna_ctx, "run-marek", &session, &marek.uuid, 200);

        let (body, is_err) = crate::dispatch::dispatch(&browse_body(&session), &admin_ctx).await;
        assert!(!is_err, "unexpected error body: {body:?}");
        let resp = expect_browse(body);
        let mut runs: Vec<&str> = resp.rows.iter().map(|r| r.run_id.as_str()).collect();
        runs.sort();
        assert_eq!(runs, vec!["run-anna", "run-marek"]);
        assert!(!resp.scoped_to_self);
    }

    /// `events.read_all` is the strictly WIDER capability, so it stands on its
    /// own: an auditor granted only it must see the whole node. Every other
    /// `read_all` test here grants both permissions, which leaves the ordering
    /// inside `resolve_scope` unpinned — checking `events.read` first would deny
    /// exactly this caller everything, which is the failure the doc comment on
    /// `resolve_scope` warns about and the reason this test grants ONE
    /// permission and not two.
    #[tokio::test]
    async fn read_all_without_read_is_still_the_wider_grant() {
        let anna = principal();
        let marek = principal();
        let auditor = principal();
        let session = format!("sess-{}", uuid::Uuid::new_v4());
        let anna_ctx = ctx_with(&anna, &[PERM_READ]);
        let auditor_ctx = ctx_with(&auditor, &[PERM_READ_ALL]);

        seed_run(&anna_ctx, "run-anna", &session, &anna.uuid, 100);
        seed_run(&anna_ctx, "run-marek", &session, &marek.uuid, 200);

        let (body, is_err) = crate::dispatch::dispatch(&browse_body(&session), &auditor_ctx).await;
        assert!(!is_err, "read_all alone must not be a denial: {body:?}");
        let resp = expect_browse(body);
        let mut runs: Vec<&str> = resp.rows.iter().map(|r| r.run_id.as_str()).collect();
        runs.sort();
        assert_eq!(
            runs,
            vec!["run-anna", "run-marek"],
            "read_all alone sees every principal's rows"
        );
        assert!(
            !resp.scoped_to_self,
            "read_all alone is not a narrowed page"
        );

        // The same grant reads any run's timeline, including one it does not own
        // — the per-run ACL is skipped for `Scope::Node`, not merely widened.
        let run = run_timeline(
            &auditor_ctx,
            &EventsRunRequest {
                run_id: "run-anna".to_string(),
                ..EventsRunRequest::default()
            },
        )
        .expect("read_all reads a run it does not own");
        match run {
            MessageBody::EventsBody(EventsPayload::RunResponse(r)) => {
                assert_eq!(r.run_id, "run-anna");
                assert!(!r.events.is_empty());
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    /// The criterion the whole feature exists to satisfy: the visibility scope
    /// comes from the SESSION, and nothing in the request can widen it.
    ///
    /// The restriction and the request filters meet here on purpose. Elsewhere
    /// they never do — the filter tests run unrestricted and the ACL tests send
    /// no filters — so a change that let a named `actor_id` (or an org, session
    /// or free-text filter) escape the narrowing would be invisible. Each filter
    /// below names a value that belongs to ANOTHER principal, and each must come
    /// back empty rather than come back with her rows.
    #[tokio::test]
    async fn a_restricted_caller_cannot_widen_scope_by_naming_another_principal() {
        let anna = principal();
        let marek = principal();
        let session = format!("sess-{}", uuid::Uuid::new_v4());
        let anna_ctx = ctx_with(&anna, &[PERM_READ]);
        let marek_ctx = ctx_with(&marek, &[PERM_READ]);
        let anna_run = format!("run-anna-{}", uuid::Uuid::new_v4());
        let marek_run = format!("run-marek-{}", uuid::Uuid::new_v4());

        seed_run(&anna_ctx, &anna_run, &session, &anna.uuid, 100);
        seed_run(&marek_ctx, &marek_run, &session, &marek.uuid, 200);

        // The payload names anna as the actor. `actor_user_id` is derived from
        // marek's session and the two are ANDed, so the page is empty — the
        // request field selects WITHIN the caller's scope and never replaces it.
        let named = EventsBrowseRequest {
            session_id: Some(session.clone()),
            actor_id: Some(anna.uuid.clone()),
            ..EventsBrowseRequest::default()
        };
        let resp = expect_browse(browse(&marek_ctx, &named).expect("browse"));
        assert!(
            resp.rows.is_empty(),
            "naming another principal's actor_id must not reach her rows: {:?}",
            resp.rows.iter().map(|r| &r.run_id).collect::<Vec<_>>()
        );
        assert!(resp.scoped_to_self, "the page is still a narrowed one");

        // Free text that matches only anna's run id: the search predicate is
        // ANDed with the visibility predicate, not substituted for it.
        let searched = EventsBrowseRequest {
            session_id: Some(session.clone()),
            search: Some(anna_run.clone()),
            ..EventsBrowseRequest::default()
        };
        let resp = expect_browse(browse(&marek_ctx, &searched).expect("browse"));
        assert!(
            resp.rows.is_empty(),
            "searching for another principal's run id must not surface it"
        );

        // Every filter the request carries, all pointed at anna at once.
        let everything = EventsBrowseRequest {
            origins: Some(vec!["chat".into()]),
            actor_id: Some(anna.uuid.clone()),
            org_id: Some("org-test".into()),
            session_id: Some(session.clone()),
            search: Some(anna_run.clone()),
            from_ms: Some(0),
            to_ms: Some(1_000),
            limit: 100,
            ..EventsBrowseRequest::default()
        };
        let resp = expect_browse(browse(&marek_ctx, &everything).expect("browse"));
        assert!(
            resp.rows.is_empty(),
            "no combination of request filters widens a session-derived scope"
        );

        // And the narrowing is a narrowing, not a blanket empty page: marek
        // naming HIMSELF still reads his own rows, so the refusals above are the
        // ACL and not a filter that matches nothing.
        let own = EventsBrowseRequest {
            session_id: Some(session.clone()),
            actor_id: Some(marek.uuid.clone()),
            ..EventsBrowseRequest::default()
        };
        let resp = expect_browse(browse(&marek_ctx, &own).expect("browse"));
        let runs: Vec<&str> = resp.rows.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(runs, vec![marek_run.as_str()]);
    }

    /// Invariant 3 at the layer that could actually undo it. The writer's
    /// omission is proven in `store`; what is proven HERE is that `to_wire`
    /// cannot put the body back. It does not forward the stored string — it
    /// re-serialises the decoded payload — so the omission has to survive a
    /// decode and an encode to reach `EventRowWire.payload_json`, and this
    /// asserts on that field rather than on the decoded value behind it.
    #[tokio::test]
    async fn an_omitted_assistant_body_never_reaches_the_wire() {
        const SECRET: &str = "the answer nobody opted in to keep";
        let anna = principal();
        let anna_ctx = ctx_with(&anna, &[PERM_READ]);
        let session = format!("sess-{}", uuid::Uuid::new_v4());
        let run_id = format!("run-{}", uuid::Uuid::new_v4());
        let pool = events_pool();
        append(
            &pool,
            &anna_ctx.state.db,
            RunEvent::new(
                &run_id,
                100,
                FlowOrigin::Chat,
                &FlowActor::user(&anna.uuid),
                EventPayload::AssistantMessage {
                    body: crate::events::ResponseBody::Text(SECRET.to_string()),
                    tokens: Some(12),
                },
            )
            .with_session(&session)
            // An organisation that never opted in, which is every organisation
            // by default — so the writer stores the omission marker.
            .with_org("org-test"),
        )
        .expect("append");

        let (body, is_err) = crate::dispatch::dispatch(&browse_body(&session), &anna_ctx).await;
        assert!(!is_err, "unexpected error body: {body:?}");
        let resp = expect_browse(body);
        let row = resp
            .rows
            .iter()
            .find(|r| r.run_id == run_id)
            .expect("the seeded row is on the page");
        assert_eq!(row.kind, "assistant_message");
        assert!(
            !row.payload_json.contains(SECRET),
            "the wire payload must not carry a body the writer omitted: {}",
            row.payload_json
        );
        assert!(
            row.payload_json.contains("\"omitted\""),
            "the omission must be VISIBLE on the wire, not an absent field: {}",
            row.payload_json
        );

        // The same row through the per-run path, which shares `to_wire`.
        let timeline = run_timeline(
            &anna_ctx,
            &EventsRunRequest {
                run_id: run_id.clone(),
                ..EventsRunRequest::default()
            },
        )
        .expect("owner reads her own run");
        match timeline {
            MessageBody::EventsBody(EventsPayload::RunResponse(r)) => {
                let wire = r
                    .events
                    .iter()
                    .find(|e| e.kind == "assistant_message")
                    .expect("the assistant message is on the timeline");
                assert!(!wire.payload_json.contains(SECRET));
                assert!(wire.payload_json.contains("\"omitted\""));
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    /// A non-owner asking for a SPECIFIC run gets `not_found`, not
    /// `PolicyDenied` — and the message is byte-identical to the one a run that
    /// does not exist produces, because the difference between the two is the
    /// fact being withheld.
    #[tokio::test]
    async fn a_non_owner_asking_for_a_run_gets_the_same_not_found_as_an_absent_run() {
        let anna = principal();
        let marek = principal();
        let session = format!("sess-{}", uuid::Uuid::new_v4());
        let anna_ctx = ctx_with(&anna, &[PERM_READ]);
        let marek_ctx = ctx_with(&marek, &[PERM_READ]);
        let run_id = format!("run-{}", uuid::Uuid::new_v4());
        seed_run(&anna_ctx, &run_id, &session, &anna.uuid, 100);

        let existing = MessageBody::EventsBody(EventsPayload::RunRequest(EventsRunRequest {
            run_id: run_id.clone(),
            ..EventsRunRequest::default()
        }));
        let absent_id = format!("run-{}", uuid::Uuid::new_v4());
        let absent = MessageBody::EventsBody(EventsPayload::RunRequest(EventsRunRequest {
            run_id: absent_id.clone(),
            ..EventsRunRequest::default()
        }));

        let denied = run_timeline(
            &marek_ctx,
            match &existing {
                MessageBody::EventsBody(EventsPayload::RunRequest(r)) => r,
                _ => unreachable!(),
            },
        )
        .expect_err("marek does not own anna's run");
        assert_eq!(denied.code, ProtocolErrorCode::NotFound);
        assert_eq!(denied.message, format!("run not found: {run_id}"));

        let missing = run_timeline(
            &marek_ctx,
            match &absent {
                MessageBody::EventsBody(EventsPayload::RunRequest(r)) => r,
                _ => unreachable!(),
            },
        )
        .expect_err("no such run");
        assert_eq!(missing.code, ProtocolErrorCode::NotFound);
        assert_eq!(missing.message, format!("run not found: {absent_id}"));

        // The owner still reads their own run, so the refusal above is an ACL
        // and not a broken lookup.
        let (body, is_err) = crate::dispatch::dispatch(&existing, &anna_ctx).await;
        assert!(!is_err, "unexpected error body: {body:?}");
        match body {
            MessageBody::EventsBody(EventsPayload::RunResponse(r)) => {
                assert_eq!(r.run_id, run_id);
                assert_eq!(r.events.len(), 1);
                assert_eq!(r.events[0].kind, "request_started");
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    /// An unattended run (camera, scheduler, unbound service key) names no
    /// user, so it belongs to no `events.read` caller — `Some(None)` must not
    /// read as "matches my None".
    #[tokio::test]
    async fn an_unowned_run_is_not_readable_without_read_all() {
        let service_ctx = ctx_with(&principal(), &[PERM_READ]);
        let run_id = format!("run-{}", uuid::Uuid::new_v4());
        let pool = events_pool();
        append(
            &pool,
            &service_ctx.state.db,
            RunEvent::new(
                &run_id,
                100,
                FlowOrigin::Camera,
                &FlowActor::api_key("key-svc", None),
                request_started(),
            ),
        )
        .expect("append");

        let err = run_timeline(
            &service_ctx,
            &EventsRunRequest {
                run_id: run_id.clone(),
                ..EventsRunRequest::default()
            },
        )
        .expect_err("an unowned run is read_all only");
        assert_eq!(err.code, ProtocolErrorCode::NotFound);
    }

    /// A page keeps the SIZES of a step's content but never its body; the body
    /// comes from `step_io`, which applies the owner rule of the run.
    #[tokio::test]
    async fn step_content_stays_off_the_page_and_is_served_to_the_owner_only() {
        let anna = principal();
        let marek = principal();
        let anna_ctx = ctx_with(&anna, &[PERM_READ]);
        let marek_ctx = ctx_with(&marek, &[PERM_READ]);
        let session = format!("sess-{}", uuid::Uuid::new_v4());
        let run_id = format!("run-{}", uuid::Uuid::new_v4());
        seed_run(&anna_ctx, &run_id, &session, &anna.uuid, 100);
        let packed = crate::events::step_io::pack("prompt do modelu", "odpowiedź").unwrap();
        let appended = append(
            &events_pool(),
            &anna_ctx.state.db,
            RunEvent::new(
                &run_id,
                101,
                FlowOrigin::Chat,
                &FlowActor::user(&anna.uuid),
                EventPayload::StepIo {
                    z: packed.z,
                    input_bytes: packed.input_bytes,
                    output_bytes: packed.output_bytes,
                    truncated: false,
                    model: None,
                    served_model: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                },
            )
            .with_session(&session),
        )
        .expect("append step io");

        let page = match browse(
            &anna_ctx,
            &EventsBrowseRequest {
                session_id: Some(session.clone()),
                ..EventsBrowseRequest::default()
            },
        )
        .expect("browse")
        {
            MessageBody::EventsBody(EventsPayload::BrowseResponse(r)) => r,
            other => panic!("unexpected response: {other:?}"),
        };
        let row = page
            .rows
            .iter()
            .find(|r| r.kind == "step_io")
            .expect("step_io row");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["z"], "", "the body must not ride on a page");
        assert_eq!(payload["input_bytes"], "prompt do modelu".len());

        let req = EventsStepIoRequest {
            run_id: run_id.clone(),
            seq: appended.seq,
        };
        match step_io(&anna_ctx, &req).expect("owner reads the content") {
            MessageBody::EventsBody(EventsPayload::StepIoResponse(r)) => {
                assert_eq!(r.input, "prompt do modelu");
                assert_eq!(r.output, "odpowiedź");
            }
            other => panic!("unexpected response: {other:?}"),
        }
        let denied = step_io(&marek_ctx, &req).expect_err("not marek's run");
        assert_eq!(denied.code, ProtocolErrorCode::NotFound);
    }

    #[tokio::test]
    async fn a_caller_without_events_read_is_denied() {
        let ctx = ctx_with(&principal(), &["some.other.permission"]);
        let err = browse(&ctx, &EventsBrowseRequest::default()).expect_err("no permission");
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
    }

    /// No org snapshot means no permission set, and a permission set is the
    /// only thing that grants any scope here. Fail closed.
    #[tokio::test]
    async fn a_call_without_an_org_context_is_refused() {
        let mut ctx = ctx_with(&principal(), &[PERM_READ]);
        ctx.org_context = None;
        let err = browse(&ctx, &EventsBrowseRequest::default()).expect_err("no org context");
        assert_eq!(err.code, ProtocolErrorCode::AuthRequired);
    }

    #[tokio::test]
    async fn a_response_variant_cannot_be_sent_as_a_request() {
        let ctx = ctx_with(&principal(), &[PERM_READ, PERM_READ_ALL]);
        let body = MessageBody::EventsBody(EventsPayload::RunResponse(EventsRunResponse {
            run_id: "run-1".into(),
            events: vec![],
            next_after_seq: None,
        }));
        let err = events_browser_dispatch(&body, &ctx)
            .await
            .expect_err("a response is not a request");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    }

    /// A slug this build does not know is refused rather than answered with an
    /// empty page: telling a stale client "no such traffic" would be a claim
    /// about the node that was never checked.
    #[tokio::test]
    async fn an_unknown_origin_slug_is_a_bad_request() {
        let ctx = ctx_with(&principal(), &[PERM_READ, PERM_READ_ALL]);
        let err = browse(
            &ctx,
            &EventsBrowseRequest {
                origins: Some(vec!["chat".into(), "telepathy".into()]),
                ..EventsBrowseRequest::default()
            },
        )
        .expect_err("unknown origin");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
        assert!(err.message.contains("telepathy"));
    }
}
