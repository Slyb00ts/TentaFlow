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

/// `repository::bus_schema_subject_modify` with the registry's errors: an
/// `edit` that fails writes nothing and its error is returned as is, and a
/// subject deleted meanwhile is `SchemaNotFound`.
fn modify_subject(
    db: &DbPool,
    instance_id: &str,
    org_id: &str,
    subject: &str,
    edit: impl FnOnce(&rusqlite::Transaction<'_>, &mut DbBusSchemaSubject) -> Result<bool, BusServiceError>,
) -> Result<DbBusSchemaSubject, BusServiceError> {
    let mut failed = None;
    let row = repository::bus_schema_subject_modify(db, instance_id, org_id, subject, |tx, row| {
        Ok(edit(tx, row).unwrap_or_else(|e| {
            failed = Some(e);
            false
        }))
    })?;
    if let Some(e) = failed {
        return Err(e);
    }
    row.ok_or_else(|| BusServiceError::SchemaNotFound {
        subject: subject.to_string(),
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
    /// When this version was deprecated; `None` while it is active.
    pub deprecated_at_ms: Option<i64>,
}

fn version_info(row: &DbBusSchemaVersion, deprecated: &[DeprecatedVersion]) -> VersionInfo {
    VersionInfo {
        subject: row.subject.clone(),
        version: row.version,
        schema_ref_id: row.schema_ref_id,
        content_hash: row.content_hash.clone(),
        created_by: row.created_by.clone(),
        created_at_ms: row.created_at_ms,
        deprecated_at_ms: deprecated_at(deprecated, row),
    }
}

/// One entry of `bus_schema_subjects.deprecated_versions_json`: what
/// happened to one version, named by number AND content — a number freed by
/// a hard delete and taken by a new registration is another version.
///
/// The list lives on the subject row, not on the version: version rows
/// replicate insert-if-absent and never change, the subject row replicates
/// LWW — and this list as a union on top of that
/// (`core_materializer::apply_bus_schema_subject`), taking the later of each
/// stamp. A deprecation is never taken back, so a union loses none; a hard
/// delete is recorded as `removed_at` (a tombstone) rather than by dropping
/// the entry, so a deprecation still travelling from a node that had not
/// seen the delete stays covered by it — and a version registered again with
/// the same content under the freed number starts active. An entry
/// deprecates its version while `deprecated_at` is later than any
/// `removed_at` (`is_active`).
///
/// Both stamps are packed ledger HLCs (`ledger_stamp`), not wall-clock
/// times: they are compared across nodes, and wall clocks that disagree
/// would drop a real deprecation or keep a stale one. The HLC is past every
/// op this node has seen, so an event recorded here after it saw another
/// node's is ordered after it. `deprecated_at_ms` on the wire is the stamp's
/// wall-clock part (`stamp_wall_ms`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeprecatedVersion {
    pub version: u32,
    #[serde(default)]
    pub content_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_at: Option<u64>,
}

impl DeprecatedVersion {
    fn is_active(&self) -> bool {
        match (self.deprecated_at, self.removed_at) {
            (Some(deprecated), Some(removed)) => deprecated > removed,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }
}

/// A stamp on the ledger's clock, packed like a topic's incarnation.
fn ledger_stamp() -> u64 {
    repository::bus_topic_generation_at(&crate::sync::runtime::core_hlc_now())
}

/// The wall-clock milliseconds of a packed stamp — what a reader is shown.
fn stamp_wall_ms(stamp: u64) -> i64 {
    (stamp >> 16) as i64
}

/// Decodes a `deprecated_versions_json` value; `None` (a payload from a
/// build before the column) is no deprecation. A value that does not decode
/// is a corrupt row, reported like a corrupt `schema_type`.
pub fn parse_deprecated_versions(
    subject: &str,
    json: Option<&str>,
) -> Result<Vec<DeprecatedVersion>, BusServiceError> {
    let Some(json) = json else {
        return Ok(Vec::new());
    };
    serde_json::from_str(json).map_err(|e| {
        BusServiceError::Db(format!(
            "corrupt bus_schema_subjects.deprecated_versions_json for subject '{subject}': {e}"
        ))
    })
}

fn subject_deprecations(row: &DbBusSchemaSubject) -> Result<Vec<DeprecatedVersion>, BusServiceError> {
    parse_deprecated_versions(&row.subject, row.deprecated_versions_json.as_deref())
}

/// Encodes the list in its merged form (`merge_deprecated_versions`).
pub fn encode_deprecated_versions(list: Vec<DeprecatedVersion>) -> String {
    serde_json::to_string(&merge_deprecated_versions(list, Vec::new()))
        .expect("a list of plain values always serializes")
}

/// The union of two lists: one entry per (version, content) with the later
/// of each stamp. Every tombstone is kept — dropping one would let a late
/// deprecation of that content revive — so the list grows with the
/// distinct contents a number has held, which hard deletes alone produce.
pub fn merge_deprecated_versions(
    a: Vec<DeprecatedVersion>,
    b: Vec<DeprecatedVersion>,
) -> Vec<DeprecatedVersion> {
    let mut merged: std::collections::BTreeMap<(u32, String), (Option<u64>, Option<u64>)> =
        std::collections::BTreeMap::new();
    for d in a.into_iter().chain(b) {
        let at = merged.entry((d.version, d.content_hash)).or_default();
        at.0 = at.0.max(d.deprecated_at);
        at.1 = at.1.max(d.removed_at);
    }
    merged
        .into_iter()
        .map(|((version, content_hash), (deprecated_at, removed_at))| DeprecatedVersion {
            version,
            content_hash,
            deprecated_at,
            removed_at,
        })
        .collect()
}

fn entry_for<'a>(
    list: &'a [DeprecatedVersion],
    version: &DbBusSchemaVersion,
) -> Option<&'a DeprecatedVersion> {
    list.iter()
        .find(|d| d.version == version.version && d.content_hash == version.content_hash)
}

/// When `version` was deprecated (wall-clock ms), `None` while it is active.
fn deprecated_at(list: &[DeprecatedVersion], version: &DbBusSchemaVersion) -> Option<i64> {
    entry_for(list, version)
        .filter(|d| d.is_active())
        .and_then(|d| d.deprecated_at)
        .map(stamp_wall_ms)
}

/// `list` with `version` deprecated or removed at `stamp`.
fn record_version_event(
    list: Vec<DeprecatedVersion>,
    version: &DbBusSchemaVersion,
    removed: bool,
    stamp: u64,
) -> Vec<DeprecatedVersion> {
    let event = DeprecatedVersion {
        version: version.version,
        content_hash: version.content_hash.clone(),
        deprecated_at: (!removed).then_some(stamp),
        removed_at: removed.then_some(stamp),
    };
    merge_deprecated_versions(list, vec![event])
}

/// The version a binding validates against, out of `versions` sorted by
/// version: the highest one that is not deprecated. When the whole subject
/// is deprecated, or every version is, the latest version keeps validating —
/// deprecation only stops new use, it never switches validation off
/// (owner decision 23.09, "Wycofanie wzoru").
fn effective_version<'a>(
    row: &DbBusSchemaSubject,
    deprecated: &[DeprecatedVersion],
    versions: &'a [DbBusSchemaVersion],
) -> Option<&'a DbBusSchemaVersion> {
    let latest = versions.last()?;
    if row.deprecated_at_ms.is_some() {
        return Some(latest);
    }
    versions
        .iter()
        .rev()
        .find(|v| deprecated_at(deprecated, v).is_none())
        .or(Some(latest))
}

