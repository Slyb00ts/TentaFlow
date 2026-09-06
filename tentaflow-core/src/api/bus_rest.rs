// =============================================================================
// Plik: api/bus_rest.rs
// Opis: cienki zewnetrzny REST endpoint dla TentaBus (PLAN §6.5/M4) —
//       `POST /v1/bus/instances/{instance_id}/topics/{topic}/records`
//       publikuje batch rekordow (CBOR lub NDJSON), `GET` na tej samej
//       sciezce konsumuje przez long-poll (plan-app-platform §3.2). Dla
//       odbiorcow ABM/CWBK i systemow, ktore nie mowia przez mesh (PLAN
//       §6.5's own framing).
//
// Instance resolution (plan-app-platform §3.2): `instance_id` w sciezce
// jest walidowany ksztaltem (`BusInstanceId::parse`) PRZED jakimkolwiek
// odczytem z bazy, a nastepnie wymaga byc zainstalowana-i-wlaczona instancja
// TentaBus (`app_gate::instance_enabled`) — instancja wylaczona i instancja
// nigdy nie zainstalowana odpowiadaja identycznie (404
// `bus_instance_not_found`), zeby nie zdradzac ktora z nich to przypadek.
// Legacy sciezka bez segmentu instancji (`/v1/bus/topics/{topic}/records`)
// jest jedynym dopuszczonym kompatybilnosciowym skrotem: rozwiazuje sie
// przez `app_gate::sole_enabled_instance` — dokladnie jedna wlaczona
// instancja, albo 404 (zero), albo 409 `bus_instance_ambiguous` z lista
// kandydatow i wskazaniem nowej, jednoznacznej sciezki. W obu przypadkach
// silnik jest pobierany przez `bus::instance(&id)` (rejestr per-instancja),
// NIGDY przez `bus::global()` — zadanie zaadresowane do jednej instancji nie
// moze nigdy trafic do innej, nawet gdy ta inna akurat dziala sama na wezle.
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
use crate::bus::instance::BusInstanceId;
use crate::bus::topics::TopicOptions;
use crate::bus::{
    self, BusCallContext, BusServiceError, ConsumerConfig, FetchedRecordMeta, PublishBatch,
    PublishRecord, TopicPartition,
};
use crate::dispatch::app_gate::{self, SoleInstanceError};
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

