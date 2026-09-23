// =============================================================================
// File: api/bus_schema_rest.rs — REST surface of the TentaBus schema registry
//       for external producers (F3-REST)
// =============================================================================
//
// SUM/tentabus/PLAN-F4-REST.md §A. Seven routes under
// `/v1/bus/instances/{instance_id}/schemas/subjects...`, each with a mandatory
// `?org_id=`, all thin wrappers over `bus::schema_registry::registry` — the same
// functions the dashboard's WS handlers (`dispatch/bus.rs`) call, so a REST
// registration and a dashboard one are indistinguishable afterwards.
//
// Who may call it. Only a GENERAL API key (`Principal::ApiKey`) with an explicit
// `resource_permissions` allow row on
// `('bus_schema_registry', composite_resource_id([instance_id, org_id]))` for
// the route's action. Read (the four GETs) and write (register, compatibility,
// delete) are separate rows and neither implies the other — a key that may
// publish schemas cannot read them back unless it was also given `read`
// (owner decision 23.09). A user- or group-bound key is refused outright: the
// registry is org-wide configuration, and such a key would otherwise act with
// a person's whole role instead of a scope an admin chose for this purpose.
//
// Order of checks, cheapest and least revealing first: principal → query →
// instance id SHAPE → scope → instance availability (`bus_rest::
// resolve_engine`, so a disabled or unknown instance answers exactly as the
// records REST does) → body → registry. The scope is checked before the
// instance is looked up, so a key without a grant learns nothing about which
// instances exist.
//
// Field policies and the subschemas derived from them are deliberately absent
// here — they are dashboard-only (owner decision F3-REST).
//
// Audit: every write request leaves one `audit_log` row whatever its outcome,
// under the action names the WS handlers already use (`bus.schema.register`,
// `bus.schema.compatibility.set`, `bus.schema.deprecate`, `bus.schema.delete`).
// A read leaves a row only when it was refused (`bus.schema.read`) — reading a
// schema is routine traffic, a refused read is a signal. Schema text is never
// logged.
//
// Rate limiting happens in the `/v1` gate (`api/unified_server.rs`, per key)
// before any of this runs.

use std::sync::Arc;

use http_body_util::{BodyExt, Limited};
use hyper::body::Bytes;
use hyper::{Request, Response, StatusCode};

use crate::api::bus_rest::{error_response, json_response, map_bus_error, resolve_engine};
use crate::api::openai::server::{OpenAIBody, V1PeerIp};
use crate::auth::acl::Principal;
use crate::bus::instance::BusInstanceId;
use crate::bus::schema_registry::{registry, Compatibility, SchemaType};
use crate::bus::BusServiceError;
use crate::db::DbPool;
use crate::routing::router::Router;

/// ACL resource type carrying API-key schema registry scopes in
/// `resource_permissions`.
pub const BUS_SCHEMA_REGISTRY_RESOURCE_TYPE: &str = "bus_schema_registry";

/// Largest request body accepted by the two body-carrying routes. The schema
/// text itself is capped at `MAX_SCHEMA_TEXT_BYTES` (256 KiB) by the registry;
/// the extra 64 KiB covers JSON string escaping of that text.
pub const MAX_SCHEMA_REGISTER_BODY_BYTES: usize = 320 * 1024;

/// What a route needs from the key's scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaAccess {
    Read,
    Write,
}

impl SchemaAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

/// One of the seven routes, with its path segments. `version` stays raw here
/// (`"latest"` or a number) so a malformed one is a 400 from the handler, not
/// an unmatched path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaRoute {
    ListSubjects,
    ListVersions { subject: String },
    GetVersion { subject: String, version: String },
    GetRefId { subject: String, version: String },
    Register { subject: String },
    SetCompatibility { subject: String },
    Delete { subject: String },
}

impl SchemaRoute {
    pub fn access(&self) -> SchemaAccess {
        match self {
            Self::ListSubjects
            | Self::ListVersions { .. }
            | Self::GetVersion { .. }
            | Self::GetRefId { .. } => SchemaAccess::Read,
            Self::Register { .. } | Self::SetCompatibility { .. } | Self::Delete { .. } => {
                SchemaAccess::Write
            }
        }
    }

    fn subject(&self) -> Option<&str> {
        match self {
            Self::ListSubjects => None,
            Self::ListVersions { subject }
            | Self::GetVersion { subject, .. }
            | Self::GetRefId { subject, .. }
            | Self::Register { subject }
            | Self::SetCompatibility { subject }
            | Self::Delete { subject } => Some(subject),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::ListSubjects => "list_subjects",
            Self::ListVersions { .. } => "list_versions",
            Self::GetVersion { .. } => "get_version",
            Self::GetRefId { .. } => "get_schema_ref_id",
            Self::Register { .. } => "register",
            Self::SetCompatibility { .. } => "set_compatibility",
            Self::Delete { .. } => "delete",
        }
    }

    /// Audit action — the WS handlers' names for the writes. `deprecate_only`
    /// is only known once the query parsed; a delete refused before that is
    /// recorded as the delete it asked to be.
    fn audit_action(&self, deprecate_only: bool) -> &'static str {
        match self {
            Self::Register { .. } => "bus.schema.register",
            Self::SetCompatibility { .. } => "bus.schema.compatibility.set",
            Self::Delete { .. } if deprecate_only => "bus.schema.deprecate",
            Self::Delete { .. } => "bus.schema.delete",
            _ => "bus.schema.read",
        }
    }
}

/// Matches `(method, path)` against the seven routes and returns the raw
/// instance segment with the route. Every other shape (including a known path
/// under the wrong method) is unmatched and falls through to the normal 404.
/// Instance ids (`tentabus-<8 hex>`) and subject names (`[A-Za-z0-9._-]`) never
/// contain `/`, so plain splitting is exact; an empty segment (a doubled slash)
/// never matches.
pub fn parse_bus_schema_route(method: &str, path: &str) -> Option<(String, SchemaRoute)> {
    let rest = path.strip_prefix("/v1/bus/instances/")?;
    let (instance, tail) = rest.split_once("/schemas/subjects")?;
    if instance.is_empty() || instance.contains('/') {
        return None;
    }
    let route = if tail.is_empty() {
        (method == "GET").then_some(SchemaRoute::ListSubjects)?
    } else {
        let segments: Vec<&str> = tail.strip_prefix('/')?.split('/').collect();
        if segments.iter().any(|s| s.is_empty()) {
            return None;
        }
        let subject = segments[0].to_string();
        match (method, &segments[1..]) {
            ("DELETE", []) => SchemaRoute::Delete { subject },
            ("GET", ["versions"]) => SchemaRoute::ListVersions { subject },
            ("POST", ["versions"]) => SchemaRoute::Register { subject },
            ("GET", ["versions", version]) => SchemaRoute::GetVersion {
                subject,
                version: (*version).to_string(),
            },
            ("GET", ["versions", version, "id"]) => SchemaRoute::GetRefId {
                subject,
                version: (*version).to_string(),
            },
            ("PUT", ["compatibility"]) => SchemaRoute::SetCompatibility { subject },
            _ => return None,
        }
    };
    Some((instance.to_string(), route))
}

#[derive(Debug, Default, PartialEq, Eq)]
struct SchemaQuery {
    org_id: Option<String>,
    version: Option<String>,
    deprecate_only: Option<bool>,
}