/// `SchemaRegisterRequest`'s response.
#[derive(Debug)]
pub struct RegisterOutcome {
    pub version: u32,
    pub schema_ref_id: u32,
    pub deduplicated: bool,
}

/// The version a topic's `schema_id` binding currently resolves to — the
/// subject's highest non-deprecated version (PLAN-F3 §3, `effective_version`).
/// Deprecation is only a marker (owner decision 23.09, "Wycofanie wzoru"): a
/// deprecated subject keeps validating every topic already bound to it with
/// its latest version, it just cannot be bound to a new topic or take a new
/// version; a deprecated VERSION is skipped while a newer or older active
/// one exists. `resolve_effective` returns `None` only
/// for a missing subject or one with zero versions (registration failed/
/// raced) — "nothing to validate against", which `publish` turns into a loud
/// `SchemaNotFound`, never a silent "validation off".
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
    let row = require_subject(db, instance_id, org_id, subject)?;
    let deprecated = subject_deprecations(&row)?;
    Ok(
        repository::bus_schema_version_list(db, instance_id, org_id, subject)?
            .iter()
            .map(|v| version_info(v, &deprecated))
            .collect(),
    )
}

/// `version: None` resolves to the version `resolve_effective` validates
/// publishes against (`effective_version`).
pub fn get(
    db: &DbPool,
    instance_id: &str,
    org_id: &str,
    subject: &str,
    version: Option<u32>,
) -> Result<(VersionInfo, String), BusServiceError> {
    let subject_row = require_subject(db, instance_id, org_id, subject)?;
    let deprecated = subject_deprecations(&subject_row)?;
    let row = match version {
        Some(v) => repository::bus_schema_version_get(db, instance_id, org_id, subject, v)?
            .ok_or_else(|| BusServiceError::SchemaVersionNotFound {
                subject: subject.to_string(),
                version: v,
            })?,
        None => {
            let mut versions =
                repository::bus_schema_version_list(db, instance_id, org_id, subject)?;
            let effective = effective_version(&subject_row, &deprecated, &versions)
                .map(|v| v.version)
                .ok_or_else(|| BusServiceError::SchemaVersionNotFound {
                    subject: subject.to_string(),
                    version: 0,
                })?;
            versions.retain(|v| v.version == effective);
            versions.remove(0)
        }
    };
    let info = version_info(&row, &deprecated);
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

    // The checks an existing subject puts a registration through — run on
    // the row read here, and again on the row a concurrent registration
    // created when this one loses the insert below.
    let existing_subject_compatibility = |row: &DbBusSchemaSubject| {
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
        Ok(existing_compat)
    };
    let mut effective_compatibility = match &existing {
        Some(row) => existing_subject_compatibility(row)?,
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
    let check_compat = |latest_row: &DbBusSchemaVersion,
                        effective_compatibility: Compatibility|
     -> Result<(), BusServiceError> {
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
                SchemaError::LimitExceeded(detail) => BusServiceError::SchemaCompareTooComplex {
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
        check_compat(latest_row, effective_compatibility)?;
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
    // window left is between the subject insert below and the version
    // insert after it. That window only matters for a subject THIS CALL
    // just created (`is_new_subject`): if `bus_schema_version_insert` then
    // fails, best-effort delete the subject row again rather than leaving
    // a version-less subject behind — an EXISTING subject is never written
    // by a registration, so there is nothing to undo for it.
    let mut is_new_subject = existing.is_none();
    let mut subject_generation = existing.as_ref().map_or(0, |row| row.generation);

    // An existing subject's row is not written at all: rewriting it from the
    // copy read above would undo a deprecation or compatibility change made
    // since. A registration that finds the subject created meanwhile by a
    // concurrent one goes on as a registration onto that existing subject:
    // its checks, its stored compatibility mode — against the version the
    // concurrent registration may have written already — and no rollback of
    // a row this call did not create.
    if is_new_subject {
        let subject_row = DbBusSchemaSubject {
            instance_id: instance_id.to_string(),
            org_id: org_id.to_string(),
            subject: subject.to_string(),
            schema_type: schema_type.as_str().to_string(),
            compatibility: effective_compatibility.as_str().to_string(),
            deprecated_at_ms: None,
            created_by: created_by.map(|s| s.to_string()),
            created_at_ms: now,
            updated_at_ms: now,
            deprecated_versions_json: Some(encode_deprecated_versions(Vec::new())),
            // The creation instant on the ledger's clock names this
            // incarnation of the subject (`DbBusSchemaSubject::generation`).
            generation: repository::bus_topic_generation_at(
                &crate::sync::runtime::core_hlc_now(),
            ),
        };
        subject_generation = subject_row.generation;
        if !repository::bus_schema_subject_insert(db, &subject_row)? {
            is_new_subject = false;
            let row = require_subject(db, instance_id, org_id, subject)?;
            subject_generation = row.generation;
            effective_compatibility = existing_subject_compatibility(&row)?;
            if let Some(latest_row) =
                repository::bus_schema_version_latest(db, instance_id, org_id, subject)?
            {
                check_compat(&latest_row, effective_compatibility)?;
            }
        }
    }

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
        subject_generation,
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
                check_compat(latest_row, effective_compatibility)?;
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
    let (schema_type, _existing) = decode_subject(&row)?;
    if compatibility != Compatibility::None && !schema_type.has_validator() {
        return Err(BusServiceError::SchemaTypeUnsupported {
            schema_type,
            operation: "check_compatibility",
        });
    }
    // The deprecation check runs on the row the write itself reads, so a
    // deprecation landing in between is never written over.
    let mut deprecated = false;
    modify_subject(db, instance_id, org_id, subject, |_, row| {
        if row.deprecated_at_ms.is_some() {
            deprecated = true;
            return Ok(false);
        }
        row.compatibility = compatibility.as_str().to_string();
        row.updated_at_ms = crate::bus::now_ms();
        Ok(true)
    })?;
    if deprecated {
        return Err(BusServiceError::InvalidArgument(format!(
            "subject '{subject}' is deprecated; cannot change its compatibility mode"
        )));
    }
    bump_generation();
    Ok(())
}

/// Deletes a subject entirely (`version: None`) or exactly one version
/// (`Some(v)`), or soft-deprecates the subject (`deprecate_only: true`,
/// `version: None`) or one of its versions (`deprecate_only: true`,
/// `Some(v)`: recorded in the subject row's `deprecated_versions_json`, see
/// `effective_version` for what validation then uses). Owner decision 3
/// (SUM/tentabus/PLAN-F3.md §9.3): a hard delete while a topic in the org
/// still has `schema_id == subject` is rejected, listing the offending
/// topics — `deprecate_only` is the soft alternative that leaves existing
/// bindings (and their validation) intact and simply stops new ones
/// (`bus::topics`' binding guard checks `deprecated_at_ms`, `register`
/// refuses a new version).
pub fn delete(
    db: &DbPool,
    instance_id: &str,
    org_id: &str,
    subject: &str,
    version: Option<u32>,
    deprecate_only: bool,
) -> Result<Vec<u32>, BusServiceError> {
    require_subject(db, instance_id, org_id, subject)?;

    if let (true, Some(v)) = (deprecate_only, version) {
        // The version is read inside the write's transaction: one hard-deleted
        // (and perhaps registered again) meanwhile must not be deprecated
        // under the content it no longer has.
        let mut changed = false;
        modify_subject(db, instance_id, org_id, subject, |tx, row| {
            let version_row = repository::bus_schema_version_get_on(
                tx,
                instance_id,
                org_id,
                subject,
                v,
            )?
            .ok_or_else(|| BusServiceError::SchemaVersionNotFound {
                subject: subject.to_string(),
                version: v,
            })?;
            let list = subject_deprecations(row)?;
            if deprecated_at(&list, &version_row).is_some() {
                return Ok(false);
            }
            // Past a tombstone of this very content (deleted and registered
            // again), or the deprecation would not count.
            let removed = entry_for(&list, &version_row).and_then(|d| d.removed_at);
            let stamp = ledger_stamp().max(removed.map_or(0, |r| r + 1));
            row.deprecated_versions_json = Some(encode_deprecated_versions(
                record_version_event(list, &version_row, false, stamp),
            ));
            row.updated_at_ms = crate::bus::now_ms();
            changed = true;
            Ok(true)
        })?;
        if changed {
            bump_generation();
        }
        return Ok(Vec::new());
    }

    if deprecate_only {
        // A deprecation removes nothing — every version stays stored and the
        // latest keeps validating — so the "removed" list is empty.
        let mut changed = false;
        modify_subject(db, instance_id, org_id, subject, |_, row| {
            if row.deprecated_at_ms.is_some() {
                return Ok(false);
            }
            let now = crate::bus::now_ms();
            row.deprecated_at_ms = Some(now);
            row.updated_at_ms = now;
            changed = true;
            Ok(true)
        })?;
        if changed {
            bump_generation();
        }
        return Ok(Vec::new());
    }

    // Hard delete only (whole subject or a single version) — the binding
    // guard never applies to `deprecate_only`, which always succeeds
    // (`F3-deprecate-only`, SUM/tentabus/DECYZJE-2026-09-22.md: "Oznaczenie
    // wersji schematu jako wycofanej działa zawsze; twarde usunięcie
    // blokowane, dopóki topik używa schematu"). Checked here, after the
    // `deprecate_only` branch has already returned, so a bound subject can
    // still be soft-deprecated at any time.
    let bound_topics = topics_by_subject(db, instance_id, org_id)?
        .remove(subject)
        .unwrap_or_default();
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
            let removed = repository::bus_schema_version_get(db, instance_id, org_id, subject, v)?
                .ok_or_else(|| BusServiceError::SchemaVersionNotFound {
                    subject: subject.to_string(),
                    version: v,
                })?;
            repository::bus_schema_version_delete(db, instance_id, org_id, subject, v)?;
            // A tombstone, not a dropped entry: a deprecation of this version
            // still travelling from a node that had not seen the delete stays
            // covered by it, and cannot reach the same content registered
            // again under the freed number.
            modify_subject(db, instance_id, org_id, subject, |_, row| {
                row.deprecated_versions_json = Some(encode_deprecated_versions(
                    record_version_event(subject_deprecations(row)?, &removed, true, ledger_stamp()),
                ));
                row.updated_at_ms = crate::bus::now_ms();
                Ok(true)
            })?;
            bump_generation();
            Ok(vec![v])
        }
    }
}

/// Every subject of the org that at least one topic is bound to, mapped to
/// those topics' names (sorted) — the set the hard-delete guard refuses on
/// and `BusSchemaSubjectWire::used_by_topics` reports. One topic-list read
/// for the whole org, however many subjects the caller then looks up.
pub fn topics_by_subject(
    db: &DbPool,
    instance_id: &str,
    org_id: &str,
) -> Result<std::collections::BTreeMap<String, Vec<String>>, BusServiceError> {
    let mut out: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for topic in repository::bus_topic_list(db, instance_id, org_id)? {
        if let Some(subject) = topic.schema_id.filter(|s| !s.is_empty()) {
            out.entry(subject).or_default().push(topic.name);
        }
    }
    for names in out.values_mut() {
        names.sort();
    }
    Ok(out)
}

/// Publish-time binding resolution (PLAN-F3 §3): the version a topic's
/// `schema_id == subject` binding currently resolves to. `None` for a
/// missing or version-less subject; a deprecated subject still resolves
/// (see `EffectiveSchema`'s doc).
pub fn resolve_effective(
    db: &DbPool,
    instance_id: &str,
    org_id: &str,
    subject: &str,
) -> Result<Option<EffectiveSchema>, BusServiceError> {
    let Some(row) = repository::bus_schema_subject_get(db, instance_id, org_id, subject)? else {
        return Ok(None);
    };
    let deprecated = subject_deprecations(&row)?;
    let chosen = if deprecated.is_empty() {
        repository::bus_schema_version_latest(db, instance_id, org_id, subject)?
    } else {
        let heads = repository::bus_schema_version_heads(db, instance_id, org_id, subject)?;
        match effective_version(&row, &deprecated, &heads).map(|v| v.version) {
            Some(version) => {
                repository::bus_schema_version_get(db, instance_id, org_id, subject, version)?
            }
            None => None,
        }
    };
    let Some(chosen) = chosen else {
        return Ok(None);
    };
    let (schema_type, compatibility) = decode_subject(&row)?;
    Ok(Some(EffectiveSchema {
        schema_type,
        compatibility,
        version: chosen.version,
        schema_ref_id: chosen.schema_ref_id,
        schema_text: chosen.schema_text,
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
    fn register_reports_a_comparison_that_gave_up_apart_from_an_incompatibility() {
        let chain = |extra: &str| {
            let mut body = String::new();
            for i in 0..100 {
                body.push_str(&if i < 99 {
                    format!(
                        r#"<xs:complexType name="n{i}"><xs:sequence><xs:element name="c" type="n{}"/></xs:sequence></xs:complexType>"#,
                        i + 1
                    )
                } else {
                    format!(r#"<xs:complexType name="n{i}"/>"#)
                });
            }
            format!(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">{extra}{body}<xs:element name="r" type="n0"/></xs:schema>"#
            )
        };
        let db = fresh_db();
        let go = |text: &str| {
            register(
                &db,
                "tentabus-00000001",
                "org-1",
                "deep",
                SchemaType::Xsd,
                text,
                None,
                None,
            )
        };
        go(&chain("")).unwrap();
        let err = go(&chain(
            "<xs:annotation><xs:documentation>x</xs:documentation></xs:annotation>",
        ))
        .unwrap_err();
        assert!(
            matches!(
                &err,
                BusServiceError::SchemaCompareTooComplex {
                    mode: "backward",
                    ..
                }
            ),
            "{err:?}"
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

    /// Owner decision 23.09 ("Wycofanie wzoru"): deprecation is a marker
    /// only — the subject keeps resolving to its latest version, so topics
    /// bound to it keep validating.
    #[test]
    fn deprecate_only_keeps_resolving_to_the_latest_version() {
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
        assert!(removed.is_empty(), "a deprecation removes no version");
        let effective = resolve_effective(&db, "tentabus-00000001", "org-1", "orders")
            .unwrap()
            .expect("a deprecated subject still resolves for validation");
        assert_eq!(effective.version, 1);
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
        assert!(removed.is_empty(), "a deprecation removes no version");
        assert!(
            resolve_effective(&db, "tentabus-00000001", "org-1", "orders")
                .unwrap()
                .is_some(),
            "the bound topic keeps validating against the deprecated subject"
        );
        // The binding itself is untouched — the bound topic keeps validating,
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
        repository::bus_schema_subject_insert(
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
                deprecated_versions_json: Some("[]".to_string()),
                generation: 0,
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
                subject_generation: 0,
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
                                subject_generation: 0,
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

    const V3_ADD_ANOTHER_OPTIONAL: &str = r#"{"type":"object","properties":{"a":{"type":"string"},
        "b":{"type":"string"},"c":{"type":"string"}},"required":["a"],"additionalProperties":false}"#;

    const INST: &str = "tentabus-00000001";

    /// `orders` with versions 1, 2 and 3.
    fn three_versions() -> DbPool {
        let db = fresh_db();
        for text in [V1, V2_ADD_OPTIONAL, V3_ADD_ANOTHER_OPTIONAL] {
            register(&db, INST, "org-1", "orders", SchemaType::JsonSchema, text, None, None)
                .unwrap();
        }
        db
    }

    fn effective(db: &DbPool) -> u32 {
        resolve_effective(db, INST, "org-1", "orders")
            .unwrap()
            .expect("the subject resolves")
            .version
    }

    fn deprecated_versions(db: &DbPool) -> Vec<(u32, bool)> {
        list_versions(db, INST, "org-1", "orders")
            .unwrap()
            .into_iter()
            .map(|v| (v.version, v.deprecated_at_ms.is_some()))
            .collect()
    }

    #[test]
    fn deprecating_a_middle_version_keeps_validation_on_the_newest() {
        let db = three_versions();
        let removed = delete(&db, INST, "org-1", "orders", Some(2), true).unwrap();
        assert!(removed.is_empty(), "a deprecation removes no version");
        assert_eq!(deprecated_versions(&db), vec![(1, false), (2, true), (3, false)]);
        assert_eq!(effective(&db), 3);
        let subject = repository::bus_schema_subject_get(&db, INST, "org-1", "orders")
            .unwrap()
            .unwrap();
        assert_eq!(subject.deprecated_at_ms, None, "only the version is deprecated");
        let versions = repository::bus_schema_version_list(&db, INST, "org-1", "orders").unwrap();
        assert_eq!(versions.len(), 3, "version rows are never touched");
    }

    #[test]
    fn choosing_the_effective_version_reads_no_schema_text_but_the_chosen_one() {
        let db = three_versions();
        delete(&db, INST, "org-1", "orders", Some(3), true).unwrap();
        let heads = repository::bus_schema_version_heads(&db, INST, "org-1", "orders").unwrap();
        assert_eq!(
            heads.iter().map(|h| h.version).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert!(heads
            .iter()
            .all(|h| h.schema_text.is_empty() && !h.content_hash.is_empty()));
        let effective = resolve_effective(&db, INST, "org-1", "orders")
            .unwrap()
            .unwrap();
        assert_eq!(
            (effective.version, effective.schema_text.as_str()),
            (2, V2_ADD_OPTIONAL)
        );
    }

    #[test]
    fn deprecating_the_newest_version_moves_validation_to_the_previous_one() {
        let db = three_versions();
        let generation_before = crate::bus::schema_registry::generation();
        delete(&db, INST, "org-1", "orders", Some(3), true).unwrap();
        assert!(
            crate::bus::schema_registry::generation() > generation_before,
            "cached validators must be re-resolved"
        );
        assert_eq!(effective(&db), 2);
        let (info, text) = get(&db, INST, "org-1", "orders", None).unwrap();
        assert_eq!((info.version, text.as_str()), (2, V2_ADD_OPTIONAL));
        assert_eq!(info.deprecated_at_ms, None);
        let (explicit, _) = get(&db, INST, "org-1", "orders", Some(3)).unwrap();
        assert!(explicit.deprecated_at_ms.is_some(), "a deprecated version stays readable");

        // Deprecating it again is a no-op that keeps the first timestamp.
        let first = explicit.deprecated_at_ms;
        delete(&db, INST, "org-1", "orders", Some(3), true).unwrap();
        let (again, _) = get(&db, INST, "org-1", "orders", Some(3)).unwrap();
        assert_eq!(again.deprecated_at_ms, first);
    }

    /// Owner decision 23.09: with every version deprecated the topic still
    /// validates — against the latest one.
    #[test]
    fn with_every_version_deprecated_the_latest_keeps_validating() {
        let db = three_versions();
        for v in [3, 1, 2] {
            delete(&db, INST, "org-1", "orders", Some(v), true).unwrap();
        }
        assert_eq!(deprecated_versions(&db), vec![(1, true), (2, true), (3, true)]);
        assert_eq!(effective(&db), 3);
        assert_eq!(get(&db, INST, "org-1", "orders", None).unwrap().0.version, 3);
    }

    #[test]
    fn a_deprecated_subject_validates_with_its_latest_version_whatever_its_versions_say() {
        let db = three_versions();
        delete(&db, INST, "org-1", "orders", Some(3), true).unwrap();
        assert_eq!(effective(&db), 2);
        delete(&db, INST, "org-1", "orders", None, true).unwrap();
        assert_eq!(effective(&db), 3);
    }

    #[test]
    fn a_deprecated_subject_refuses_a_new_version_and_a_new_binding() {
        let db = three_versions();
        delete(&db, INST, "org-1", "orders", None, true).unwrap();
        let err = register(
            &db,
            INST,
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            r#"{"type":"object"}"#,
            None,
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, BusServiceError::InvalidArgument(ref m) if m.contains("deprecated")),
            "{err:?}"
        );
        let err = crate::bus::topics::create_topic(
            &db,
            INST,
            "org-1",
            "orders.events",
            crate::bus::topics::TopicOptions {
                schema_id: Some("orders".to_string()),
                content_type: Some("application/json".to_string()),
                ..Default::default()
            },
            tentaflow_protocol::environment::NodeEnvironment::Test,
            1_000,
        )
        .unwrap_err();
        assert!(
            matches!(err, BusServiceError::InvalidTopicConfig { ref reason } if reason.contains("deprecated")),
            "{err:?}"
        );
    }

    #[test]
    fn deprecating_a_version_that_does_not_exist_is_refused() {
        let db = three_versions();
        let err = delete(&db, INST, "org-1", "orders", Some(9), true).unwrap_err();
        assert!(
            matches!(err, BusServiceError::SchemaVersionNotFound { version: 9, .. }),
            "{err:?}"
        );
        assert_eq!(deprecated_versions(&db), vec![(1, false), (2, false), (3, false)]);
    }

    /// A version number freed by a hard delete is taken by the next
    /// registration; the deprecation of the deleted version must not pass to
    /// it, and a registration keeps the deprecations of the versions it did
    /// not replace.
    #[test]
    fn a_new_version_keeps_earlier_deprecations_and_never_inherits_a_deleted_ones() {
        let db = three_versions();
        delete(&db, INST, "org-1", "orders", Some(1), true).unwrap();
        delete(&db, INST, "org-1", "orders", Some(3), true).unwrap();
        assert_eq!(delete(&db, INST, "org-1", "orders", Some(3), false).unwrap(), vec![3]);
        assert_eq!(deprecated_versions(&db), vec![(1, true), (2, false)]);

        let out = register(
            &db,
            INST,
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            r#"{"type":"object","properties":{"a":{"type":"string"},"b":{"type":"string"},"d":{"type":"string"}},
                "required":["a"],"additionalProperties":false}"#,
            None,
            None,
        )
        .unwrap();
        assert_eq!(out.version, 3);
        assert_eq!(deprecated_versions(&db), vec![(1, true), (2, false), (3, false)]);
        assert_eq!(effective(&db), 3);
    }

    #[test]
    fn a_corrupt_deprecated_versions_column_is_reported_not_ignored() {
        let db = three_versions();
        repository::bus_schema_subject_modify(&db, INST, "org-1", "orders", |_, row| {
            row.deprecated_versions_json = Some("not json".to_string());
            Ok(true)
        })
        .unwrap();
        assert!(matches!(
            resolve_effective(&db, INST, "org-1", "orders"),
            Err(BusServiceError::Db(_))
        ));
    }

    /// A deprecation made while a registration is between reading the
    /// subject and writing its version — of a version, and of the whole
    /// subject — survives the registration: it no longer writes the subject
    /// row back from the copy it read.
    #[test]
    fn a_registration_never_writes_back_a_deprecation_made_meanwhile() {
        let db = fresh_db();
        for text in [V1, V2_ADD_OPTIONAL] {
            register(&db, INST, "org-1", "race", SchemaType::JsonSchema, text, None, None).unwrap();
        }
        let before = repository::bus_schema_subject_get(&db, INST, "org-1", "race")
            .unwrap()
            .unwrap();
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hook_db = db.clone();
        let hook_fired = fired.clone();
        let _guard = install_hook_for_test(std::sync::Arc::new(move |stage, subject, _| {
            if stage == test_hooks::Stage::SlotDecided
                && subject == "race"
                && !hook_fired.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                delete(&hook_db, INST, "org-1", "race", Some(1), true).unwrap();
                delete(&hook_db, INST, "org-1", "race", None, true).unwrap();
            }
        }));
        let out = register(
            &db,
            INST,
            "org-1",
            "race",
            SchemaType::JsonSchema,
            V3_ADD_ANOTHER_OPTIONAL,
            None,
            None,
        )
        .unwrap();
        assert!(fired.load(std::sync::atomic::Ordering::SeqCst), "the hook ran");
        assert_eq!(out.version, 3);
        let after = repository::bus_schema_subject_get(&db, INST, "org-1", "race")
            .unwrap()
            .unwrap();
        assert!(after.deprecated_at_ms.is_some(), "the subject's deprecation survives");
        assert!(after.updated_at_ms >= before.updated_at_ms);
        let deprecated: Vec<u32> = list_versions(&db, INST, "org-1", "race")
            .unwrap()
            .into_iter()
            .filter(|v| v.deprecated_at_ms.is_some())
            .map(|v| v.version)
            .collect();
        assert_eq!(deprecated, vec![1], "the version's deprecation survives");
    }

    /// Two first registrations of one subject race: the other creates it
    /// (default `Backward`, v1) while this one — asking for `None` — sits
    /// between reading "no subject" and inserting it. This one then goes on
    /// as a registration onto an existing subject: an explicit compatibility
    /// that differs from the stored mode is refused, and nothing is written.
    #[test]
    fn a_registration_that_loses_the_subject_insert_follows_the_stored_compatibility() {
        let db = fresh_db();
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hook_db = db.clone();
        let hook_fired = fired.clone();
        let _guard = install_hook_for_test(std::sync::Arc::new(move |stage, subject, _| {
            if stage == test_hooks::Stage::SlotDecided
                && subject == "first-race"
                && !hook_fired.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                register(&hook_db, INST, "org-1", "first-race", SchemaType::JsonSchema, V1, None, None)
                    .unwrap();
            }
        }));
        let err = register(
            &db,
            INST,
            "org-1",
            "first-race",
            SchemaType::JsonSchema,
            V2_ADD_REQUIRED,
            Some(Compatibility::None),
            None,
        )
        .unwrap_err();
        assert!(fired.load(std::sync::atomic::Ordering::SeqCst), "the hook ran");
        assert!(
            matches!(err, BusServiceError::InvalidArgument(ref m) if m.contains("compatibility")),
            "{err:?}"
        );
        let versions: Vec<u32> = list_versions(&db, INST, "org-1", "first-race")
            .unwrap()
            .into_iter()
            .map(|v| v.version)
            .collect();
        assert_eq!(versions, vec![1], "the refused registration wrote no version");
        let subject = repository::bus_schema_subject_get(&db, INST, "org-1", "first-race")
            .unwrap()
            .unwrap();
        assert_eq!(subject.compatibility, "backward");
    }

    /// A hard delete leaves a tombstone, so the same content registered
    /// again under the freed number starts active; a subject records its
    /// incarnation at creation.
    #[test]
    fn a_hard_deleted_deprecated_version_registered_again_is_active() {
        let db = three_versions();
        delete(&db, INST, "org-1", "orders", Some(3), true).unwrap();
        assert_eq!(delete(&db, INST, "org-1", "orders", Some(3), false).unwrap(), vec![3]);
        let out = register(
            &db,
            INST,
            "org-1",
            "orders",
            SchemaType::JsonSchema,
            V3_ADD_ANOTHER_OPTIONAL,
            None,
            None,
        )
        .unwrap();
        assert_eq!((out.version, out.deduplicated), (3, false));
        assert_eq!(deprecated_versions(&db), vec![(1, false), (2, false), (3, false)]);
        assert_eq!(effective(&db), 3);
        let subject = repository::bus_schema_subject_get(&db, INST, "org-1", "orders")
            .unwrap()
            .unwrap();
        let list = parse_deprecated_versions("orders", subject.deprecated_versions_json.as_deref())
            .unwrap();
        assert_eq!(list.len(), 1, "one tombstone for v3: {list:?}");
        assert!(list[0].removed_at.is_some());
        assert_ne!(subject.generation, 0, "a subject names its incarnation");
    }

    /// A tombstone stamped by a node whose clock runs ahead does not swallow
    /// a later deprecation made here: events are stamped on the ledger's
    /// clock and a deprecation is placed past the tombstone of its own
    /// content. What the wire shows is still a wall-clock time.
    #[test]
    fn a_deprecation_after_a_tombstone_from_a_clock_ahead_still_counts() {
        let db = three_versions();
        let v1 = repository::bus_schema_version_get(&db, INST, "org-1", "orders", 1)
            .unwrap()
            .unwrap();
        let far_ahead = repository::bus_topic_generation_at(
            &crate::sync::ledger::HybridLogicalTimestamp {
                wall_time_ms: crate::bus::now_ms() + 3_600_000,
                logical: 0,
                node_id: "node-ahead".to_string(),
            },
        );
        repository::bus_schema_subject_modify(&db, INST, "org-1", "orders", |_, row| {
            row.deprecated_versions_json = Some(encode_deprecated_versions(vec![DeprecatedVersion {
                version: 1,
                content_hash: v1.content_hash.clone(),
                deprecated_at: None,
                removed_at: Some(far_ahead),
            }]));
            Ok(true)
        })
        .unwrap();
        let before = crate::bus::now_ms();
        delete(&db, INST, "org-1", "orders", Some(1), true).unwrap();
        let deprecated = list_versions(&db, INST, "org-1", "orders").unwrap()[0].deprecated_at_ms;
        let shown = deprecated.expect("the deprecation counts");
        assert!(
            shown >= before && shown <= crate::bus::now_ms() + 3_600_000,
            "a wall-clock time, not a packed stamp: {shown}"
        );
        assert_eq!(effective(&db), 3);
    }

    /// Every tombstone is kept, one per content: dropping the older content's
    /// would let its late deprecation revive.
    #[test]
    fn a_tombstone_of_each_content_survives_the_merge() {
        let tombstone = |hash: &str, at: u64| DeprecatedVersion {
            version: 3,
            content_hash: hash.to_string(),
            deprecated_at: None,
            removed_at: Some(at),
        };
        let merged = merge_deprecated_versions(
            vec![tombstone("a", 50), tombstone("b", 60)],
            vec![DeprecatedVersion {
                version: 3,
                content_hash: "a".to_string(),
                deprecated_at: Some(40),
                removed_at: None,
            }],
        );
        assert_eq!(merged.len(), 2);
        let content_a = DbBusSchemaVersion {
            instance_id: INST.to_string(),
            org_id: "org-1".to_string(),
            subject: "orders".to_string(),
            version: 3,
            schema_text: String::new(),
            content_hash: "a".to_string(),
            schema_ref_id: 1,
            created_by: None,
            created_at_ms: 0,
            subject_generation: 0,
        };
        assert_eq!(deprecated_at(&merged, &content_a), None, "the late deprecation stays covered");
    }
}