/// Matches the primary, path-scoped form `POST|GET
/// /v1/bus/instances/{instance_id}/topics/{topic}/records`
/// (plan-app-platform §3.2) and the legacy, instance-less form `POST|GET
/// /v1/bus/topics/{topic}/records`. Returns `(instance, topic)` — `instance`
/// is `Some` only for the new form; the legacy form resolves through
/// `app_gate::sole_enabled_instance` instead (§3.2's one permitted
/// compatibility affordance). Every other `/v1/bus/...` shape is unhandled
/// (falls through to the normal 404).
///
/// Neither segment is templating-crate material: topic names are
/// `^[a-z0-9]([a-z0-9.\-]{1,126})$` (PLAN §7.1) and instance ids are
/// `^tentabus-[0-9a-f]{8}$` (`BusInstanceId::parse`) — neither ever contains
/// `/`, so a bare split/strip is an exact match for both. An empty instance
/// segment (a doubled slash, e.g. `.../instances//topics/...`) and a topic
/// segment containing `/` (extra path segments) are both rejected here,
/// before `BusInstanceId::parse` ever runs — that parse is the shape's
/// SECOND check (§3.2: validated before any DB read), not its first.
pub fn parse_bus_records_path(path: &str) -> Option<(Option<&str>, &str)> {
    if let Some(rest) = path.strip_prefix("/v1/bus/instances/") {
        let (instance, after_instance) = rest.split_once("/topics/")?;
        let topic = after_instance.strip_suffix("/records")?;
        if instance.is_empty() || instance.contains('/') || topic.is_empty() || topic.contains('/')
        {
            return None;
        }
        return Some((Some(instance), topic));
    }
    let rest = path.strip_prefix("/v1/bus/topics/")?;
    let topic = rest.strip_suffix("/records")?;
    if topic.is_empty() || topic.contains('/') {
        None
    } else {
        Some((None, topic))
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

fn error_response(
    status: StatusCode,
    error_type: &str,
    message: impl Into<String>,
) -> Response<OpenAIBody> {
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
            let mut resp = error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limit_error",
                e.to_string(),
            );
            resp.headers_mut().insert(
                "Retry-After",
                (retry_after_ms / 1000).max(1).to_string().parse().unwrap(),
            );
            resp
        }
        BusServiceError::QuotaRequestTooLarge { .. }
        | BusServiceError::MaxTopicsExceeded { .. }
        | BusServiceError::MaxPartitionsExceeded { .. } => error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "quota_exceeded",
            e.to_string(),
        ),
        BusServiceError::PayloadTooLarge { .. } => error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            e.to_string(),
        ),
        BusServiceError::TopicAlreadyExists { .. } | BusServiceError::OffsetRegression { .. } => {
            error_response(StatusCode::CONFLICT, "conflict_error", e.to_string())
        }
        BusServiceError::InvalidTopicName { .. }
        | BusServiceError::InvalidTopicConfig { .. }
        | BusServiceError::InvalidArgument(_)
        | BusServiceError::DedupKeyRequired { .. }
        | BusServiceError::NotSubscribed { .. } => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            e.to_string(),
        ),
        // SUM/tentabus/POLITYKI-POL.md: a field policy rejected the
        // request/payload — same "bad request from this caller" shape as
        // the invalid-argument group above, not a server-side error.
        BusServiceError::FieldNotAllowed { .. }
        | BusServiceError::RequiredFieldMissing { .. }
        | BusServiceError::FieldPolicyPayloadMalformed { .. } => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            e.to_string(),
        ),
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

// ---- Instance resolution (plan-app-platform §3.2) --------------------------

/// The flat `{"error": "...", ["instances": [...]], ["message": "..."]}`
/// shape §3.2 specifies for instance-resolution failures — distinct from
/// `error_response`'s nested `{"error": {"type", "message", "code"}}` shape
/// (kept for `BusServiceError`s, unchanged): a caller distinguishing "no
/// instance" from "ambiguous, pick one of these" needs a short machine
/// code and, for the ambiguous case, the actual candidate list — not prose
/// wrapped in an object one level deeper.
fn instance_error_response(
    status: StatusCode,
    error: &str,
    instances: Option<&[String]>,
    message: Option<String>,
) -> Response<OpenAIBody> {
    let mut body = serde_json::json!({ "error": error });
    if let Some(instances) = instances {
        body["instances"] = serde_json::json!(instances);
    }
    if let Some(message) = message {
        body["message"] = serde_json::json!(message);
    }
    json_response(status, serde_json::to_vec(&body).unwrap_or_default())
}

/// Every currently ENABLED instance of the `tentabus` package, for the 409
/// `bus_instance_ambiguous` body's `instances` list. Best-effort: a lookup
/// failure here (vanishingly unlikely right after `sole_enabled_instance`
/// itself just succeeded at listing the same table) degrades to an empty
/// list rather than turning an already-decided 409 into a 500.
fn enabled_instance_ids(db: &crate::db::DbPool) -> Vec<String> {
    crate::db::repository::list_package_instances(db, BusInstanceId::PACKAGE_ID)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, enabled, _)| *enabled)
        .map(|(addon_id, _, _)| addon_id)
        .collect()
}

