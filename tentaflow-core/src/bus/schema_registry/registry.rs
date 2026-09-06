// =============================================================================
// File: bus/schema_registry/registry.rs — subject/version lifecycle (F3)
// =============================================================================
// SUM/tentabus/PLAN-F3.md §2/§9. This is the org-scoped CRUD half of the
// schema registry: subject registration/versioning/deprecation/deletion and
// "what is the effective schema for this topic's binding right now" — all
// on top of `db/repository.rs`'s `bus_schema_subject_*`/`bus_schema_version_*`
// functions (track A) and `SchemaKindOps` (this module's sibling files,
// `compile`/`check_compatibility`/`derive_subschema`). Free functions taking
// `&DbPool`, same shape as `bus::field_policies::{set_policy,delete_policy,
// list_policies}` — authorization is the dispatch layer's job, never this
// module's (every function here trusts its caller already checked
// `bus.admin`/site-Admin as appropriate for the operation).
//
// Owner decision 3 (delete hard-rejects while a topic binds the subject) is
// enforced in `delete` below by scanning `bus_topic_list` directly — the
// registry has no reason to depend on `bus::topics` for this, a raw
// `schema_id` string comparison against every topic row in the org is
// exactly the check PLAN-F3 §9.3 describes and needs nothing from that
// module's `TopicConfig` parsing.
// =============================================================================

use crate::bus::BusServiceError;
use crate::db::repository::{
    self, BusSchemaVersionInsertError, DbBusSchemaSubject, DbBusSchemaVersion,
};
use crate::db::DbPool;

use super::{
    bump_generation, content_hash, schema_ref_id_for, Compatibility, SchemaError, SchemaType,
    MAX_SCHEMA_TEXT_BYTES,
};

/// Longest a subject name may be — mirrors `bus::topics::MAX_TOPIC_NAME_LEN`
/// in spirit (an admin-chosen identifier, not user content), picked
/// independently since a subject is not a topic and has no DLQ-prefix budget
/// to protect.
pub const MAX_SUBJECT_NAME_LEN: usize = 128;

fn validate_subject_name(subject: &str) -> Result<(), BusServiceError> {
    if subject.is_empty() || subject.len() > MAX_SUBJECT_NAME_LEN {
        return Err(BusServiceError::InvalidArgument(format!(
            "subject name must be 1-{MAX_SUBJECT_NAME_LEN} bytes, got {}",
            subject.len()
        )));
    }
    if !subject
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(BusServiceError::InvalidArgument(format!(
            "subject name '{subject}' contains characters outside [A-Za-z0-9._-]"
        )));
    }
    Ok(())
}

fn decode_subject(
    row: &DbBusSchemaSubject,
) -> Result<(SchemaType, Compatibility), BusServiceError> {
    let schema_type = SchemaType::parse(&row.schema_type).ok_or_else(|| {
        BusServiceError::Db(format!(
            "corrupt bus_schema_subjects.schema_type '{}' for subject '{}'",
            row.schema_type, row.subject
        ))
    })?;
    let compatibility = Compatibility::parse(&row.compatibility).ok_or_else(|| {
        BusServiceError::Db(format!(
            "corrupt bus_schema_subjects.compatibility '{}' for subject '{}'",
            row.compatibility, row.subject
        ))
    })?;
    Ok((schema_type, compatibility))
}

fn require_subject(
    db: &DbPool,
    instance_id: &str,
    org_id: &str,
    subject: &str,
) -> Result<DbBusSchemaSubject, BusServiceError> {
    repository::bus_schema_subject_get(db, instance_id, org_id, subject)?.ok_or_else(|| {
        BusServiceError::SchemaNotFound {
            subject: subject.to_string(),
        }
    })
}

