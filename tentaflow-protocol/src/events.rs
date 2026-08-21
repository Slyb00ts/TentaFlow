// =============================================================================
// File: events.rs
// Purpose: Wire types for the admin Events browser (§2.10) over the run event
//          log. Two questions: a page of events ACROSS runs (newest first,
//          server-side filtering, keyset paging) and the timeline of ONE run.
//          Packed into a single `EventsPayload` inner enum so the whole surface
//          burns one `MessageBody` discriminant slot (same pack pattern as
//          storage.rs / camera.rs).
//
// Append-only: new variants go at the END of `EventsPayload` and new struct
// fields carry `#[serde(default)]`, so a peer that predates a field still
// decodes the message instead of failing the frame. On an `Option<T>` field the
// attribute is belt-and-braces — serde already lets an absent key decode as
// `None` — but on a scalar (`limit`, `after_seq`) it is the whole mechanism,
// and it keeps the rule one rule instead of two.
//
// The row carries `payload_json` VERBATIM rather than a typed payload enum.
// The typed enum (`events::store::EventPayload`) lives in tentaflow-core and
// cannot be referenced from here; mirroring it would create a second,
// drift-prone spelling of the same contract. The stored JSON is internally
// tagged with the same `kind` as the column, so it is self-describing, and it
// is what the writer already redacted — shipping it unchanged is the only form
// that cannot re-expose something the writer chose to omit.
// =============================================================================

use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// Position in the browse order, which is `at_ms DESC, run_id DESC, seq DESC`.
///
/// All three parts are needed: `at_ms` is not unique (a burst of events shares
/// a millisecond) and only `(run_id, seq)` — the table's primary key — makes
/// the order total. A cursor missing the tiebreakers would drop or repeat the
/// rows that share the boundary millisecond.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct EventsCursor {
    pub at_ms: i64,
    pub run_id: String,
    pub seq: i64,
}

/// One page of the cross-run browse.
///
/// Every filter is optional and every one of them is applied in SQL. The
/// visibility rule is NOT a filter and is deliberately absent from this
/// struct: which runs a caller may see follows from their permissions, and a
/// wire field would be a request to widen it.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Default)]
pub struct EventsBrowseRequest {
    /// Origin slugs (`chat`, `code_studio`, …) — the primary filter, rendered
    /// as multi-select chips.
    ///
    /// `None` = no origin constraint. `Some([])` = the user turned every chip
    /// off, which means NOTHING matches — the opposite of no constraint. The
    /// two states are kept apart precisely so that deselecting everything
    /// cannot show everything.
    #[serde(default)]
    pub origins: Option<Vec<String>>,
    /// Exact `actor_id`: a user's uuid, an API key uid, an addon instance id
    /// or a system component id — whichever the actor picker selected.
    #[serde(default)]
    pub actor_id: Option<String>,
    #[serde(default)]
    pub org_id: Option<String>,
    /// Per-session view (a Code Studio session, a project chat).
    #[serde(default)]
    pub session_id: Option<String>,
    /// Deep link from an audit entry: the audit row and the timeline carry the
    /// same `correlation_id`.
    #[serde(default)]
    pub correlation_id: Option<String>,
    /// Inclusive lower bound on `at_ms` (epoch milliseconds).
    #[serde(default)]
    pub from_ms: Option<i64>,
    /// Inclusive upper bound on `at_ms` (epoch milliseconds).
    #[serde(default)]
    pub to_ms: Option<i64>,
    /// Free text, matched case-insensitively against the run id, the call id,
    /// the flow node id and the stored payload.
    #[serde(default)]
    pub search: Option<String>,
    /// `None` = first page; otherwise the `next_cursor` of the previous page.
    #[serde(default)]
    pub cursor: Option<EventsCursor>,
    /// Page size; clamped server-side to the store's read limit.
    #[serde(default)]
    pub limit: u32,
}

/// One `run_events` row as stored. Names and nullability follow the columns
/// exactly, so the browser shows what is on disk and nothing that is not:
/// a `None` here means the writer had no value, never that this layer lost it.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct EventRowWire {
    pub run_id: String,
    pub seq: i64,
    /// Epoch milliseconds. The ONLY time this row carries — an entry that
    /// opened something still running has no end, and none is invented.
    pub at_ms: i64,
    /// Event kind slug, identical to the `kind` tag inside `payload_json`.
    pub kind: String,
    /// `FlowOrigin` slug.
    pub origin: String,
    /// `ActorKind` slug.
    pub actor_kind: String,
    #[serde(default)]
    pub actor_id: Option<String>,
    /// The user behind the actor: the user themselves, or the user an API key
    /// is bound to. `None` on a service key with no binding — which the UI
    /// must show as unbound rather than as an empty field.
    #[serde(default)]
    pub actor_user_id: Option<String>,
    /// `None` = no organisation was minted for the run (camera, scheduler,
    /// maintenance), not "unknown tenant".
    #[serde(default)]
    pub org_id: Option<String>,
    #[serde(default)]
    pub correlation_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    /// Flow node that produced the entry, not a mesh node.
    #[serde(default)]
    pub node_id: Option<String>,
    /// Pairs a `tool_call` with its `tool_result`.
    #[serde(default)]
    pub call_id: Option<String>,
    /// The stored payload, already redacted, internally tagged with `kind`.
    pub payload_json: String,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct EventsBrowseResponse {
    pub rows: Vec<EventRowWire>,
    /// Position to resume from; `None` when the server saw the end of the
    /// result set.
    #[serde(default)]
    pub next_cursor: Option<EventsCursor>,
    /// True when the server narrowed the page to the caller's own runs
    /// because they hold `events.read` but not `events.read_all`. The browser
    /// states the scope instead of presenting a partial node as the whole one.
    pub scoped_to_self: bool,
}