/// Strict query parse (same shape as `bus_rest::parse_query`): unknown or
/// duplicate keys are errors, values are URL-decoded. `version` and
/// `deprecate_only` exist only on DELETE.
fn parse_query(raw: &str, is_delete: bool) -> Result<SchemaQuery, &'static str> {
    let mut q = SchemaQuery::default();
    for piece in raw.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = piece.split_once('=').unwrap_or((piece, ""));
        let decoded = urlencoding::decode(v).map_err(|_| "invalid_query_encoding")?;
        match k {
            "org_id" => {
                if q.org_id.replace(decoded.into_owned()).is_some() {
                    return Err("duplicate_org_id");
                }
            }
            "version" if is_delete => {
                if q.version.replace(decoded.into_owned()).is_some() {
                    return Err("duplicate_version");
                }
            }
            "deprecate_only" if is_delete => {
                let value = match decoded.as_ref() {
                    "true" | "1" => true,
                    "false" | "0" => false,
                    _ => return Err("invalid_deprecate_only"),
                };
                if q.deprecate_only.replace(value).is_some() {
                    return Err("duplicate_deprecate_only");
                }
            }
            _ => return Err("unknown_query_key"),
        }
    }
    Ok(q)
}

/// `"latest"` → `None`, a positive integer → `Some`. Versions start at 1.
fn parse_version(raw: &str) -> Result<Option<u32>, String> {
    if raw == "latest" {
        return Ok(None);
    }
    match raw.parse::<u32>() {
        Ok(v) if v >= 1 => Ok(Some(v)),
        _ => Err(format!(
            "version must be 'latest' or a positive integer, got '{raw}'"
        )),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterBody {
    schema_type: String,
    schema_text: String,
    #[serde(default)]
    compatibility: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CompatibilityBody {
    compatibility: String,
}

/// Everything the audit row needs, filled in as the request is understood. A
/// request refused early carries only what was known by then.
#[derive(Default)]
struct AuditFacts {
    org_id: Option<String>,
    key_uid: Option<String>,
    deprecate_only: bool,
    reason: Option<String>,
    outcome: Option<serde_json::Value>,
}

/// Router entry point: resolves the database and hands off to `handle`.
pub async fn handle_request(
    req: Request<hyper::body::Incoming>,
    router: Arc<Router>,
    instance: String,
    route: SchemaRoute,
) -> Result<Response<OpenAIBody>, hyper::Error> {
    let Some(db) = router.db.clone() else {
        return Ok(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "internal_error",
            "database unavailable",
        ));
    };
    Ok(handle(req, db, &instance, route).await)
}

/// Serves one schema registry REST request and writes its audit row (always
/// for a write, only on refusal for a read). Generic over the body so tests
/// drive the exact production path with in-memory bodies.
pub async fn handle<B>(
    req: Request<B>,
    db: DbPool,
    instance: &str,
    route: SchemaRoute,
) -> Response<OpenAIBody>
where
    B: hyper::body::Body<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let peer_ip = req.extensions().get::<V1PeerIp>().map(|p| p.0.clone());
    let user_agent = req
        .headers()
        .get(hyper::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.chars().take(256).collect::<String>());
    let mut facts = AuditFacts::default();
    let response = serve(req, &db, instance, &route, &mut facts).await;

    let status = response.status();
    let refused = matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN);
    if route.access() == SchemaAccess::Write || refused {
        audit(
            &db,
            &route,
            instance,
            status,
            &facts,
            peer_ip.as_deref(),
            user_agent.as_deref(),
        );
    }
    response
}

async fn serve<B>(
    req: Request<B>,
    db: &DbPool,
    instance: &str,
    route: &SchemaRoute,
    facts: &mut AuditFacts,
) -> Response<OpenAIBody>
where
    B: hyper::body::Body<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let key_uid = match req.extensions().get::<Principal>() {
        Some(Principal::ApiKey { uid }) => uid.clone(),
        Some(_) => {
            facts.reason = Some("principal_not_general_api_key".to_string());
            return error_response(
                StatusCode::FORBIDDEN,
                "permission_error",
                "the schema registry REST API accepts only a general API key with a \
                 bus_schema_registry scope",
            );
        }
        None => {
            facts.reason = Some("no_principal".to_string());
            return error_response(
                StatusCode::UNAUTHORIZED,
                "authentication_error",
                "missing API key",
            );
        }
    };
    facts.key_uid = Some(key_uid.clone());

    let is_delete = matches!(route, SchemaRoute::Delete { .. });
    let query = match parse_query(req.uri().query().unwrap_or(""), is_delete) {
        Ok(q) => q,
        Err(e) => {
            facts.reason = Some(e.to_string());
            return error_response(StatusCode::BAD_REQUEST, "invalid_request_error", e);
        }
    };
    facts.deprecate_only = query.deprecate_only.unwrap_or(false);
    let org_id = match query.org_id.as_deref() {
        Some(o) if !o.is_empty() => o.to_string(),
        _ => {
            facts.reason = Some("missing_org_id".to_string());
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "missing required query parameter 'org_id'",
            );
        }
    };
    facts.org_id = Some(org_id.clone());

    // Shape only — no DB read — so the scope check below runs on an id that
    // could name a real instance.
    if let Err(e) = BusInstanceId::parse(instance) {
        facts.reason = Some("invalid_instance_id".to_string());
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            e.to_string(),
        );
    }

    let access = route.access();
    let scope_id = crate::sync::resource_id::composite_resource_id(&[instance, &org_id]);
    if !crate::auth::acl::check_v1_access(
        db,
        BUS_SCHEMA_REGISTRY_RESOURCE_TYPE,
        &scope_id,
        access.as_str(),
        &Principal::ApiKey {
            uid: key_uid.clone(),
        },
    ) {
        facts.reason = Some(format!("api_key_scope_denied:{}", access.as_str()));
        return error_response(
            StatusCode::FORBIDDEN,
            "permission_error",
            format!(
                "this API key may not {} schemas of this instance for organisation '{org_id}'",
                access.as_str()
            ),
        );
    }

    // Registry rows live in the platform DB, but they are only this node's to
    // serve while it runs the instance — the same availability rule, and the
    // same 404/503 answers, as the records REST.
    let instance_id = match resolve_engine(db, Some(instance)) {
        Ok((id, _svc)) => id.as_str().to_string(),
        Err(resp) => {
            facts.reason = Some("instance_unavailable".to_string());
            return resp;
        }
    };

    let body = match route {
        SchemaRoute::Register { .. } | SchemaRoute::SetCompatibility { .. } => {
            match read_capped_body(req).await {
                Ok(bytes) => Some(bytes),
                Err(BodyError::TooLarge) => {
                    facts.reason = Some("body_too_large".to_string());
                    return error_response(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "payload_too_large",
                        format!(
                            "request body exceeds the {MAX_SCHEMA_REGISTER_BODY_BYTES}-byte limit"
                        ),
                    );
                }
                Err(BodyError::Unreadable(message)) => {
                    facts.reason = Some("body_unreadable".to_string());
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_request_error",
                        format!("failed to read request body: {message}"),
                    );
                }
            }
        }
        _ => None,
    };

    let ctx = RegistryCall {
        db: db.clone(),
        instance_id,
        org_id,
        created_by: format!("api_key:{key_uid}"),
    };
    match run_route(ctx, route, body, &query).await {
        Ok((status, json, outcome)) => {
            facts.outcome = outcome;
            json_response(status, serde_json::to_vec(&json).unwrap_or_default())
        }
        Err(RouteError::BadRequest(message)) => {
            facts.reason = Some(message.clone());
            error_response(StatusCode::BAD_REQUEST, "invalid_request_error", message)
        }
        Err(RouteError::Registry(e)) => {
            facts.reason = Some(e.to_string());
            map_bus_error(&e)
        }
        Err(RouteError::Internal(message)) => {
            facts.reason = Some(message.clone());
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
        }
    }
}

enum BodyError {
    TooLarge,
    Unreadable(String),
}

