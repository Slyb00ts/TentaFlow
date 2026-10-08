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
// org_id` wymagane wszedzie indziej), so every call names its organisation
// with an explicit `?org_id=`. Two kinds of key are accepted:
//
// * A user-bound key (`Principal::User`) acts as its user: membership in the
//   organisation is verified (`org::repo::get_user_role_in_org`, fail-closed
//   403), then `BusService::publish`/`open_consumer` run with `ctx.actor =
//   user_id` through `InstanceBusAuthorizer` (permission matrix + per-topic
//   ACL), exactly like every other caller of the bus.
// * A general key (`Principal::ApiKey`, package K, owner decision P5: one
//   general key may also carry read/write rights to the messages of chosen
//   topics) acts as the key itself (`ActorKind::ApiKey`, `ctx.actor` = key
//   uid). Its only rights are the topic's `api_key` ACL rows for exactly
//   `read`/`write` — default DENY, no matrix row, never `admin`, never a
//   reserved topic, never auto-creation (`bus_authorizer`'s `API KEY
//   SUBJECTS` doc). The right is checked before the instance is looked up
//   (on the legacy path: before it is resolved), so a key without a grant
//   learns nothing about which instances or topics exist, and again by the
//   engine's authorizer on the call itself. A key consumes only under its
//   own groups (`k:<key uid>` / `k:<key uid>.<name>`), which no other caller
//   may use. Its requests are audited under `api_key:<uid>` with the key's
//   name through `bus::key_audit`: the first request of every outcome at
//   once, the rest counted per minute and flushed.
//
// A group-bound key (`Principal::Group`) is refused: it has neither a user
// to act as nor grants of its own.
// =============================================================================

use crate::api::openai::server::{OpenAIBody, V1PeerIp};
use crate::auth::acl::Principal;
use crate::auth::actor::ActorKind;
use crate::bus::groups::CommitMode;
use crate::bus::instance::BusInstanceId;
use crate::bus::key_audit;
use crate::bus::{
    self, BusAction, BusCallContext, BusServiceError, ConsumerConfig, FetchedRecordMeta,
    PublishBatch, PublishRecord, PublishResult, TopicPartition,
};
use crate::dispatch::app_gate::{self, SoleInstanceError};
use crate::routing::router::Router;
use crate::services::bus_authorizer::{
    api_key_owns_group, api_key_topic_allows, api_key_topic_granted_anywhere,
};

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