/// Resolves the REST caller's target `BusInstanceId` — from the new
/// path-scoped form (§3.2) when the caller named one, or (the one permitted
/// compatibility affordance, symmetric with the SDK's own default, §3.4)
/// through `app_gate::sole_enabled_instance` for the legacy form.
/// `BusInstanceId::parse` — shape only — runs BEFORE any DB read either way;
/// existence, package membership and enabled state are `app_gate`'s job
/// right after. A named instance that is disabled and one that was simply
/// never installed answer identically (`bus_instance_not_found`): a caller
/// gets no signal to distinguish "typo" from "turned off", the same
/// uniform-unavailable shape `dispatch::app_gate` uses elsewhere.
fn resolve_instance(
    db: &crate::db::DbPool,
    instance: Option<&str>,
) -> std::result::Result<BusInstanceId, Response<OpenAIBody>> {
    match instance {
        Some(raw) => {
            let id = BusInstanceId::parse(raw).map_err(|e| {
                error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    e.to_string(),
                )
            })?;
            if !app_gate::instance_enabled(db, BusInstanceId::PACKAGE_ID, id.as_str()) {
                return Err(instance_error_response(
                    StatusCode::NOT_FOUND,
                    "bus_instance_not_found",
                    None,
                    None,
                ));
            }
            Ok(id)
        }
        None => match app_gate::sole_enabled_instance(db, BusInstanceId::PACKAGE_ID) {
            Ok(addon_id) => BusInstanceId::parse(&addon_id).map_err(|e| {
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    e.to_string(),
                )
            }),
            Err(SoleInstanceError::None) | Err(SoleInstanceError::Disabled) => {
                Err(instance_error_response(
                    StatusCode::NOT_FOUND,
                    "bus_instance_not_found",
                    None,
                    None,
                ))
            }
            Err(SoleInstanceError::Ambiguous(_)) => {
                let instances = enabled_instance_ids(db);
                Err(instance_error_response(
                    StatusCode::CONFLICT,
                    "bus_instance_ambiguous",
                    Some(&instances),
                    Some(
                        "more than one TentaBus instance is enabled — address one explicitly: \
                         POST|GET /v1/bus/instances/{instance_id}/topics/{topic}/records"
                            .to_string(),
                    ),
                ))
            }
            Err(SoleInstanceError::Lookup) => Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "bus instance lookup failed".to_string(),
            )),
        },
    }
}

/// `resolve_instance` plus the running-engine lookup — replaces both
/// `bus::global()` call sites (`handle_publish`/`handle_consume`).
/// `bus::instance(&id)` returning `None` means the instance is enabled in
/// the DB but this node has no engine for it yet (a narrow boot/enable
/// race) — `SERVICE_UNAVAILABLE` naming that instance, never a silent
/// fallback to whichever OTHER instance happens to be running on this node:
/// that fallback is the exact cross-instance leak this endpoint must not
/// have.
fn resolve_engine(
    db: &crate::db::DbPool,
    instance: Option<&str>,
) -> std::result::Result<(BusInstanceId, Arc<bus::BusService>), Response<OpenAIBody>> {
    let id = resolve_instance(db, instance)?;
    match bus::instance(&id) {
        Some(svc) => Ok((id, svc)),
        None => Err(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "internal_error",
            format!("bus instance '{}' is not running on this node", id.as_str()),
        )),
    }
}

