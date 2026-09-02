// =============================================================================
// File: bus/field_policies.rs — TentaBus field-level access policies
// =============================================================================
// SUM/tentabus/POLITYKI-POL.md, decided with the owner via AskUserQuestion
// on 01.09.2026:
//   - write violation -> mode=reject: the WHOLE batch is rejected on any
//     disallowed or missing-required field, never stripped.
//   - read violation  -> hide only: a restricted field is simply omitted
//     from the projected payload, no pseudonymization integration.
//   - granularity     -> top-level JSON object keys only, v1 has no nested
//     dotted-path support (e.g. `patient.address.street`).
//   - enforcement     -> external AND internal: a policy applies to EVERY
//     publish/fetch/peek on a policy-bearing topic, not just callers
//     crossing the REST/WS/bridge/host-function boundary. See
//     `BusService::publish`/`ConsumerHandle::fetch`/`BusService::peek` for
//     the three call sites.
//   - payload format  -> pluggable per topic (`payload_format::PayloadFormat`,
//     SUM/tentabus/POLITYKI-POL-FORMATY.md). v1 shipped JSON-object-only
//     (covers FHIR R4, since its resources are JSON); F0 moves that JSON
//     logic behind `payload_format::PayloadFieldFormat` as the reference
//     implementation and routes by the topic's `content_type`, so later
//     phases (XML, HL7 v2, Avro, Protobuf, Thrift) add a format without
//     touching this file's enforcement logic. A payload that does not
//     parse as its topic's resolved format fails closed
//     (`BusServiceError::FieldPolicyPayloadMalformed`) rather than
//     silently bypassing the policy.
//
// A topic with no matching `bus_field_policies` row is entirely unaffected
// (opt-in feature) — `resolve` returning `None` is a plain indexed point
// lookup, same uncached shape as `RbacBusAuthorizer::topic_acl_allows`
// (not `BusService::topic_config_cache`: caching risks a stale "no policy"
// entry surviving after a policy is created, which isn't worth it here
// without a measured need). This keeps the overwhelmingly common no-policy
// case exactly as fast as before this feature — the JSON decode/validate
// cost this file adds is only ever paid by a topic that actually opted in.
// =============================================================================

use std::collections::BTreeSet;

use bytes::Bytes;

use crate::db::repository::{self, DbBusFieldPolicy};
use crate::db::DbPool;

use super::payload_format::PayloadFormat;
use super::{topics, BusServiceError};

/// Sentinel `subject_id` for the topic-wide wildcard row. Chosen over
/// `NULL` specifically to avoid SQLite's NULL-uniqueness pitfall in the
/// table's composite `PRIMARY KEY` — two `subject_type='any'` rows for the
/// same `(org_id, topic, direction)` would otherwise both satisfy a
/// `NULL`-based uniqueness check.
pub const SUBJECT_ANY: &str = "*";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Write,
    Read,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Write => "write",
            Direction::Read => "read",
        }
    }

    /// Inverse of `as_str` — for wire-facing callers (dispatch layer)
    /// decoding the `direction: String` field of `FieldPolicySetRequest`/
    /// `FieldPolicyDeleteRequest`.
    pub fn parse(s: &str) -> Option<Direction> {
        match s {
            "write" => Some(Direction::Write),
            "read" => Some(Direction::Read),
            _ => None,
        }
    }
}

/// Resolved, decoded policy for one `(org_id, topic, subject, direction)`.
#[derive(Debug, Clone)]
pub struct FieldPolicy {
    pub fields: BTreeSet<String>,
    pub required_fields: BTreeSet<String>,
}

pub(crate) fn decode(row: DbBusFieldPolicy, topic: &str) -> Result<FieldPolicy, BusServiceError> {
    let fields: BTreeSet<String> = serde_json::from_str(&row.fields_json).map_err(|e| {
        BusServiceError::Db(format!(
            "corrupt bus_field_policies.fields_json for topic '{topic}': {e}"
        ))
    })?;
    let required_fields: BTreeSet<String> = match row.required_fields_json {
        Some(json) => serde_json::from_str(&json).map_err(|e| {
            BusServiceError::Db(format!(
                "corrupt bus_field_policies.required_fields_json for topic '{topic}': {e}"
            ))
        })?,
        None => BTreeSet::new(),
    };
    Ok(FieldPolicy {
        fields,
        required_fields,
    })
}