pub(crate) fn json_response(status: StatusCode, body: Vec<u8>) -> Response<OpenAIBody> {
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

pub(crate) fn error_response(
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
/// Shared with the schema registry REST (`api/bus_schema_rest.rs`), which
/// maps the same registry errors the WS surface does.
pub(crate) fn map_bus_error(e: &BusServiceError) -> Response<OpenAIBody> {
    match e {
        BusServiceError::TopicNotFound { .. } => {
            error_response(StatusCode::NOT_FOUND, "not_found_error", e.to_string())
        }
        // PLAN §7.2: the org's `bus.autocreate` ceiling refused the
        // auto-creation this request opted into — a policy denial about the
        // org, the same class as an ACL one, not a malformed request.
        // Package K: data hiding with no topic-wide rule refuses a key.
        BusServiceError::PermissionDenied { .. }
        | BusServiceError::AutocreateDisabled { .. }
        | BusServiceError::KeyNeedsTopicWideRule { .. } => {
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
        | BusServiceError::MaxPartitionsExceeded { .. }
        | BusServiceError::MaxBytesTotalExceeded { .. } => error_response(
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

// ---- Caller resolution and key audit (package K) ---------------------------

/// Who the `/v1` gate authenticated behind a records REST call.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RestCaller {
    /// A user-bound key: the call acts as that user, who must belong to
    /// `?org_id=`, and meets the permission matrix + topic ACL like every
    /// dashboard call of that user.
    User { user_id: String },
    /// A general key: the call acts as the key itself and holds only the
    /// topic rights granted to it (`bus_authorizer::api_key_topic_allows`).
    ApiKey { uid: String },
}

/// The resolved caller plus the organisation the call is scoped to.
#[derive(Debug)]
struct RestActor {
    caller: RestCaller,
    org_id: String,
}

impl RestActor {
    fn call_context(&self, instance_id: BusInstanceId) -> BusCallContext {
        let (actor, actor_kind) = match &self.caller {
            RestCaller::User { user_id } => (user_id.clone(), ActorKind::User),
            RestCaller::ApiKey { uid } => (uid.clone(), ActorKind::ApiKey),
        };
        BusCallContext {
            instance_id,
            org_id: self.org_id.clone(),
            actor: Some(actor),
            actor_kind,
            correlation_id: None,
            origin: "v1.bus.rest".to_string(),
        }
    }
}

/// Resolves the caller and `?org_id=` into a `RestActor`, or an error
/// response — see this file's header doc for why `org_id` must be explicit.
/// A general key's uid and organisation are noted in `facts` as soon as they
/// are known, so even an early refusal is audited under the key.
fn resolve_actor(
    db: &crate::db::DbPool,
    principal: Option<&Principal>,
    query: &BusRecordsQuery,
    facts: &mut KeyAuditFacts,
) -> std::result::Result<RestActor, Response<OpenAIBody>> {
    let caller = match principal {
        Some(Principal::User { user_id, .. }) => RestCaller::User {
            user_id: user_id.clone(),
        },
        Some(Principal::ApiKey { uid }) => {
            facts.key_uid = Some(uid.clone());
            RestCaller::ApiKey { uid: uid.clone() }
        }
        Some(Principal::Group { .. }) => {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "TentaBus REST access requires a user-bound API key or a general API key with \
                 topic rights (a 'group' key has no organization or grant to act with)"
                    .to_string(),
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
            facts.reason = Some("missing_org_id".to_string());
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "missing required query parameter 'org_id'".to_string(),
            ));
        }
    };
    match caller {
        RestCaller::User { user_id } => {
            match crate::services::org::repo::get_user_role_in_org(db, &user_id, &org_id) {
                Ok(Some(_)) => Ok(RestActor {
                    caller: RestCaller::User { user_id },
                    org_id,
                }),
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
        // A key belongs to no organisation: its grants do. Every topic right
        // is stored under `(instance, org, topic)`, so naming an organisation
        // the key holds nothing in finds no row and is refused by the topic
        // check — the same binding the schema registry REST gets from its
        // `(instance, org)` scope id.
        RestCaller::ApiKey { uid } => {
            if let Err(e) = crate::bus::topics::validate_org_id(&org_id) {
                facts.reason = Some("invalid_org_id".to_string());
                return Err(map_bus_error(&e));
            }
            facts.org_id = Some(org_id.clone());
            Ok(RestActor {
                caller: RestCaller::ApiKey { uid },
                org_id,
            })
        }
    }
}

/// A general key's right to `action` on `topic`, asked before the instance's
/// engine is looked up (same order as the schema registry REST), so a key
/// without a grant learns nothing about which instances or topics exist: the
/// answer is the same 403 whatever the path names. The engine's own
/// authorizer asks the same question again on the call itself. A no-op for a
/// user caller, whose request keeps its original order.
///
/// On the instance-less legacy path the instance is resolved only once the
/// key holds the right on this topic of this organisation in SOME instance —
/// before that, resolving it would tell the key how many instances exist and
/// (in the 409) their ids.
fn precheck_api_key(
    db: &crate::db::DbPool,
    actor: &RestActor,
    instance: Option<&str>,
    topic: &str,
    action: BusAction,
    facts: &mut KeyAuditFacts,
) -> std::result::Result<(), Response<OpenAIBody>> {
    let RestCaller::ApiKey { uid } = &actor.caller else {
        return Ok(());
    };
    let verb = match action {
        BusAction::Produce => "write",
        _ => "read",
    };
    let refuse = |facts: &mut KeyAuditFacts| {
        facts.reason = Some(format!("api_key_topic_denied:{verb}"));
        error_response(
            StatusCode::FORBIDDEN,
            "permission_error",
            format!(
                "this API key may not {verb} messages of this topic for organisation '{}'",
                actor.org_id
            ),
        )
    };
    let instance_id = match instance {
        Some(raw) => BusInstanceId::parse(raw).map_err(|e| {
            facts.reason = Some("invalid_instance_id".to_string());
            error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                e.to_string(),
            )
        })?,
        None => {
            if !api_key_topic_granted_anywhere(db, &actor.org_id, topic, uid, action) {
                return Err(refuse(facts));
            }
            resolve_instance(db, None).inspect_err(|_| {
                facts.reason = Some("instance_unavailable".to_string());
            })?
        }
    };
    if api_key_topic_allows(db, instance_id.as_str(), &actor.org_id, topic, uid, action) {
        Ok(())
    } else {
        Err(refuse(facts))
    }
}

/// What a general key's REST request is audited with, filled in as the
/// request is understood. `key_uid` being `Some` is what marks a key
/// request: a user-bound key's call is audited by the bus itself, as that
/// user, exactly as before package K.
#[derive(Default)]
struct KeyAuditFacts {
    key_uid: Option<String>,
    org_id: Option<String>,
    reason: Option<String>,
    records: Option<usize>,
}

/// Hands a general key's finished request to the windowed key audit
/// (`bus::key_audit`: the first request of each outcome at once, the rest
/// counted and flushed). Nothing for a user caller.
#[allow(clippy::too_many_arguments)]
fn audit_key_request(
    db: &crate::db::DbPool,
    action: &'static str,
    instance: Option<&str>,
    topic: &str,
    status: StatusCode,
    facts: KeyAuditFacts,
    peer_ip: Option<String>,
    user_agent: Option<String>,
) {
    let Some(key_uid) = facts.key_uid else {
        return;
    };
    // Path segments are caller-controlled; only well-formed ones are
    // written as they are.
    let topic = if crate::bus::topics::validate_user_topic_name(topic).is_ok() {
        topic.to_string()
    } else {
        "<invalid>".to_string()
    };
    let instance = match instance {
        Some(raw) if BusInstanceId::parse(raw).is_ok() => raw.to_string(),
        Some(_) => "<invalid>".to_string(),
        None => String::new(),
    };
    key_audit::record(
        db,
        key_audit::KeyRequest {
            key_uid,
            org_id: facts.org_id,
            instance,
            topic,
            action,
            http_status: status.as_u16(),
            reason: if status.is_success() {
                None
            } else {
                Some(
                    facts
                        .reason
                        .unwrap_or_else(|| format!("http_{}", status.as_u16())),
                )
            },
            records: facts.records.unwrap_or(0) as u64,
            peer_ip,
            user_agent,
        },
    );
}

/// Peer address and user agent of a request, for its audit row.
fn request_origin<B>(req: &Request<B>) -> (Option<String>, Option<String>) {
    let peer_ip = req.extensions().get::<V1PeerIp>().map(|p| p.0.clone());
    let user_agent = req
        .headers()
        .get(hyper::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.chars().take(256).collect::<String>());
    (peer_ip, user_agent)
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
pub(crate) fn resolve_engine(
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
    let Some(db) = router.db.clone() else {
        return Ok(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "internal_error",
            "database unavailable".to_string(),
        ));
    };
    Ok(publish(req, &db, instance.as_deref(), &topic).await)
}

/// Serves one publish request and, for a general key, writes its audit row.
/// Generic over the body so tests drive the exact production path with
/// in-memory bodies.
pub(crate) async fn publish<B>(
    req: Request<B>,
    db: &crate::db::DbPool,
    instance: Option<&str>,
    topic: &str,
) -> Response<OpenAIBody>
where
    B: hyper::body::Body<Data = Bytes>,
    B::Error: std::fmt::Display,
{
    let (peer_ip, user_agent) = request_origin(&req);
    let mut facts = KeyAuditFacts::default();
    let response = serve_publish(req, db, instance, topic, &mut facts).await;
    audit_key_request(
        db,
        "bus.rest.publish",
        instance,
        topic,
        response.status(),
        facts,
        peer_ip,
        user_agent,
    );
    response
}

async fn serve_publish<B>(
    req: Request<B>,
    db: &crate::db::DbPool,
    instance: Option<&str>,
    topic: &str,
    facts: &mut KeyAuditFacts,
) -> Response<OpenAIBody>
where
    B: hyper::body::Body<Data = Bytes>,
    B::Error: std::fmt::Display,
{
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
            facts.reason = Some(e.to_string());
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                e.to_string(),
            );
        }
    };
    let actor = match resolve_actor(db, principal.as_ref(), &query, facts) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if let Err(resp) = precheck_api_key(db, &actor, instance, topic, BusAction::Produce, facts) {
        return resp;
    }
    // Resolved (and the running engine looked up) before touching the
    // request body — §3.2: a request addressed to instance B must never
    // fall back to A, and a malformed/unavailable instance should fail as
    // cheaply as possible.
    let (instance_id, svc) = match resolve_engine(db, instance) {
        Ok(v) => v,
        Err(resp) => {
            facts.reason = Some("instance_unavailable".to_string());
            return resp;
        }
    };

    let body_bytes = match req.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) => {
            facts.reason = Some("body_unreadable".to_string());
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("failed to read request body: {e}"),
            );
        }
    };

    let (records, create_if_missing) = if is_cbor {
        let input: BusPublishInput = match minicbor::decode(&body_bytes) {
            Ok(v) => v,
            Err(e) => {
                facts.reason = Some("invalid_cbor".to_string());
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    format!("invalid CBOR body: {e}"),
                );
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
                facts.reason = Some("invalid_ndjson".to_string());
                return error_response(StatusCode::BAD_REQUEST, "invalid_request_error", msg);
            }
        };
        (records, query.create_if_missing.unwrap_or(false))
    };

    if records.is_empty() {
        facts.reason = Some("empty_batch".to_string());
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "publish batch has no records".to_string(),
        );
    }
    if records.len() > MAX_PUBLISH_RECORDS {
        facts.reason = Some("batch_too_large".to_string());
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!(
                "batch of {} records exceeds the {MAX_PUBLISH_RECORDS} limit",
                records.len()
            ),
        );
    }
    facts.records = Some(records.len());

    let ctx = actor.call_context(instance_id);
    let batch = PublishBatch {
        partition: None,
        producer: None,
        records,
    };
    // Same create-if-missing retry shape as `host_functions/bus.rs`'s
    // `bus_publish_v1`: try the publish first, and only pay for a
    // `create_topic` round-trip on the (rare) miss. Auto-creation is an
    // administrative act (`BusAction::Admin`), which a general key never
    // holds (`bus_authorizer`'s `API KEY SUBJECTS` doc).
    //
    // Both `publish` and `create_topic` block the calling thread
    // (`Partition::append_batch` ends in `blocking_recv` on the writer
    // thread's channel), and `bus::mod`'s own doc requires every async
    // caller to hand that off — `block_in_place` here, the same way
    // `consume` below already does it for `ConsumerHandle::fetch`.
    // Called straight from this async fn it panicked ("Cannot block the
    // current thread from within a runtime") and the client saw an empty
    // reply on a killed connection, not an error: every REST publish failed
    // that way.
    let result = tokio::task::block_in_place(|| match svc.publish(&ctx, topic, batch.clone()) {
        Ok(r) => Ok(r),
        Err(BusServiceError::TopicNotFound { .. }) if create_if_missing => svc
            .autocreate_topic(&ctx, topic)
            .and_then(|_| svc.publish(&ctx, topic, batch)),
        Err(e) => Err(e),
    });

    match result {
        Ok(r) => {
            // `schema_rejected` (PLAN-F3 §4.5): records quarantined to the
            // DLQ under `validation = dlq`; `schema_dropped`: records that
            // failed validation and whose quarantine copy could not be
            // written — lost. Both additive, always present.
            let body = publish_response_json(&r);
            json_response(
                StatusCode::OK,
                serde_json::to_vec(&body).unwrap_or_default(),
            )
        }
        Err(e) => {
            facts.reason = Some(bus_error_reason(&e));
            map_bus_error(&e)
        }
    }
}