/// Timeline of one run, oldest first — the inspector's cursor over
/// `store::read_run`.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Default)]
pub struct EventsRunRequest {
    pub run_id: String,
    /// Exclusive lower bound on `seq`; 0 starts at the beginning.
    #[serde(default)]
    pub after_seq: i64,
    #[serde(default)]
    pub limit: u32,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct EventsRunResponse {
    pub run_id: String,
    pub events: Vec<EventRowWire>,
    /// `after_seq` for the next page, or `None` at the end of the run's log.
    #[serde(default)]
    pub next_after_seq: Option<i64>,
}

/// The whole Events-browser surface behind one `MessageBody::EventsBody`.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub enum EventsPayload {
    BrowseRequest(EventsBrowseRequest),
    BrowseResponse(EventsBrowseResponse),
    RunRequest(EventsRunRequest),
    RunResponse(EventsRunResponse),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;

    fn row() -> EventRowWire {
        EventRowWire {
            run_id: "run-1".into(),
            seq: 3,
            at_ms: 1_760_000_000_000,
            kind: "tool_call".into(),
            origin: "code_studio".into(),
            actor_kind: "api_key".into(),
            actor_id: Some("key-42".into()),
            actor_user_id: None,
            org_id: Some("org-a".into()),
            correlation_id: Some("corr-9".into()),
            session_id: Some("sess-7".into()),
            node_id: Some("llm-1".into()),
            call_id: Some("c-a70".into()),
            payload_json: r#"{"kind":"tool_call","name":"core.fs_read","arguments":{}}"#.into(),
        }
    }

    #[test]
    fn events_browse_roundtrip() {
        let body = MessageBody::EventsBody(EventsPayload::BrowseRequest(EventsBrowseRequest {
            origins: Some(vec!["code_studio".into(), "api".into()]),
            actor_id: Some("key-42".into()),
            org_id: None,
            session_id: None,
            correlation_id: Some("corr-9".into()),
            from_ms: Some(1),
            to_ms: Some(2),
            search: Some("fs_read".into()),
            cursor: Some(EventsCursor {
                at_ms: 5,
                run_id: "run-1".into(),
                seq: 3,
            }),
            limit: 50,
        }));
        let bytes = crate::cbor::encode(&body).expect("encode");
        assert_eq!(crate::cbor::decode::<MessageBody>(&bytes).expect("decode"), body);

        let resp = MessageBody::EventsBody(EventsPayload::BrowseResponse(EventsBrowseResponse {
            rows: vec![row()],
            next_cursor: Some(EventsCursor {
                at_ms: 1_760_000_000_000,
                run_id: "run-1".into(),
                seq: 3,
            }),
            scoped_to_self: true,
        }));
        let bytes = crate::cbor::encode(&resp).expect("encode");
        assert_eq!(crate::cbor::decode::<MessageBody>(&bytes).expect("decode"), resp);
    }

    #[test]
    fn events_run_roundtrip() {
        let req = MessageBody::EventsBody(EventsPayload::RunRequest(EventsRunRequest {
            run_id: "run-1".into(),
            after_seq: 2,
            limit: 100,
        }));
        let bytes = crate::cbor::encode(&req).expect("encode");
        assert_eq!(crate::cbor::decode::<MessageBody>(&bytes).expect("decode"), req);

        let resp = MessageBody::EventsBody(EventsPayload::RunResponse(EventsRunResponse {
            run_id: "run-1".into(),
            events: vec![row()],
            next_after_seq: Some(3),
        }));
        let bytes = crate::cbor::encode(&resp).expect("encode");
        assert_eq!(crate::cbor::decode::<MessageBody>(&bytes).expect("decode"), resp);
    }

    /// A peer that predates a field must still decode the message, which is
    /// the whole reason the optional fields carry `#[serde(default)]`. Encoding
    /// a map that omits every defaulted key is exactly what such a peer sends.
    #[test]
    fn browse_request_decodes_without_the_defaulted_fields() {
        let bare: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
        let bytes = crate::cbor::encode(&bare).expect("encode");
        let decoded: EventsBrowseRequest = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(decoded, EventsBrowseRequest::default());
        assert!(decoded.origins.is_none(), "an absent origin list is no constraint");
    }

    /// Ciborium tags an externally-tagged enum by variant NAME, not by index:
    /// the encoding of a newtype variant is a one-entry map keyed by the name.
    /// That is why appending a variant is safe and why renaming one is not —
    /// and it contradicts the "256 variants by index" claim carried in several
    /// older protocol files. Pinned here rather than argued in a comment.
    #[test]
    fn message_body_is_tagged_by_variant_name() {
        let body = MessageBody::EventsBody(EventsPayload::RunRequest(EventsRunRequest {
            run_id: "run-1".into(),
            after_seq: 0,
            limit: 1,
        }));
        let bytes = crate::cbor::encode(&body).expect("encode");
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("EventsBody"), "outer variant name must be on the wire");
        assert!(text.contains("RunRequest"), "inner variant name must be on the wire");
    }
}
