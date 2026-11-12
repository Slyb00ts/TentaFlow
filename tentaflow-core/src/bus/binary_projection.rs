// =============================================================================
// File: bus/binary_projection.rs — read projections of Avro/Protobuf/Thrift
//       records (F4 B6)
// =============================================================================
// SUM/tentabus/POLITYKI-POL-FORMATY.md, decision 1: a record of a topic whose
// messages are Avro, Protobuf or Thrift is read through a field policy by
// RE-ENCODING it under a sub-schema. The record is decoded with the version of
// the pattern it was WRITTEN under (the id stamped into it by the publish
// that validated it, `FetchedRecordMeta::schema_id`; an unstamped record names
// no version and is withheld), the fields the
// reader's rule does not show are left out
// (`SchemaAwareFieldFormat::project`), and the result is stamped with the id of
// the sub-schema that describes it (`derived_ref_id_for`), which the reader
// resolves back to the sub-schema's text (`BusService::schema_for_reader`).
//
// What this module owns is the part that must not be redone per record: the
// compiled writer versions (an LRU keyed by the content-addressed ref id, so a
// version stays valid forever and a node that registered newer versions still
// reads old records) and the derived sub-schema texts (an LRU keyed by version
// content and the kept field set). A topic
// without a read rule never reaches any of it.
//
// Fail closed: a record that is not valid under its writer version, a stamp
// that names no version of the topic's pattern, a pattern that no longer
// compiles, a rule that shows a field reaching the message itself — every one of them is a withheld record (`WITHHELD`), never a
// payload shown as it is.
// =============================================================================

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::num::NonZeroUsize;
use std::sync::Arc;

use bytes::Bytes;
use parking_lot::Mutex;

use super::schema_registry::{self, CompiledSchema, SchemaType, ValidationBudget};
use super::BusServiceError;
use crate::db::repository;
use crate::db::DbPool;

/// Compiled writer versions kept per node (all organizations, all subjects).
const VERSION_CACHE_ENTRIES: usize = 48;
/// Derived sub-schemas kept per node.
const DERIVED_CACHE_ENTRIES: usize = 512;
/// Scans of a subject's versions remembered (one per reader's set of visible
/// fields).
const SCAN_CACHE_ENTRIES: usize = 256;
/// Versions of a subject (newest first) tried when a reader asks for a
/// sub-schema this node has not derived since it started.
const MAX_VERSIONS_SCANNED: usize = 64;

/// One version of a pattern, compiled once.
pub(crate) struct WriterVersion {
    pub version: u32,
    pub schema_ref_id: u32,
    pub content_hash: String,
    pub schema_type: SchemaType,
    text: String,
    compiled: CompiledSchema,
    /// Names of the top-level fields the version declares.
    pub declared: BTreeSet<String>,
}

/// The sub-schema of one version for one set of kept fields.
pub(crate) struct DerivedSchema {
    pub ref_id: u32,
    pub text: String,
    pub kept: BTreeSet<String>,
}

type DerivedKey = (String, String, String, [u8; 32]);

/// The caches behind every binary read projection of one bus instance
/// (`BusService::binary_projector`, shared with its `ConsumerHandle`s).
pub(crate) struct BinaryProjector {
    db: DbPool,
    instance_id: String,
    versions: Mutex<lru::LruCache<(String, String, u32), Arc<WriterVersion>>>,
    /// Derivation is a function of the version's text and the kept names, so
    /// a refusal (a field reaching the message itself, a pattern no longer
    /// readable) is remembered as the sentence it was refused with: such a
    /// version would otherwise be parsed and derived again, and logged, by
    /// every read of one of its records.
    derived: Mutex<lru::LruCache<DerivedKey, Result<Arc<DerivedSchema>, String>>>,
    /// `(org, subject, id of a sub-schema)` -> `schema_ref_id` of the version
    /// it was derived from: how a reader's request for the schema of a record
    /// finds the version without trying them all (`BusService::schema_for_reader`).
    sources: Mutex<lru::LruCache<(String, String, u32), u32>>,
    /// `(org, subject, digest of the visible fields)` -> the registry
    /// generation a scan of the subject's newest versions ran at, and the
    /// sub-schema id each of them derives to (with its `schema_ref_id`). A
    /// request for an id nobody derived is answered from it, so guessing ids
    /// costs one scan per registry change, not one per request.
    scans: Mutex<lru::LruCache<ScanKey, (u64, HashMap<u32, u32>)>>,
    #[cfg(test)]
    scans_run: std::sync::atomic::AtomicUsize,
}

type ScanKey = (String, String, [u8; 32]);