/// The reason an audit row gives for a refused bus call: the error's variant,
/// never its text — a field-policy or schema error can quote payload fields.
fn bus_error_reason(e: &BusServiceError) -> String {
    let debug = format!("{e:?}");
    let variant = debug
        .split(|c: char| !c.is_ascii_alphanumeric())
        .next()
        .unwrap_or_default();
    format!("bus:{variant}")
}

/// The body of a successful publish.
fn publish_response_json(r: &PublishResult) -> serde_json::Value {
    serde_json::json!({
        "published": r.accepted,
        "schema_rejected": r.schema_rejected,
        "schema_dropped": r.schema_dropped,
    })
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
    let Some(db) = router.db.clone() else {
        return Ok(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "internal_error",
            "database unavailable".to_string(),
        ));
    };
    Ok(consume(req, &db, instance.as_deref(), &topic).await)
}

/// Serves one consume request and, for a general key, writes its audit row.
pub(crate) async fn consume<B>(
    req: Request<B>,
    db: &crate::db::DbPool,
    instance: Option<&str>,
    topic: &str,
) -> Response<OpenAIBody> {
    let (peer_ip, user_agent) = request_origin(&req);
    let mut facts = KeyAuditFacts::default();
    let response = serve_consume(&req, db, instance, topic, &mut facts);
    audit_key_request(
        db,
        "bus.rest.consume",
        instance,
        topic,
        response.status(),
        facts,
        peer_ip,
        user_agent,
    );
    response
}

fn serve_consume<B>(
    req: &Request<B>,
    db: &crate::db::DbPool,
    instance: Option<&str>,
    topic: &str,
    facts: &mut KeyAuditFacts,
) -> Response<OpenAIBody> {
    let principal = req.extensions().get::<Principal>().cloned();
    let query = match parse_query(req.uri().query().unwrap_or("")) {
        Ok(q) => q,
        Err(e) => {
            facts.reason = Some(e.to_string());
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                e.to_string(),
            );
        }
    };
    let actor = match resolve_actor(db, principal.as_ref(), &query, facts) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(group) = query.group.clone().filter(|g| !g.is_empty()) else {
        facts.reason = Some("missing_group".to_string());
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "missing required query parameter 'group'".to_string(),
        );
    };
    if let RestCaller::ApiKey { uid } = &actor.caller {
        if !api_key_owns_group(uid, &group) {
            facts.reason = Some("api_key_group_not_owned".to_string());
            return error_response(
                StatusCode::FORBIDDEN,
                "permission_error",
                format!(
                    "a general API key consumes only under its own consumer groups: \
                     'k:{uid}' or 'k:{uid}.<name>'"
                ),
            );
        }
    }
    if let Err(resp) = precheck_api_key(db, &actor, instance, topic, BusAction::Consume, facts) {
        return resp;
    }
    // See `serve_publish`'s identical comment — an addressed-but-unavailable
    // instance never falls back.
    let (instance_id, svc) = match resolve_engine(db, instance) {
        Ok(v) => v,
        Err(resp) => {
            facts.reason = Some("instance_unavailable".to_string());
            return resp;
        }
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

    let ctx = actor.call_context(instance_id);
    // Blocking too, though not through the `blocking_recv` that `bus::mod`'s
    // BLOCKING note names for `publish`/`fetch`: `open_consumer` opens a full
    // `Partition` — writer thread and directory flock included — for every
    // partition it subscribes to (that module's PARTITION HANDLE LIFETIME
    // note), so it gets handed off the executor like the `fetch` below.
    let opened = tokio::task::block_in_place(|| {
        svc.open_consumer(
            &ctx,
            &group,
            &[topic.to_string()],
            ConsumerConfig {
                commit_mode: CommitMode::Explicit,
            },
        )
    });
    let handle = match opened {
        Ok(h) => h,
        Err(e) => {
            facts.reason = Some(bus_error_reason(&e));
            return map_bus_error(&e);
        }
    };

    // `ConsumerHandle::fetch` blocks the calling thread for up to
    // `max_wait_ms` (its own doc: callers on a Tokio executor MUST NOT call
    // it directly from an async fn) — `block_in_place` is the same pattern
    // `host_functions/bus.rs`'s `bus_consume_next_v1` already uses for this
    // exact call. The records it returns are already projected through the
    // caller's data-hiding rules (`ConsumerHandle::fetch`) — for a general
    // key, the topic-wide rule (`field_policies::resolve`).
    let fetched = tokio::task::block_in_place(|| handle.fetch(max_bytes, max_wait_ms));
    let batch = match fetched {
        Ok(b) => b,
        Err(e) => {
            facts.reason = Some(bus_error_reason(&e));
            return map_bus_error(&e);
        }
    };
    facts.records = Some(batch.records.len());

    if batch.records.is_empty() {
        let body = serde_json::json!({ "records": [] });
        return json_response(
            StatusCode::OK,
            serde_json::to_vec(&body).unwrap_or_default(),
        );
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
                    topic: topic.to_string(),
                    partition,
                },
                offset + 1,
            )
        })
        .collect();

    let records_json: Vec<serde_json::Value> =
        batch.records.into_iter().map(record_to_json).collect();
    // Blocking as well — `bus/reactor.rs`'s `commit_offsets` hands this same
    // call off the async runtime for the same reason.
    if let Err(e) = tokio::task::block_in_place(|| handle.commit(&commit_offsets)) {
        // The records were already fetched and are about to be returned to
        // the caller — a commit failure here must not silently drop them,
        // but it does mean a redelivery is possible on the next poll (the
        // same at-least-once trade-off `note_delivery_failure`'s retry path
        // already makes elsewhere in this codebase).
        tracing::warn!(topic = %topic, group = %group, error = %e, "v1 bus REST: post-fetch commit failed, records already returned to caller");
    }

    let body = serde_json::json!({ "records": records_json });
    json_response(
        StatusCode::OK,
        serde_json::to_vec(&body).unwrap_or_default(),
    )
}