// ---- POST /v1/bus/instances/{instance_id}/topics/{topic}/records -----------

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
                Ok((
                    k,
                    Bytes::from(
                        decode_b64(&v).map_err(|e| format!("line {}: header value: {e}", i + 1))?,
                    ),
                ))
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
    instance: Option<String>,
    topic: String,
) -> std::result::Result<Response<OpenAIBody>, hyper::Error> {
    let Some(db) = router.db.as_ref() else {
        return Ok(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "internal_error",
            "database unavailable".to_string(),
        ));
    };
    // Resolved (and the running engine looked up) before touching the
    // request body — §3.2: a request addressed to instance B must never
    // fall back to A, and a malformed/unavailable instance should fail as
    // cheaply as possible.
    let (instance_id, svc) = match resolve_engine(db, instance.as_deref()) {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
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
        Err(e) => {
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                e.to_string(),
            ))
        }
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
            Err(msg) => {
                return Ok(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    msg,
                ))
            }
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
            format!(
                "batch of {} records exceeds the {MAX_PUBLISH_RECORDS} limit",
                records.len()
            ),
        ));
    }

    let ctx = BusCallContext {
        instance_id,
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
    //
    // Both `publish` and `create_topic` block the calling thread
    // (`Partition::append_batch` ends in `blocking_recv` on the writer
    // thread's channel), and `bus::mod`'s own doc requires every async
    // caller to hand that off — `block_in_place` here, the same way
    // `handle_consume` below already does it for `ConsumerHandle::fetch`.
    // Called straight from this async fn it panicked ("Cannot block the
    // current thread from within a runtime") and the client saw an empty
    // reply on a killed connection, not an error: every REST publish failed
    // that way.
    let result = tokio::task::block_in_place(|| match svc.publish(&ctx, &topic, batch.clone()) {
        Ok(r) => Ok(r),
        Err(BusServiceError::TopicNotFound { .. }) if create_if_missing => svc
            .create_topic(&ctx, &topic, TopicOptions::default())
            .and_then(|_| svc.publish(&ctx, &topic, batch)),
        Err(e) => Err(e),
    });

    match result {
        Ok(r) => {
            // `schema_rejected` (PLAN-F3 §4.5): records quarantined to the
            // DLQ under `validation = dlq`; additive, always present.
            let body = serde_json::json!({
                "published": r.accepted,
                "schema_rejected": r.schema_rejected,
            });
            Ok(json_response(
                StatusCode::OK,
                serde_json::to_vec(&body).unwrap_or_default(),
            ))
        }
        Err(e) => Ok(map_bus_error(&e)),
    }
}