/// Refuses an over-cap body twice: from `Content-Length` before a byte is
/// read, and — for a body that declared no length or a false one — while
/// collecting, via `Limited`, which stops at the cap instead of buffering an
/// unbounded stream and measuring it afterwards.
async fn read_capped_body<B>(req: Request<B>) -> Result<Bytes, BodyError>
where
    B: hyper::body::Body<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let declared = req
        .headers()
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|len| len > MAX_SCHEMA_REGISTER_BODY_BYTES as u64) {
        return Err(BodyError::TooLarge);
    }
    match Limited::new(req.into_body(), MAX_SCHEMA_REGISTER_BODY_BYTES)
        .collect()
        .await
    {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(e) if e.is::<http_body_util::LengthLimitError>() => Err(BodyError::TooLarge),
        Err(e) => Err(BodyError::Unreadable(e.to_string())),
    }
}

struct RegistryCall {
    db: DbPool,
    instance_id: String,
    org_id: String,
    created_by: String,
}

enum RouteError {
    BadRequest(String),
    Registry(BusServiceError),
    Internal(String),
}

impl From<BusServiceError> for RouteError {
    fn from(e: BusServiceError) -> Self {
        Self::Registry(e)
    }
}

type RouteOk = (StatusCode, serde_json::Value, Option<serde_json::Value>);

fn parse_json<T: serde::de::DeserializeOwned>(body: Option<Bytes>) -> Result<T, RouteError> {
    serde_json::from_slice(body.as_deref().unwrap_or_default())
        .map_err(|e| RouteError::BadRequest(format!("invalid JSON body: {e}")))
}

fn parse_compatibility(raw: &str) -> Result<Compatibility, RouteError> {
    Compatibility::parse(raw).ok_or_else(|| {
        RouteError::BadRequest(
            "compatibility must be one of none|backward|forward|full".to_string(),
        )
    })
}

/// Runs the registry call on the blocking pool (SQLite I/O) and shapes the
/// JSON answer. The third element is the audit summary of a successful write.
async fn run_route(
    call: RegistryCall,
    route: &SchemaRoute,
    body: Option<Bytes>,
    query: &SchemaQuery,
) -> Result<RouteOk, RouteError> {
    // Everything request-derived is validated here, before the blocking hop,
    // so a malformed request never occupies a blocking thread.
    enum Op {
        ListSubjects,
        ListVersions(String),
        Get(String, Option<u32>, bool),
        Register(String, SchemaType, String, Option<Compatibility>),
        SetCompatibility(String, Compatibility),
        Delete(String, Option<u32>, bool),
    }
    let op = match route {
        SchemaRoute::ListSubjects => Op::ListSubjects,
        SchemaRoute::ListVersions { subject } => Op::ListVersions(subject.clone()),
        SchemaRoute::GetVersion { subject, version } => Op::Get(
            subject.clone(),
            parse_version(version).map_err(RouteError::BadRequest)?,
            false,
        ),
        SchemaRoute::GetRefId { subject, version } => Op::Get(
            subject.clone(),
            parse_version(version).map_err(RouteError::BadRequest)?,
            true,
        ),
        SchemaRoute::Register { subject } => {
            let parsed: RegisterBody = parse_json(body)?;
            let kind = SchemaType::parse(&parsed.schema_type).ok_or_else(|| {
                RouteError::BadRequest(
                    "schema_type must be one of json_schema|avro|protobuf|thrift".to_string(),
                )
            })?;
            let compatibility = parsed
                .compatibility
                .as_deref()
                .map(parse_compatibility)
                .transpose()?;
            Op::Register(subject.clone(), kind, parsed.schema_text, compatibility)
        }
        SchemaRoute::SetCompatibility { subject } => {
            let parsed: CompatibilityBody = parse_json(body)?;
            Op::SetCompatibility(subject.clone(), parse_compatibility(&parsed.compatibility)?)
        }
        SchemaRoute::Delete { subject } => {
            let version = match query.version.as_deref() {
                None => None,
                Some("latest") => {
                    return Err(RouteError::BadRequest(
                        "delete needs an explicit version number, not 'latest'".to_string(),
                    ))
                }
                Some(raw) => parse_version(raw).map_err(RouteError::BadRequest)?,
            };
            Op::Delete(
                subject.clone(),
                version,
                query.deprecate_only.unwrap_or(false),
            )
        }
    };

    let RegistryCall {
        db,
        instance_id,
        org_id,
        created_by,
    } = call;
    let joined = tokio::task::spawn_blocking(move || -> Result<RouteOk, RouteError> {
        let (db, inst, org) = (&db, instance_id.as_str(), org_id.as_str());
        Ok(match op {
            Op::ListSubjects => {
                let subjects: Vec<_> = registry::list_subjects(db, inst, org)?
                    .into_iter()
                    .map(|s| {
                        serde_json::json!({
                            "subject": s.subject,
                            "schema_type": s.schema_type.as_str(),
                            "compatibility": s.compatibility.as_str(),
                            "deprecated_at_ms": s.deprecated_at_ms,
                            "latest_version": s.latest_version,
                            "created_at_ms": s.created_at_ms,
                            "updated_at_ms": s.updated_at_ms,
                        })
                    })
                    .collect();
                (
                    StatusCode::OK,
                    serde_json::json!({ "subjects": subjects }),
                    None,
                )
            }
            Op::ListVersions(subject) => {
                let versions: Vec<_> = registry::list_versions(db, inst, org, &subject)?
                    .into_iter()
                    .map(|v| {
                        serde_json::json!({
                            "version": v.version,
                            "schema_ref_id": v.schema_ref_id,
                            "content_hash": v.content_hash,
                            "created_at_ms": v.created_at_ms,
                        })
                    })
                    .collect();
                (
                    StatusCode::OK,
                    serde_json::json!({ "subject": subject, "versions": versions }),
                    None,
                )
            }
            Op::Get(subject, version, id_only) => {
                let (info, schema_text) = registry::get(db, inst, org, &subject, version)?;
                let json = if id_only {
                    serde_json::json!({
                        "subject": info.subject,
                        "version": info.version,
                        "schema_ref_id": info.schema_ref_id,
                    })
                } else {
                    serde_json::json!({
                        "subject": info.subject,
                        "version": info.version,
                        "schema_ref_id": info.schema_ref_id,
                        "content_hash": info.content_hash,
                        "created_at_ms": info.created_at_ms,
                        "schema_text": schema_text,
                    })
                };
                (StatusCode::OK, json, None)
            }
            Op::Register(subject, kind, text, compatibility) => {
                let out = registry::register(
                    db,
                    inst,
                    org,
                    &subject,
                    kind,
                    &text,
                    compatibility,
                    Some(&created_by),
                )?;
                let summary = serde_json::json!({
                    "schema_type": kind.as_str(),
                    "version": out.version,
                    "deduplicated": out.deduplicated,
                });
                // 201 only when a version was actually created; an identical
                // re-registration answers with the version that already exists.
                let status = if out.deduplicated {
                    StatusCode::OK
                } else {
                    StatusCode::CREATED
                };
                (
                    status,
                    serde_json::json!({
                        "subject": subject,
                        "version": out.version,
                        "schema_ref_id": out.schema_ref_id,
                        "deduplicated": out.deduplicated,
                    }),
                    Some(summary),
                )
            }
            Op::SetCompatibility(subject, compatibility) => {
                registry::set_compatibility(db, inst, org, &subject, compatibility)?;
                (
                    StatusCode::OK,
                    serde_json::json!({
                        "subject": subject,
                        "compatibility": compatibility.as_str(),
                    }),
                    Some(serde_json::json!({ "compatibility": compatibility.as_str() })),
                )
            }
            Op::Delete(subject, version, deprecate_only) => {
                let removed = registry::delete(db, inst, org, &subject, version, deprecate_only)?;
                (
                    StatusCode::OK,
                    serde_json::json!({
                        "subject": subject,
                        "removed_versions": removed,
                        "deprecated": deprecate_only,
                    }),
                    Some(serde_json::json!({ "versions": removed })),
                )
            }
        })
    })
    .await;
    joined.map_err(|e| RouteError::Internal(format!("schema registry task failed: {e}")))?
}

