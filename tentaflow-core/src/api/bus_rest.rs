// =============================================================================
// Plik: api/bus_rest.rs
// Opis: cienki zewnetrzny REST endpoint dla TentaBus (PLAN §6.5/M4) —
//       `POST /v1/bus/topics/{topic}/records` publikuje batch rekordow
//       (CBOR lub NDJSON), `GET /v1/bus/topics/{topic}/records` konsumuje
//       przez long-poll. Dla odbiorcow ABM/CWBK i systemow, ktore nie mowia
//       przez mesh (PLAN §6.5's own framing).
//
// Org resolution (nie okreslone wprost w PLAN §6.5): caly istniejacy `/v1/*`
// surface (openai/server.rs) jest jednoorganizacyjny w praktyce — zaden
// handler tam nie ma pojecia `org_id`, `Principal`/`UserContext` tez go nie
// niosa. TentaBus jest jednak z zalozenia wieloorganizacyjny (`BusCallContext.
// org_id` wymagane wszedzie indziej). Zamiast zgadywac jedna organizacje
// (np. `DEFAULT_ORG_ID`) lub wymyslac nowy bypass authorization, ten endpoint
// wymaga jawnego `?org_id=` I klucza API zwiazanego z realnym uzytkownikiem
// (`Principal::User`) — weryfikuje CZLONKOSTWO w tej organizacji
// (`org::repo::get_user_role_in_org`, fail-closed 403 gdy brak), a nastepnie
// wola `BusService::publish`/`open_consumer` z `ctx.actor = user_id` —
// dokladnie ta sama, juz przetestowana sciezka `RbacBusAuthorizer` (globalny
// RBAC `bus.read`/`bus.write` + per-topic ACL) ktora obsluguje kazdego innego
// wywolujacego busa. Zero nowego bypassu authorization. `Group`/`ApiKey`
// principale (klucze bez zwiazanego uzytkownika) sa poza zakresem tego
// endpointu — nie maja czlonkostwa w zadnej organizacji do zweryfikowania.
// =============================================================================

use crate::api::openai::server::OpenAIBody;
use crate::auth::acl::Principal;
use crate::bus::groups::CommitMode;
use crate::bus::topics::TopicOptions;
use crate::bus::{
    self, BusCallContext, BusServiceError, ConsumerConfig, FetchedRecordMeta, PublishBatch,
    PublishRecord, TopicPartition,
};
use crate::routing::router::Router;

use base64::Engine;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::{Request, Response, StatusCode};

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use tentaflow_sdk_spec::{BusHeader as CborBusHeader, BusPublishInput, BusRecordIn};

/// Same cap as the addon SDK's `bus_publish_v1` (`host_functions/bus.rs`'s
/// `MAX_PUBLISH_RECORDS`) — one REST call is bounded the same way regardless
/// of which boundary (WASM ABI or HTTP) it crosses.
const MAX_PUBLISH_RECORDS: usize = 1000;
/// Mirrors `host_functions/bus.rs`'s `MAX_CONSUME_RECORDS`/`MAX_CONSUME_WAIT_MS`.
const MAX_CONSUME_RECORDS: u32 = 1000;
const DEFAULT_CONSUME_RECORDS: u32 = 100;
const MAX_CONSUME_WAIT_MS: u32 = 5_000;
const DEFAULT_CONSUME_WAIT_MS: u32 = 5_000;
const CONSUME_RECORD_BYTE_ESTIMATE: usize = 1024;

/// True for exactly `POST|GET /v1/bus/topics/{topic}/records` — every other
/// `/v1/bus/...` shape is unhandled (falls through to the normal 404).
pub fn topic_from_records_path(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/v1/bus/topics/")?;
    let topic = rest.strip_suffix("/records")?;
    // Topic names are `^[a-z0-9]([a-z0-9.\-]{1,126})$` (PLAN §7.1) — never
    // contain `/`, so a bare strip-prefix/strip-suffix is an exact match,
    // no path-templating crate needed for this one dynamic segment.
    if topic.is_empty() || topic.contains('/') {
        None
    } else {
        Some(topic)
    }
}