/// Subject-level view — `SchemaSubjectListRequest`'s wire response, and
/// `bus::topics`' binding guard (via a direct `bus_schema_subject_get`, not
/// this struct — that guard only needs the row, not the derived
/// `latest_version`).
#[derive(Debug)]
pub struct SubjectInfo {
    pub subject: String,
    pub schema_type: SchemaType,
    pub compatibility: Compatibility,
    pub deprecated_at_ms: Option<i64>,
    pub latest_version: Option<u32>,
    pub created_by: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// One version's metadata (no `schema_text` — `SchemaVersionListRequest`'s
/// response is metadata-only per PLAN-F3 §6.1; `get` below returns the text).
#[derive(Debug)]
pub struct VersionInfo {
    pub subject: String,
    pub version: u32,
    pub schema_ref_id: u32,
    pub content_hash: String,
    pub created_by: Option<String>,
    pub created_at_ms: i64,
}

fn version_info(row: &DbBusSchemaVersion) -> VersionInfo {
    VersionInfo {
        subject: row.subject.clone(),
        version: row.version,
        schema_ref_id: row.schema_ref_id,
        content_hash: row.content_hash.clone(),
        created_by: row.created_by.clone(),
        created_at_ms: row.created_at_ms,
    }
}

/// `SchemaRegisterRequest`'s response.
#[derive(Debug)]
pub struct RegisterOutcome {
    pub version: u32,
    pub schema_ref_id: u32,
    pub deduplicated: bool,
}

/// The version a topic's `schema_id` binding currently resolves to — highest
/// non-deprecated version of a non-deprecated subject (PLAN-F3 §3).
/// `resolve_effective` returns `None` for a missing OR deprecated subject OR
/// one with zero versions (registration failed/raced) — every one of those
/// is "nothing to validate against" from a publish-time caller's point of
/// view, so they collapse to the same `Option::None`.
#[derive(Debug)]
pub struct EffectiveSchema {
    pub schema_type: SchemaType,
    pub compatibility: Compatibility,
    pub version: u32,
    pub schema_ref_id: u32,
    pub schema_text: String,
}

pub fn list_subjects(
    db: &DbPool,
    instance_id: &str,
    org_id: &str,
) -> Result<Vec<SubjectInfo>, BusServiceError> {
    let rows = repository::bus_schema_subject_list(db, instance_id, org_id)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let (schema_type, compatibility) = decode_subject(&row)?;
        let latest_version =
            repository::bus_schema_version_latest(db, instance_id, org_id, &row.subject)?
                .map(|v| v.version);
        out.push(SubjectInfo {
            subject: row.subject,
            schema_type,
            compatibility,
            deprecated_at_ms: row.deprecated_at_ms,
            latest_version,
            created_by: row.created_by,
            created_at_ms: row.created_at_ms,
            updated_at_ms: row.updated_at_ms,
        });
    }
    Ok(out)
}

pub fn list_versions(
    db: &DbPool,
    instance_id: &str,
    org_id: &str,
    subject: &str,
) -> Result<Vec<VersionInfo>, BusServiceError> {
    require_subject(db, instance_id, org_id, subject)?;
    Ok(
        repository::bus_schema_version_list(db, instance_id, org_id, subject)?
            .iter()
            .map(version_info)
            .collect(),
    )
}

/// `version: None` resolves to the latest version regardless of deprecation
/// (an admin reading a specific/latest version's text is not the same
/// operation as `resolve_effective`'s publish-time binding resolution, which
/// DOES fail closed on a deprecated subject).
pub fn get(
    db: &DbPool,
    instance_id: &str,
    org_id: &str,
    subject: &str,
    version: Option<u32>,
) -> Result<(VersionInfo, String), BusServiceError> {
    require_subject(db, instance_id, org_id, subject)?;
    let row = match version {
        Some(v) => repository::bus_schema_version_get(db, instance_id, org_id, subject, v)?
            .ok_or_else(|| BusServiceError::SchemaVersionNotFound {
                subject: subject.to_string(),
                version: v,
            })?,
        None => repository::bus_schema_version_latest(db, instance_id, org_id, subject)?
            .ok_or_else(|| BusServiceError::SchemaVersionNotFound {
                subject: subject.to_string(),
                version: 0,
            })?,
    };
    let info = version_info(&row);
    Ok((info, row.schema_text))
}