fn names_digest(names: &BTreeSet<String>) -> [u8; 32] {
    let mut digest = blake3::Hasher::new();
    for name in names {
        digest.update(name.as_bytes());
        digest.update(&[0]);
    }
    *digest.finalize().as_bytes()
}

fn unreadable_message(subject: &str, version: u32, e: impl std::fmt::Display) -> String {
    format!(
        "subject '{subject}' v{version}: {}: {e}",
        super::STORED_SCHEMA_UNREADABLE
    )
}

fn unreadable(subject: &str, version: u32, e: impl std::fmt::Display) -> BusServiceError {
    BusServiceError::InvalidArgument(unreadable_message(subject, version, e))
}

impl BinaryProjector {
    pub(crate) fn new(db: DbPool, instance_id: String) -> BinaryProjector {
        BinaryProjector {
            db,
            instance_id,
            versions: Mutex::new(lru::LruCache::new(
                NonZeroUsize::new(VERSION_CACHE_ENTRIES).expect("nonzero"),
            )),
            derived: Mutex::new(lru::LruCache::new(
                NonZeroUsize::new(DERIVED_CACHE_ENTRIES).expect("nonzero"),
            )),
            sources: Mutex::new(lru::LruCache::new(
                NonZeroUsize::new(DERIVED_CACHE_ENTRIES).expect("nonzero"),
            )),
            scans: Mutex::new(lru::LruCache::new(
                NonZeroUsize::new(SCAN_CACHE_ENTRIES).expect("nonzero"),
            )),
            #[cfg(test)]
            scans_run: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Compiles a stored version (or serves it from the cache).
    fn compile(
        &self,
        org_id: &str,
        subject: &str,
        schema_type: SchemaType,
        version: u32,
        schema_ref_id: u32,
        content_hash: String,
        text: String,
    ) -> Result<Arc<WriterVersion>, BusServiceError> {
        let key = (org_id.to_string(), subject.to_string(), schema_ref_id);
        if let Some(hit) = self.versions.lock().get(&key) {
            if hit.content_hash == content_hash {
                return Ok(hit.clone());
            }
        }
        let format = schema_type
            .field_format()
            .ok_or_else(|| BusServiceError::InvalidArgument("not a binary pattern".into()))?;
        let compiled = schema_type
            .ops()
            .compile(&text)
            .map_err(|e| unreadable(subject, version, e))?;
        let declared = format
            .declared_fields(&compiled)
            .map_err(|e| unreadable(subject, version, e))?
            .into_iter()
            .collect();
        let entry = Arc::new(WriterVersion {
            version,
            schema_ref_id,
            content_hash,
            schema_type,
            text,
            compiled,
            declared,
        });
        self.versions.lock().put(key, entry.clone());
        Ok(entry)
    }

    /// The version a record was written under: the one its stamp names. A
    /// record without a stamp (the topic did not validate it, or it failed
    /// the check) names no version, and reading it with the version the topic
    /// uses now would let an evolved pattern read the bytes of one field as
    /// another (Avro has no field tags): `None`, like a stamp that names no
    /// version of `subject`.
    pub(crate) fn writer_of(
        &self,
        org_id: &str,
        subject: &str,
        stamp: u32,
    ) -> Result<Option<Arc<WriterVersion>>, BusServiceError> {
        if stamp == 0 {
            return Ok(None);
        }
        if let Some(hit) =
            self.versions
                .lock()
                .get(&(org_id.to_string(), subject.to_string(), stamp))
        {
            return Ok(Some(hit.clone()));
        }
        let Some(row) = repository::bus_schema_version_by_ref_id(
            &self.db,
            &self.instance_id,
            org_id,
            subject,
            stamp,
        )?
        else {
            return Ok(None);
        };
        let Some(subject_row) =
            repository::bus_schema_subject_get(&self.db, &self.instance_id, org_id, subject)?
        else {
            return Ok(None);
        };
        let Some(schema_type) = SchemaType::parse(&subject_row.schema_type) else {
            return Ok(None);
        };
        self.compile(
            org_id,
            subject,
            schema_type,
            row.version,
            row.schema_ref_id,
            row.content_hash,
            row.schema_text,
        )
        .map(Some)
    }

    /// The sub-schema of `version` for the fields of `visible` it declares.
    pub(crate) fn derived(
        &self,
        org_id: &str,
        subject: &str,
        version: &WriterVersion,
        visible: &BTreeSet<String>,
    ) -> Result<Arc<DerivedSchema>, BusServiceError> {
        let kept: BTreeSet<String> = visible
            .iter()
            .filter(|name| version.declared.contains(*name))
            .cloned()
            .collect();
        let key: DerivedKey = (
            org_id.to_string(),
            subject.to_string(),
            version.content_hash.clone(),
            names_digest(&kept),
        );
        let cached = self.derived.lock().get(&key).cloned();
        match cached {
            Some(Ok(hit)) => {
                self.note_source(org_id, subject, &hit, version);
                return Ok(hit);
            }
            Some(Err(refusal)) => return Err(BusServiceError::InvalidArgument(refusal)),
            None => {}
        }
        let text = match version
            .schema_type
            .ops()
            .derive_subschema(&version.text, &kept)
        {
            Ok(text) => text,
            Err(e) => {
                let refusal = unreadable_message(subject, version.version, e);
                tracing::warn!(
                    org_id, subject, version = version.version, error = %refusal,
                    "binary read projection: no sub-schema can be derived from this version                      for the reader's fields; its records are withheld from such readers"
                );
                self.derived.lock().put(key, Err(refusal.clone()));
                return Err(BusServiceError::InvalidArgument(refusal));
            }
        };
        let entry = Arc::new(DerivedSchema {
            ref_id: schema_registry::derived_ref_id_for(
                org_id,
                subject,
                &version.content_hash,
                &kept,
            ),
            text,
            kept,
        });
        self.derived.lock().put(key, Ok(entry.clone()));
        self.note_source(org_id, subject, &entry, version);
        Ok(entry)
    }

    fn note_source(
        &self,
        org_id: &str,
        subject: &str,
        derived: &DerivedSchema,
        version: &WriterVersion,
    ) {
        self.sources.lock().put(
            (org_id.to_string(), subject.to_string(), derived.ref_id),
            version.schema_ref_id,
        );
    }

    /// The sub-schema the reader whose rule shows `visible` is served under
    /// `derived_id`: the version it was derived from is the one the last
    /// projection under that id remembered, else the one a scan of the
    /// subject's newest versions found (once per registry change and set of
    /// visible fields, whatever ids are asked for). `None` when no version
    /// derives to that id for THESE visible fields — a rule changed since the
    /// record was read answers nothing, so a reader never gets a sub-schema
    /// wider than what its rule shows now.
    pub(crate) fn derived_by_id(
        &self,
        org_id: &str,
        subject: &str,
        visible: &BTreeSet<String>,
        derived_id: u32,
    ) -> Result<Option<(Arc<WriterVersion>, Arc<DerivedSchema>)>, BusServiceError> {
        let remembered = self
            .sources
            .lock()
            .get(&(org_id.to_string(), subject.to_string(), derived_id))
            .copied();
        let stamp = match remembered {
            Some(stamp) => stamp,
            None => match self.scanned_source(org_id, subject, visible, derived_id)? {
                Some(stamp) => stamp,
                None => return Ok(None),
            },
        };
        let Some(version) = self.writer_of(org_id, subject, stamp)? else {
            return Ok(None);
        };
        let derived = self.derived(org_id, subject, &version, visible)?;
        Ok((derived.ref_id == derived_id).then_some((version, derived)))
    }

    /// `schema_ref_id` of the version that derives to `derived_id` for
    /// `visible`, from the scan of the subject's newest versions.
    fn scanned_source(
        &self,
        org_id: &str,
        subject: &str,
        visible: &BTreeSet<String>,
        derived_id: u32,
    ) -> Result<Option<u32>, BusServiceError> {
        let key: ScanKey = (
            org_id.to_string(),
            subject.to_string(),
            names_digest(visible),
        );
        let generation = schema_registry::generation();
        if let Some((scanned_at, found)) = self.scans.lock().get(&key) {
            if *scanned_at == generation {
                return Ok(found.get(&derived_id).copied());
            }
        }
        #[cfg(test)]
        self.scans_run
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut heads =
            repository::bus_schema_version_heads(&self.db, &self.instance_id, org_id, subject)?;
        heads.reverse();
        let mut found = HashMap::new();
        for head in heads.into_iter().take(MAX_VERSIONS_SCANNED) {
            let derived = self
                .writer_of(org_id, subject, head.schema_ref_id)
                .and_then(|version| match version {
                    Some(version) => self.derived(org_id, subject, &version, visible).map(Some),
                    None => Ok(None),
                });
            match derived {
                Ok(Some(derived)) => {
                    found.insert(derived.ref_id, head.schema_ref_id);
                }
                Ok(None) => {}
                // A version the rule cannot be applied to (it does not
                // compile, a shown field reaches the message itself) derives
                // to nothing: it is not what the reader is served.
                Err(e) => tracing::debug!(
                    org_id, subject, error = %e,
                    "binary read projection: a version has no sub-schema for this reader"
                ),
            }
        }
        let answer = found.get(&derived_id).copied();
        self.scans.lock().put(key, (generation, found));
        Ok(answer)
    }

    /// The top-level fields `payload` sets under `version`
    /// (`SchemaAwareFieldFormat::list_fields`).
    pub(crate) fn present_fields(
        &self,
        version: &WriterVersion,
        payload: &[u8],
        budget: &mut ValidationBudget,
    ) -> Result<BTreeSet<String>, schema_registry::SchemaError> {
        match version.schema_type.field_format() {
            Some(format) => format.list_fields(&version.compiled, payload, budget),
            None => Ok(BTreeSet::new()),
        }
    }

    /// Refused derivations remembered so far (tests).
    #[cfg(test)]
    pub(crate) fn derivation_refusals_cached(&self) -> usize {
        self.derived
            .lock()
            .iter()
            .filter(|(_, entry)| entry.is_err())
            .count()
    }

    /// Scans of a subject's versions run so far (tests).
    #[cfg(test)]
    pub(crate) fn scans_run(&self) -> usize {
        self.scans_run.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Forgets which version every sub-schema id was derived from, as a
    /// restart does (tests).
    #[cfg(test)]
    pub(crate) fn forget_sources(&self) {
        self.sources.lock().clear();
    }

    /// Whether anything is cached (tests: a topic without rules must leave
    /// this empty).
    #[cfg(test)]
    pub(crate) fn is_untouched(&self) -> bool {
        self.versions.lock().is_empty() && self.derived.lock().is_empty()
    }

    pub(crate) fn field_policy_walk_budget(org_id: &str) -> ValidationBudget {
        ValidationBudget::for_batch(org_id, 0)
    }
}

/// A record the reader may not see at all.
pub(crate) const WITHHELD: (Bytes, u32) = (Bytes::new(), 0);

/// One reader's binary projection of one topic: the pattern the topic is
/// bound to and what is resolved from it so far, for one read call (a fetch
/// batch, a peek).
pub(crate) struct BinaryRead {
    projector: Arc<BinaryProjector>,
    org_id: String,
    subject: String,
    budget: RefCell<ValidationBudget>,
    /// Stamp -> the version it names and the sub-schema for the reader's
    /// fields (`None` when the stamp names nothing readable).
    seen: RefCell<HashMap<u32, Option<(Arc<WriterVersion>, Arc<DerivedSchema>)>>>,
}

impl BinaryRead {
    pub(crate) fn new(
        projector: Arc<BinaryProjector>,
        org_id: &str,
        subject: String,
    ) -> BinaryRead {
        BinaryRead {
            budget: RefCell::new(BinaryProjector::field_policy_walk_budget(org_id)),
            projector,
            org_id: org_id.to_string(),
            subject,
            seen: RefCell::new(HashMap::new()),
        }
    }

    fn resolve(
        &self,
        stamp: u32,
        visible: &BTreeSet<String>,
    ) -> Option<(Arc<WriterVersion>, Arc<DerivedSchema>)> {
        if let Some(known) = self.seen.borrow().get(&stamp) {
            return known.clone();
        }
        let resolved = match self.projector.writer_of(&self.org_id, &self.subject, stamp) {
            Ok(Some(version)) => self
                .projector
                .derived(&self.org_id, &self.subject, &version, visible)
                // `derived` has logged a refusal once, when it was first made.
                .ok()
                .map(|derived| (version, derived)),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(
                    org_id = %self.org_id, subject = %self.subject, error = %e,
                    "binary read projection: the writer schema of a record is unusable; \
                     the record is withheld"
                );
                None
            }
        };
        self.seen.borrow_mut().insert(stamp, resolved.clone());
        resolved
    }

    /// `payload` as the reader whose rule shows `visible` reads it, with the
    /// id of the sub-schema it is written under; `WITHHELD` when it cannot be
    /// projected.
    pub(crate) fn project(
        &self,
        visible: &BTreeSet<String>,
        stamp: u32,
        payload: &[u8],
    ) -> (Bytes, u32) {
        let Some((version, derived)) = self.resolve(stamp, visible) else {
            return WITHHELD;
        };
        let Some(format) = version.schema_type.field_format() else {
            return WITHHELD;
        };
        match format.project(
            &version.compiled,
            payload,
            &derived.kept,
            &mut self.budget.borrow_mut(),
        ) {
            Ok(bytes) => (Bytes::from(bytes), derived.ref_id),
            Err(e) => {
                tracing::debug!(
                    org_id = %self.org_id, subject = %self.subject, error = %e,
                    "binary read projection: a record is not valid under its writer schema; withheld"
                );
                WITHHELD
            }
        }
    }
}