#[derive(Debug, Default)]
struct BusRecordsQuery {
    org_id: Option<String>,
    group: Option<String>,
    max_records: Option<u32>,
    wait_ms: Option<u32>,
    create_if_missing: Option<bool>,
}

/// Same strict-parse shape as `api::legal::parse_query`/`api::frames::parse_query`
/// (duplicate/unknown keys are errors, values are URL-decoded).
fn parse_query(raw: &str) -> std::result::Result<BusRecordsQuery, &'static str> {
    let mut q = BusRecordsQuery::default();
    if raw.is_empty() {
        return Ok(q);
    }
    for piece in raw.split('&') {
        if piece.is_empty() {
            continue;
        }
        let mut it = piece.splitn(2, '=');
        let k = it.next().unwrap_or("");
        let v = it.next().unwrap_or("");
        let decoded = urlencoding::decode(v)
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| v.to_string());
        match k {
            "org_id" => {
                if q.org_id.is_some() {
                    return Err("duplicate_org_id");
                }
                q.org_id = Some(decoded);
            }
            "group" => {
                if q.group.is_some() {
                    return Err("duplicate_group");
                }
                q.group = Some(decoded);
            }
            "max_records" => {
                if q.max_records.is_some() {
                    return Err("duplicate_max_records");
                }
                q.max_records = Some(decoded.parse().map_err(|_| "invalid_max_records")?);
            }
            "wait_ms" => {
                if q.wait_ms.is_some() {
                    return Err("duplicate_wait_ms");
                }
                q.wait_ms = Some(decoded.parse().map_err(|_| "invalid_wait_ms")?);
            }
            "create_if_missing" => {
                if q.create_if_missing.is_some() {
                    return Err("duplicate_create_if_missing");
                }
                q.create_if_missing = Some(decoded == "true" || decoded == "1");
            }
            _ => return Err("unknown_query_key"),
        }
    }
    Ok(q)
}

fn json_response(status: StatusCode, body: Vec<u8>) -> Response<OpenAIBody> {
    let stream = futures::stream::once(async move { Ok(Frame::data(Bytes::from(body))) });
    let boxed_stream: Pin<
        Box<dyn futures::Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>,
    > = Box::pin(stream);
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(StreamBody::new(boxed_stream))
        .unwrap()
}

fn error_response(status: StatusCode, error_type: &str, message: impl Into<String>) -> Response<OpenAIBody> {
    let body = serde_json::json!({
        "error": {
            "type": error_type,
            "message": message.into(),
            "code": error_type,
        }
    });
    json_response(status, serde_json::to_vec(&body).unwrap_or_default())
}