/// Resolves the effective policy for `(org_id, topic, direction)` against
/// `actor` — a raw actor string that is EITHER a real `user_id` or an
/// `addon_id`, indistinguishable by type (`BusCallContext.actor` carries no
/// type tag). This deliberately mirrors the same scope limitation
/// `RbacBusAuthorizer::topic_acl_allows` already accepts elsewhere in this
/// codebase (only `subject_type=="user"` is matched against the raw actor)
/// rather than inventing new machinery to fully solve a problem this
/// codebase already tolerates.
///
/// Precedence: an exact `subject_type='user'` row for `actor` wins over the
/// `subject_type='any'` wildcard row. `Ok(None)` means "unrestricted" — no
/// matching row of either kind — which is also forced unconditionally for
/// every `__`-prefixed reserved topic (system-internal topics, e.g.
/// `__bus.metrics`, are never subject-policy-bearing).
pub fn resolve(
    pool: &DbPool,
    org_id: &str,
    topic: &str,
    actor: &str,
    direction: Direction,
) -> Result<Option<FieldPolicy>, BusServiceError> {
    if topic.starts_with(topics::RESERVED_PREFIX) {
        return Ok(None);
    }
    if let Some(row) =
        repository::bus_field_policy_get(pool, org_id, topic, "user", actor, direction.as_str())?
    {
        return decode(row, topic).map(Some);
    }
    if let Some(row) = repository::bus_field_policy_get(
        pool,
        org_id,
        topic,
        "any",
        SUBJECT_ANY,
        direction.as_str(),
    )? {
        return decode(row, topic).map(Some);
    }
    Ok(None)
}

/// Validates one record's payload against a resolved WRITE policy.
/// `mode=reject` (the owner's choice): the first violation found is
/// returned as-is so the caller can fail the whole batch atomically, never
/// silently stripping the offending fields. `format` is the topic's
/// resolved wire format (`payload_format::PayloadFormat::from_content_type`)
/// — the caller resolves it once per topic, not per record.
pub fn validate_write(
    policy: &FieldPolicy,
    format: PayloadFormat,
    payload: &[u8],
    topic: &str,
) -> Result<(), BusServiceError> {
    let codec = format.codec();
    let present = codec.list_fields(payload).map_err(|_| {
        BusServiceError::FieldPolicyPayloadMalformed {
            topic: topic.to_string(),
            format: format.as_str(),
        }
    })?;
    let mut disallowed: Vec<String> = present
        .iter()
        .filter(|f| !policy.fields.contains(*f))
        .cloned()
        .collect();
    if !disallowed.is_empty() {
        disallowed.sort();
        return Err(BusServiceError::FieldNotAllowed {
            topic: topic.to_string(),
            fields: disallowed,
        });
    }
    let mut missing: Vec<String> = policy
        .required_fields
        .iter()
        .filter(|f| !present.contains(*f))
        .cloned()
        .collect();
    if !missing.is_empty() {
        missing.sort();
        return Err(BusServiceError::RequiredFieldMissing {
            topic: topic.to_string(),
            fields: missing,
        });
    }
    Ok(())
}

/// Projects one record's payload through a resolved READ policy.
/// "Hide only" (the owner's choice): a restricted field is simply absent
/// from the result, re-emitted in the SAME wire format. Fails CLOSED — any
/// payload the format's codec cannot parse becomes that format's empty
/// projection (e.g. `{}` for JSON) rather than being returned verbatim,
/// since a policy that cannot parse the payload cannot prove what is safe
/// to show.
pub fn project_read(policy: &FieldPolicy, format: PayloadFormat, payload: &Bytes) -> Bytes {
    let codec = format.codec();
    match codec.project(payload, &policy.fields) {
        Ok(bytes) => bytes,
        Err(_) => codec.empty_projection(),
    }
}

/// Creates or replaces the policy row for `(org_id, topic, subject_type,
/// subject_id, direction)`. `subject_type` must be `"user"` or `"any"`
/// (mirroring the table's own `CHECK` constraint, validated here first so
/// the caller gets a clean `InvalidArgument` instead of a raw SQL error);
/// `subject_type="any"` requires `subject_id == SUBJECT_ANY`.
/// `required_fields` must be a subset of `fields` — a field cannot be
/// required if it is not even allowed.
#[allow(clippy::too_many_arguments)]
pub fn set_policy(
    pool: &DbPool,
    org_id: &str,
    topic: &str,
    subject_type: &str,
    subject_id: &str,
    direction: Direction,
    fields: &BTreeSet<String>,
    required_fields: &BTreeSet<String>,
) -> Result<(), BusServiceError> {
    if subject_type != "user" && subject_type != "any" {
        return Err(BusServiceError::InvalidArgument(format!(
            "subject_type must be 'user' or 'any', got '{subject_type}'"
        )));
    }
    if subject_type == "any" && subject_id != SUBJECT_ANY {
        return Err(BusServiceError::InvalidArgument(format!(
            "subject_type='any' requires subject_id='{SUBJECT_ANY}'"
        )));
    }
    if !required_fields.is_subset(fields) {
        return Err(BusServiceError::InvalidArgument(
            "required_fields must be a subset of fields".to_string(),
        ));
    }
    // SUM/tentabus/POLITYKI-POL-FORMATY.md (F0): a policy's field names are
    // meaningless without a wire format to interpret them against, and a
    // policy on a topic that does not exist is an orphaned row nothing can
    // ever enforce — both fail closed here rather than writing a silently
    // unusable row.
    let cfg = topics::get_topic(pool, org_id, topic)?.ok_or_else(|| {
        BusServiceError::TopicNotFound {
            name: topic.to_string(),
        }
    })?;
    let codec = PayloadFormat::from_content_type(&cfg.content_type).codec();
    for f in fields.iter().chain(required_fields.iter()) {
        codec.validate_field_name(f).map_err(|e| {
            BusServiceError::InvalidArgument(format!(
                "field '{f}' is not a valid field address for topic '{topic}'s payload format: {e}"
            ))
        })?;
    }
    let now = super::now_ms();
    let existing =
        repository::bus_field_policy_get(pool, org_id, topic, subject_type, subject_id, direction.as_str())?;
    let created_at_ms = existing.map(|r| r.created_at_ms).unwrap_or(now);
    let row = DbBusFieldPolicy {
        org_id: org_id.to_string(),
        topic: topic.to_string(),
        subject_type: subject_type.to_string(),
        subject_id: subject_id.to_string(),
        direction: direction.as_str().to_string(),
        fields_json: serde_json::to_string(fields).map_err(|e| BusServiceError::Db(e.to_string()))?,
        required_fields_json: if required_fields.is_empty() {
            None
        } else {
            Some(
                serde_json::to_string(required_fields)
                    .map_err(|e| BusServiceError::Db(e.to_string()))?,
            )
        },
        created_at_ms,
        updated_at_ms: now,
    };
    repository::bus_field_policy_set(pool, &row)?;
    Ok(())
}