fn audit(
    db: &DbPool,
    route: &SchemaRoute,
    instance: &str,
    status: StatusCode,
    facts: &AuditFacts,
    peer_ip: Option<&str>,
    user_agent: Option<&str>,
) {
    let (result, severity) = match status.as_u16() {
        200..=299 => ("ok", "info"),
        401 | 403 => ("denied", "warn"),
        400..=499 => ("rejected", "warn"),
        _ => ("error", "error"),
    };
    let risk_class = match route.access() {
        SchemaAccess::Write => "B",
        SchemaAccess::Read => "C",
    };
    let details = serde_json::json!({
        "surface": "v1.bus.schema.rest",
        "route": route.name(),
        "instance_id": instance,
        "auth": "api_key",
        "api_key_uid": facts.key_uid,
        "http_status": status.as_u16(),
        "reason": facts.reason,
        "outcome": facts.outcome,
        "user_agent": user_agent.unwrap_or_default(),
    })
    .to_string();
    if let Err(e) = crate::db::repository::log_audit_full(
        db,
        None,
        None,
        route.audit_action(facts.deprecate_only),
        Some("bus_schema_subject"),
        Some(route.subject().unwrap_or("*")),
        Some(&details),
        severity,
        risk_class,
        Some(result),
        facts.org_id.as_deref(),
        peer_ip,
        None,
    ) {
        tracing::warn!(error = %e, route = route.name(), "schema registry REST: audit write failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::bus_rest::test_support::start_test_instance;
    use crate::bus::topics::TopicOptions;
    use crate::bus::BusCallContext;
    use crate::db::models::AuditLogFilters;
    use crate::dispatch::state::AppState;
    use http_body_util::{Full, StreamBody};
    use hyper::body::Frame;

    const ORG: &str = "org-rest";
    const SCHEMA_V1: &str =
        r#"{"type":"object","properties":{"a":{"type":"string"}},"required":["a"]}"#;
    const SCHEMA_V2: &str = r#"{"type":"object","properties":{"a":{"type":"string"},"b":{"type":"string"}},"required":["a"]}"#;

    // ---- path parsing ------------------------------------------------------

    fn parsed(method: &str, path: &str) -> Option<(String, SchemaRoute)> {
        parse_bus_schema_route(method, path)
    }

    #[test]
    fn parses_all_seven_routes() {
        let base = "/v1/bus/instances/tentabus-a1b2c3d4/schemas/subjects";
        let inst = "tentabus-a1b2c3d4".to_string();
        let s = || "orders.v1".to_string();
        assert_eq!(
            parsed("GET", base),
            Some((inst.clone(), SchemaRoute::ListSubjects))
        );
        assert_eq!(
            parsed("GET", &format!("{base}/orders.v1/versions")),
            Some((inst.clone(), SchemaRoute::ListVersions { subject: s() }))
        );
        assert_eq!(
            parsed("GET", &format!("{base}/orders.v1/versions/latest")),
            Some((
                inst.clone(),
                SchemaRoute::GetVersion {
                    subject: s(),
                    version: "latest".into()
                }
            ))
        );
        assert_eq!(
            parsed("POST", &format!("{base}/orders.v1/versions")),
            Some((inst.clone(), SchemaRoute::Register { subject: s() }))
        );
        assert_eq!(
            parsed("GET", &format!("{base}/orders.v1/versions/3/id")),
            Some((
                inst.clone(),
                SchemaRoute::GetRefId {
                    subject: s(),
                    version: "3".into()
                }
            ))
        );
        assert_eq!(
            parsed("PUT", &format!("{base}/orders.v1/compatibility")),
            Some((inst.clone(), SchemaRoute::SetCompatibility { subject: s() }))
        );
        assert_eq!(
            parsed("DELETE", &format!("{base}/orders.v1")),
            Some((inst, SchemaRoute::Delete { subject: s() }))
        );
    }

    #[test]
    fn rejects_wrong_methods_and_shapes() {
        let base = "/v1/bus/instances/tentabus-a1b2c3d4/schemas/subjects";
        assert_eq!(parsed("POST", base), None);
        assert_eq!(parsed("DELETE", &format!("{base}/s/versions")), None);
        assert_eq!(parsed("PUT", &format!("{base}/s/versions")), None);
        assert_eq!(parsed("GET", &format!("{base}/s")), None);
        assert_eq!(parsed("GET", &format!("{base}/s/versions/1/id/x")), None);
        assert_eq!(parsed("GET", &format!("{base}/s/unknown")), None);
        assert_eq!(parsed("GET", &format!("{base}//versions")), None);
        assert_eq!(parsed("GET", &format!("{base}/s/versions/")), None);
        assert_eq!(parsed("GET", &format!("{base}X")), None);
        assert_eq!(parsed("GET", "/v1/bus/instances//schemas/subjects"), None);
        assert_eq!(
            parsed("GET", "/v1/bus/instances/a/b/schemas/subjects"),
            None
        );
        // Derived subschemas and field policies have no REST route.
        assert_eq!(parsed("GET", &format!("{base}/s/versions/1/derived")), None);
        assert_eq!(
            parsed(
                "GET",
                "/v1/bus/instances/tentabus-a1b2c3d4/topics/t/records"
            ),
            None
        );
    }

    #[test]
    fn records_and_schema_paths_never_overlap() {
        // A subject named like the records segments still routes to schemas
        // only, and the records parser ignores every schema path.
        let p = "/v1/bus/instances/tentabus-a1b2c3d4/schemas/subjects/topics/versions";
        assert!(parsed("GET", p).is_some());
        assert_eq!(crate::api::bus_rest::parse_bus_records_path(p), None);
    }

    #[test]
    fn query_is_strict() {
        assert_eq!(
            parse_query("org_id=o%2D1", false)
                .unwrap()
                .org_id
                .as_deref(),
            Some("o-1")
        );
        assert_eq!(
            parse_query("org_id=a&org_id=b", false),
            Err("duplicate_org_id")
        );
        assert_eq!(parse_query("version=1", false), Err("unknown_query_key"));
        let q = parse_query("org_id=o&version=2&deprecate_only=true", true).unwrap();
        assert_eq!(q.version.as_deref(), Some("2"));
        assert_eq!(q.deprecate_only, Some(true));
        assert_eq!(
            parse_query("deprecate_only=maybe", true),
            Err("invalid_deprecate_only")
        );
    }

    #[test]
    fn version_parse() {
        assert_eq!(parse_version("latest"), Ok(None));
        assert_eq!(parse_version("7"), Ok(Some(7)));
        assert!(parse_version("0").is_err());
        assert!(parse_version("-1").is_err());
        assert!(parse_version("v1").is_err());
    }

    // ---- handler fixtures --------------------------------------------------

    struct Fixture {
        state: Arc<AppState>,
        _dir: tempfile::TempDir,
        instance: BusInstanceId,
        svc: Arc<crate::bus::BusService>,
    }

    fn fixture(suffix: &str) -> Fixture {
        let state = AppState::for_test();
        let (dir, instance, svc) = start_test_instance(&state, suffix);
        Fixture {
            state,
            _dir: dir,
            instance,
            svc,
        }
    }

    impl Fixture {
        fn db(&self) -> DbPool {
            self.state.db.clone()
        }

        /// A general key with exactly `actions` on this instance + `org`.
        fn key(&self, org: &str, actions: &[&str]) -> String {
            let scope_id =
                crate::sync::resource_id::composite_resource_id(&[self.instance.as_str(), org]);
            let scopes: Vec<(String, String, String)> = actions
                .iter()
                .map(|a| {
                    (
                        BUS_SCHEMA_REGISTRY_RESOURCE_TYPE.to_string(),
                        scope_id.clone(),
                        (*a).to_string(),
                    )
                })
                .collect();
            let (_id, uid) = crate::db::repository::create_api_key_with_scopes(
                &self.state.db,
                &format!("verifier-{}", uuid::Uuid::new_v4()),
                "sk-...test",
                "schema-producer",
                "general",
                None,
                60,
                &scopes,
                None,
                None,
            )
            .expect("create key");
            uid
        }

        fn path(&self, tail: &str) -> String {
            format!(
                "/v1/bus/instances/{}/schemas/subjects{tail}",
                self.instance.as_str()
            )
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn body_json(resp: Response<OpenAIBody>) -> serde_json::Value {
        let bytes = rt().block_on(async move {
            resp.into_body()
                .collect()
                .await
                .expect("collect body")
                .to_bytes()
        });
        serde_json::from_slice(&bytes).expect("json body")
    }

    /// Sends one request through `handle` exactly as the router would, with
    /// `principal` in the extensions and `Content-Length` declared.
    fn send(
        fx: &Fixture,
        method: &str,
        path_and_query: &str,
        principal: Option<Principal>,
        body: &str,
    ) -> Response<OpenAIBody> {
        let path = path_and_query.split('?').next().unwrap();
        let (instance, route) =
            parse_bus_schema_route(method, path).expect("test path must be a schema route");
        let mut builder = Request::builder()
            .method(method)
            .uri(path_and_query)
            .header(hyper::header::CONTENT_LENGTH, body.len().to_string());
        if let Some(p) = principal {
            builder = builder.extension(p);
        }
        let req = builder
            .extension(V1PeerIp("203.0.113.9".to_string()))
            .body(Full::new(Bytes::from(body.to_string())))
            .unwrap();
        rt().block_on(handle(req, fx.db(), &instance, route))
    }

    fn api_key(uid: &str) -> Option<Principal> {
        Some(Principal::ApiKey {
            uid: uid.to_string(),
        })
    }

    fn register_body(schema: &str) -> String {
        serde_json::json!({ "schema_type": "json_schema", "schema_text": schema }).to_string()
    }

    fn audit_rows(fx: &Fixture, action: &str) -> Vec<crate::db::models::AuditLogEntry> {
        crate::db::repository::list_audit_logs(
            &fx.state.db,
            &AuditLogFilters {
                action: Some(action.to_string()),
                ..Default::default()
            },
            0,
            100,
        )
        .expect("list audit")
    }

    // ---- auth matrix -------------------------------------------------------

    #[test]
    fn no_principal_is_401_and_audited() {
        let fx = fixture("cc000001");
        let resp = send(
            &fx,
            "GET",
            &format!("{}?org_id={ORG}", fx.path("")),
            None,
            "",
        );
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(audit_rows(&fx, "bus.schema.read").len(), 1);
    }

    #[test]
    fn user_and_group_principals_are_refused() {
        let fx = fixture("cc000002");
        // Even a site admin's user-bound key: this surface is for scoped keys.
        for principal in [
            Principal::User {
                user_id: "u-admin".into(),
                role: "admin".into(),
            },
            Principal::Group {
                group_id: "g-1".into(),
            },
        ] {
            let resp = send(
                &fx,
                "GET",
                &format!("{}?org_id={ORG}", fx.path("")),
                Some(principal),
                "",
            );
            assert_eq!(resp.status(), StatusCode::FORBIDDEN);
            assert_eq!(body_json(resp)["error"]["type"], "permission_error");
        }
        assert_eq!(audit_rows(&fx, "bus.schema.read").len(), 2);
    }

    #[test]
    fn key_without_scope_is_403_for_read_and_write() {
        let fx = fixture("cc000003");
        let uid = fx.key(ORG, &[]);
        let read = send(
            &fx,
            "GET",
            &format!("{}?org_id={ORG}", fx.path("")),
            api_key(&uid),
            "",
        );
        assert_eq!(read.status(), StatusCode::FORBIDDEN);
        let write = send(
            &fx,
            "POST",
            &format!("{}?org_id={ORG}", fx.path("/s1/versions")),
            api_key(&uid),
            &register_body(SCHEMA_V1),
        );
        assert_eq!(write.status(), StatusCode::FORBIDDEN);
        assert_eq!(audit_rows(&fx, "bus.schema.read").len(), 1);
        let writes = audit_rows(&fx, "bus.schema.register");
        assert_eq!(writes.len(), 1);
        let details: serde_json::Value =
            serde_json::from_str(writes[0].details.as_deref().unwrap()).unwrap();
        assert_eq!(details["api_key_uid"], uid.as_str());
        assert_eq!(details["reason"], "api_key_scope_denied:write");
        assert_eq!(writes[0].ip_address.as_deref(), Some("203.0.113.9"));
    }

    #[test]
    fn read_key_cannot_write() {
        let fx = fixture("cc000004");
        let uid = fx.key(ORG, &["read"]);
        let base = format!("?org_id={ORG}");
        for (method, tail, body) in [
            ("POST", "/s1/versions", register_body(SCHEMA_V1)),
            (
                "PUT",
                "/s1/compatibility",
                r#"{"compatibility":"none"}"#.to_string(),
            ),
            ("DELETE", "/s1", String::new()),
        ] {
            let resp = send(
                &fx,
                method,
                &format!("{}{base}", fx.path(tail)),
                api_key(&uid),
                &body,
            );
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{method} {tail}");
        }
        // The read itself is allowed (empty registry).
        let list = send(
            &fx,
            "GET",
            &format!("{}{base}", fx.path("")),
            api_key(&uid),
            "",
        );
        assert_eq!(list.status(), StatusCode::OK);
    }

    #[test]
    fn write_key_cannot_read() {
        let fx = fixture("cc000005");
        let writer = fx.key(ORG, &["write"]);
        let q = format!("?org_id={ORG}");
        let created = send(
            &fx,
            "POST",
            &format!("{}{q}", fx.path("/s1/versions")),
            api_key(&writer),
            &register_body(SCHEMA_V1),
        );
        assert_eq!(created.status(), StatusCode::CREATED);
        for tail in [
            "",
            "/s1/versions",
            "/s1/versions/latest",
            "/s1/versions/1/id",
        ] {
            let resp = send(
                &fx,
                "GET",
                &format!("{}{q}", fx.path(tail)),
                api_key(&writer),
                "",
            );
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "GET {tail}");
        }
        assert_eq!(audit_rows(&fx, "bus.schema.read").len(), 4);
    }

    #[test]
    fn scope_is_bound_to_instance_and_org() {
        let fx = fixture("cc000006");
        let other_org_key = fx.key("org-other", &["read", "write"]);
        let resp = send(
            &fx,
            "GET",
            &format!("{}?org_id={ORG}", fx.path("")),
            api_key(&other_org_key),
            "",
        );
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // A key for this org on ANOTHER instance id never reaches this one,
        // and an unknown instance is not revealed to it.
        let resp = send(
            &fx,
            "GET",
            &format!("/v1/bus/instances/tentabus-deadbeef/schemas/subjects?org_id={ORG}"),
            api_key(&fx.key(ORG, &["read"])),
            "",
        );
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn deny_row_overrides_allow() {
        let fx = fixture("cc000007");
        let uid = fx.key(ORG, &["read"]);
        let scope_id =
            crate::sync::resource_id::composite_resource_id(&[fx.instance.as_str(), ORG]);
        crate::db::repository::resource_permissions::set_with_action(
            &fx.state.db,
            BUS_SCHEMA_REGISTRY_RESOURCE_TYPE,
            &scope_id,
            "api_key",
            &uid,
            "read",
            "deny",
        )
        .unwrap();
        let resp = send(
            &fx,
            "GET",
            &format!("{}?org_id={ORG}", fx.path("")),
            api_key(&uid),
            "",
        );
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn missing_org_and_bad_instance_are_400() {
        let fx = fixture("cc000008");
        let uid = fx.key(ORG, &["read"]);
        let resp = send(&fx, "GET", &fx.path(""), api_key(&uid), "");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let resp = send(
            &fx,
            "GET",
            &format!("/v1/bus/instances/not-an-instance/schemas/subjects?org_id={ORG}"),
            api_key(&uid),
            "",
        );
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // ---- instance availability --------------------------------------------

    #[test]
    fn disabled_instance_answers_like_records_rest() {
        let fx = fixture("cc000009");
        let uid = fx.key(ORG, &["read", "write"]);
        crate::db::repository::set_addon_enabled(&fx.state.db, fx.instance.as_str(), false)
            .unwrap();
        let resp = send(
            &fx,
            "GET",
            &format!("{}?org_id={ORG}", fx.path("")),
            api_key(&uid),
            "",
        );
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            body_json(resp),
            serde_json::json!({ "error": "bus_instance_not_found" })
        );
        // Same answer for a write, and the write is audited as rejected.
        let resp = send(
            &fx,
            "POST",
            &format!("{}?org_id={ORG}", fx.path("/s1/versions")),
            api_key(&uid),
            &register_body(SCHEMA_V1),
        );
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let rows = audit_rows(&fx, "bus.schema.register");
        assert_eq!(rows.len(), 1);
        let details: serde_json::Value =
            serde_json::from_str(rows[0].details.as_deref().unwrap()).unwrap();
        assert_eq!(details["reason"], "instance_unavailable");
    }

    // ---- body cap ------------------------------------------------------------

    #[test]
    fn declared_oversize_body_is_413_before_reading() {
        let fx = fixture("cc00000a");
        let uid = fx.key(ORG, &["write"]);
        // The declared length alone decides: the body stream would fail if
        // it were ever polled.
        let failing = futures::stream::once(async {
            Err::<Frame<Bytes>, std::io::Error>(std::io::Error::other("must not be read"))
        });
        let path = fx.path("/s1/versions");
        let (instance, route) = parse_bus_schema_route("POST", &path).unwrap();
        let req = Request::builder()
            .method("POST")
            .uri(format!("{path}?org_id={ORG}"))
            .header(
                hyper::header::CONTENT_LENGTH,
                (MAX_SCHEMA_REGISTER_BODY_BYTES + 1).to_string(),
            )
            .extension(Principal::ApiKey { uid: uid.clone() })
            .body(StreamBody::new(failing))
            .unwrap();
        let resp = rt().block_on(handle(req, fx.db(), &instance, route));
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body_json(resp)["error"]["code"], "payload_too_large");
        assert_eq!(audit_rows(&fx, "bus.schema.register").len(), 1);
    }

    #[test]
    fn undeclared_oversize_body_is_413_while_reading() {
        let fx = fixture("cc00000b");
        let uid = fx.key(ORG, &["write"]);
        let big = "x".repeat(MAX_SCHEMA_REGISTER_BODY_BYTES + 1);
        let path = fx.path("/s1/versions");
        let (instance, route) = parse_bus_schema_route("POST", &path).unwrap();
        // No Content-Length: only the collected length can refuse it.
        let req = Request::builder()
            .method("POST")
            .uri(format!("{path}?org_id={ORG}"))
            .extension(Principal::ApiKey { uid })
            .body(Full::new(Bytes::from(big)))
            .unwrap();
        let resp = rt().block_on(handle(req, fx.db(), &instance, route));
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(
            crate::bus::schema_registry::registry::list_subjects(
                &fx.state.db,
                fx.instance.as_str(),
                ORG
            )
            .unwrap()
            .is_empty(),
            "nothing may be registered from a refused body"
        );
    }

    // ---- full round trip ---------------------------------------------------

    #[test]
    fn full_round_trip_with_dedup_and_deprecate_while_bound() {
        let fx = fixture("cc00000c");
        let uid = fx.key(ORG, &["read", "write"]);
        let q = format!("?org_id={ORG}");
        let key = || api_key(&uid);

        let v1 = send(
            &fx,
            "POST",
            &format!("{}{q}", fx.path("/orders/versions")),
            key(),
            &register_body(SCHEMA_V1),
        );
        assert_eq!(v1.status(), StatusCode::CREATED);
        let v1 = body_json(v1);
        assert_eq!(v1["version"], 1);
        assert_eq!(v1["deduplicated"], false);

        // Identical content → the existing version, 200, no new row.
        let dup = send(
            &fx,
            "POST",
            &format!("{}{q}", fx.path("/orders/versions")),
            key(),
            &register_body(SCHEMA_V1),
        );
        assert_eq!(dup.status(), StatusCode::OK);
        let dup = body_json(dup);
        assert_eq!(dup["version"], 1);
        assert_eq!(dup["deduplicated"], true);
        assert_eq!(dup["schema_ref_id"], v1["schema_ref_id"]);

        let v2 = send(
            &fx,
            "POST",
            &format!("{}{q}", fx.path("/orders/versions")),
            key(),
            &register_body(SCHEMA_V2),
        );
        assert_eq!(v2.status(), StatusCode::CREATED);
        assert_eq!(body_json(v2)["version"], 2);

        let subjects = body_json(send(&fx, "GET", &format!("{}{q}", fx.path("")), key(), ""));
        assert_eq!(subjects["subjects"][0]["subject"], "orders");
        assert_eq!(subjects["subjects"][0]["latest_version"], 2);
        assert_eq!(subjects["subjects"][0]["compatibility"], "backward");
        assert!(subjects["subjects"][0].get("created_by").is_none());

        let versions = body_json(send(
            &fx,
            "GET",
            &format!("{}{q}", fx.path("/orders/versions")),
            key(),
            "",
        ));
        assert_eq!(versions["versions"].as_array().unwrap().len(), 2);

        let latest = body_json(send(
            &fx,
            "GET",
            &format!("{}{q}", fx.path("/orders/versions/latest")),
            key(),
            "",
        ));
        assert_eq!(latest["version"], 2);
        assert_eq!(latest["schema_text"], SCHEMA_V2);

        let id = body_json(send(
            &fx,
            "GET",
            &format!("{}{q}", fx.path("/orders/versions/1/id")),
            key(),
            "",
        ));
        assert_eq!(id["schema_ref_id"], v1["schema_ref_id"]);
        assert!(id.get("schema_text").is_none());

        let missing = send(
            &fx,
            "GET",
            &format!("{}{q}", fx.path("/orders/versions/9")),
            key(),
            "",
        );
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let compat = send(
            &fx,
            "PUT",
            &format!("{}{q}", fx.path("/orders/compatibility")),
            key(),
            r#"{"compatibility":"full"}"#,
        );
        assert_eq!(compat.status(), StatusCode::OK);
        assert_eq!(body_json(compat)["compatibility"], "full");

        // Bind a topic to the subject: a hard delete must now be refused, a
        // deprecation must still succeed (F3-deprecate-only).
        let ctx = BusCallContext {
            instance_id: fx.instance.clone(),
            org_id: ORG.to_string(),
            actor: Some("tester".to_string()),
            correlation_id: None,
            origin: "test".to_string(),
        };
        fx.svc
            .create_topic(
                &ctx,
                "orders",
                TopicOptions {
                    schema_id: Some("orders".to_string()),
                    ..TopicOptions::default()
                },
            )
            .expect("bind topic to subject");

        let hard = send(
            &fx,
            "DELETE",
            &format!("{}{q}", fx.path("/orders")),
            key(),
            "",
        );
        assert_eq!(hard.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(hard)["error"]["message"]
            .as_str()
            .unwrap()
            .contains("bound by topics"));

        let dep = send(
            &fx,
            "DELETE",
            &format!("{}{q}&deprecate_only=true", fx.path("/orders")),
            key(),
            "",
        );
        assert_eq!(dep.status(), StatusCode::OK);
        let dep = body_json(dep);
        assert_eq!(dep["deprecated"], true);
        assert_eq!(dep["removed_versions"], serde_json::json!([1, 2]));

        let after = body_json(send(&fx, "GET", &format!("{}{q}", fx.path("")), key(), ""));
        assert!(after["subjects"][0]["deprecated_at_ms"].is_i64());

        // A deprecated subject takes no new versions.
        let refused = send(
            &fx,
            "POST",
            &format!("{}{q}", fx.path("/orders/versions")),
            key(),
            &register_body(SCHEMA_V1),
        );
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);

        // Audit: every write left a row with its outcome, no read did.
        let registers = audit_rows(&fx, "bus.schema.register");
        assert_eq!(registers.len(), 4, "3 accepted + 1 refused registration");
        assert!(
            registers.iter().all(|r| r
                .details
                .as_deref()
                .is_some_and(|d| !d.contains("properties"))),
            "schema text must never reach the audit log"
        );
        assert_eq!(audit_rows(&fx, "bus.schema.compatibility.set").len(), 1);
        assert_eq!(audit_rows(&fx, "bus.schema.delete").len(), 1);
        assert_eq!(audit_rows(&fx, "bus.schema.deprecate").len(), 1);
        assert!(audit_rows(&fx, "bus.schema.read").is_empty());
    }

    #[test]
    fn malformed_bodies_and_versions_are_400() {
        let fx = fixture("cc00000d");
        let uid = fx.key(ORG, &["read", "write"]);
        let q = format!("?org_id={ORG}");
        for body in [
            "not json",
            r#"{"schema_type":"xml","schema_text":"{}"}"#,
            r#"{"schema_type":"json_schema","schema_text":"{}","extra":1}"#,
            r#"{"schema_type":"json_schema","schema_text":"{}","compatibility":"sideways"}"#,
        ] {
            let resp = send(
                &fx,
                "POST",
                &format!("{}{q}", fx.path("/s/versions")),
                api_key(&uid),
                body,
            );
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{body}");
        }
        let resp = send(
            &fx,
            "GET",
            &format!("{}{q}", fx.path("/s/versions/zero")),
            api_key(&uid),
            "",
        );
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let resp = send(
            &fx,
            "DELETE",
            &format!("{}{q}&version=latest", fx.path("/s")),
            api_key(&uid),
            "",
        );
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let resp = send(
            &fx,
            "GET",
            &format!("{}{q}&deprecate_only=true", fx.path("")),
            api_key(&uid),
            "",
        );
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn rest_registration_is_visible_to_the_dashboard_registry() {
        let fx = fixture("cc00000e");
        let uid = fx.key(ORG, &["write"]);
        let resp = send(
            &fx,
            "POST",
            &format!("{}?org_id={ORG}", fx.path("/shared/versions")),
            api_key(&uid),
            &register_body(SCHEMA_V1),
        );
        assert_eq!(resp.status(), StatusCode::CREATED);
        let (info, text) = crate::bus::schema_registry::registry::get(
            &fx.state.db,
            fx.instance.as_str(),
            ORG,
            "shared",
            None,
        )
        .unwrap();
        assert_eq!(info.version, 1);
        assert_eq!(text, SCHEMA_V1);
        assert_eq!(
            info.created_by.as_deref(),
            Some(format!("api_key:{uid}").as_str())
        );
    }

    // ---- issuing the scope from the dashboard (binary protocol) -------------

    /// An admin session backed by a real, active account — the dispatcher
    /// refuses a session whose account does not exist before any handler runs.
    fn admin_ctx(state: Arc<AppState>) -> crate::dispatch::HandlerContext {
        let id = crate::db::repository::create_user_account(
            &state.db,
            &format!("schema-admin-{}", uuid::Uuid::new_v4()),
            "not-a-login-hash",
            "Schema admin",
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

    fn scope_ref(resource_id: &str, action: Option<&str>) -> tentaflow_protocol::ResourceRef {
        tentaflow_protocol::ResourceRef {
            resource_type: BUS_SCHEMA_REGISTRY_RESOURCE_TYPE.to_string(),
            resource_id: resource_id.to_string(),
            action: action.map(str::to_string),
        }
    }

    fn create_key(
        ctx: &crate::dispatch::HandlerContext,
        scopes: Vec<tentaflow_protocol::ResourceRef>,
    ) -> Result<String, tentaflow_protocol::ProtocolError> {
        let req = tentaflow_protocol::MessageBody::ApiKeyCreateRequestBody(
            tentaflow_protocol::ApiKeyCreateRequest {
                name: "producer".to_string(),
                key_type: "general".to_string(),
                subject_id: None,
                scope_resources: scopes,
            },
        );
        match rt().block_on(crate::dispatch::dispatch(&req, ctx)) {
            (tentaflow_protocol::MessageBody::ApiKeyCreateResponseBody(r), false) => Ok(r.key_id),
            (tentaflow_protocol::MessageBody::Error(e), true) => Err(e),
            (other, _) => panic!("unexpected create response {other:?}"),
        }
    }

    /// A refusal must come from the scope validation itself, not from some
    /// earlier gate (an unknown session, a missing policy) that would make
    /// the assertion pass for the wrong reason.
    fn assert_refused(
        result: Result<String, tentaflow_protocol::ProtocolError>,
        code: tentaflow_protocol::ProtocolErrorCode,
        needle: &str,
    ) {
        let err = result.expect_err("scope must be refused");
        assert_eq!(err.code, code, "{err:?}");
        assert!(err.message.contains(needle), "{err:?} lacks '{needle}'");
    }

    const DEFAULT_ORG: &str = crate::services::org::DEFAULT_ORG_ID;

    #[test]
    fn dashboard_issues_read_and_write_as_separate_grants() {
        let fx = fixture("dd000001");
        let ctx = admin_ctx(fx.state.clone());
        let id =
            crate::sync::resource_id::composite_resource_id(&[fx.instance.as_str(), DEFAULT_ORG]);
        let uid = create_key(
            &ctx,
            vec![scope_ref(&id, Some("read")), scope_ref(&id, Some("write"))],
        )
        .expect("key with both grants");
        let mut actions: Vec<String> =
            crate::db::repository::resource_permissions::list_for_subject(
                &fx.state.db,
                "api_key",
                &uid,
            )
            .unwrap()
            .into_iter()
            .map(|r| r.action)
            .collect();
        actions.sort();
        assert_eq!(actions, vec!["read".to_string(), "write".to_string()]);

        // The issued key is exactly what the REST gate checks.
        let resp = send(
            &fx,
            "GET",
            &format!("{}?org_id={DEFAULT_ORG}", fx.path("")),
            api_key(&uid),
            "",
        );
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn dashboard_refuses_a_scope_without_a_real_action() {
        let fx = fixture("dd000002");
        let ctx = admin_ctx(fx.state.clone());
        let id =
            crate::sync::resource_id::composite_resource_id(&[fx.instance.as_str(), DEFAULT_ORG]);
        let before = crate::db::repository::list_api_keys(&fx.state.db)
            .unwrap()
            .len();
        for action in [None, Some("*"), Some("admin"), Some("READ")] {
            assert_refused(
                create_key(&ctx, vec![scope_ref(&id, action)]),
                tentaflow_protocol::ProtocolErrorCode::BadRequest,
                "need action 'read' or 'write'",
            );
        }
        // An action on a type whose grants carry none is refused too.
        let model_with_action = tentaflow_protocol::ResourceRef {
            resource_type: "model".to_string(),
            resource_id: "gpt-4o".to_string(),
            action: Some("read".to_string()),
        };
        assert_refused(
            create_key(&ctx, vec![model_with_action]),
            tentaflow_protocol::ProtocolErrorCode::BadRequest,
            "action is only valid",
        );
        assert_eq!(
            crate::db::repository::list_api_keys(&fx.state.db)
                .unwrap()
                .len(),
            before
        );
    }

    #[test]
    fn dashboard_refuses_unknown_instances_orgs_and_malformed_ids() {
        let fx = fixture("dd000003");
        let ctx = admin_ctx(fx.state.clone());
        let unknown_instance =
            crate::sync::resource_id::composite_resource_id(&["tentabus-deadbeef", DEFAULT_ORG]);
        let unknown_org =
            crate::sync::resource_id::composite_resource_id(&[fx.instance.as_str(), "org-missing"]);
        let three_parts = crate::sync::resource_id::composite_resource_id(&[
            fx.instance.as_str(),
            DEFAULT_ORG,
            "extra",
        ]);
        use tentaflow_protocol::ProtocolErrorCode::{BadRequest, NotFound};
        for (id, code, needle) in [
            (
                unknown_instance.as_str(),
                NotFound,
                "TentaBus instance not found",
            ),
            (unknown_org.as_str(), NotFound, "organisation not found"),
            (
                three_parts.as_str(),
                BadRequest,
                "exactly an instance and an organisation",
            ),
            (
                fx.instance.as_str(),
                BadRequest,
                "exactly an instance and an organisation",
            ),
        ] {
            assert_refused(
                create_key(&ctx, vec![scope_ref(id, Some("read"))]),
                code,
                needle,
            );
        }

        let org = crate::services::org::create_organization(
            &fx.state.db,
            "Closed",
            "closed",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        crate::services::org::delete_organization(&fx.state.db, &org.org_id).unwrap();
        let deleted =
            crate::sync::resource_id::composite_resource_id(&[fx.instance.as_str(), &org.org_id]);
        assert_refused(
            create_key(&ctx, vec![scope_ref(&deleted, Some("read"))]),
            NotFound,
            "organisation not found",
        );
    }

    #[test]
    fn clearing_one_action_keeps_the_other() {
        let fx = fixture("dd000004");
        let ctx = admin_ctx(fx.state.clone());
        let id =
            crate::sync::resource_id::composite_resource_id(&[fx.instance.as_str(), DEFAULT_ORG]);
        let uid = create_key(
            &ctx,
            vec![scope_ref(&id, Some("read")), scope_ref(&id, Some("write"))],
        )
        .unwrap();
        let clear = tentaflow_protocol::MessageBody::ApiKeyScopeClearRequest {
            key_uid: uid.clone(),
            resource_type: BUS_SCHEMA_REGISTRY_RESOURCE_TYPE.to_string(),
            resource_id: id.clone(),
            action: Some("write".to_string()),
        };
        let (_resp, is_err) = rt().block_on(crate::dispatch::dispatch(&clear, &ctx));
        assert!(!is_err);
        let rows = crate::db::repository::resource_permissions::list_for_subject(
            &fx.state.db,
            "api_key",
            &uid,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, "read");

        // The write route is now refused, the read route still served.
        let q = format!("?org_id={DEFAULT_ORG}");
        let write = send(
            &fx,
            "POST",
            &format!("{}{q}", fx.path("/s/versions")),
            api_key(&uid),
            &register_body(SCHEMA_V1),
        );
        assert_eq!(write.status(), StatusCode::FORBIDDEN);
        let read = send(
            &fx,
            "GET",
            &format!("{}{q}", fx.path("")),
            api_key(&uid),
            "",
        );
        assert_eq!(read.status(), StatusCode::OK);

        // Setting one action through the matrix path stores exactly that row.
        let set = tentaflow_protocol::MessageBody::ApiKeyScopeSetRequest {
            key_uid: uid.clone(),
            resource_type: BUS_SCHEMA_REGISTRY_RESOURCE_TYPE.to_string(),
            resource_id: id,
            access_level: "allow".to_string(),
            action: Some("write".to_string()),
        };
        let (_resp, is_err) = rt().block_on(crate::dispatch::dispatch(&set, &ctx));
        assert!(!is_err);
        let write = send(
            &fx,
            "POST",
            &format!("{}{q}", fx.path("/s/versions")),
            api_key(&uid),
            &register_body(SCHEMA_V1),
        );
        assert_eq!(write.status(), StatusCode::CREATED);
    }

    #[test]
    fn generic_iam_setter_cannot_write_an_action_blind_schema_grant() {
        let fx = fixture("dd000005");
        let ctx = admin_ctx(fx.state.clone());
        let id =
            crate::sync::resource_id::composite_resource_id(&[fx.instance.as_str(), DEFAULT_ORG]);
        let req = tentaflow_protocol::MessageBody::IamBody(
            tentaflow_protocol::IamPayload::ReqSetPermission {
                resource_type: BUS_SCHEMA_REGISTRY_RESOURCE_TYPE.to_string(),
                resource_id: id,
                subject_type: "api_key".to_string(),
                subject_id: "any".to_string(),
                access_level: "allow".to_string(),
            },
        );
        let (resp, is_err) = rt().block_on(crate::dispatch::dispatch(&req, &ctx));
        assert!(is_err);
        let tentaflow_protocol::MessageBody::Error(err) = resp else {
            panic!("expected an error, got {resp:?}");
        };
        assert_eq!(err.code, tentaflow_protocol::ProtocolErrorCode::BadRequest);
        assert!(err.message.contains("set per API key"), "{err:?}");
        assert_eq!(
            crate::db::repository::resource_permissions::count_for_subject(
                &fx.state.db,
                "api_key",
                "any"
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn organisation_list_names_live_organisations_only() {
        let state = AppState::for_test();
        let ctx = admin_ctx(state.clone());
        let gone = crate::services::org::create_organization(
            &state.db, "Gone", "gone", None, None, None, None,
        )
        .unwrap();
        crate::services::org::delete_organization(&state.db, &gone.org_id).unwrap();
        let req = tentaflow_protocol::MessageBody::IamBody(
            tentaflow_protocol::IamPayload::ReqListOrganizations,
        );
        let (resp, is_err) = rt().block_on(crate::dispatch::dispatch(&req, &ctx));
        assert!(!is_err, "{resp:?}");
        let tentaflow_protocol::MessageBody::IamBody(
            tentaflow_protocol::IamPayload::ResListOrganizations { orgs },
        ) = resp
        else {
            panic!("unexpected response {resp:?}");
        };
        assert!(orgs
            .iter()
            .any(|o| o.org_id == DEFAULT_ORG && !o.name.is_empty()));
        assert!(orgs.iter().all(|o| o.org_id != gone.org_id));
    }
}