/// Same intent as `host_functions/bus.rs`'s `map_bus_error`, HTTP-flavored.
fn map_bus_error(e: &BusServiceError) -> Response<OpenAIBody> {
    match e {
        BusServiceError::TopicNotFound { .. } => {
            error_response(StatusCode::NOT_FOUND, "not_found_error", e.to_string())
        }
        BusServiceError::PermissionDenied { .. } => {
            error_response(StatusCode::FORBIDDEN, "permission_error", e.to_string())
        }
        BusServiceError::QuotaExceeded { retry_after_ms }
        | BusServiceError::Throttled { retry_after_ms } => {
            let mut resp = error_response(StatusCode::TOO_MANY_REQUESTS, "rate_limit_error", e.to_string());
            resp.headers_mut().insert(
                "Retry-After",
                (retry_after_ms / 1000).max(1).to_string().parse().unwrap(),
            );
            resp
        }
        BusServiceError::QuotaRequestTooLarge { .. }
        | BusServiceError::MaxTopicsExceeded { .. }
        | BusServiceError::MaxPartitionsExceeded { .. } => {
            error_response(StatusCode::TOO_MANY_REQUESTS, "quota_exceeded", e.to_string())
        }
        BusServiceError::PayloadTooLarge { .. } => {
            error_response(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large", e.to_string())
        }
        BusServiceError::TopicAlreadyExists { .. } | BusServiceError::OffsetRegression { .. } => {
            error_response(StatusCode::CONFLICT, "conflict_error", e.to_string())
        }
        BusServiceError::InvalidTopicName { .. }
        | BusServiceError::InvalidTopicConfig { .. }
        | BusServiceError::InvalidArgument(_)
        | BusServiceError::DedupKeyRequired { .. }
        | BusServiceError::NotSubscribed { .. } => {
            error_response(StatusCode::BAD_REQUEST, "invalid_request_error", e.to_string())
        }
        // SUM/tentabus/POLITYKI-POL.md: a field policy rejected the
        // request/payload — same "bad request from this caller" shape as
        // the invalid-argument group above, not a server-side error.
        BusServiceError::FieldNotAllowed { .. }
        | BusServiceError::RequiredFieldMissing { .. }
        | BusServiceError::FieldPolicyPayloadMalformed { .. } => {
            error_response(StatusCode::BAD_REQUEST, "invalid_request_error", e.to_string())
        }
        // SUM/tentabus/PLAN-F3.md: a bound schema subject/version vanished
        // out from under a topic — loud, not silently ignored.
        BusServiceError::SchemaNotFound { .. } | BusServiceError::SchemaVersionNotFound { .. } => {
            error_response(StatusCode::NOT_FOUND, "not_found_error", e.to_string())
        }
        // Same "bad request from this caller" shape as the field-policy
        // group above: a schema-registry write/publish was rejected by
        // caller-controlled input (a violating payload, an incompatible
        // schema change, an unsupported type/operation, or the ~1e-9
        // `schema_ref_id` collision PLAN-F3 §2.1 documents as a loud,
        // caller-visible failure).
        BusServiceError::SchemaViolation { .. }
        | BusServiceError::SchemaIncompatible { .. }
        | BusServiceError::SchemaTypeUnsupported { .. }
        | BusServiceError::SchemaRefIdCollision { .. } => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            e.to_string(),
        ),
        _ => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            e.to_string(),
        ),
    }
}

/// Resolves the caller into `(user_id, org_id)` for a `BusCallContext`, or an
/// error response — see this file's header doc for why `org_id` must be
/// explicit and the principal must be a real user.
fn resolve_actor(
    db: &crate::db::DbPool,
    principal: Option<&Principal>,
    query: &BusRecordsQuery,
) -> std::result::Result<(String, String), Response<OpenAIBody>> {
    let user_id = match principal {
        Some(Principal::User { user_id, .. }) => user_id.clone(),
        Some(_) => {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "TentaBus REST access requires a user-bound API key (a 'group' or 'general' key has no organization to scope this call to)".to_string(),
            ))
        }
        None => {
            return Err(error_response(
                StatusCode::UNAUTHORIZED,
                "authentication_error",
                "Brak Principal dla zadania /v1/bus".to_string(),
            ))
        }
    };
    let org_id = match &query.org_id {
        Some(o) if !o.is_empty() => o.clone(),
        _ => {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "missing required query parameter 'org_id'".to_string(),
            ))
        }
    };
    match crate::services::org::repo::get_user_role_in_org(db, &user_id, &org_id) {
        Ok(Some(_)) => Ok((user_id, org_id)),
        Ok(None) => Err(error_response(
            StatusCode::FORBIDDEN,
            "permission_error",
            format!("user has no membership in org '{org_id}'"),
        )),
        Err(e) => Err(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            format!("org membership lookup failed: {e}"),
        )),
    }
}

// ---- POST /v1/bus/topics/{topic}/records -----------------------------------

/// One NDJSON line — mirrors `BusRecordIn`'s shape (key/headers/payload), but
/// JSON-safe: byte fields are base64. `key`/`headers` are optional (default
/// keyless, no headers); `payload_b64` is the only required field.
#[derive(serde::Deserialize)]
struct NdjsonRecord {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    headers: HashMap<String, String>,
    payload_b64: String,
}

fn decode_b64(s: &str) -> std::result::Result<Vec<u8>, &'static str> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|_| "invalid_base64")
}

