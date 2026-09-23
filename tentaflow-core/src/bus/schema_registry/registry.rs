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

/// Test-only interleave hooks for `register`. `#[cfg(test)]` throughout, so
/// neither this cell nor any of the call sites below exist in a release
/// build. They exist because `register`'s `VersionSlotTaken` retry — and,
/// past it, the double-loss arm that is the only way two concurrent
/// `register` calls reach `compensate` — is reachable only when callers sit
/// inside the same window at the same time, and nothing outside this module
/// can park a caller there.
#[cfg(test)]
mod test_hooks {
    use parking_lot::Mutex;
    use std::sync::Arc;

    /// The two points inside `register` a hook is called at.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Stage {
        /// The next version slot has been decided from
        /// `bus_schema_version_latest` and nothing has been written yet.
        /// Two callers held here both compute the SAME slot, which is what
        /// forces one of them onto the retry.
        SlotDecided,
        /// The version insert came back `VersionSlotTaken`: this caller lost
        /// the slot and is about to re-read `latest` and try once more.
        SlotTaken,
        /// The RETRY's slot has been decided from a freshly re-read
        /// `bus_schema_version_latest` and the retried insert has not run
        /// yet. A caller held here loses that slot too if anything else
        /// claims it meanwhile, which is the only interleave that reaches
        /// `register`'s double-loss `compensate` arm.
        RetrySlotDecided,
    }

    pub type Hook = Arc<dyn Fn(Stage, &str, u32) + Send + Sync>;

    static HOOK: Mutex<Option<Hook>> = Mutex::new(None);

    pub fn install(hook: Hook) {
        *HOOK.lock() = Some(hook);
    }

    pub fn clear() {
        *HOOK.lock() = None;
    }

    /// Cloned out from under the lock before the call: a hook parks its
    /// caller at a rendezvous by design, and holding this cell's lock
    /// across that would stop the very thread it is waiting for from
    /// reading its own hook.
    pub fn fire(stage: Stage, subject: &str, version: u32) {
        let hook = HOOK.lock().clone();
        if let Some(hook) = hook {
            hook(stage, subject, version);
        }
    }
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

    // The slot is decided, nothing is written yet — the exact window a
    // concurrent registration has to be inside for both to claim the same
    // version number (`test_hooks`' own doc).
    #[cfg(test)]
    test_hooks::fire(test_hooks::Stage::SlotDecided, subject, next_version);

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
            #[cfg(test)]
            test_hooks::fire(test_hooks::Stage::SlotTaken, subject, next_version);
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

            // The retry's slot is decided and still unwritten — the window
            // a further concurrent registration has to claim
            // `retried_version` in for this call to lose twice and reach
            // `compensate` below (`test_hooks`' own doc).
            #[cfg(test)]
            test_hooks::fire(
                test_hooks::Stage::RetrySlotDecided,
                subject,
                retried_version,
            );

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

    // Hard delete only (whole subject or a single version) — the binding
    // guard never applies to `deprecate_only`, which always succeeds
    // (`F3-deprecate-only`, SUM/tentabus/DECYZJE-2026-09-22.md: "Oznaczenie
    // wersji schematu jako wycofanej działa zawsze; twarde usunięcie
    // blokowane, dopóki topik używa schematu"). Checked here, after the
    // `deprecate_only` branch has already returned, so a bound subject can
    // still be soft-deprecated at any time.
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

    /// `F3-deprecate-only` (SUM/tentabus/DECYZJE-2026-09-22.md): marking a
    /// subject deprecated must ALWAYS succeed, even while a topic still
    /// binds it — only a HARD delete stays blocked in that case. The
    /// original bug ran the bound-topics guard before the `deprecate_only`
    /// branch could ever return, so this reproduces the exact shape of the
    /// test right above it (a topic bound to the subject) but asserts the
    /// opposite outcome for `deprecate_only: true`.
    #[test]
    fn deprecate_only_succeeds_while_a_topic_still_binds_the_subject() {
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

        let removed = delete(&db, "tentabus-00000001", "org-1", "orders", None, true)
            .expect("deprecate_only must succeed even while orders.events still binds it");
        assert_eq!(removed, vec![1]);
        assert!(
            resolve_effective(&db, "tentabus-00000001", "org-1", "orders")
                .unwrap()
                .is_none(),
            "a deprecated subject must no longer resolve for validation"
        );
        // The binding itself is untouched — the bound topic keeps reading,
        // only NEW bindings/versions are refused from here on.
        assert_eq!(
            crate::bus::topics::get_topic(&db, "tentabus-00000001", "org-1", "orders.events")
                .unwrap()
                .unwrap()
                .schema_id
                .as_deref(),
            Some("orders")
        );

        // A subsequent HARD delete must still be refused while bound.
        let err = delete(&db, "tentabus-00000001", "org-1", "orders", None, false).unwrap_err();
        assert!(matches!(err, BusServiceError::InvalidArgument(_)));
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

    /// Serializes every test that installs a `test_hooks` hook. That cell is
    /// process-global and the lib test binary runs its tests in parallel, so
    /// two tests installing at once would each silently overwrite the
    /// other's hook — and a `register` that never meets its hook simply
    /// stops reproducing the interleave its test was written for, which
    /// surfaces as a baffling assertion failure rather than as a race.
    /// Poisoning is recovered rather than propagated: one failing hook test
    /// must fail alone, not take every other one down with it.
    static HOOK_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Installs `hook` and guarantees it is cleared again — on the normal
    /// path AND on the unwind from a failed assertion or a panicking worker
    /// thread, so a red test can never leave the process-global cell
    /// installed for the rest of the binary. Holds `HOOK_TEST_LOCK` for the
    /// guard's whole lifetime. Same drop-guard shape as
    /// `bus::replication::router`'s own `with_decoy_manager` fixture.
    fn install_hook_for_test(hook: test_hooks::Hook) -> impl Drop {
        struct Guard {
            _lock: std::sync::MutexGuard<'static, ()>,
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                test_hooks::clear();
            }
        }
        let lock = HOOK_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        test_hooks::install(hook);
        Guard { _lock: lock }
    }

    /// Ceiling on every rendezvous in the two concurrency tests below. Far
    /// above what parking two threads through a handful of in-memory SQLite
    /// statements can cost even on a loaded machine, and finite so that a
    /// party which never arrives fails the test instead of wedging the whole
    /// binary.
    const GATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    /// A meeting point for `parties` threads that CANNOT hang the test
    /// binary. `std::sync::Barrier::wait` has no timed form: if one
    /// `register` call returns before reaching its fire point — any `?`
    /// above it — its peer blocks in the barrier forever, the `join` below
    /// blocks with it, and `cargo test` never terminates. A hung suite that
    /// no per-test timeout can break is strictly worse than a red test, so
    /// this gate panics on timeout instead: the waiting thread fails, `join`
    /// hands back `Err`, and the test reports it.
    struct TimedGate {
        arrived: std::sync::Mutex<usize>,
        released: std::sync::Condvar,
        parties: usize,
    }

    impl TimedGate {
        fn new(parties: usize) -> Self {
            Self {
                arrived: std::sync::Mutex::new(0),
                released: std::sync::Condvar::new(),
                parties,
            }
        }

        /// Blocks until `parties` callers have arrived, or panics after
        /// `GATE_TIMEOUT`. `what` names the rendezvous in that panic, so a
        /// failure says which interleave never formed.
        fn wait(&self, what: &str) {
            let mut arrived = self
                .arrived
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *arrived += 1;
            if *arrived >= self.parties {
                self.released.notify_all();
                return;
            }
            let parties = self.parties;
            let (_arrived, wait_result) = self
                .released
                .wait_timeout_while(arrived, GATE_TIMEOUT, |count| *count < parties)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assert!(
                !wait_result.timed_out(),
                "gate '{what}': fewer than {parties} parties arrived within {GATE_TIMEOUT:?} — \
                 the interleave under test never formed, most likely because a concurrent \
                 register returned before reaching its fire point"
            );
        }
    }

    #[test]
    fn two_concurrent_registrations_of_a_new_subject_settle_on_versions_1_and_2() {
        // PLAN-F3's own outstanding item: `register`'s `VersionSlotTaken`
        // retry is reachable only while two callers sit in the same window,
        // so it had only ever run single-threaded. Two real threads are
        // parked at the slot decision until both have computed version 1
        // from the same "subject does not exist yet" read; exactly one then
        // loses the insert, retries against the winner's committed row, and
        // lands on version 2.
        //
        // What this test does NOT cover: the new-subject compensation. The
        // retry here SUCCEEDS, and every `compensate` call site sits on a
        // failure arm, so that closure never runs for the whole duration of
        // this test and the final-state assertions below are satisfied by
        // the winner plus the retry alone. The interleave that does reach
        // `compensate` is the test immediately below this one.
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};

        const SUBJECT: &str = "orders-slot-race";
        let db = fresh_db();

        let gate = Arc::new(TimedGate::new(2));
        let decided_slots = Arc::new(Mutex::new(Vec::new()));
        let slot_taken_hits = Arc::new(AtomicUsize::new(0));
        let _hook = {
            let gate = Arc::clone(&gate);
            let decided_slots = Arc::clone(&decided_slots);
            let slot_taken_hits = Arc::clone(&slot_taken_hits);
            let hook: test_hooks::Hook = Arc::new(move |stage, subject: &str, version| {
                // The hook cell is process-global and this binary runs its
                // tests in parallel — react only to THIS test's subject.
                if subject != SUBJECT {
                    return;
                }
                match stage {
                    test_hooks::Stage::SlotDecided => {
                        decided_slots
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .push(version);
                        gate.wait("both callers decide the same first slot");
                    }
                    test_hooks::Stage::SlotTaken => {
                        slot_taken_hits.fetch_add(1, Ordering::SeqCst);
                    }
                    // The retry is expected to succeed on this interleave.
                    test_hooks::Stage::RetrySlotDecided => {}
                }
            });
            install_hook_for_test(hook)
        };

        // Two DIFFERENT texts on purpose: identical content collides on the
        // content-hash index instead, which is the dedup path, not the
        // version-slot race under test. `Compatibility::None` keeps the
        // retry's re-run compatibility check out of the picture, so the
        // outcome cannot depend on which text happens to win version 1.
        let mut handles = Vec::new();
        for text in [V1, V2_ADD_OPTIONAL] {
            let db = Arc::clone(&db);
            handles.push(std::thread::spawn(move || {
                register(
                    &db,
                    "tentabus-00000001",
                    "org-1",
                    SUBJECT,
                    SchemaType::JsonSchema,
                    text,
                    Some(Compatibility::None),
                    Some("alice"),
                )
            }));
        }
        let outcomes: Vec<RegisterOutcome> = handles
            .into_iter()
            .map(|h| {
                h.join()
                    .expect("register thread panicked")
                    .expect("both concurrent registrations must succeed")
            })
            .collect();

        let decided = decided_slots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert_eq!(
            decided,
            vec![1u32, 1],
            "this test only proves anything if BOTH callers decided on the same version slot \
             before either of them wrote"
        );
        assert_eq!(
            slot_taken_hits.load(Ordering::SeqCst),
            1,
            "exactly one caller must have lost the slot and taken the VersionSlotTaken retry"
        );

        let mut versions: Vec<u32> = outcomes.iter().map(|o| o.version).collect();
        versions.sort_unstable();
        assert_eq!(
            versions,
            vec![1, 2],
            "the loser must retry onto the NEXT slot, never reuse or skip one"
        );
        assert!(
            outcomes.iter().all(|o| !o.deduplicated),
            "two different schema texts must never be reported as a dedup hit"
        );

        // Final state of a race both callers survived: one subject row, two
        // contiguous versions, both texts intact. These say nothing about
        // `compensate` — see this test's own doc for why.
        let subjects = list_subjects(&db, "tentabus-00000001", "org-1").unwrap();
        assert_eq!(
            subjects.len(),
            1,
            "the race must leave exactly one subject row, not one per writer and not zero"
        );
        assert_eq!(subjects[0].subject, SUBJECT);
        assert_eq!(subjects[0].latest_version, Some(2));

        let stored: Vec<u32> = list_versions(&db, "tentabus-00000001", "org-1", SUBJECT)
            .unwrap()
            .iter()
            .map(|v| v.version)
            .collect();
        assert_eq!(
            stored,
            vec![1, 2],
            "stored versions must be contiguous and ordered, with no gap left by the loser"
        );

        // Both writers' content survived — neither row was lost, replaced,
        // or overwritten by the other.
        let mut texts: Vec<String> = stored
            .iter()
            .map(|v| {
                get(&db, "tentabus-00000001", "org-1", SUBJECT, Some(*v))
                    .unwrap()
                    .1
            })
            .collect();
        texts.sort();
        let mut expected = vec![V1.to_string(), V2_ADD_OPTIONAL.to_string()];
        expected.sort();
        assert_eq!(texts, expected);
    }

    #[test]
    fn a_loser_whose_retry_also_loses_leaves_the_winners_rows_alone() {
        // Review finding #2b, in the interleave it was written for.
        // `register` upserts the subject row before inserting its first
        // version and deletes that row again if the insert fails — but
        // `is_new_subject` only records what THIS call read at the START.
        // Here both callers read "subject does not exist", the winner then
        // commits version 1 under it, and the loser's own retry fails.
        // `bus_schema_versions` references `bus_schema_subjects(instance_id,
        // org_id, subject)` `ON DELETE CASCADE` (`db/repository.rs`'s DDL),
        // so an unconditional delete at that point would take the winner's
        // committed row with it. The re-check inside `compensate` must find
        // the subject no longer version-less and leave it in place.
        //
        // How the arm that calls `compensate` is reached deterministically:
        // the loser is parked once more after it has re-read `latest` and
        // computed its retry slot (`Stage::RetrySlotDecided`), and the hook
        // writes into exactly that slot the row a further concurrent
        // registration would have committed. The retried insert then comes
        // back `VersionSlotTaken` a second time — the one arm that runs
        // `compensate` and then returns the "taken concurrently twice in a
        // row" error, so a call returning that error is itself the proof
        // that compensation ran.
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};

        const SUBJECT: &str = "orders-compensation-race";
        // A third text, distinct from both racers': a different
        // `content_hash`, and therefore a different `schema_ref_id`, so the
        // loser's retried insert violates the version PRIMARY KEY alone and
        // maps to `VersionSlotTaken` rather than to either UNIQUE index.
        const INTERLOPER: &str = r#"{"type":"object","properties":{"c":{"type":"string"}},
            "additionalProperties":false}"#;

        let db = fresh_db();
        let gate = Arc::new(TimedGate::new(2));
        let slot_taken_hits = Arc::new(AtomicUsize::new(0));
        let retry_slots = Arc::new(Mutex::new(Vec::new()));
        let _hook = {
            let gate = Arc::clone(&gate);
            let hook_db = Arc::clone(&db);
            let slot_taken_hits = Arc::clone(&slot_taken_hits);
            let retry_slots = Arc::clone(&retry_slots);
            let hook: test_hooks::Hook = Arc::new(move |stage, subject: &str, version| {
                if subject != SUBJECT {
                    return;
                }
                match stage {
                    test_hooks::Stage::SlotDecided => {
                        gate.wait("both callers decide the same first slot");
                    }
                    test_hooks::Stage::SlotTaken => {
                        slot_taken_hits.fetch_add(1, Ordering::SeqCst);
                    }
                    test_hooks::Stage::RetrySlotDecided => {
                        // Stands in for a third concurrent registration
                        // committing `version` between the loser's re-read
                        // of `latest` and its own retried insert. Written
                        // through the same repository function `register`
                        // itself uses, so it lands as a real row under real
                        // constraints, not as a fixture shortcut.
                        let hash = content_hash(INTERLOPER);
                        let schema_ref_id = schema_ref_id_for("org-1", SUBJECT, &hash);
                        repository::bus_schema_version_insert(
                            &hook_db,
                            &DbBusSchemaVersion {
                                instance_id: "tentabus-00000001".to_string(),
                                org_id: "org-1".to_string(),
                                subject: SUBJECT.to_string(),
                                version,
                                schema_text: INTERLOPER.to_string(),
                                content_hash: hash,
                                schema_ref_id,
                                created_by: None,
                                created_at_ms: 1,
                            },
                        )
                        .expect("the stand-in registration must claim the retry slot");
                        retry_slots
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .push(version);
                    }
                }
            });
            install_hook_for_test(hook)
        };

        let mut handles = Vec::new();
        for text in [V1, V2_ADD_OPTIONAL] {
            let db = Arc::clone(&db);
            handles.push(std::thread::spawn(move || {
                let outcome = register(
                    &db,
                    "tentabus-00000001",
                    "org-1",
                    SUBJECT,
                    SchemaType::JsonSchema,
                    text,
                    Some(Compatibility::None),
                    Some("alice"),
                );
                (text, outcome)
            }));
        }
        let results: Vec<(&str, Result<RegisterOutcome, BusServiceError>)> = handles
            .into_iter()
            .map(|h| h.join().expect("register thread panicked"))
            .collect();

        assert_eq!(
            slot_taken_hits.load(Ordering::SeqCst),
            1,
            "exactly one caller must have lost the first slot and entered the retry"
        );
        assert_eq!(
            retry_slots
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
            vec![2u32],
            "the loser must have retried onto slot 2 — the slot the stand-in registration \
             then took from it"
        );

        let winners: Vec<(&str, u32)> = results
            .iter()
            .filter_map(|(text, outcome)| outcome.as_ref().ok().map(|o| (*text, o.version)))
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "exactly one of the two racers must have committed; got {results:?}"
        );
        assert_eq!(winners[0].1, 1, "the winner must own version 1");

        let loser_err = results
            .iter()
            .find_map(|(_, outcome)| outcome.as_ref().err())
            .expect("the other racer must have failed its retry");
        match loser_err {
            BusServiceError::Db(msg) => assert!(
                msg.contains("twice in a row"),
                "the loser must fail through the double-loss arm — the only arm that runs \
                 `compensate` on this interleave: {msg}"
            ),
            other => panic!("expected the double-loss Db error, got {other:?}"),
        }

        // The point of the test: compensation ran (the error above is only
        // returned after it does) and did NOT delete the subject, so neither
        // the winner's version nor the stand-in's was cascaded away with it.
        let subjects = list_subjects(&db, "tentabus-00000001", "org-1").unwrap();
        assert_eq!(
            subjects.len(),
            1,
            "the compensating loser must leave the subject row it found already populated"
        );
        assert_eq!(subjects[0].subject, SUBJECT);
        assert_eq!(subjects[0].latest_version, Some(2));

        let stored: Vec<u32> = list_versions(&db, "tentabus-00000001", "org-1", SUBJECT)
            .unwrap()
            .iter()
            .map(|v| v.version)
            .collect();
        assert_eq!(
            stored,
            vec![1, 2],
            "both already-committed versions must survive the loser's compensation"
        );
        assert_eq!(
            get(&db, "tentabus-00000001", "org-1", SUBJECT, Some(1))
                .unwrap()
                .1,
            winners[0].0,
            "version 1 must still hold the winner's own text"
        );
        assert_eq!(
            get(&db, "tentabus-00000001", "org-1", SUBJECT, Some(2))
                .unwrap()
                .1,
            INTERLOPER,
            "version 2 must still hold the stand-in registration's text"
        );
    }
}