/// Registers a new version (or returns the existing one, content-addressed
/// dedup) for `subject`, creating the subject on first write. See this
/// module's frozen contract (SUM/tentabus/PLAN-F3.md) for the exact
/// semantics; summarized:
///   - subject name / schema text size validated, `compile` must succeed;
///   - an EXISTING subject's `schema_type` must match, and an explicit
///     `compatibility` different from the stored one is rejected (use
///     `set_compatibility`);
///   - registering onto a DEPRECATED subject is rejected;
///   - identical content (by hash) short-circuits to the existing version,
///     `deduplicated: true`, no write;
///   - otherwise checked against the latest version under the subject's
///     compatibility mode (skipped for a brand-new subject, which has
///     nothing to be compatible WITH yet);
///   - a NEW subject defaults to `Backward`; a non-`None` compatibility on a
///     type with no validator (`avro`/`protobuf`/`thrift`) is rejected
///     UNLESS the caller explicitly passes `Some(Compatibility::None)`.
#[allow(clippy::too_many_arguments)]
pub fn register(
    db: &DbPool,
    instance_id: &str,
    org_id: &str,
    subject: &str,
    schema_type: SchemaType,
    schema_text: &str,
    compatibility: Option<Compatibility>,
    created_by: Option<&str>,
) -> Result<RegisterOutcome, BusServiceError> {
    validate_subject_name(subject)?;
    if schema_text.len() > MAX_SCHEMA_TEXT_BYTES {
        return Err(BusServiceError::InvalidArgument(format!(
            "schema text of {} bytes exceeds the {MAX_SCHEMA_TEXT_BYTES}-byte cap",
            schema_text.len()
        )));
    }
    schema_type
        .ops()
        .compile(schema_text)
        .map_err(|e| BusServiceError::InvalidArgument(format!("schema: {e}")))?;

    let now = crate::bus::now_ms();
    let existing = repository::bus_schema_subject_get(db, instance_id, org_id, subject)?;

    let effective_compatibility = match &existing {
        Some(row) => {
            let (existing_type, existing_compat) = decode_subject(row)?;
            if existing_type != schema_type {
                return Err(BusServiceError::InvalidArgument(format!(
                    "subject '{subject}' is registered as {}, cannot register a {} schema",
                    existing_type.as_str(),
                    schema_type.as_str()
                )));
            }
            if row.deprecated_at_ms.is_some() {
                return Err(BusServiceError::InvalidArgument(format!(
                    "subject '{subject}' is deprecated; cannot register a new version"
                )));
            }
            if let Some(requested) = compatibility {
                if requested != existing_compat {
                    return Err(BusServiceError::InvalidArgument(
                        "compatibility differs from the subject's stored mode; use \
                         compatibility_set to change it"
                            .to_string(),
                    ));
                }
            }
            existing_compat
        }
        None => {
            let requested = compatibility.unwrap_or(Compatibility::Backward);
            if requested != Compatibility::None && !schema_type.has_validator() {
                return Err(BusServiceError::SchemaTypeUnsupported {
                    schema_type,
                    operation: "check_compatibility",
                });
            }
            requested
        }
    };

    let hash = content_hash(schema_text);
    if let Some(dup) =
        repository::bus_schema_version_by_content_hash(db, instance_id, org_id, subject, &hash)?
    {
        return Ok(RegisterOutcome {
            version: dup.version,
            schema_ref_id: dup.schema_ref_id,
            deduplicated: true,
        });
    }

    // Extracted so the `VersionSlotTaken` retry below can re-run the SAME
    // check against a freshly re-read `latest` (review finding #2) instead
    // of duplicating this logic.
    let check_compat = |latest_row: &DbBusSchemaVersion| -> Result<(), BusServiceError> {
        if effective_compatibility == Compatibility::None {
            return Ok(());
        }
        schema_type
            .ops()
            .check_compatibility(
                &latest_row.schema_text,
                schema_text,
                effective_compatibility,
            )
            .map_err(|e| match e {
                SchemaError::Incompatible(detail) => BusServiceError::SchemaIncompatible {
                    subject: subject.to_string(),
                    mode: effective_compatibility.as_str(),
                    detail,
                },
                SchemaError::Unsupported {
                    schema_type,
                    operation,
                } => BusServiceError::SchemaTypeUnsupported {
                    schema_type,
                    operation,
                },
                other => BusServiceError::InvalidArgument(other.to_string()),
            })
    };

    let latest = repository::bus_schema_version_latest(db, instance_id, org_id, subject)?;
    if let Some(latest_row) = &latest {
        check_compat(latest_row)?;
    }

    let next_version = latest.as_ref().map(|v| v.version + 1).unwrap_or(1);
    let schema_ref_id = schema_ref_id_for(org_id, subject, &hash);

    // Not transactional (review finding #7): `db/repository.rs` has no
    // helper that spans a subject upsert, a version insert, AND both their
    // sync write-captures in one atomic unit — every repository function
    // here manages its own connection acquire/drop/capture, and building
    // that helper would be a much larger repository-wide change than this
    // fix warrants. Every REJECTABLE check (name/size/compile/dedup/
    // compatibility) already runs above, before either write, so the ONLY
    // window left is between the subject upsert below and the version
    // insert after it. That window only matters for a subject THIS CALL
    // just created (`is_new_subject`): if `bus_schema_version_insert` then
    // fails, best-effort delete the subject row again rather than leaving
    // a version-less subject behind — an EXISTING subject being re-upserted
    // (same shape, idempotent) is harmless to leave in place on failure,
    // since it was already there before this call.
    let is_new_subject = existing.is_none();

    let subject_row = DbBusSchemaSubject {
        instance_id: instance_id.to_string(),
        org_id: org_id.to_string(),
        subject: subject.to_string(),
        schema_type: schema_type.as_str().to_string(),
        compatibility: effective_compatibility.as_str().to_string(),
        deprecated_at_ms: None,
        created_by: existing
            .as_ref()
            .and_then(|r| r.created_by.clone())
            .or_else(|| created_by.map(|s| s.to_string())),
        created_at_ms: existing.as_ref().map(|r| r.created_at_ms).unwrap_or(now),
        updated_at_ms: now,
    };
    repository::bus_schema_subject_upsert(db, &subject_row)?;

    let version_row = DbBusSchemaVersion {
        instance_id: instance_id.to_string(),
        org_id: org_id.to_string(),
        subject: subject.to_string(),
        version: next_version,
        schema_text: schema_text.to_string(),
        content_hash: hash,
        schema_ref_id,
        created_by: created_by.map(|s| s.to_string()),
        created_at_ms: now,
    };
    // Best-effort compensation for `is_new_subject`: delete the subject row
    // this call just created, logging (never masking) a secondary failure.
    // Not called for `ContentHashCollision` below — that means a CONCURRENT
    // registration of the identical content won the race and its version
    // row already exists, so the subject is NOT version-less, it is simply
    // owned by the other writer now; deleting it would destroy real data.
    //
    // Review finding #2b: `is_new_subject` alone used to be enough to
    // delete unconditionally — but it only reflects what THIS call's own
    // read saw at the START, before any writes. A concurrent registration
    // of the identical subject can commit ITS version between this call's
    // subject upsert and its own (failing) version insert; deleting the
    // subject at that point would cascade away the concurrent writer's
    // real, already-committed version. Re-check right before deleting:
    // only a subject that is STILL version-less at compensation time is
    // safe to remove.
    let compensate = |db: &DbPool| {
        if !is_new_subject {
            return;
        }
        match repository::bus_schema_version_list(db, instance_id, org_id, subject) {
            Ok(versions) if versions.is_empty() => {
                if let Err(e) =
                    repository::bus_schema_subject_delete(db, instance_id, org_id, subject)
                {
                    tracing::error!(
                        org_id, subject, error = %e,
                        "schema registry: failed to roll back a just-created subject after its \
                         first version insert failed; a version-less subject row may remain"
                    );
                }
            }
            Ok(_) => {
                // A concurrent registration's version now occupies this
                // subject — no longer version-less, so leave it in place
                // rather than destroying real data (review finding #2b).
            }
            Err(e) => {
                tracing::error!(
                    org_id, subject, error = %e,
                    "schema registry: failed to check whether the just-created subject is \
                     still version-less before rollback compensation; leaving it in place \
                     rather than risking deletion of a concurrent registration's data"
                );
            }
        }
    };

    match repository::bus_schema_version_insert(db, &version_row) {
        Ok(()) => {}
        Err(BusSchemaVersionInsertError::SchemaRefIdCollision { .. }) => {
            compensate(db);
            return Err(BusServiceError::SchemaRefIdCollision {
                subject: subject.to_string(),
                version: next_version,
            });
        }
        Err(BusSchemaVersionInsertError::ContentHashCollision { .. }) => {
            // Lost a race with a concurrent identical registration; the
            // winner's row is now authoritative — report IT, not an error.
            return match repository::bus_schema_version_by_content_hash(
                db,
                instance_id,
                org_id,
                subject,
                &version_row.content_hash,
            )? {
                Some(dup) => Ok(RegisterOutcome {
                    version: dup.version,
                    schema_ref_id: dup.schema_ref_id,
                    deduplicated: true,
                }),
                None => Err(BusServiceError::Db(
                    "content_hash collision reported but no matching row found".to_string(),
                )),
            };
        }
        Err(BusSchemaVersionInsertError::VersionSlotTaken { .. }) => {
            // Review finding #2: lost a race for this exact version slot —
            // most often two concurrent registrations of a brand-new
            // subject both computing "version 1" from the same
            // `latest == None` read. Re-read the latest version (now
            // reflecting whatever the concurrent writer just committed),
            // re-run the compatibility check against IT, and retry the
            // insert ONCE. `schema_ref_id` never needs recomputing here —
            // it is content-hash-derived (`schema_ref_id_for`), not
            // slot-derived, so it is unaffected by which slot wins.
            let retried_latest =
                repository::bus_schema_version_latest(db, instance_id, org_id, subject)?;
            if let Some(latest_row) = &retried_latest {
                check_compat(latest_row)?;
            }
            let retried_version = retried_latest.map(|v| v.version + 1).unwrap_or(1);
            let retried_row = DbBusSchemaVersion {
                version: retried_version,
                ..version_row
            };
            match repository::bus_schema_version_insert(db, &retried_row) {
                Ok(()) => {
                    bump_generation();
                    return Ok(RegisterOutcome {
                        version: retried_version,
                        schema_ref_id,
                        deduplicated: false,
                    });
                }
                Err(BusSchemaVersionInsertError::ContentHashCollision { .. }) => {
                    return match repository::bus_schema_version_by_content_hash(
                        db,
                        instance_id,
                        org_id,
                        subject,
                        &retried_row.content_hash,
                    )? {
                        Some(dup) => Ok(RegisterOutcome {
                            version: dup.version,
                            schema_ref_id: dup.schema_ref_id,
                            deduplicated: true,
                        }),
                        None => Err(BusServiceError::Db(
                            "content_hash collision reported but no matching row found".to_string(),
                        )),
                    };
                }
                Err(BusSchemaVersionInsertError::SchemaRefIdCollision { .. }) => {
                    compensate(db);
                    return Err(BusServiceError::SchemaRefIdCollision {
                        subject: subject.to_string(),
                        version: retried_version,
                    });
                }
                Err(BusSchemaVersionInsertError::VersionSlotTaken { .. }) => {
                    // A second consecutive loss is contention this call
                    // does not retry further (PLAN-F3 review finding #2:
                    // "retry once") — the caller can simply register again.
                    compensate(db);
                    return Err(BusServiceError::Db(format!(
                        "schema registry: the version slot for subject '{subject}' was taken \
                         concurrently twice in a row; retry the registration"
                    )));
                }
                Err(BusSchemaVersionInsertError::Other(e)) => {
                    compensate(db);
                    return Err(e.into());
                }
            }
        }
        Err(BusSchemaVersionInsertError::Other(e)) => {
            compensate(db);
            return Err(e.into());
        }
    }

    bump_generation();
    Ok(RegisterOutcome {
        version: next_version,
        schema_ref_id,
        deduplicated: false,
    })
}