/// Fixtures shared by the records REST tests below and the schema registry
/// REST tests (`api/bus_schema_rest.rs`): both need a real, registry-visible
/// engine behind an installed-and-enabled instance.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Local double of the process-wide test authorizer every `bus::mod`
    /// test module keeps its own copy of (`AllowAllAuthorizer`'s doc there
    /// notes it is intentionally not shared: it is a private `#[cfg(test)]`
    /// item). The suites using it test instance ROUTING and the schema
    /// registry REST gate, not topic RBAC, so an always-allow authorizer keeps
    /// the fixtures focused.
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

    /// An admin session backed by a real, active account — the dispatcher
    /// refuses a session whose account does not exist before any handler runs.
    pub(crate) fn admin_ctx(
        state: Arc<crate::dispatch::state::AppState>,
    ) -> crate::dispatch::HandlerContext {
        let id = crate::db::repository::create_user_account(
            &state.db,
            &format!("bus-admin-{}", uuid::Uuid::new_v4()),
            "not-a-login-hash",
            "Bus admin",
            "",
        )
        .expect("admin account");
        state
            .db
            .write()
            .unwrap()
            .execute(
                "UPDATE user_accounts SET must_change_password = 0, is_active = 1 WHERE id = ?1",
                [&id],
            )
            .expect("activate admin account");
        crate::dispatch::HandlerContext {
            session: tentaflow_protocol::SessionAuth::UserSession {
                user_id: *uuid::Uuid::parse_str(&id).unwrap().as_bytes(),
                role: Some("admin".to_string()),
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state,
            origin: crate::dispatch::RequestOrigin::Local,
            org_context: None,
        }
    }

    /// Installs an ENABLED `tentabus` instance (`suffix` must be 8 lowercase
    /// hex chars — `BusInstanceId::parse`'s shape) and starts a real,
    /// registry-visible engine for it (`bus::init_instance`, exactly what
    /// `resolve_engine`'s `bus::instance` lookup reads from). The returned
    /// `TempDir` must outlive every use of the engine.
    pub(crate) fn start_test_instance(
        state: &Arc<crate::dispatch::state::AppState>,
        suffix: &str,
    ) -> (tempfile::TempDir, BusInstanceId, Arc<bus::BusService>) {
        start_test_instance_with(state, suffix, |_| Arc::new(AllowAllAuthorizer))
    }

    /// `start_test_instance` with the authorizer `authorizer` builds for the
    /// new instance's id — the production `InstanceBusAuthorizer` for the
    /// suites that test who may publish and consume.
    pub(crate) fn start_test_instance_with(
        state: &Arc<crate::dispatch::state::AppState>,
        suffix: &str,
        authorizer: impl FnOnce(&BusInstanceId) -> Arc<dyn bus::BusAuthorizer>,
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
            authorizer: authorizer(&id),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .expect("bus init_instance");
        (dir, id, svc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the tests still provision topics explicitly; the handler's own
    // auto-create path goes through `BusService::autocreate_topic`, which
    // picks the options itself.
    use crate::bus::topics::TopicOptions;
    use test_support::start_test_instance;

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

    fn test_state() -> Arc<crate::dispatch::state::AppState> {
        crate::dispatch::state::AppState::for_test()
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
            actor_kind: crate::auth::actor::ActorKind::User,
            correlation_id: None,
            origin: "test".to_string(),
        };
        let ctx_b = BusCallContext {
            instance_id: id_b.clone(),
            org_id: "org-1".to_string(),
            actor: Some("tester".to_string()),
            actor_kind: crate::auth::actor::ActorKind::User,
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

    /// A publish whose schema-violating record could not be quarantined
    /// says so: `schema_dropped` next to `published`/`schema_rejected`.
    #[test]
    fn publish_response_reports_dropped_records() {
        let body = publish_response_json(&PublishResult {
            duplicate: false,
            accepted: 1,
            deduplicated: 0,
            partitions: Vec::new(),
            schema_rejected: 0,
            schema_dropped: 2,
        });
        assert_eq!(
            body,
            serde_json::json!({"published": 1, "schema_rejected": 0, "schema_dropped": 2})
        );
    }

    // ---- General API keys (package K, owner decision P5) --------------------

    use crate::addon::permissions::PermissionChecker;
    use crate::bus::field_policies;
    use crate::db::models::AuditLogFilters;
    use crate::services::bus_authorizer::{topic_acl_resource_id, InstanceBusAuthorizer};
    use http_body_util::Full;
    use test_support::{admin_ctx, start_test_instance_with};

    const ORG: &str = "org-1";
    const TOPIC: &str = "orders";

    /// A real engine behind the production `InstanceBusAuthorizer`, with one
    /// JSON topic `orders` in `org-1`, created by a matrix admin.
    struct KeyFixture {
        state: Arc<crate::dispatch::state::AppState>,
        _dir: tempfile::TempDir,
        instance: BusInstanceId,
        svc: Arc<bus::BusService>,
        checker: Arc<PermissionChecker>,
    }

    fn key_fixture(suffix: &str) -> KeyFixture {
        let state = test_state();
        let checker = Arc::new(PermissionChecker::new(state.db.clone()));
        let (db, auth_checker) = (state.db.clone(), checker.clone());
        let (dir, instance, svc) = start_test_instance_with(&state, suffix, move |id| {
            Arc::new(InstanceBusAuthorizer::new(db, id.clone(), auth_checker))
        });
        let fx = KeyFixture {
            state,
            _dir: dir,
            instance,
            svc,
            checker,
        };
        fx.state
            .db
            .write()
            .unwrap()
            .execute(
                "INSERT OR IGNORE INTO organizations (org_id, name, slug, status, created_at) \
                 VALUES (?1, ?1, ?1, 'active', '2026-09-29T00:00:00Z')",
                rusqlite::params![ORG],
            )
            .unwrap();
        fx.grant("u-admin", "bus.admin");
        fx.grant("u-admin", "bus.write");
        fx.svc
            .create_topic(
                &fx.user_ctx("u-admin"),
                TOPIC,
                TopicOptions {
                    partitions: Some(1),
                    content_type: Some("application/json".to_string()),
                    ..TopicOptions::default()
                },
            )
            .expect("create topic");
        fx
    }

    impl KeyFixture {
        fn db(&self) -> crate::db::DbPool {
            self.state.db.clone()
        }

        fn grant(&self, user_id: &str, perm: &str) {
            crate::db::repository::upsert_permission(
                &self.state.db,
                self.instance.as_str(),
                "user",
                user_id,
                perm,
                "allow",
                None,
            )
            .unwrap();
            self.checker.refresh_addon(self.instance.as_str());
            // The dashboard gate reads the node's own checker.
            if let Some(checker) = self.state.permission_checker.as_ref() {
                checker.refresh_addon(self.instance.as_str());
            }
        }

        fn user_ctx(&self, user_id: &str) -> BusCallContext {
            BusCallContext {
                instance_id: self.instance.clone(),
                org_id: ORG.to_string(),
                actor: Some(user_id.to_string()),
                actor_kind: ActorKind::User,
                correlation_id: None,
                origin: "test".to_string(),
            }
        }

        /// A general key holding exactly `actions` on `orders` in `org-1`.
        fn key(&self, actions: &[&str]) -> String {
            let (_, uid) = crate::db::repository::create_api_key(
                &self.state.db,
                &format!("verifier-{}", uuid::Uuid::new_v4()),
                "sk-...abcdef",
                "Laboratorium LIS",
                "general",
                None,
                60,
            )
            .unwrap();
            let resource_id = topic_acl_resource_id(self.instance.as_str(), ORG, TOPIC);
            for action in actions {
                assert!(crate::db::repository::resource_permissions::set_topic_rule(
                    &self.state.db,
                    &resource_id,
                    "api_key",
                    &uid,
                    action,
                    "allow",
                )
                .unwrap());
            }
            uid
        }

        fn publish_as(&self, principal: Option<Principal>, query: &str, body: &str) -> Reply {
            self.send_to(
                self.instance.as_str(),
                TOPIC,
                "POST",
                principal,
                query,
                body,
            )
        }

        fn consume_as(&self, principal: Option<Principal>, query: &str) -> Reply {
            self.send_to(self.instance.as_str(), TOPIC, "GET", principal, query, "")
        }

        /// Sends one request through `publish`/`consume` exactly as the
        /// router does, on a multi-threaded runtime (both block in place).
        fn send_to(
            &self,
            instance: &str,
            topic: &str,
            method: &str,
            principal: Option<Principal>,
            query: &str,
            body: &str,
        ) -> Reply {
            let mut builder = Request::builder().method(method).uri(format!(
                "/v1/bus/instances/{instance}/topics/{topic}/records?{query}"
            ));
            if let Some(p) = principal {
                builder = builder.extension(p);
            }
            let req = builder
                .extension(V1PeerIp("203.0.113.9".to_string()))
                .body(Full::new(Bytes::from(body.to_string())))
                .unwrap();
            let (db, instance, topic) = (self.db(), instance.to_string(), topic.to_string());
            let is_post = method == "POST";
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let resp = tokio::spawn(async move {
                    if is_post {
                        publish(req, &db, Some(&instance), &topic).await
                    } else {
                        consume(req, &db, Some(&instance), &topic).await
                    }
                })
                .await
                .expect("request task");
                let status = resp.status();
                let bytes = resp
                    .into_body()
                    .collect()
                    .await
                    .expect("collect body")
                    .to_bytes();
                Reply {
                    status,
                    json: serde_json::from_slice(&bytes).expect("json body"),
                }
            })
        }

        fn audit_rows(&self, action: &str) -> Vec<crate::db::models::AuditLogEntry> {
            crate::db::repository::list_audit_logs(
                &self.state.db,
                &AuditLogFilters {
                    action: Some(action.to_string()),
                    ..Default::default()
                },
                0,
                100,
            )
            .expect("list audit")
        }
    }

    struct Reply {
        status: StatusCode,
        json: serde_json::Value,
    }

    fn key_principal(uid: &str) -> Option<Principal> {
        Some(Principal::ApiKey {
            uid: uid.to_string(),
        })
    }

    fn ndjson(payload: &str) -> String {
        let b64 = base64::engine::general_purpose::STANDARD.encode(payload);
        format!(r#"{{"payload_b64":"{b64}"}}"#)
    }

    fn payloads(reply: &Reply) -> Vec<serde_json::Value> {
        reply.json["records"]
            .as_array()
            .expect("records array")
            .iter()
            .map(|r| {
                let raw = base64::engine::general_purpose::STANDARD
                    .decode(r["payload"].as_str().unwrap())
                    .unwrap();
                serde_json::from_slice(&raw).expect("json payload")
            })
            .collect()
    }

    fn org_query() -> String {
        format!("org_id={ORG}")
    }

    fn group_query(group: &str) -> String {
        format!("org_id={ORG}&group={group}&wait_ms=0")
    }

    /// The query of a key consuming under its own group `k:<uid>`.
    fn own_group_query(key_uid: &str) -> String {
        group_query(&format!("k:{key_uid}"))
    }

    /// Default DENY, refused before the instance is looked up: a key with no
    /// grant gets the same 403 for a real instance and for one that was never
    /// installed, and every refusal is audited under the key's id and name.
    #[test]
    fn key_without_a_grant_is_refused_and_learns_nothing() {
        let fx = key_fixture("cccc3001");
        let key = fx.key(&[]);
        let publish = fx.publish_as(key_principal(&key), &org_query(), &ndjson(r#"{"id":1}"#));
        assert_eq!(publish.status, StatusCode::FORBIDDEN);
        let consume = fx.consume_as(key_principal(&key), &own_group_query(&key));
        assert_eq!(consume.status, StatusCode::FORBIDDEN);
        let unknown = fx.send_to(
            "tentabus-deadbeef",
            TOPIC,
            "POST",
            key_principal(&key),
            &org_query(),
            &ndjson(r#"{"id":1}"#),
        );
        assert_eq!(unknown.status, StatusCode::FORBIDDEN);

        // Both refusals share one bucket (same key, action and reason): the
        // first is written at once, the second with the flush.
        assert_eq!(fx.audit_rows("bus.rest.publish").len(), 1);
        crate::bus::key_audit::flush_for(&fx.db());
        let rows = fx.audit_rows("bus.rest.publish");
        assert_eq!(rows.len(), 2);
        for row in &rows {
            let details: serde_json::Value =
                serde_json::from_str(row.details.as_deref().unwrap()).unwrap();
            assert_eq!(details["api_key_uid"], key.as_str());
            assert_eq!(details["api_key_name"], "Laboratorium LIS");
            assert_eq!(details["reason"], "api_key_topic_denied:write");
            assert_eq!(
                row.user_id.as_deref(),
                Some(format!("api_key:{key}").as_str())
            );
        }
        assert_eq!(fx.audit_rows("bus.rest.consume").len(), 1);
    }

    /// The instance-less legacy path resolves nothing for a key without a
    /// grant: with two instances enabled it answers the same 403, not the 409
    /// that would list them. Once the key holds the right somewhere, it gets
    /// the answer every caller gets there.
    #[test]
    fn legacy_path_reveals_no_instance_to_a_key_without_a_grant() {
        let fx = key_fixture("cccc3009");
        let (_dir_b, _id_b, _svc_b) = start_test_instance(&fx.state, "cccc3010");
        let key = fx.key(&[]);
        let legacy = |principal| {
            let req = Request::builder()
                .method("POST")
                .uri(format!("/v1/bus/topics/{TOPIC}/records?{}", org_query()))
                .extension::<Principal>(principal)
                .body(Full::new(Bytes::from(ndjson(r#"{"id":1}"#))))
                .unwrap();
            let db = fx.db();
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                tokio::spawn(async move { publish(req, &db, None, TOPIC).await })
                    .await
                    .expect("request task")
                    .status()
            })
        };
        let no_grant = Principal::ApiKey { uid: key.clone() };
        assert_eq!(legacy(no_grant), StatusCode::FORBIDDEN);

        let writer = fx.key(&["write"]);
        assert_eq!(
            legacy(Principal::ApiKey { uid: writer }),
            StatusCode::CONFLICT
        );
    }

    /// Read and write are separate grants over REST too, and a key consumes
    /// only under groups named after itself.
    #[test]
    fn write_key_publishes_and_read_key_consumes_under_its_own_group() {
        let fx = key_fixture("cccc3002");
        let writer = fx.key(&["write"]);
        let reader = fx.key(&["read"]);

        let published = fx.publish_as(key_principal(&writer), &org_query(), &ndjson(r#"{"id":1}"#));
        assert_eq!(published.status, StatusCode::OK, "{}", published.json);
        assert_eq!(published.json["published"], 1);
        assert_eq!(
            fx.consume_as(key_principal(&writer), &own_group_query(&writer))
                .status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            fx.publish_as(key_principal(&reader), &org_query(), &ndjson(r#"{"id":2}"#))
                .status,
            StatusCode::FORBIDDEN
        );

        let foreign = fx.consume_as(key_principal(&reader), &group_query("billing"));
        assert_eq!(foreign.status, StatusCode::FORBIDDEN);
        let own = fx.consume_as(
            key_principal(&reader),
            &group_query(&format!("k:{reader}.lis")),
        );
        assert_eq!(own.status, StatusCode::OK, "{}", own.json);
        assert_eq!(payloads(&own), vec![serde_json::json!({"id": 1})]);
    }

    /// A grant belongs to one organisation; the call must name one.
    #[test]
    fn key_grant_does_not_reach_another_organisation() {
        let fx = key_fixture("cccc3003");
        let reader = fx.key(&["read"]);
        let other_org = fx.consume_as(
            key_principal(&reader),
            &format!("org_id=org-2&group=k:{reader}&wait_ms=0"),
        );
        assert_eq!(other_org.status, StatusCode::FORBIDDEN);
        let no_org = fx.consume_as(key_principal(&reader), &format!("group=k:{reader}"));
        assert_eq!(no_org.status, StatusCode::BAD_REQUEST);
    }

    /// A key that no longer exists is refused even if rows naming it are
    /// still there (older data, or rows that reached this node before the
    /// key's own deletion): revoking removes them, this is the second line.
    #[test]
    fn revoked_key_is_refused() {
        let fx = key_fixture("cccc3004");
        let writer = fx.key(&["write"]);
        fx.state
            .db
            .write()
            .unwrap()
            .execute("DELETE FROM api_keys WHERE uid = ?1", [&writer])
            .unwrap();
        let reply = fx.publish_as(key_principal(&writer), &org_query(), &ndjson(r#"{"id":1}"#));
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
    }

    /// Auto-creation is administrative: a key with write rights on one topic
    /// cannot conjure another, and learns nothing about whether it exists.
    #[test]
    fn key_cannot_create_a_topic() {
        let fx = key_fixture("cccc3005");
        let writer = fx.key(&["write"]);
        let reply = fx.send_to(
            fx.instance.as_str(),
            "invoices",
            "POST",
            key_principal(&writer),
            &format!("{}&create_if_missing=true", org_query()),
            &ndjson(r#"{"id":1}"#),
        );
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
        assert!(
            crate::bus::topics::get_topic(&fx.state.db, fx.instance.as_str(), ORG, "invoices")
                .unwrap()
                .is_none()
        );
    }

    /// A user-bound key keeps acting as its user: organisation membership
    /// plus the permission matrix, no key rows, no key audit rows.
    #[test]
    fn user_bound_key_keeps_acting_as_its_user() {
        let fx = key_fixture("cccc3006");
        {
            let conn = fx.state.db.write().unwrap();
            conn.execute(
                "INSERT OR IGNORE INTO organizations (org_id, name, slug, status, created_at) \
                 VALUES (?1, ?1, ?1, 'active', '2026-09-29T00:00:00Z')",
                rusqlite::params![ORG],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO user_accounts (id, username, password_hash, display_name) \
                 VALUES ('u-writer', 'writer', 'x', 'Writer')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
                 VALUES (?1, 'u-writer', 'role-supervisor', '2026-09-29T00:00:00Z', 'test')",
                rusqlite::params![ORG],
            )
            .unwrap();
        }
        fx.grant("u-writer", "bus.write");
        let user = |id: &str| {
            Some(Principal::User {
                user_id: id.to_string(),
                role: "user".to_string(),
            })
        };
        let ok = fx.publish_as(user("u-writer"), &org_query(), &ndjson(r#"{"id":1}"#));
        assert_eq!(ok.status, StatusCode::OK, "{}", ok.json);
        let outsider = fx.publish_as(user("u-outsider"), &org_query(), &ndjson(r#"{"id":1}"#));
        assert_eq!(outsider.status, StatusCode::FORBIDDEN);
        let group_key = fx.publish_as(
            Some(Principal::Group {
                group_id: "g-1".to_string(),
            }),
            &org_query(),
            &ndjson(r#"{"id":1}"#),
        );
        assert_eq!(group_key.status, StatusCode::BAD_REQUEST);
        assert!(fx.audit_rows("bus.rest.publish").is_empty());
    }

    /// Data hiding applies to a key's reads: a key reads through the
    /// topic-wide rule — never the raw record, and never a user's rule that
    /// happens to carry the key's id.
    #[test]
    fn key_reads_are_projected_through_data_hiding() {
        let fx = key_fixture("cccc3007");
        let own_rule = fx.key(&["read"]);
        let wildcard_only = fx.key(&["read"]);
        let fields = |names: &[&str]| -> std::collections::BTreeSet<String> {
            names.iter().map(|n| n.to_string()).collect()
        };
        let set = |subject_type: &str, subject_id: &str, allowed: &[&str]| {
            field_policies::set_policy(
                &fx.state.db,
                fx.instance.as_str(),
                ORG,
                TOPIC,
                subject_type,
                subject_id,
                field_policies::Direction::Read,
                &fields(allowed),
                &fields(&[]),
                field_policies::BusFieldPolicyExpect::Any,
            )
            .expect("set policy")
        };
        set("any", field_policies::SUBJECT_ANY, &["id", "name"]);
        set("user", &own_rule, &["id", "name", "pesel"]);
        fx.svc
            .publish(
                &fx.user_ctx("u-admin"),
                TOPIC,
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![PublishRecord {
                        key: None,
                        headers: vec![],
                        payload: Bytes::from_static(
                            br#"{"id":1,"name":"Jan","pesel":"44051401359"}"#,
                        ),
                        timestamp_ms: chrono::Utc::now().timestamp_millis(),
                        schema_id: 0,
                    }],
                },
            )
            .expect("publish");

        let own = fx.consume_as(key_principal(&own_rule), &own_group_query(&own_rule));
        assert_eq!(own.status, StatusCode::OK, "{}", own.json);
        assert_eq!(
            payloads(&own),
            vec![serde_json::json!({"id": 1, "name": "Jan"})]
        );
        let wildcard = fx.consume_as(
            key_principal(&wildcard_only),
            &own_group_query(&wildcard_only),
        );
        assert_eq!(wildcard.status, StatusCode::OK, "{}", wildcard.json);
        assert_eq!(
            payloads(&wildcard),
            vec![serde_json::json!({"id": 1, "name": "Jan"})]
        );
    }

    /// Sum of `requests` and `records` over the key's rows of one action.
    fn audited(fx: &KeyFixture, action: &str, key: &str) -> (usize, u64, u64) {
        let rows: Vec<serde_json::Value> = fx
            .audit_rows(action)
            .iter()
            .map(|r| serde_json::from_str(r.details.as_deref().unwrap()).unwrap())
            .filter(|d: &serde_json::Value| d["api_key_uid"] == key)
            .collect();
        let sum = |field: &str| rows.iter().map(|d| d[field].as_u64().unwrap()).sum();
        (rows.len(), sum("requests"), sum("records"))
    }

    /// A key's successful requests write their first row at once and the
    /// rest as one flushed row carrying how many requests and records they
    /// were — nothing is lost when no further request arrives, and a flushed
    /// bucket is gone (a second flush writes nothing).
    #[test]
    fn successful_key_requests_are_counted_and_flushed() {
        let fx = key_fixture("cccc3008");
        let writer = fx.key(&["write"]);
        let bodies = [
            ndjson(r#"{"id":1}"#),
            format!("{}\n{}", ndjson(r#"{"id":2}"#), ndjson(r#"{"id":3}"#)),
            ndjson(r#"{"id":4}"#),
        ];
        for body in &bodies {
            let reply = fx.publish_as(key_principal(&writer), &org_query(), body);
            assert_eq!(reply.status, StatusCode::OK, "{}", reply.json);
        }
        // Only the first request is written at once; the rest are counted.
        assert_eq!(audited(&fx, "bus.rest.publish", &writer), (1, 1, 1));
        crate::bus::key_audit::flush_for(&fx.db());
        assert_eq!(audited(&fx, "bus.rest.publish", &writer), (2, 3, 4));
        crate::bus::key_audit::flush_for(&fx.db());
        assert_eq!(audited(&fx, "bus.rest.publish", &writer), (2, 3, 4));
    }

    /// Refusals are windowed the same way, per key and reason: the first is
    /// written at once, the rest counted and flushed.
    #[test]
    fn refused_key_requests_are_counted_and_flushed() {
        let fx = key_fixture("cccc3011");
        let key = fx.key(&[]);
        for topic in [TOPIC, "invoices", "lab.results"] {
            let reply = fx.send_to(
                fx.instance.as_str(),
                topic,
                "POST",
                key_principal(&key),
                &org_query(),
                &ndjson(r#"{"id":1}"#),
            );
            assert_eq!(reply.status, StatusCode::FORBIDDEN);
        }
        assert_eq!(audited(&fx, "bus.rest.publish", &key), (1, 1, 0));
        crate::bus::key_audit::flush_for(&fx.db());
        assert_eq!(audited(&fx, "bus.rest.publish", &key), (2, 3, 0));
    }

    /// Package K, item 5: what the bus itself writes about a key names it as
    /// one — `tf.actor_kind` on its records, `api_key:<uid>` in its audit.
    #[test]
    fn the_bus_names_a_key_as_a_key() {
        let fx = key_fixture("cccc3012");
        let writer = fx.key(&["write"]);
        let reader = fx.key(&["read"]);
        let published = fx.publish_as(key_principal(&writer), &org_query(), &ndjson(r#"{"id":1}"#));
        assert_eq!(published.status, StatusCode::OK, "{}", published.json);
        let read = fx.consume_as(key_principal(&reader), &own_group_query(&reader));
        assert_eq!(read.status, StatusCode::OK, "{}", read.json);
        let headers = &read.json["records"][0]["headers"];
        let header = |name: &str| {
            String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(headers[name].as_str().unwrap())
                    .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(header("tf.actor"), writer);
        assert_eq!(header("tf.actor_kind"), "api_key");

        // A refusal inside the engine (no grant) is audited under the key.
        let key_ctx = BusCallContext {
            actor: Some(reader.clone()),
            actor_kind: ActorKind::ApiKey,
            ..fx.user_ctx("unused")
        };
        let refused = fx.svc.publish(
            &key_ctx,
            TOPIC,
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![PublishRecord {
                    key: None,
                    headers: vec![],
                    payload: Bytes::from_static(br#"{"id":2}"#),
                    timestamp_ms: chrono::Utc::now().timestamp_millis(),
                    schema_id: 0,
                }],
            },
        );
        assert!(matches!(
            refused,
            Err(BusServiceError::PermissionDenied { .. })
        ));
        let denied = fx.audit_rows("bus.produce.denied");
        assert_eq!(denied.len(), 1);
        assert_eq!(
            denied[0].user_id.as_deref(),
            Some(format!("api_key:{reader}").as_str())
        );
    }

    /// Package K, item 1: a `k:` group belongs to its key alone — a user who
    /// may read the topic cannot consume (and so commit) under it.
    #[test]
    fn a_user_cannot_consume_under_a_key_group() {
        let fx = key_fixture("cccc3013");
        let reader = fx.key(&["read"]);
        {
            let conn = fx.state.db.write().unwrap();
            conn.execute(
                "INSERT OR IGNORE INTO organizations (org_id, name, slug, status, created_at) \
                 VALUES (?1, ?1, ?1, 'active', '2026-09-29T00:00:00Z')",
                rusqlite::params![ORG],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO user_accounts (id, username, password_hash, display_name) \
                 VALUES ('u-reader', 'reader', 'x', 'Reader')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
                 VALUES (?1, 'u-reader', 'role-supervisor', '2026-09-29T00:00:00Z', 'test')",
                rusqlite::params![ORG],
            )
            .unwrap();
        }
        fx.grant("u-reader", "bus.read");
        let user = Some(Principal::User {
            user_id: "u-reader".to_string(),
            role: "user".to_string(),
        });
        let own = fx.consume_as(user.clone(), &group_query("lis"));
        assert_eq!(own.status, StatusCode::OK, "{}", own.json);
        for group in [format!("k:{reader}"), format!("k:{reader}.lis")] {
            let taken = fx.consume_as(user.clone(), &group_query(&group));
            assert_eq!(
                taken.status,
                StatusCode::FORBIDDEN,
                "{group}: {}",
                taken.json
            );
        }
        // Nor may another key use this key's group.
        let other = fx.key(&["read"]);
        let foreign = fx.consume_as(key_principal(&other), &own_group_query(&reader));
        assert_eq!(foreign.status, StatusCode::FORBIDDEN);
    }

    /// Package K, item 6: data hiding fails closed for a key. Rules written
    /// only for chosen subjects restrict the topic; a key, which meets only
    /// the topic-wide rule, is refused both ways until that rule exists.
    #[test]
    fn a_key_is_refused_on_a_topic_hidden_only_for_chosen_subjects() {
        let fx = key_fixture("cccc3014");
        let key = fx.key(&["read", "write"]);
        let fields = |names: &[&str]| -> std::collections::BTreeSet<String> {
            names.iter().map(|n| n.to_string()).collect()
        };
        let set = |subject_type: &str, subject_id: &str, direction, allowed: &[&str]| {
            field_policies::set_policy(
                &fx.state.db,
                fx.instance.as_str(),
                ORG,
                TOPIC,
                subject_type,
                subject_id,
                direction,
                &fields(allowed),
                &fields(&[]),
                field_policies::BusFieldPolicyExpect::Any,
            )
            .expect("set policy")
        };
        fx.svc
            .publish(
                &fx.user_ctx("u-admin"),
                TOPIC,
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![PublishRecord {
                        key: None,
                        headers: vec![],
                        payload: Bytes::from_static(br#"{"id":1,"pesel":"44051401359"}"#),
                        timestamp_ms: chrono::Utc::now().timestamp_millis(),
                        schema_id: 0,
                    }],
                },
            )
            .expect("publish");
        set("user", "u-nurse", field_policies::Direction::Read, &["id"]);
        set("user", "u-nurse", field_policies::Direction::Write, &["id"]);

        let read = fx.consume_as(key_principal(&key), &own_group_query(&key));
        assert_eq!(read.status, StatusCode::FORBIDDEN, "{}", read.json);
        let write = fx.publish_as(
            key_principal(&key),
            &org_query(),
            &ndjson(r#"{"id":2,"pesel":"x"}"#),
        );
        assert_eq!(write.status, StatusCode::FORBIDDEN, "{}", write.json);

        set(
            "any",
            field_policies::SUBJECT_ANY,
            field_policies::Direction::Read,
            &["id"],
        );
        set(
            "any",
            field_policies::SUBJECT_ANY,
            field_policies::Direction::Write,
            &["id"],
        );
        let read = fx.consume_as(key_principal(&key), &own_group_query(&key));
        assert_eq!(read.status, StatusCode::OK, "{}", read.json);
        assert_eq!(payloads(&read), vec![serde_json::json!({"id": 1})]);
        let write = fx.publish_as(key_principal(&key), &org_query(), &ndjson(r#"{"id":2}"#));
        assert_eq!(write.status, StatusCode::OK, "{}", write.json);
    }

    // ---- Issuing topic rights to a key from the dashboard (binary protocol) --

    use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode};

    fn dispatch_blocking(
        req: &MessageBody,
        ctx: &crate::dispatch::HandlerContext,
    ) -> Result<MessageBody, ProtocolError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        match rt.block_on(crate::dispatch::dispatch(req, ctx)) {
            (MessageBody::Error(e), true) => Err(e),
            (other, false) => Ok(other),
            (other, is_err) => panic!("unexpected dispatch result {other:?} ({is_err})"),
        }
    }

    /// A site-admin session that also administers `orders` of `org-1`:
    /// `bus.admin` on the instance plus the organisation's admin role — the
    /// gate a key's topic right passes (`require_key_topic_rights_admin`).
    fn topic_admin_ctx(fx: &KeyFixture) -> crate::dispatch::HandlerContext {
        let mut ctx = admin_ctx(fx.state.clone());
        let user_id = session_user(&ctx);
        fx.grant(&user_id, "bus.admin");
        ctx.org_context = Some(crate::services::rbac::OrgContext {
            user_id,
            org_id: ORG.to_string(),
            role_id: "test-role".to_string(),
            permissions: ["org.admin".to_string()].into_iter().collect(),
        });
        ctx
    }

    fn session_user(ctx: &crate::dispatch::HandlerContext) -> String {
        match &ctx.session {
            tentaflow_protocol::SessionAuth::UserSession { user_id, .. } => {
                uuid::Uuid::from_bytes(*user_id).to_string()
            }
            other => panic!("unexpected session {other:?}"),
        }
    }

    fn topic_scope(resource_id: &str, action: Option<&str>) -> tentaflow_protocol::ResourceRef {
        tentaflow_protocol::ResourceRef {
            resource_type: "topic".to_string(),
            resource_id: resource_id.to_string(),
            action: action.map(str::to_string),
        }
    }

    fn create_general_key(
        ctx: &crate::dispatch::HandlerContext,
        scopes: Vec<tentaflow_protocol::ResourceRef>,
    ) -> Result<String, ProtocolError> {
        let req = MessageBody::ApiKeyCreateRequestBody(tentaflow_protocol::ApiKeyCreateRequest {
            name: "Laboratorium LIS".to_string(),
            key_type: "general".to_string(),
            subject_id: None,
            scope_resources: scopes,
        });
        match dispatch_blocking(&req, ctx)? {
            MessageBody::ApiKeyCreateResponseBody(r) => Ok(r.key_id),
            other => panic!("unexpected create response {other:?}"),
        }
    }

    fn scope_set(
        ctx: &crate::dispatch::HandlerContext,
        key_uid: &str,
        resource_id: &str,
        action: Option<&str>,
    ) -> Result<MessageBody, ProtocolError> {
        dispatch_blocking(
            &MessageBody::ApiKeyScopeSetRequest {
                key_uid: key_uid.to_string(),
                resource_type: "topic".to_string(),
                resource_id: resource_id.to_string(),
                access_level: "allow".to_string(),
                action: action.map(str::to_string),
            },
            ctx,
        )
    }

    fn scope_list(ctx: &crate::dispatch::HandlerContext, key_uid: &str) -> Vec<(String, String)> {
        match dispatch_blocking(
            &MessageBody::ApiKeyScopeListRequest {
                key_uid: key_uid.to_string(),
            },
            ctx,
        )
        .expect("scope list")
        {
            MessageBody::ApiKeyScopeListResponse { entries } => {
                let mut rows: Vec<_> = entries
                    .into_iter()
                    .map(|e| (e.resource_type, e.action))
                    .collect();
                rows.sort();
                rows
            }
            other => panic!("unexpected list response {other:?}"),
        }
    }

    fn assert_code(result: Result<impl std::fmt::Debug, ProtocolError>, code: ProtocolErrorCode) {
        let err = result.expect_err("must be refused");
        assert_eq!(err.code, code, "{err:?}");
    }

    /// A key's topic right names `read` or `write` over a well-formed
    /// `(instance, org, topic)` id of an existing topic; anything else is
    /// refused by the scope validation itself, at creation and at set.
    #[test]
    fn topic_scopes_need_read_or_write_on_an_existing_topic() {
        let fx = key_fixture("cccc3101");
        let ctx = topic_admin_ctx(&fx);
        let id = topic_acl_resource_id(fx.instance.as_str(), ORG, TOPIC);

        for action in [None, Some("*"), Some("admin")] {
            assert_code(
                create_general_key(&ctx, vec![topic_scope(&id, action)]),
                ProtocolErrorCode::BadRequest,
            );
        }
        let uid = create_general_key(&ctx, vec![]).unwrap();
        for action in [None, Some("*"), Some("admin")] {
            assert_code(
                scope_set(&ctx, &uid, &id, action),
                ProtocolErrorCode::BadRequest,
            );
        }
        let two_parts =
            crate::sync::resource_id::composite_resource_id(&[fx.instance.as_str(), ORG]);
        let bad_instance = topic_acl_resource_id("not-an-instance", ORG, TOPIC);
        let reserved = topic_acl_resource_id(fx.instance.as_str(), ORG, "__dlq.orders");
        for bad in [
            two_parts.as_str(),
            bad_instance.as_str(),
            reserved.as_str(),
            TOPIC,
        ] {
            assert_code(
                scope_set(&ctx, &uid, bad, Some("read")),
                ProtocolErrorCode::BadRequest,
            );
        }
        let missing = topic_acl_resource_id(fx.instance.as_str(), ORG, "invoices");
        assert_code(
            scope_set(&ctx, &uid, &missing, Some("read")),
            ProtocolErrorCode::NotFound,
        );
        assert!(scope_list(&ctx, &uid).is_empty());
    }

    /// Issued from the dashboard, read and write are separate rows: the
    /// list shows both, clearing one keeps the other, and the REST follows.
    #[test]
    fn dashboard_grants_and_revokes_topic_rights_per_action() {
        let fx = key_fixture("cccc3102");
        let ctx = topic_admin_ctx(&fx);
        let id = topic_acl_resource_id(fx.instance.as_str(), ORG, TOPIC);
        let uid = create_general_key(&ctx, vec![topic_scope(&id, Some("write"))]).unwrap();
        scope_set(&ctx, &uid, &id, Some("read")).expect("grant read");
        assert_eq!(
            scope_list(&ctx, &uid),
            vec![
                ("topic".to_string(), "read".to_string()),
                ("topic".to_string(), "write".to_string()),
            ]
        );
        let published = fx.publish_as(key_principal(&uid), &org_query(), &ndjson(r#"{"id":1}"#));
        assert_eq!(published.status, StatusCode::OK, "{}", published.json);

        dispatch_blocking(
            &MessageBody::ApiKeyScopeClearRequest {
                key_uid: uid.clone(),
                resource_type: "topic".to_string(),
                resource_id: id.clone(),
                action: Some("write".to_string()),
            },
            &ctx,
        )
        .expect("revoke write");
        assert_eq!(
            scope_list(&ctx, &uid),
            vec![("topic".to_string(), "read".to_string())]
        );
        let refused = fx.publish_as(key_principal(&uid), &org_query(), &ndjson(r#"{"id":2}"#));
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        let read = fx.consume_as(key_principal(&uid), &own_group_query(&uid));
        assert_eq!(read.status, StatusCode::OK, "{}", read.json);
        assert_eq!(payloads(&read), vec![serde_json::json!({"id": 1})]);
    }

    /// Package K, item 7: a site administrator is not enough to open a
    /// topic to a key — the key handlers also pass the topic's admin gate for
    /// the organisation the right names, and every change is audited as the
    /// topic access change it is (`bus.acl.set`).
    #[test]
    fn key_topic_rights_need_the_topics_administrator() {
        let fx = key_fixture("cccc3103");
        let id = topic_acl_resource_id(fx.instance.as_str(), ORG, TOPIC);
        let site_admin_only = admin_ctx(fx.state.clone());
        assert_code(
            create_general_key(&site_admin_only, vec![topic_scope(&id, Some("read"))]),
            ProtocolErrorCode::AuthRequired,
        );
        let uid = create_general_key(&site_admin_only, vec![]).unwrap();
        assert_code(
            scope_set(&site_admin_only, &uid, &id, Some("read")),
            ProtocolErrorCode::AuthRequired,
        );

        // Administering another organisation is not enough either.
        let mut other_org = topic_admin_ctx(&fx);
        other_org.org_context.as_mut().unwrap().org_id = "org-2".to_string();
        assert_code(
            scope_set(&other_org, &uid, &id, Some("read")),
            ProtocolErrorCode::PolicyDenied,
        );
        assert!(scope_list(&site_admin_only, &uid).is_empty());

        let admin = topic_admin_ctx(&fx);
        scope_set(&admin, &uid, &id, Some("read")).expect("the topic's admin grants");
        assert_code(
            dispatch_blocking(
                &MessageBody::ApiKeyScopeClearRequest {
                    key_uid: uid.clone(),
                    resource_type: "topic".to_string(),
                    resource_id: id.clone(),
                    action: Some("read".to_string()),
                },
                &site_admin_only,
            ),
            ProtocolErrorCode::AuthRequired,
        );
        dispatch_blocking(
            &MessageBody::ApiKeyScopeClearRequest {
                key_uid: uid.clone(),
                resource_type: "topic".to_string(),
                resource_id: id.clone(),
                action: Some("read".to_string()),
            },
            &admin,
        )
        .expect("the topic's admin revokes");
        let acl_rows: Vec<String> = fx
            .audit_rows("bus.acl.set")
            .into_iter()
            .map(|r| r.details.unwrap_or_default())
            .collect();
        assert_eq!(acl_rows.len(), 2, "{acl_rows:?}");
        assert!(acl_rows
            .iter()
            .all(|d| d.contains(&uid) && d.contains("api_key")));
        assert!(acl_rows.iter().any(|d| d.contains("access_level=allow")));
        assert!(acl_rows.iter().any(|d| d.contains("access_level=clear")));
    }

    /// Package K, review 2: revoking must always work. A right on an
    /// instance that was disabled since is cleared by the topic's admin (no
    /// running engine is asked for), and one on an instance that is gone by
    /// the site administrator alone.
    #[test]
    fn a_key_topic_right_is_revocable_on_a_disabled_or_removed_instance() {
        let fx = key_fixture("cccc3104");
        let id = topic_acl_resource_id(fx.instance.as_str(), ORG, TOPIC);
        let admin = topic_admin_ctx(&fx);
        let uid = create_general_key(
            &admin,
            vec![
                topic_scope(&id, Some("read")),
                topic_scope(&id, Some("write")),
            ],
        )
        .unwrap();
        let clear = |ctx: &crate::dispatch::HandlerContext, action: &str| {
            dispatch_blocking(
                &MessageBody::ApiKeyScopeClearRequest {
                    key_uid: uid.clone(),
                    resource_type: "topic".to_string(),
                    resource_id: id.clone(),
                    action: Some(action.to_string()),
                },
                ctx,
            )
        };

        crate::db::repository::set_addon_enabled(&fx.state.db, fx.instance.as_str(), false)
            .expect("disable instance");
        clear(&admin, "read").expect("the topic's admin revokes on a disabled instance");
        assert_eq!(
            scope_list(&admin, &uid),
            vec![("topic".to_string(), "write".to_string())]
        );

        // While the instance is installed, a site admin alone still may not.
        let site_admin_only = admin_ctx(fx.state.clone());
        assert_code(
            clear(&site_admin_only, "write"),
            ProtocolErrorCode::AuthRequired,
        );

        fx.state
            .db
            .write()
            .unwrap()
            .execute(
                "DELETE FROM addons WHERE addon_id = ?1",
                rusqlite::params![fx.instance.as_str()],
            )
            .expect("remove instance");
        clear(&site_admin_only, "write")
            .expect("the site admin revokes a removed instance's right");
        assert!(scope_list(&site_admin_only, &uid).is_empty());
    }

    /// Package K, review 2: reading a consumer group's lag is not consuming
    /// — an administrator reads a key's `k:` group without being refused
    /// and without a denial written for every poll.
    #[test]
    fn an_admin_reads_a_key_groups_lag() {
        let fx = key_fixture("cccc3105");
        let reader = fx.key(&["read"]);
        let read = fx.consume_as(key_principal(&reader), &own_group_query(&reader));
        assert_eq!(read.status, StatusCode::OK, "{}", read.json);
        fx.grant("u-admin", "bus.read");
        let lag = fx
            .svc
            .group_lag(&fx.user_ctx("u-admin"), &format!("k:{reader}"), TOPIC)
            .expect("the admin reads the key group's lag");
        assert_eq!(lag.len(), 1);
        assert!(fx.audit_rows("bus.consume.denied").is_empty());
    }
}