fn parse_ndjson_records(body: &[u8]) -> std::result::Result<Vec<PublishRecord>, String> {
    let text = std::str::from_utf8(body).map_err(|_| "body is not valid UTF-8".to_string())?;
    let now = chrono::Utc::now().timestamp_millis();
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let rec: NdjsonRecord = serde_json::from_str(line)
            .map_err(|e| format!("line {}: invalid JSON record: {e}", i + 1))?;
        let key = match rec.key {
            Some(k) => Some(Bytes::from(
                decode_b64(&k).map_err(|e| format!("line {}: key: {e}", i + 1))?,
            )),
            None => None,
        };
        let payload = decode_b64(&rec.payload_b64)
            .map_err(|e| format!("line {}: payload_b64: {e}", i + 1))?;
        let headers = rec
            .headers
            .into_iter()
            .map(|(k, v)| -> std::result::Result<(String, Bytes), String> {
                Ok((k, Bytes::from(decode_b64(&v).map_err(|e| format!("line {}: header value: {e}", i + 1))?)))
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        out.push(PublishRecord {
            key,
            headers,
            payload: Bytes::from(payload),
            timestamp_ms: now,
            schema_id: 0,
        });
    }
    Ok(out)
}

fn cbor_records_to_publish(records: Vec<BusRecordIn>) -> Vec<PublishRecord> {
    let now = chrono::Utc::now().timestamp_millis();
    records
        .into_iter()
        .map(|r| PublishRecord {
            key: r.key.map(Bytes::from),
            headers: r
                .headers
                .into_iter()
                .map(|h: CborBusHeader| (h.name, Bytes::from(h.value)))
                .collect(),
            payload: Bytes::from(r.payload),
            timestamp_ms: now,
            schema_id: 0,
        })
        .collect()
}

pub async fn handle_publish(
    req: Request<Incoming>,
    router: Arc<Router>,
    topic: String,
) -> std::result::Result<Response<OpenAIBody>, hyper::Error> {
    let Some(db) = router.db.as_ref() else {
        return Ok(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "internal_error",
            "database unavailable".to_string(),
        ));
    };
    let principal = req.extensions().get::<Principal>().cloned();
    let is_cbor = req
        .headers()
        .get(hyper::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.starts_with("application/cbor"))
        .unwrap_or(false);
    let query = match parse_query(req.uri().query().unwrap_or("")) {
        Ok(q) => q,
        Err(e) => return Ok(error_response(StatusCode::BAD_REQUEST, "invalid_request_error", e.to_string())),
    };
    let (user_id, org_id) = match resolve_actor(db, principal.as_ref(), &query) {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };

    let body_bytes = req.collect().await?.to_bytes();

    let (records, create_if_missing) = if is_cbor {
        let input: BusPublishInput = match minicbor::decode(&body_bytes) {
            Ok(v) => v,
            Err(e) => {
                return Ok(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    format!("invalid CBOR body: {e}"),
                ))
            }
        };
        (
            cbor_records_to_publish(input.records),
            input.create_if_missing.unwrap_or(false),
        )
    } else {
        let records = match parse_ndjson_records(&body_bytes) {
            Ok(r) => r,
            Err(msg) => return Ok(error_response(StatusCode::BAD_REQUEST, "invalid_request_error", msg)),
        };
        (records, query.create_if_missing.unwrap_or(false))
    };

    if records.is_empty() {
        return Ok(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "publish batch has no records".to_string(),
        ));
    }
    if records.len() > MAX_PUBLISH_RECORDS {
        return Ok(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!("batch of {} records exceeds the {MAX_PUBLISH_RECORDS} limit", records.len()),
        ));
    }

    let Some(svc) = bus::global() else {
        return Ok(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "internal_error",
            "bus service not initialized".to_string(),
        ));
    };
    let ctx = BusCallContext {
        instance_id: bus::instance::BusInstanceId::parse(svc.instance_id())
            .expect("BusService::instance_id() is always a valid BusInstanceId"),
        org_id,
        actor: Some(user_id),
        correlation_id: None,
        origin: "v1.bus.rest".to_string(),
    };
    let batch = PublishBatch {
        partition: None,
        producer: None,
        records,
    };
    // Same create-if-missing retry shape as `host_functions/bus.rs`'s
    // `bus_publish_v1`: try the publish first, and only pay for a
    // `create_topic` round-trip on the (rare) miss.
    let result = match svc.publish(&ctx, &topic, batch.clone()) {
        Ok(r) => Ok(r),
        Err(BusServiceError::TopicNotFound { .. }) if create_if_missing => {
            svc.create_topic(&ctx, &topic, TopicOptions::default())
                .and_then(|_| svc.publish(&ctx, &topic, batch))
        }
        Err(e) => Err(e),
    };

    match result {
        Ok(r) => {
            // `schema_rejected` (PLAN-F3 §4.5): records quarantined to the
            // DLQ under `validation = dlq`; additive, always present.
            let body = serde_json::json!({
                "published": r.accepted,
                "schema_rejected": r.schema_rejected,
            });
            Ok(json_response(StatusCode::OK, serde_json::to_vec(&body).unwrap_or_default()))
        }
        Err(e) => Ok(map_bus_error(&e)),
    }
}