// ---- GET /v1/bus/instances/{instance_id}/topics/{topic}/records ------------

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
    instance: Option<String>,
    topic: String,
) -> std::result::Result<Response<OpenAIBody>, hyper::Error> {
    let Some(db) = router.db.as_ref() else {
        return Ok(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "internal_error",
            "database unavailable".to_string(),
        ));
    };
    // See `handle_publish`'s identical comment — resolved before anything
    // else so an addressed-but-unavailable instance never falls back.
    let (instance_id, svc) = match resolve_engine(db, instance.as_deref()) {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let principal = req.extensions().get::<Principal>().cloned();
    let query = match parse_query(req.uri().query().unwrap_or("")) {
        Ok(q) => q,
        Err(e) => {
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                e.to_string(),
            ))
        }
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
    let max_wait_ms = query
        .wait_ms
        .unwrap_or(DEFAULT_CONSUME_WAIT_MS)
        .min(MAX_CONSUME_WAIT_MS);
    let max_bytes = (max_records as usize)
        .saturating_mul(CONSUME_RECORD_BYTE_ESTIMATE)
        .max(64 * 1024);

    let ctx = BusCallContext {
        instance_id,
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
        return Ok(json_response(
            StatusCode::OK,
            serde_json::to_vec(&body).unwrap_or_default(),
        ));
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

    let records_json: Vec<serde_json::Value> =
        batch.records.into_iter().map(record_to_json).collect();
    if let Err(e) = handle.commit(&commit_offsets) {
        // The records were already fetched and are about to be returned to
        // the caller — a commit failure here must not silently drop them,
        // but it does mean a redelivery is possible on the next poll (the
        // same at-least-once trade-off `note_delivery_failure`'s retry path
        // already makes elsewhere in this codebase).
        tracing::warn!(topic = %topic, group = %group, error = %e, "v1 bus REST: post-fetch commit failed, records already returned to caller");
    }

    let body = serde_json::json!({ "records": records_json });
    Ok(json_response(
        StatusCode::OK,
        serde_json::to_vec(&body).unwrap_or_default(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bus_records_path_matches_the_legacy_shape() {
        assert_eq!(
            parse_bus_records_path("/v1/bus/topics/orders.created/records"),
            Some((None, "orders.created"))
        );
    }

    #[test]
    fn parse_bus_records_path_matches_the_new_instance_scoped_shape() {
        assert_eq!(
            parse_bus_records_path(
                "/v1/bus/instances/tentabus-a1b2c3d4/topics/orders.created/records"
            ),
            Some((Some("tentabus-a1b2c3d4"), "orders.created"))
        );
    }

    #[test]
    fn parse_bus_records_path_rejects_legacy_wrong_shapes() {
        assert_eq!(parse_bus_records_path("/v1/bus/topics/records"), None);
        assert_eq!(parse_bus_records_path("/v1/bus/topics//records"), None);
        assert_eq!(parse_bus_records_path("/v1/bus/topics/a/b/records"), None);
        assert_eq!(
            parse_bus_records_path("/v1/bus/topics/orders.created"),
            None
        );
        assert_eq!(parse_bus_records_path("/v1/models"), None);
    }

    #[test]
    fn parse_bus_records_path_rejects_an_empty_instance_segment() {
        // A doubled slash where the instance id should be.
        assert_eq!(
            parse_bus_records_path("/v1/bus/instances//topics/orders/records"),
            None
        );
    }

    #[test]
    fn parse_bus_records_path_rejects_a_topic_containing_a_slash() {
        assert_eq!(
            parse_bus_records_path(
                "/v1/bus/instances/tentabus-a1b2c3d4/topics/orders/created/records"
            ),
            None
        );
    }

    #[test]
    fn parse_bus_records_path_rejects_extra_segments() {
        // An extra segment folded into the instance id (before `/topics/`).
        assert_eq!(
            parse_bus_records_path(
                "/v1/bus/instances/tentabus-a1b2c3d4/extra/topics/orders/records"
            ),
            None
        );
        // An extra trailing segment after `/records`.
        assert_eq!(
            parse_bus_records_path(
                "/v1/bus/instances/tentabus-a1b2c3d4/topics/orders/records/extra"
            ),
            None
        );
        // No `/topics/` separator at all.
        assert_eq!(
            parse_bus_records_path("/v1/bus/instances/tentabus-a1b2c3d4/records"),
            None
        );
    }

    #[test]
    fn parse_query_reads_all_known_keys() {
        let q =
            parse_query("org_id=org-1&group=g1&max_records=50&wait_ms=2000&create_if_missing=true")
                .unwrap();
        assert_eq!(q.org_id.as_deref(), Some("org-1"));
        assert_eq!(q.group.as_deref(), Some("g1"));
        assert_eq!(q.max_records, Some(50));
        assert_eq!(q.wait_ms, Some(2000));
        assert_eq!(q.create_if_missing, Some(true));
    }

    #[test]
    fn parse_query_rejects_unknown_and_duplicate_keys() {
        assert_eq!(parse_query("bogus=1").unwrap_err(), "unknown_query_key");
        assert_eq!(
            parse_query("org_id=a&org_id=b").unwrap_err(),
            "duplicate_org_id"
        );
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

    // ---- Instance resolution / cross-instance isolation (plan-app-platform §3.2) ----

    /// Local double of the process-wide test authorizer every `bus::mod`
    /// test module keeps its own copy of (`AllowAllAuthorizer`'s doc there
    /// notes it is intentionally not shared: it is a private `#[cfg(test)]`
    /// item). This suite is about instance ROUTING, not RBAC, so an
    /// always-allow authorizer keeps the fixtures focused.
    struct AllowAllAuthorizer;
    impl bus::BusAuthorizer for AllowAllAuthorizer {
        fn authorize(
            &self,
            _ctx: &BusCallContext,
            _action: bus::BusAction,
            _topic: &str,
        ) -> std::result::Result<(), BusServiceError> {
            Ok(())
        }
        fn authorize_group(
            &self,
            _ctx: &BusCallContext,
            _action: bus::BusAction,
            _topic: &str,
            _group: &str,
        ) -> std::result::Result<(), BusServiceError> {
            Ok(())
        }
        fn generation(&self) -> u64 {
            0
        }
    }

    fn test_state() -> Arc<crate::dispatch::state::AppState> {
        crate::dispatch::state::AppState::for_test()
    }

    /// Installs an ENABLED `tentabus` instance (`suffix` must be 8 lowercase
    /// hex chars — `BusInstanceId::parse`'s shape) and starts a real,
    /// registry-visible engine for it (`bus::init_instance`, exactly what
    /// `resolve_engine`'s `bus::instance` lookup reads from). The returned
    /// `TempDir` must outlive every use of the engine.
    fn start_test_instance(
        state: &Arc<crate::dispatch::state::AppState>,
        suffix: &str,
    ) -> (tempfile::TempDir, BusInstanceId, Arc<bus::BusService>) {
        let addon_id = app_gate::test_support::install_app_instance(
            state,
            BusInstanceId::PACKAGE_ID,
            suffix,
            &[],
        );
        let id = BusInstanceId::parse(&addon_id).expect("test suffix produces a valid instance id");
        let dir = tempfile::tempdir().expect("bus dir");
        let local_conn = rusqlite::Connection::open_in_memory().expect("open local db");
        crate::bus::db::migrate(&local_conn).expect("migrate local db");
        let local_db: crate::db::DbPool = Arc::new(crate::db::Db::from_connection(local_conn));
        let svc = bus::init_instance(bus::BusInitConfig {
            instance_id: id.clone(),
            local_db,
            bus_dir: dir.path().to_path_buf(),
            db: state.db.clone(),
            authorizer: Arc::new(AllowAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .expect("bus init_instance");
        (dir, id, svc)
    }

    #[test]
    fn resolve_instance_rejects_a_disabled_instance() {
        let state = test_state();
        let (_dir, id, _svc) = start_test_instance(&state, "aaaa1001");
        crate::db::repository::set_addon_enabled(&state.db, id.as_str(), false)
            .expect("disable instance");
        let resp = resolve_instance(&state.db, Some(id.as_str())).unwrap_err();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn resolve_instance_rejects_an_id_that_is_well_formed_but_not_installed() {
        let state = test_state();
        // Shape-valid, never installed.
        let resp = resolve_instance(&state.db, Some("tentabus-deadbeef")).unwrap_err();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn resolve_instance_rejects_a_malformed_id_before_any_db_read() {
        let state = test_state();
        let resp = resolve_instance(&state.db, Some("../../etc/passwd")).unwrap_err();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn resolve_instance_legacy_path_404_when_none_enabled() {
        let state = test_state();
        let resp = resolve_instance(&state.db, None).unwrap_err();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn resolve_instance_legacy_path_resolves_the_sole_enabled_instance() {
        let state = test_state();
        let (_dir, id, _svc) = start_test_instance(&state, "aaaa1002");
        let resolved = resolve_instance(&state.db, None)
            .map_err(|r| r.status())
            .expect("sole enabled instance");
        assert_eq!(resolved, id);
    }

    #[test]
    fn resolve_instance_legacy_path_is_ambiguous_when_two_instances_enabled() {
        let state = test_state();
        let (_dir_a, id_a, _svc_a) = start_test_instance(&state, "aaaa1003");
        let (_dir_b, id_b, _svc_b) = start_test_instance(&state, "aaaa1004");
        let resp = resolve_instance(&state.db, None).unwrap_err();
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        let body_bytes = collect_body(resp);
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(json["error"], "bus_instance_ambiguous");
        let mut instances: Vec<String> = json["instances"]
            .as_array()
            .expect("instances array")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        instances.sort();
        let mut expected = vec![id_a.as_str().to_string(), id_b.as_str().to_string()];
        expected.sort();
        assert_eq!(instances, expected);
        // §3.2: the message must name the new, unambiguous path form.
        let message = json["message"].as_str().expect("message present");
        assert!(message.contains("/v1/bus/instances/"));
    }

    /// The cross-instance guarantee this whole change exists for (plan-app-
    /// platform's owner requirement): with two real, running engines A and
    /// B, `resolve_engine` addressed to B must resolve to B's OWN service —
    /// never A's — so a fetch on B can never observe a record published
    /// only to A, even though both share the identical org/topic/group
    /// names and the same underlying platform `db`.
    #[test]
    fn resolve_engine_never_returns_another_instances_records() {
        let state = test_state();
        let (_dir_a, id_a, svc_a) = start_test_instance(&state, "bbbb2001");
        let (_dir_b, id_b, svc_b) = start_test_instance(&state, "bbbb2002");

        let ctx_a = BusCallContext {
            instance_id: id_a.clone(),
            org_id: "org-1".to_string(),
            actor: Some("tester".to_string()),
            correlation_id: None,
            origin: "test".to_string(),
        };
        let ctx_b = BusCallContext {
            instance_id: id_b.clone(),
            org_id: "org-1".to_string(),
            actor: Some("tester".to_string()),
            correlation_id: None,
            origin: "test".to_string(),
        };
        svc_a
            .create_topic(&ctx_a, "orders", TopicOptions::default())
            .expect("create topic on A");
        svc_b
            .create_topic(&ctx_b, "orders", TopicOptions::default())
            .expect("create topic on B");
        svc_a
            .publish(
                &ctx_a,
                "orders",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![PublishRecord {
                        key: None,
                        headers: vec![],
                        payload: Bytes::from_static(b"instance-a-only"),
                        timestamp_ms: chrono::Utc::now().timestamp_millis(),
                        schema_id: 0,
                    }],
                },
            )
            .expect("publish to A");

        // A request addressed to B (through the exact resolution path the
        // REST handlers use) must resolve B's engine, not A's.
        let (resolved_id, resolved_svc) = resolve_engine(&state.db, Some(id_b.as_str()))
            .map_err(|r| r.status())
            .expect("resolve B");
        assert_eq!(resolved_id, id_b);
        assert!(Arc::ptr_eq(&resolved_svc, &svc_b));

        let handle_b = resolved_svc
            .open_consumer(
                &ctx_b,
                "g1",
                &["orders".to_string()],
                ConsumerConfig {
                    commit_mode: CommitMode::Explicit,
                },
            )
            .expect("open consumer on B");
        let batch_b = handle_b.fetch(64 * 1024, 50).expect("fetch on B");
        assert!(
            batch_b.records.is_empty(),
            "instance B must never see instance A's records"
        );

        // Sanity: A's own record IS there, proving the empty result above
        // is isolation, not an empty topic on both sides.
        let (resolved_id_a, resolved_svc_a) = resolve_engine(&state.db, Some(id_a.as_str()))
            .map_err(|r| r.status())
            .expect("resolve A");
        assert_eq!(resolved_id_a, id_a);
        let handle_a = resolved_svc_a
            .open_consumer(
                &ctx_a,
                "g1",
                &["orders".to_string()],
                ConsumerConfig {
                    commit_mode: CommitMode::Explicit,
                },
            )
            .expect("open consumer on A");
        let batch_a = handle_a.fetch(64 * 1024, 50).expect("fetch on A");
        assert_eq!(batch_a.records.len(), 1);
        assert_eq!(
            batch_a.records[0].payload,
            Bytes::from_static(b"instance-a-only")
        );
    }

    /// Drains a `Response<OpenAIBody>` built by `json_response`/
    /// `instance_error_response` into its raw bytes — `OpenAIBody` is a
    /// boxed one-shot stream, so there is no cheaper way to inspect it than
    /// actually polling it to completion.
    fn collect_body(resp: Response<OpenAIBody>) -> Vec<u8> {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime")
            .block_on(async move {
                use http_body_util::BodyExt;
                resp.into_body()
                    .collect()
                    .await
                    .expect("collect response body")
                    .to_bytes()
                    .to_vec()
            })
    }
}