/// Changes a subject's compatibility mode. `Compatibility::None` is always
/// accepted; anything else requires a type with a validator in this build
/// (`SchemaType::has_validator`). Rejects a deprecated subject (review
/// finding #8) — same check `register` already applies; a deprecated
/// subject accepts no new versions, so changing the mode it would apply
/// them under is a dangling, pointless write, and `deprecate_only`'s own
/// contract ("stops new bindings/versions") would otherwise have a hole.
pub fn set_compatibility(
    db: &DbPool,
    instance_id: &str,
    org_id: &str,
    subject: &str,
    compatibility: Compatibility,
) -> Result<(), BusServiceError> {
    let row = require_subject(db, instance_id, org_id, subject)?;
    if row.deprecated_at_ms.is_some() {
        return Err(BusServiceError::InvalidArgument(format!(
            "subject '{subject}' is deprecated; cannot change its compatibility mode"
        )));
    }
    let (schema_type, _existing) = decode_subject(&row)?;
    if compatibility != Compatibility::None && !schema_type.has_validator() {
        return Err(BusServiceError::SchemaTypeUnsupported {
            schema_type,
            operation: "check_compatibility",
        });
    }
    let now = crate::bus::now_ms();
    let updated = DbBusSchemaSubject {
        compatibility: compatibility.as_str().to_string(),
        updated_at_ms: now,
        ..row
    };
    repository::bus_schema_subject_upsert(db, &updated)?;
    bump_generation();
    Ok(())
}

