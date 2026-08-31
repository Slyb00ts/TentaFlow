// ===== File: events/browse.rs — the cross-run query behind the Events browser =====
//
// `store::read_run` answers "what happened in THIS run". The browser asks the
// other question — "what happened on this node, newest first, narrowed to the
// origins / actor / window I care about" — and that is a different query shape:
// no run in hand, a variable set of predicates, and a page boundary that has to
// survive a log which is appended to and swept while a person is reading it.
//
// **Paging is KEYSET, not OFFSET.** Two reasons, both structural rather than
// stylistic. (1) `run_events` is written continuously and the retention sweep
// deletes from the other end, so the row COUNT before any given row changes
// between two requests; `LIMIT ?  OFFSET ?` names a count, so page 2 fetched a
// second later silently repeats rows that new appends pushed down and skips the
// ones that fell off. A cursor names a POSITION in the order, so it stays
// correct under both. (2) SQLite implements OFFSET by producing and discarding
// the skipped rows, so the cost of page N grows with N — on a diagnostic log
// sized in millions of rows the deep pages are exactly the ones an investigator
// scrolls to. The price is that there is no jump-to-page-17, which a timeline
// browser does not offer anyway.
//
// The order is `at_ms DESC, run_id DESC, seq DESC`. `at_ms` alone is NOT a
// total order — a burst of events shares a millisecond — so the cursor carries
// the table's primary key as the tiebreaker; without it the rows sharing the
// boundary millisecond are dropped or repeated.
//
// **Visibility is a WHERE clause, never a post-filter.** `restrict_to_actor_user_id`
// is part of the query, so a caller who may see only their own runs never has
// another principal's row in memory, cannot page past a hidden row into a wrong
// cursor, and gets a full page of what they may see rather than a page with
// holes in it.
//
// Nothing here derives, pairs or totals anything: a page is the rows as stored.
// A run that is still in flight has an opening event and no closing one, and
// that is what the browser receives — the duration it does not have is not
// computed here (invariant 6).

use anyhow::{anyhow, Result};
use rusqlite::types::Value;

use crate::db::DbPool;
use crate::flow_engine::dispatcher::FlowOrigin;

use super::store::{
    decode_stored_row, read_stored_row, StoredEvent, MAX_READ_LIMIT, STORED_COLUMNS,
};

/// Position in the browse order. Produced by [`browse`], consumed by the next
/// call; the three parts together are unique because `(run_id, seq)` is the
/// table's primary key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventCursor {
    pub at_ms: i64,
    pub run_id: String,
    pub seq: i64,
}

/// Everything the browse query may be narrowed by.
///
/// `origins` is typed as `FlowOrigin` rather than as strings: the slugs reach
/// SQL from a closed enum, so an origin this build does not know is refused at
/// the edge instead of quietly matching no rows and looking like an empty node.
#[derive(Debug, Default)]
pub struct EventFilter<'a> {
    /// `None` = no origin constraint. `Some(&[])` = every origin was deselected,
    /// which matches NOTHING — deselecting everything must not show everything.
    pub origins: Option<&'a [FlowOrigin]>,
    /// Exact `actor_id`: a user uuid, an API key uid, an addon instance id or a
    /// system component id.
    pub actor_id: Option<&'a str>,
    pub org_id: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub correlation_id: Option<&'a str>,
    /// Inclusive bounds on `at_ms`, epoch milliseconds.
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    /// Free text over the run id, the call id, the flow node id and the stored
    /// payload. The payload was redacted by the writer, so this searches what
    /// is on disk and can never surface a value the writer removed.
    pub search: Option<&'a str>,
    /// VISIBILITY, not a user-supplied filter: when set, only rows whose
    /// `actor_user_id` equals it. `actor_user_id` rather than `actor_id`
    /// because a run made by an API key bound to this user IS this user's run,
    /// and `actor_id` would hold the key's uid instead.
    pub restrict_to_actor_user_id: Option<&'a str>,
    /// `None` = first page.
    pub cursor: Option<&'a EventCursor>,
}