// ---- GET /v1/bus/topics/{topic}/records -------------------------------------

fn record_to_json(r: FetchedRecordMeta) -> serde_json::Value {
    serde_json::json!({
        "topic": r.topic,
        "partition": r.partition,
        "offset": r.offset,
        "timestamp_ms": r.timestamp_ms,
        "key": r.key.map(|b| base64::engine::general_purpose::STANDARD.encode(b)),
        "headers": r
            .headers
            .into_iter()
            .map(|(k, v)| (
                String::from_utf8_lossy(&k).into_owned(),
                base64::engine::general_purpose::STANDARD.encode(v),
            ))
            .collect::<HashMap<String, String>>(),
        "payload": base64::engine::general_purpose::STANDARD.encode(r.payload),
    })
}

pub async fn handle_consume(
    req: Request<Incoming>,
    router: Arc<Router>,
    topic: String,
) -> std::result::Result<Response<OpenAIBody>, hyper::Error> {
    let Some(db) = router.db.as_ref() else {
        return Ok(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "internal_error",
            "database unavailable".to_string(),
        ));
    };
    let principal = req.extensions().get::<Principal>().cloned();
    let query = match parse_query(req.uri().query().unwrap_or("")) {
        Ok(q) => q,
        Err(e) => return Ok(error_response(StatusCode::BAD_REQUEST, "invalid_request_error", e.to_string())),
    };
    let Some(group) = query.group.clone().filter(|g| !g.is_empty()) else {
        return Ok(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "missing required query parameter 'group'".to_string(),
        ));
    };
    let (user_id, org_id) = match resolve_actor(db, principal.as_ref(), &query) {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let max_records = query
        .max_records
        .unwrap_or(DEFAULT_CONSUME_RECORDS)
        .clamp(1, MAX_CONSUME_RECORDS);
    let max_wait_ms = query.wait_ms.unwrap_or(DEFAULT_CONSUME_WAIT_MS).min(MAX_CONSUME_WAIT_MS);
    let max_bytes = (max_records as usize)
        .saturating_mul(CONSUME_RECORD_BYTE_ESTIMATE)
        .max(64 * 1024);

    let Some(svc) = bus::global() else {
        return Ok(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "internal_error",
            "bus service not initialized".to_string(),
        ));
    };
    let ctx = BusCallContext {
        instance_id: bus::instance::BusInstanceId::parse(svc.instance_id())
            .expect("BusService::instance_id() is always a valid BusInstanceId"),
        org_id,
        actor: Some(user_id),
        correlation_id: None,
        origin: "v1.bus.rest".to_string(),
    };
    let handle = match svc.open_consumer(
        &ctx,
        &group,
        std::slice::from_ref(&topic),
        ConsumerConfig {
            commit_mode: CommitMode::Explicit,
        },
    ) {
        Ok(h) => h,
        Err(e) => return Ok(map_bus_error(&e)),
    };

    // `ConsumerHandle::fetch` blocks the calling thread for up to
    // `max_wait_ms` (its own doc: callers on a Tokio executor MUST NOT call
    // it directly from an async fn) — `block_in_place` is the same pattern
    // `host_functions/bus.rs`'s `bus_consume_next_v1` already uses for this
    // exact call.
    let fetched = tokio::task::block_in_place(|| handle.fetch(max_bytes, max_wait_ms));
    let batch = match fetched {
        Ok(b) => b,
        Err(e) => return Ok(map_bus_error(&e)),
    };

    if batch.records.is_empty() {
        let body = serde_json::json!({ "records": [] });
        return Ok(json_response(StatusCode::OK, serde_json::to_vec(&body).unwrap_or_default()));
    }

    // At-least-once: commit right after a successful HTTP response is built,
    // to the offset one past the highest fetched per partition. This thin
    // endpoint has no separate commit call (PLAN §6.5 does not define one),
    // so auto-commit-on-delivery is the only complete, non-half-finished
    // contract available here — a caller that needs exactly-once or
    // explicit ack should consume through the mesh/addon path instead.
    let mut max_offset: HashMap<u32, u64> = HashMap::new();
    for r in &batch.records {
        max_offset
            .entry(r.partition)
            .and_modify(|o| *o = (*o).max(r.offset))
            .or_insert(r.offset);
    }
    let commit_offsets: Vec<(TopicPartition, u64)> = max_offset
        .into_iter()
        .map(|(partition, offset)| {
            (
                TopicPartition {
                    topic: topic.clone(),
                    partition,
                },
                offset + 1,
            )
        })
        .collect();

    let records_json: Vec<serde_json::Value> = batch.records.into_iter().map(record_to_json).collect();
    if let Err(e) = handle.commit(&commit_offsets) {
        // The records were already fetched and are about to be returned to
        // the caller — a commit failure here must not silently drop them,
        // but it does mean a redelivery is possible on the next poll (the
        // same at-least-once trade-off `note_delivery_failure`'s retry path
        // already makes elsewhere in this codebase).
        tracing::warn!(topic = %topic, group = %group, error = %e, "v1 bus REST: post-fetch commit failed, records already returned to caller");
    }

    let body = serde_json::json!({ "records": records_json });
    Ok(json_response(StatusCode::OK, serde_json::to_vec(&body).unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_from_records_path_matches_exact_shape() {
        assert_eq!(
            topic_from_records_path("/v1/bus/topics/orders.created/records"),
            Some("orders.created")
        );
    }

    #[test]
    fn topic_from_records_path_rejects_wrong_shapes() {
        assert_eq!(topic_from_records_path("/v1/bus/topics/records"), None);
        assert_eq!(topic_from_records_path("/v1/bus/topics//records"), None);
        assert_eq!(topic_from_records_path("/v1/bus/topics/a/b/records"), None);
        assert_eq!(topic_from_records_path("/v1/bus/topics/orders.created"), None);
        assert_eq!(topic_from_records_path("/v1/models"), None);
    }

    #[test]
    fn parse_query_reads_all_known_keys() {
        let q = parse_query("org_id=org-1&group=g1&max_records=50&wait_ms=2000&create_if_missing=true").unwrap();
        assert_eq!(q.org_id.as_deref(), Some("org-1"));
        assert_eq!(q.group.as_deref(), Some("g1"));
        assert_eq!(q.max_records, Some(50));
        assert_eq!(q.wait_ms, Some(2000));
        assert_eq!(q.create_if_missing, Some(true));
    }

    #[test]
    fn parse_query_rejects_unknown_and_duplicate_keys() {
        assert_eq!(parse_query("bogus=1").unwrap_err(), "unknown_query_key");
        assert_eq!(parse_query("org_id=a&org_id=b").unwrap_err(), "duplicate_org_id");
    }

    #[test]
    fn parse_ndjson_records_decodes_base64_payload() {
        let payload_b64 = base64::engine::general_purpose::STANDARD.encode(b"hello");
        let line = format!(r#"{{"payload_b64":"{payload_b64}"}}"#);
        let records = parse_ndjson_records(line.as_bytes()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].payload, Bytes::from_static(b"hello"));
        assert!(records[0].key.is_none());
    }

    #[test]
    fn parse_ndjson_records_skips_blank_lines() {
        let payload_b64 = base64::engine::general_purpose::STANDARD.encode(b"x");
        let body = format!("\n  \n{{\"payload_b64\":\"{payload_b64}\"}}\n\n");
        let records = parse_ndjson_records(body.as_bytes()).unwrap();
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn parse_ndjson_records_rejects_invalid_base64() {
        let err = parse_ndjson_records(br#"{"payload_b64":"not-base64!!"}"#).unwrap_err();
        assert!(err.contains("payload_b64"));
    }
}