/// Deletes a subject entirely (`version: None`) or exactly one version
/// (`Some(v)`), or soft-deprecates it (`deprecate_only: true`, which cannot
/// be combined with a specific `version`). Owner decision 3
/// (SUM/tentabus/PLAN-F3.md §9.3): ANY of these while a topic in the org
/// still has `schema_id == subject` is hard-rejected, listing the offending
/// topics — `deprecate_only` is the soft alternative that leaves existing
/// bindings intact and simply stops new ones (`bus::topics`' binding guard
/// checks `deprecated_at_ms`).
pub fn delete(
    db: &DbPool,
    instance_id: &str,
    org_id: &str,
    subject: &str,
    version: Option<u32>,
    deprecate_only: bool,
) -> Result<Vec<u32>, BusServiceError> {
    let row = require_subject(db, instance_id, org_id, subject)?;
    if deprecate_only && version.is_some() {
        return Err(BusServiceError::InvalidArgument(
            "deprecate_only cannot be combined with a specific version".to_string(),
        ));
    }

    let bound_topics: Vec<String> = repository::bus_topic_list(db, instance_id, org_id)?
        .into_iter()
        .filter(|t| t.schema_id.as_deref() == Some(subject))
        .map(|t| t.name)
        .collect();
    if !bound_topics.is_empty() {
        return Err(BusServiceError::InvalidArgument(format!(
            "schema subject '{subject}' is bound by topics: {}",
            bound_topics.join(", ")
        )));
    }

    if deprecate_only {
        let versions: Vec<u32> =
            repository::bus_schema_version_list(db, instance_id, org_id, subject)?
                .into_iter()
                .map(|v| v.version)
                .collect();
        if row.deprecated_at_ms.is_none() {
            let now = crate::bus::now_ms();
            let updated = DbBusSchemaSubject {
                deprecated_at_ms: Some(now),
                updated_at_ms: now,
                ..row
            };
            repository::bus_schema_subject_upsert(db, &updated)?;
            bump_generation();
        }
        return Ok(versions);
    }

    match version {
        None => {
            let versions: Vec<u32> =
                repository::bus_schema_version_list(db, instance_id, org_id, subject)?
                    .into_iter()
                    .map(|v| v.version)
                    .collect();
            repository::bus_schema_subject_delete(db, instance_id, org_id, subject)?;
            bump_generation();
            Ok(versions)
        }
        Some(v) => {
            repository::bus_schema_version_get(db, instance_id, org_id, subject, v)?.ok_or_else(
                || BusServiceError::SchemaVersionNotFound {
                    subject: subject.to_string(),
                    version: v,
                },
            )?;
            repository::bus_schema_version_delete(db, instance_id, org_id, subject, v)?;
            bump_generation();
            Ok(vec![v])
        }
    }
}