/// One page plus the position to resume from.
#[derive(Debug)]
pub struct EventPage {
    pub events: Vec<StoredEvent>,
    /// `None` when the page came back short, which is the only evidence this
    /// query has that the result set ended. A page that filled exactly to the
    /// limit yields a cursor even when nothing follows it — the next request
    /// then comes back empty. Claiming "no more" without having looked would be
    /// asserting something the query never observed.
    pub next_cursor: Option<EventCursor>,
}

/// Escapes the LIKE metacharacters so a search for `100%` looks for the literal
/// text and not for "anything starting with 100". The escape character itself
/// goes first, otherwise it would escape the escapes this function just added.
fn like_pattern(needle: &str) -> String {
    let escaped = needle
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

/// A page of events across runs, newest first.
///
/// `limit` is clamped exactly as `store::read_run` clamps it — one ceiling for
/// every reader of this table, so no caller can turn a browse into a full dump.
pub fn browse(pool: &DbPool, filter: &EventFilter<'_>, limit: usize) -> Result<EventPage> {
    // An explicitly empty origin set is answered without touching the database:
    // there is no row it could match, and `origin IN ()` is not valid SQLite.
    if filter.origins.is_some_and(|origins| origins.is_empty()) {
        return Ok(EventPage {
            events: Vec::new(),
            next_cursor: None,
        });
    }

    let limit = limit.clamp(1, MAX_READ_LIMIT);
    let mut conditions: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if let Some(origins) = filter.origins {
        let placeholders = vec!["?"; origins.len()].join(", ");
        conditions.push(format!("origin IN ({placeholders})"));
        params.extend(origins.iter().map(|o| Value::Text(o.as_str().to_string())));
    }
    if let Some(actor_id) = filter.actor_id {
        conditions.push("actor_id = ?".to_string());
        params.push(Value::Text(actor_id.to_string()));
    }
    if let Some(org_id) = filter.org_id {
        conditions.push("org_id = ?".to_string());
        params.push(Value::Text(org_id.to_string()));
    }
    if let Some(session_id) = filter.session_id {
        conditions.push("session_id = ?".to_string());
        params.push(Value::Text(session_id.to_string()));
    }
    if let Some(correlation_id) = filter.correlation_id {
        conditions.push("correlation_id = ?".to_string());
        params.push(Value::Text(correlation_id.to_string()));
    }
    if let Some(from_ms) = filter.from_ms {
        conditions.push("at_ms >= ?".to_string());
        params.push(Value::Integer(from_ms));
    }
    if let Some(to_ms) = filter.to_ms {
        conditions.push("at_ms <= ?".to_string());
        params.push(Value::Integer(to_ms));
    }
    if let Some(needle) = filter.search.filter(|s| !s.is_empty()) {
        conditions.push(
            "(run_id LIKE ? ESCAPE '\\' OR call_id LIKE ? ESCAPE '\\' \
             OR node_id LIKE ? ESCAPE '\\' OR payload_json LIKE ? ESCAPE '\\')"
                .to_string(),
        );
        let pattern = like_pattern(needle);
        for _ in 0..4 {
            params.push(Value::Text(pattern.clone()));
        }
    }
    if let Some(user_id) = filter.restrict_to_actor_user_id {
        conditions.push("actor_user_id = ?".to_string());
        params.push(Value::Text(user_id.to_string()));
    }
    if let Some(cursor) = filter.cursor {
        // "Strictly after this position" spelled out in the ORDER BY's own
        // order. Written as nested comparisons rather than as a row-value
        // `(a, b, c) < (?, ?, ?)` because a row-value comparison is not usable
        // by SQLite's index machinery here.
        conditions.push(
            "(at_ms < ? OR (at_ms = ? AND (run_id < ? OR (run_id = ? AND seq < ?))))".to_string(),
        );
        params.push(Value::Integer(cursor.at_ms));
        params.push(Value::Integer(cursor.at_ms));
        params.push(Value::Text(cursor.run_id.clone()));
        params.push(Value::Text(cursor.run_id.clone()));
        params.push(Value::Integer(cursor.seq));
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };
    params.push(Value::Integer(limit as i64));

    let sql = format!(
        "SELECT {STORED_COLUMNS} FROM run_events{where_clause} \
         ORDER BY at_ms DESC, run_id DESC, seq DESC LIMIT ?"
    );

    let conn = pool.read().map_err(|e| anyhow!("events db read: {e}"))?;
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), read_stored_row)?;

    let mut events = Vec::new();
    for row in rows {
        events.push(decode_stored_row(row?)?);
    }

    let next_cursor = (events.len() == limit)
        .then(|| events.last())
        .flatten()
        .map(|last| EventCursor {
            at_ms: last.at_ms,
            run_id: last.run_id.clone(),
            seq: last.seq,
        });

    Ok(EventPage {
        events,
        next_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbPool;
    use crate::events::store::{append, run_actor_user_id, EventPayload, ResponseBody, RunEvent};
    use crate::events::test_support::{events_db, main_db};
    use crate::flow_engine::dispatcher::FlowActor;

    fn request_started() -> EventPayload {
        EventPayload::RequestStarted {
            model: Some("qwen3".into()),
            flow_id: None,
            service_type: Some("llm".into()),
            modality: Some("text".into()),
        }
    }

    /// Appends one event and returns nothing — every test below cares about
    /// what comes back out, not about the `seq` going in.
    fn seed(
        pool: &DbPool,
        main: &DbPool,
        run_id: &str,
        at_ms: i64,
        origin: FlowOrigin,
        actor: &FlowActor,
        payload: EventPayload,
    ) {
        append(
            pool,
            main,
            RunEvent::new(run_id, at_ms, origin, actor, payload),
        )
        .expect("append");
    }

    fn run_ids(page: &EventPage) -> Vec<(&str, i64)> {
        page.events
            .iter()
            .map(|e| (e.run_id.as_str(), e.at_ms))
            .collect()
    }

    #[test]
    fn a_page_comes_back_newest_first() {
        let (_dir, pool) = events_db();
        let main = main_db();
        let anna = FlowActor::user("anna");
        for (run, at) in [("r-a", 100), ("r-b", 300), ("r-c", 200)] {
            seed(
                &pool,
                &main,
                run,
                at,
                FlowOrigin::Chat,
                &anna,
                request_started(),
            );
        }

        let page = browse(&pool, &EventFilter::default(), 10).unwrap();
        assert_eq!(
            run_ids(&page),
            vec![("r-b", 300), ("r-c", 200), ("r-a", 100)]
        );
        assert!(
            page.next_cursor.is_none(),
            "a short page has seen the end of the result set"
        );
    }

    /// The whole reason paging is keyset. A page boundary is taken, the log is
    /// appended to (which is what a live node does between two clicks), and the
    /// next page must continue exactly where the first ended — an OFFSET would
    /// re-serve the row the new appends pushed down.
    #[test]
    fn a_cursor_survives_appends_that_land_after_it() {
        let (_dir, pool) = events_db();
        let main = main_db();
        let anna = FlowActor::user("anna");
        for (run, at) in [("r-1", 100), ("r-2", 200), ("r-3", 300), ("r-4", 400)] {
            seed(
                &pool,
                &main,
                run,
                at,
                FlowOrigin::Chat,
                &anna,
                request_started(),
            );
        }

        let first = browse(&pool, &EventFilter::default(), 2).unwrap();
        assert_eq!(run_ids(&first), vec![("r-4", 400), ("r-3", 300)]);
        let cursor = first
            .next_cursor
            .expect("a full page yields a resume point");

        // Two newer runs arrive while the reader is looking at page one.
        seed(
            &pool,
            &main,
            "r-5",
            500,
            FlowOrigin::Chat,
            &anna,
            request_started(),
        );
        seed(
            &pool,
            &main,
            "r-6",
            600,
            FlowOrigin::Chat,
            &anna,
            request_started(),
        );

        let second = browse(
            &pool,
            &EventFilter {
                cursor: Some(&cursor),
                ..EventFilter::default()
            },
            2,
        )
        .unwrap();
        assert_eq!(
            run_ids(&second),
            vec![("r-2", 200), ("r-1", 100)],
            "the cursor names a position, so newer rows do not shift the page"
        );
    }

    /// `at_ms` is not unique. Rows sharing the boundary millisecond must be
    /// neither repeated nor dropped, which is what the `(run_id, seq)` half of
    /// the cursor is for.
    #[test]
    fn rows_sharing_a_millisecond_are_paged_without_loss_or_repeat() {
        let (_dir, pool) = events_db();
        let main = main_db();
        let anna = FlowActor::user("anna");
        for run in ["r-1", "r-2", "r-3", "r-4"] {
            seed(
                &pool,
                &main,
                run,
                777,
                FlowOrigin::Chat,
                &anna,
                request_started(),
            );
        }

        let mut seen: Vec<String> = Vec::new();
        let mut cursor: Option<EventCursor> = None;
        // Bounded on purpose. Four rows at two per page need three requests;
        // a cursor that fails to advance would otherwise re-serve the same page
        // for ever, and a test that hangs reports nothing at all.
        for _ in 0..4 {
            let page = browse(
                &pool,
                &EventFilter {
                    cursor: cursor.as_ref(),
                    ..EventFilter::default()
                },
                2,
            )
            .unwrap();
            if page.events.is_empty() {
                break;
            }
            seen.extend(page.events.iter().map(|e| e.run_id.clone()));
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        seen.sort();
        assert_eq!(seen, vec!["r-1", "r-2", "r-3", "r-4"]);
    }

    #[test]
    fn the_origin_filter_selects_only_the_named_origins() {
        let (_dir, pool) = events_db();
        let main = main_db();
        let anna = FlowActor::user("anna");
        seed(
            &pool,
            &main,
            "r-chat",
            100,
            FlowOrigin::Chat,
            &anna,
            request_started(),
        );
        seed(
            &pool,
            &main,
            "r-code",
            200,
            FlowOrigin::CodeStudio,
            &anna,
            request_started(),
        );
        seed(
            &pool,
            &main,
            "r-api",
            300,
            FlowOrigin::Api,
            &anna,
            request_started(),
        );

        let origins = [FlowOrigin::Chat, FlowOrigin::Api];
        let page = browse(
            &pool,
            &EventFilter {
                origins: Some(&origins),
                ..EventFilter::default()
            },
            10,
        )
        .unwrap();
        assert_eq!(run_ids(&page), vec![("r-api", 300), ("r-chat", 100)]);
    }

    /// Deselecting every chip means "show nothing". Collapsing that into "no
    /// constraint" would show the user the opposite of what they asked for.
    #[test]
    fn an_empty_origin_set_matches_nothing() {
        let (_dir, pool) = events_db();
        let main = main_db();
        let anna = FlowActor::user("anna");
        seed(
            &pool,
            &main,
            "r-chat",
            100,
            FlowOrigin::Chat,
            &anna,
            request_started(),
        );

        let page = browse(
            &pool,
            &EventFilter {
                origins: Some(&[]),
                ..EventFilter::default()
            },
            10,
        )
        .unwrap();
        assert!(page.events.is_empty());
        assert!(page.next_cursor.is_none());
    }

    /// The visibility narrowing is a WHERE clause. Proven at the query layer so
    /// the handler test above it is not the only thing standing between one
    /// principal and another's timeline.
    #[test]
    fn the_visibility_filter_excludes_another_principals_rows() {
        let (_dir, pool) = events_db();
        let main = main_db();
        let anna = FlowActor::user("anna");
        let marek = FlowActor::user("marek");
        // An API key BOUND to anna: her run, made through a key.
        let anna_key = FlowActor::api_key("key-42", Some("anna".into()));
        // A service key with no binding: nobody's run.
        let service = FlowActor::api_key("key-svc", None);

        seed(
            &pool,
            &main,
            "r-anna",
            100,
            FlowOrigin::Chat,
            &anna,
            request_started(),
        );
        seed(
            &pool,
            &main,
            "r-marek",
            200,
            FlowOrigin::Chat,
            &marek,
            request_started(),
        );
        seed(
            &pool,
            &main,
            "r-anna-key",
            300,
            FlowOrigin::Api,
            &anna_key,
            request_started(),
        );
        seed(
            &pool,
            &main,
            "r-service",
            400,
            FlowOrigin::Api,
            &service,
            request_started(),
        );

        let page = browse(
            &pool,
            &EventFilter {
                restrict_to_actor_user_id: Some("anna"),
                ..EventFilter::default()
            },
            10,
        )
        .unwrap();
        assert_eq!(
            run_ids(&page),
            vec![("r-anna-key", 300), ("r-anna", 100)],
            "a key bound to anna is anna's run; marek's and the unbound key's are not"
        );
    }

    #[test]
    fn the_actor_filter_matches_the_actor_id_not_the_bound_user() {
        let (_dir, pool) = events_db();
        let main = main_db();
        let anna = FlowActor::user("anna");
        let anna_key = FlowActor::api_key("key-42", Some("anna".into()));
        seed(
            &pool,
            &main,
            "r-anna",
            100,
            FlowOrigin::Chat,
            &anna,
            request_started(),
        );
        seed(
            &pool,
            &main,
            "r-key",
            200,
            FlowOrigin::Api,
            &anna_key,
            request_started(),
        );

        let page = browse(
            &pool,
            &EventFilter {
                actor_id: Some("key-42"),
                ..EventFilter::default()
            },
            10,
        )
        .unwrap();
        assert_eq!(run_ids(&page), vec![("r-key", 200)]);
    }

    #[test]
    fn the_time_window_bounds_are_inclusive() {
        let (_dir, pool) = events_db();
        let main = main_db();
        let anna = FlowActor::user("anna");
        for (run, at) in [("r-1", 100), ("r-2", 200), ("r-3", 300)] {
            seed(
                &pool,
                &main,
                run,
                at,
                FlowOrigin::Chat,
                &anna,
                request_started(),
            );
        }

        let page = browse(
            &pool,
            &EventFilter {
                from_ms: Some(100),
                to_ms: Some(200),
                ..EventFilter::default()
            },
            10,
        )
        .unwrap();
        assert_eq!(run_ids(&page), vec![("r-2", 200), ("r-1", 100)]);
    }

    #[test]
    fn the_correlation_and_session_filters_narrow_to_one_thread() {
        let (_dir, pool) = events_db();
        let main = main_db();
        let anna = FlowActor::user("anna");
        append(
            &pool,
            &main,
            RunEvent::new("r-1", 100, FlowOrigin::Chat, &anna, request_started())
                .with_correlation("corr-9")
                .with_session("sess-7"),
        )
        .unwrap();
        append(
            &pool,
            &main,
            RunEvent::new("r-2", 200, FlowOrigin::Chat, &anna, request_started())
                .with_correlation("corr-other")
                .with_session("sess-other"),
        )
        .unwrap();

        let by_corr = browse(
            &pool,
            &EventFilter {
                correlation_id: Some("corr-9"),
                ..EventFilter::default()
            },
            10,
        )
        .unwrap();
        assert_eq!(run_ids(&by_corr), vec![("r-1", 100)]);

        let by_session = browse(
            &pool,
            &EventFilter {
                session_id: Some("sess-7"),
                ..EventFilter::default()
            },
            10,
        )
        .unwrap();
        assert_eq!(run_ids(&by_session), vec![("r-1", 100)]);
    }

    #[test]
    fn the_org_filter_never_reaches_rows_with_no_tenant() {
        let (_dir, pool) = events_db();
        let main = main_db();
        let anna = FlowActor::user("anna");
        append(
            &pool,
            &main,
            RunEvent::new("r-org", 100, FlowOrigin::Chat, &anna, request_started())
                .with_org("org-a"),
        )
        .unwrap();
        // A camera trigger: no organisation was minted for it.
        seed(
            &pool,
            &main,
            "r-none",
            200,
            FlowOrigin::Camera,
            &FlowActor::system(),
            request_started(),
        );

        let page = browse(
            &pool,
            &EventFilter {
                org_id: Some("org-a"),
                ..EventFilter::default()
            },
            10,
        )
        .unwrap();
        assert_eq!(run_ids(&page), vec![("r-org", 100)]);
    }

    #[test]
    fn free_text_search_reaches_the_stored_payload() {
        let (_dir, pool) = events_db();
        let main = main_db();
        let anna = FlowActor::user("anna");
        seed(
            &pool,
            &main,
            "r-tool",
            100,
            FlowOrigin::CodeStudio,
            &anna,
            EventPayload::ToolResult {
                ok: true,
                summary: "28 passed".into(),
            },
        );
        seed(
            &pool,
            &main,
            "r-other",
            200,
            FlowOrigin::CodeStudio,
            &anna,
            EventPayload::ToolResult {
                ok: false,
                summary: "TOOL_TIMEOUT".into(),
            },
        );

        let page = browse(
            &pool,
            &EventFilter {
                search: Some("28 passed"),
                ..EventFilter::default()
            },
            10,
        )
        .unwrap();
        assert_eq!(run_ids(&page), vec![("r-tool", 100)]);
    }

    /// A `%` typed into the search box is a literal, not "match anything".
    /// Without the escape this query would return both rows.
    #[test]
    fn search_wildcards_are_escaped_to_literals() {
        let (_dir, pool) = events_db();
        let main = main_db();
        let anna = FlowActor::user("anna");
        seed(
            &pool,
            &main,
            "r-literal",
            100,
            FlowOrigin::Api,
            &anna,
            EventPayload::ToolResult {
                ok: true,
                summary: "coverage 100%done".into(),
            },
        );
        seed(
            &pool,
            &main,
            "r-decoy",
            200,
            FlowOrigin::Api,
            &anna,
            EventPayload::ToolResult {
                ok: true,
                summary: "coverage 100 of the done items".into(),
            },
        );

        let page = browse(
            &pool,
            &EventFilter {
                search: Some("100%done"),
                ..EventFilter::default()
            },
            10,
        )
        .unwrap();
        assert_eq!(run_ids(&page), vec![("r-literal", 100)]);
    }

    /// Invariant 3: the browse query reads what the writer stored. An assistant
    /// body the writer omitted (no organisation opt-in) must still read as an
    /// omission here — there is no read path that recovers it.
    #[test]
    fn an_omitted_assistant_body_stays_omitted_on_the_browse_path() {
        let (_dir, pool) = events_db();
        let main = main_db();
        let anna = FlowActor::user("anna");
        append(
            &pool,
            &main,
            RunEvent::new(
                "r-1",
                100,
                FlowOrigin::Chat,
                &anna,
                EventPayload::AssistantMessage {
                    body: ResponseBody::Text("the answer nobody opted in to keep".into()),
                    tokens: Some(12),
                },
            )
            .with_org("org-a"),
        )
        .unwrap();

        let page = browse(&pool, &EventFilter::default(), 10).unwrap();
        match &page.events[0].payload {
            EventPayload::AssistantMessage { body, .. } => {
                assert_eq!(body.text(), None, "the body was never stored");
                assert!(matches!(body, ResponseBody::Omitted(_)));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    /// `run_actor_user_id` answers the browser's per-run ACL, and the two
    /// "no owner" states are NOT the same: a run that does not exist, and a run
    /// that exists but names no user. Collapsing them would make an unattended
    /// run readable by whoever asked first.
    #[test]
    fn run_ownership_separates_an_absent_run_from_an_unowned_one() {
        let (_dir, pool) = events_db();
        let main = main_db();
        seed(
            &pool,
            &main,
            "r-service",
            100,
            FlowOrigin::Api,
            &FlowActor::api_key("key-svc", None),
            request_started(),
        );
        seed(
            &pool,
            &main,
            "r-anna",
            200,
            FlowOrigin::Chat,
            &FlowActor::user("anna"),
            request_started(),
        );

        assert_eq!(run_actor_user_id(&pool, "r-missing").unwrap(), None);
        assert_eq!(run_actor_user_id(&pool, "r-service").unwrap(), Some(None));
        assert_eq!(
            run_actor_user_id(&pool, "r-anna").unwrap(),
            Some(Some("anna".to_string()))
        );
    }

    /// The clamp `read_run` applies is the clamp the browse applies: no caller
    /// can turn one page request into a dump of the log.
    #[test]
    fn the_page_size_is_clamped_to_the_store_ceiling() {
        let (_dir, pool) = events_db();
        let main = main_db();
        let anna = FlowActor::user("anna");
        for i in 0..(MAX_READ_LIMIT + 5) {
            seed(
                &pool,
                &main,
                &format!("r-{i}"),
                i as i64,
                FlowOrigin::Chat,
                &anna,
                request_started(),
            );
        }

        let page = browse(&pool, &EventFilter::default(), usize::MAX).unwrap();
        assert_eq!(page.events.len(), MAX_READ_LIMIT);
        // A zero page size is nonsense, not "everything"; the clamp's lower
        // bound turns it into the smallest page there is.
        let one = browse(&pool, &EventFilter::default(), 0).unwrap();
        assert_eq!(one.events.len(), 1);
    }
}
