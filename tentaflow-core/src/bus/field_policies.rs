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
//   - payload format  -> JSON objects only. This covers FHIR R4 (its
//     resources are JSON); HL7 v2 (pipe-delimited, not JSON) is explicitly
//     out of scope for v1. The client's actual integration list was never
//     confirmed by the owner, so this is a documented default, not a
//     client requirement — non-JSON payloads on a policy-bearing topic fail
//     closed (`BusServiceError::FieldPolicyPayloadNotJson`) rather than
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
}

/// Resolved, decoded policy for one `(org_id, topic, subject, direction)`.
#[derive(Debug, Clone)]
pub struct FieldPolicy {
    pub fields: BTreeSet<String>,
    pub required_fields: BTreeSet<String>,
}

fn decode(row: DbBusFieldPolicy, topic: &str) -> Result<FieldPolicy, BusServiceError> {
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
/// silently stripping the offending fields.
pub fn validate_write(
    policy: &FieldPolicy,
    payload: &[u8],
    topic: &str,
) -> Result<(), BusServiceError> {
    let value: serde_json::Value = serde_json::from_slice(payload).map_err(|_| {
        BusServiceError::FieldPolicyPayloadNotJson {
            topic: topic.to_string(),
        }
    })?;
    let serde_json::Value::Object(obj) = value else {
        return Err(BusServiceError::FieldPolicyPayloadNotJson {
            topic: topic.to_string(),
        });
    };
    let mut disallowed: Vec<String> = obj
        .keys()
        .filter(|k| !policy.fields.contains(*k))
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
        .filter(|f| !obj.contains_key(*f))
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
/// from the result. Fails CLOSED — any payload that is not a JSON object
/// (malformed JSON, or a JSON scalar/array at the top level) becomes an
/// empty object rather than being returned verbatim, since a policy that
/// cannot parse the payload cannot prove what is safe to show.
pub fn project_read(policy: &FieldPolicy, payload: &Bytes) -> Bytes {
    let Ok(serde_json::Value::Object(obj)) = serde_json::from_slice::<serde_json::Value>(payload)
    else {
        return Bytes::from_static(b"{}");
    };
    let filtered: serde_json::Map<String, serde_json::Value> = obj
        .into_iter()
        .filter(|(k, _)| policy.fields.contains(k))
        .collect();
    match serde_json::to_vec(&serde_json::Value::Object(filtered)) {
        Ok(bytes) => Bytes::from(bytes),
        Err(_) => Bytes::from_static(b"{}"),
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
        assert!(validate_write(&p, payload, "t").is_ok());
    }

    #[test]
    fn validate_write_rejects_disallowed_field() {
        let p = policy(&["patient_id"], &[]);
        let payload = br#"{"patient_id":"123","ssn":"999-99-9999"}"#;
        let err = validate_write(&p, payload, "t").unwrap_err();
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
        let err = validate_write(&p, payload, "t").unwrap_err();
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
        let err = validate_write(&p, b"[1,2,3]", "t").unwrap_err();
        assert!(matches!(
            err,
            BusServiceError::FieldPolicyPayloadNotJson { .. }
        ));
    }

    #[test]
    fn validate_write_rejects_malformed_json() {
        let p = policy(&["a"], &[]);
        let err = validate_write(&p, b"not json", "t").unwrap_err();
        assert!(matches!(
            err,
            BusServiceError::FieldPolicyPayloadNotJson { .. }
        ));
    }

    #[test]
    fn project_read_filters_to_allowed_fields_only() {
        let p = policy(&["patient_id"], &[]);
        let payload = Bytes::from_static(br#"{"patient_id":"123","ssn":"999-99-9999"}"#);
        let out = project_read(&p, &payload);
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v, serde_json::json!({"patient_id": "123"}));
    }

    #[test]
    fn project_read_fails_closed_on_non_object_payload() {
        let p = policy(&["a"], &[]);
        let payload = Bytes::from_static(b"not json");
        let out = project_read(&p, &payload);
        assert_eq!(out.as_ref(), b"{}");
    }

    #[test]
    fn project_read_fails_closed_on_top_level_array() {
        let p = policy(&["a"], &[]);
        let payload = Bytes::from_static(b"[1,2,3]");
        let out = project_read(&p, &payload);
        assert_eq!(out.as_ref(), b"{}");
    }
}