/// Publish-time binding resolution (PLAN-F3 §3): the version a topic's
/// `schema_id == subject` binding currently resolves to. `None` for a
/// missing, deprecated, or version-less subject.
pub fn resolve_effective(
    db: &DbPool,
    instance_id: &str,
    org_id: &str,
    subject: &str,
) -> Result<Option<EffectiveSchema>, BusServiceError> {
    let Some(row) = repository::bus_schema_subject_get(db, instance_id, org_id, subject)? else {
        return Ok(None);
    };
    if row.deprecated_at_ms.is_some() {
        return Ok(None);
    }
    let Some(latest) = repository::bus_schema_version_latest(db, instance_id, org_id, subject)?
    else {
        return Ok(None);
    };
    let (schema_type, compatibility) = decode_subject(&row)?;
    Ok(Some(EffectiveSchema {
        schema_type,
        compatibility,
        version: latest.version,
        schema_ref_id: latest.schema_ref_id,
        schema_text: latest.schema_text,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repository::bus_test_support::create_bus_tables;
    use std::path::Path;

    fn fresh_db() -> DbPool {
        let pool = crate::db::init(Path::new(":memory:")).expect("cannot build test DB");
        create_bus_tables(&pool).expect("bus fixture tables");
        pool
    }

    const V1: &str = r#"{"type":"object","properties":{"a":{"type":"string"}},"required":["a"],
        "additionalProperties":false}"#;
    const V2_ADD_OPTIONAL: &str = r#"{"type":"object","properties":{"a":{"type":"string"},
        "b":{"type":"string"}},"required":["a"],"additionalProperties":false}"#;
    const V2_ADD_REQUIRED: &str = r#"{"type":"object","properties":{"a":{"type":"string"},
        "b":{"type":"string"}},"required":["a","b"],"additionalProperties":false}"#;

    #[test]
    fn register_returns_v1_then_v2() {
        let db = fresh_db();
        let out1 = register(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            V1,
            None,
            Some("alice"),
        )
        .unwrap();
        assert_eq!(out1.version, 1);
        assert!(!out1.deduplicated);

        let out2 = register(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            V2_ADD_OPTIONAL,
            None,
            Some("alice"),
        )
        .unwrap();
        assert_eq!(out2.version, 2);
        assert!(!out2.deduplicated);
        assert_ne!(out1.schema_ref_id, out2.schema_ref_id);
    }

    #[test]
    fn schema_ref_id_is_content_derived_not_slot_derived() {
        // `schema_ref_id` must be derived from CONTENT, not from the
        // version SLOT number: delete a version, then register text
        // identical to it again. It lands in a NEW slot (version numbers
        // are never reused), but because the bytes match exactly it must
        // get the ORIGINAL ref id back. Slot-derived ids would instead
        // hand out a fresh id, leaving old on-disk records (already
        // stamped with the original id) unable to resolve to the
        // re-registered content, and would let a genuinely different
        // schema later claim the vacated slot's id.
        // `Compatibility::None` isolates this from the compatibility
        // matrix (covered separately) — deleting v1 and reintroducing it
        // while v2 (which relaxes `additionalProperties`) is latest would
        // otherwise fail the default Backward check for unrelated reasons.
        let db = fresh_db();
        let out1 = register(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            V1,
            Some(Compatibility::None),
            Some("alice"),
        )
        .unwrap();
        register(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            V2_ADD_OPTIONAL,
            Some(Compatibility::None),
            Some("alice"),
        )
        .unwrap();

        delete(&db, "tentabus-00000001", "org-1", "orders", Some(1), false).unwrap();
        let out3 = register(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            V1,
            Some(Compatibility::None),
            Some("alice"),
        )
        .unwrap();
        assert_eq!(out3.version, 3, "version numbers are never reused");
        assert_eq!(
            out3.schema_ref_id, out1.schema_ref_id,
            "same content must resolve to the same ref id regardless of which \
             version slot it occupies"
        );
    }

    #[test]
    fn register_dedups_on_identical_text() {
        let db = fresh_db();
        let out1 = register(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            V1,
            None,
            None,
        )
        .unwrap();
        let out2 = register(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            V1,
            None,
            None,
        )
        .unwrap();
        assert_eq!(out1.version, out2.version);
        assert_eq!(out1.schema_ref_id, out2.schema_ref_id);
        assert!(out2.deduplicated);
        assert_eq!(
            list_versions(&db, "tentabus-00000001", "org-1", "orders")
                .unwrap()
                .len(),
            1,
            "identical content must not create a second version row"
        );
    }

    #[test]
    fn register_rejects_a_backward_incompatible_new_required_field() {
        let db = fresh_db();
        register(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            V1,
            None,
            None,
        )
        .unwrap();
        let err = register(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            V2_ADD_REQUIRED,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, BusServiceError::SchemaIncompatible { .. }));
    }

    #[test]
    fn register_with_compatibility_none_allows_anything() {
        let db = fresh_db();
        register(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            V1,
            Some(Compatibility::None),
            None,
        )
        .unwrap();
        let out = register(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            V2_ADD_REQUIRED,
            None,
            None,
        )
        .unwrap();
        assert_eq!(out.version, 2);
    }

    #[test]
    fn register_avro_requires_explicit_compatibility_none() {
        let db = fresh_db();
        let avro_text = r#"{"type":"record","name":"X","fields":[]}"#;
        let err = register(
            &db,
            "tentabus-00000001",
            "org-1",
            "events",
            SchemaType::Avro,
            avro_text,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, BusServiceError::SchemaTypeUnsupported { .. }));

        let out = register(
            &db,
            "tentabus-00000001",
            "org-1",
            "events",
            SchemaType::Avro,
            avro_text,
            Some(Compatibility::None),
            None,
        )
        .unwrap();
        assert_eq!(out.version, 1);
    }

    #[test]
    fn delete_hard_rejects_while_a_topic_binds_the_subject_and_succeeds_after_unbinding() {
        let db = fresh_db();
        register(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            V1,
            None,
            None,
        )
        .unwrap();
        crate::bus::topics::create_topic(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders.events",
            crate::bus::topics::TopicOptions {
                schema_id: Some("orders".to_string()),
                ..Default::default()
            },
            tentaflow_protocol::environment::NodeEnvironment::Test,
            1_000,
        )
        .unwrap();

        let err = delete(&db, "tentabus-00000001", "org-1", "orders", None, false).unwrap_err();
        match err {
            BusServiceError::InvalidArgument(msg) => {
                assert!(msg.contains("orders.events"), "{msg}");
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }

        crate::bus::topics::update_topic(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders.events",
            crate::bus::topics::TopicOptions {
                schema_id: Some(String::new()),
                ..Default::default()
            },
            2_000,
        )
        .unwrap();

        let removed = delete(&db, "tentabus-00000001", "org-1", "orders", None, false).unwrap();
        assert_eq!(removed, vec![1]);
        assert!(
            repository::bus_schema_subject_get(&db, "tentabus-00000001", "org-1", "orders")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn deprecate_only_then_resolve_effective_is_none() {
        let db = fresh_db();
        register(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            V1,
            None,
            None,
        )
        .unwrap();
        assert!(
            resolve_effective(&db, "tentabus-00000001", "org-1", "orders")
                .unwrap()
                .is_some()
        );

        let removed = delete(&db, "tentabus-00000001", "org-1", "orders", None, true).unwrap();
        assert_eq!(removed, vec![1]);
        assert!(
            resolve_effective(&db, "tentabus-00000001", "org-1", "orders")
                .unwrap()
                .is_none()
        );
        // Versions themselves must survive a deprecate_only call.
        assert_eq!(
            list_versions(&db, "tentabus-00000001", "org-1", "orders")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn generation_bumps_on_register_set_compatibility_and_delete() {
        let db = fresh_db();
        let g0 = super::super::generation();
        register(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            V1,
            None,
            None,
        )
        .unwrap();
        let g1 = super::super::generation();
        assert!(g1 > g0);

        set_compatibility(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            Compatibility::Full,
        )
        .unwrap();
        let g2 = super::super::generation();
        assert!(g2 > g1);

        delete(&db, "tentabus-00000001", "org-1", "orders", None, false).unwrap();
        let g3 = super::super::generation();
        assert!(g3 > g2);
    }

    #[test]
    fn register_rejects_an_invalid_subject_name() {
        let db = fresh_db();
        let err = register(
            &db,
            "tentabus-00000001",
            "org-1",
            "",
            SchemaType::JsonSchema,
            V1,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, BusServiceError::InvalidArgument(_)));

        let err = register(
            &db,
            "tentabus-00000001",
            "org-1",
            "has a space",
            SchemaType::JsonSchema,
            V1,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, BusServiceError::InvalidArgument(_)));
    }

    #[test]
    fn register_rejects_oversize_schema_text() {
        let db = fresh_db();
        let huge = format!(
            r#"{{"type":"object","description":"{}"}}"#,
            "x".repeat(MAX_SCHEMA_TEXT_BYTES)
        );
        let err = register(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            &huge,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, BusServiceError::InvalidArgument(_)));
    }

    #[test]
    fn register_rejects_an_unsupported_keyword() {
        let db = fresh_db();
        let bad = r#"{"type":"object","if":{}}"#;
        let err = register(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            bad,
            None,
            None,
        )
        .unwrap_err();
        match err {
            BusServiceError::InvalidArgument(msg) => assert!(msg.starts_with("schema: ")),
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn set_compatibility_rejects_a_deprecated_subject() {
        let db = fresh_db();
        register(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            V1,
            None,
            None,
        )
        .unwrap();
        delete(&db, "tentabus-00000001", "org-1", "orders", None, true).unwrap();

        let err = set_compatibility(
            &db,
            "tentabus-00000001",
            "org-1",
            "orders",
            Compatibility::Full,
        )
        .unwrap_err();
        assert!(matches!(err, BusServiceError::InvalidArgument(_)));
    }

    #[test]
    fn register_rolls_back_a_brand_new_subject_when_its_first_version_insert_fails() {
        // Review finding #7: `register` upserts the subject row before
        // inserting its first version. If that insert then fails, a
        // subject with ZERO versions must not survive the call — best-
        // effort compensation deletes it again.
        let db = fresh_db();
        let new_text = r#"{"type":"object","properties":{"z":{"type":"string"}},
            "additionalProperties":false}"#;
        let hash = content_hash(new_text);
        let colliding_id = schema_ref_id_for("org-1", "new-subject", &hash);

        // Seed a DIFFERENT subject that already occupies `colliding_id` —
        // forces `bus_schema_version_insert` to fail with
        // `SchemaRefIdCollision` the moment `register` tries to claim the
        // same id for "new-subject"'s very first version.
        repository::bus_schema_subject_upsert(
            &db,
            &DbBusSchemaSubject {
                instance_id: "tentabus-00000001".to_string(),
                org_id: "org-1".to_string(),
                subject: "occupant".to_string(),
                schema_type: SchemaType::JsonSchema.as_str().to_string(),
                compatibility: Compatibility::None.as_str().to_string(),
                deprecated_at_ms: None,
                created_by: None,
                created_at_ms: 1,
                updated_at_ms: 1,
            },
        )
        .unwrap();
        repository::bus_schema_version_insert(
            &db,
            &DbBusSchemaVersion {
                instance_id: "tentabus-00000001".to_string(),
                org_id: "org-1".to_string(),
                subject: "occupant".to_string(),
                version: 1,
                schema_text: "{}".to_string(),
                content_hash: "occupant-hash".to_string(),
                schema_ref_id: colliding_id,
                created_by: None,
                created_at_ms: 1,
            },
        )
        .unwrap();

        let err = register(
            &db,
            "tentabus-00000001",
            "org-1",
            "new-subject",
            SchemaType::JsonSchema,
            new_text,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, BusServiceError::SchemaRefIdCollision { .. }));

        assert!(
            repository::bus_schema_subject_get(&db, "tentabus-00000001", "org-1", "new-subject")
                .unwrap()
                .is_none(),
            "a version-less subject must not survive a failed first registration"
        );
    }
}