pub fn delete_policy(
    pool: &DbPool,
    org_id: &str,
    topic: &str,
    subject_type: &str,
    subject_id: &str,
    direction: Direction,
) -> Result<(), BusServiceError> {
    repository::bus_field_policy_delete(
        pool,
        org_id,
        topic,
        subject_type,
        subject_id,
        direction.as_str(),
    )?;
    Ok(())
}

pub fn list_policies(
    pool: &DbPool,
    org_id: &str,
    topic: &str,
) -> Result<Vec<DbBusFieldPolicy>, BusServiceError> {
    Ok(repository::bus_field_policy_list_for_topic(
        pool, org_id, topic,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(fields: &[&str], required: &[&str]) -> FieldPolicy {
        FieldPolicy {
            fields: fields.iter().map(|s| s.to_string()).collect(),
            required_fields: required.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn validate_write_allows_payload_within_allow_list() {
        let p = policy(&["patient_id", "status"], &[]);
        let payload = br#"{"patient_id":"123","status":"ok"}"#;
        assert!(validate_write(&p, PayloadFormat::Json, payload, "t").is_ok());
    }

    #[test]
    fn validate_write_rejects_disallowed_field() {
        let p = policy(&["patient_id"], &[]);
        let payload = br#"{"patient_id":"123","ssn":"999-99-9999"}"#;
        let err = validate_write(&p, PayloadFormat::Json, payload, "t").unwrap_err();
        match err {
            BusServiceError::FieldNotAllowed { fields, .. } => {
                assert_eq!(fields, vec!["ssn".to_string()]);
            }
            other => panic!("expected FieldNotAllowed, got {other:?}"),
        }
    }

    #[test]
    fn validate_write_rejects_missing_required_field() {
        let p = policy(&["patient_id", "status"], &["status"]);
        let payload = br#"{"patient_id":"123"}"#;
        let err = validate_write(&p, PayloadFormat::Json, payload, "t").unwrap_err();
        match err {
            BusServiceError::RequiredFieldMissing { fields, .. } => {
                assert_eq!(fields, vec!["status".to_string()]);
            }
            other => panic!("expected RequiredFieldMissing, got {other:?}"),
        }
    }

    #[test]
    fn validate_write_rejects_non_object_payload() {
        let p = policy(&["a"], &[]);
        let err = validate_write(&p, PayloadFormat::Json, b"[1,2,3]", "t").unwrap_err();
        assert!(matches!(
            err,
            BusServiceError::FieldPolicyPayloadMalformed { .. }
        ));
    }

    #[test]
    fn validate_write_rejects_malformed_json() {
        let p = policy(&["a"], &[]);
        let err = validate_write(&p, PayloadFormat::Json, b"not json", "t").unwrap_err();
        assert!(matches!(
            err,
            BusServiceError::FieldPolicyPayloadMalformed { .. }
        ));
    }

    #[test]
    fn project_read_filters_to_allowed_fields_only() {
        let p = policy(&["patient_id"], &[]);
        let payload = Bytes::from_static(br#"{"patient_id":"123","ssn":"999-99-9999"}"#);
        let out = project_read(&p, PayloadFormat::Json, &payload);
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v, serde_json::json!({"patient_id": "123"}));
    }

    #[test]
    fn project_read_fails_closed_on_non_object_payload() {
        let p = policy(&["a"], &[]);
        let payload = Bytes::from_static(b"not json");
        let out = project_read(&p, PayloadFormat::Json, &payload);
        assert_eq!(out.as_ref(), b"{}");
    }

    #[test]
    fn project_read_fails_closed_on_top_level_array() {
        let p = policy(&["a"], &[]);
        let payload = Bytes::from_static(b"[1,2,3]");
        let out = project_read(&p, PayloadFormat::Json, &payload);
        assert_eq!(out.as_ref(), b"{}");
    }
}
