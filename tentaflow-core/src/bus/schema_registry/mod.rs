// =============================================================================
// File: bus/schema_registry/mod.rs — per-kind schema operations (F3)
// =============================================================================
// SUM/tentabus/PLAN-F3.md. A topic may bind a registered, versioned schema
// subject (`bus_topics.schema_id` = subject name) and opt into
// `validation = warn | dlq` on publish. This module is the format-specific
// half: one `SchemaKindOps` implementation per `SchemaType`, structured like
// `bus::payload_format` — one submodule per kind, deliberately NO shared
// intermediate representation (an Avro sub-schema and a JSON Schema
// sub-schema have nothing in common structurally).
//
// Owner decisions (02.09.2026):
//   - `json_schema` is fully implemented here (validate, derive sub-schema
//     for field-policy read projections, version compatibility) by a
//     HAND-WRITTEN SUBSET validator with zero new dependencies. Any keyword
//     outside the supported subset is REJECTED at registration time
//     (`compile`), never silently ignored — a partial validator that skips
//     keywords would be worse than none for a compliance-facing feature.
//   - `avro` / `protobuf` / `thrift` are storage-only until F4
//     (`stored_only`): `compile` is a shape smoke-check, every other
//     operation returns `SchemaError::Unsupported`.
//   - XSD is out of scope entirely (no pure-Rust validator; libxml2 would
//     be a native dependency).
//
// Everything expensive or rejectable happens in `compile` (admin time).
// `validate` runs on the publish hot path for opted-in topics only and must
// not allocate on the success path.
// =============================================================================

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};

mod json_schema;
pub mod registry;
mod stored_only;

/// Hard cap on registered schema text, checked before compile and before
/// insert — an admin-supplied schema is a DoS surface (PLAN-F3 R2).
pub const MAX_SCHEMA_TEXT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaType {
    JsonSchema,
    Avro,
    Protobuf,
    Thrift,
}

impl SchemaType {
    /// Persisted form (`bus_schema_subjects.schema_type` CHECK constraint).
    pub fn as_str(self) -> &'static str {
        match self {
            SchemaType::JsonSchema => "json_schema",
            SchemaType::Avro => "avro",
            SchemaType::Protobuf => "protobuf",
            SchemaType::Thrift => "thrift",
        }
    }

    pub fn parse(s: &str) -> Option<SchemaType> {
        match s {
            "json_schema" => Some(SchemaType::JsonSchema),
            "avro" => Some(SchemaType::Avro),
            "protobuf" => Some(SchemaType::Protobuf),
            "thrift" => Some(SchemaType::Thrift),
            _ => None,
        }
    }

    /// Whether this build can actually evaluate payloads against schemas
    /// of this type — gates `bus_topics.validation != off` (PLAN-F3 §3
    /// rule 3). F4 flips the binary kinds to `true` by adding validators.
    pub fn has_validator(self) -> bool {
        matches!(self, SchemaType::JsonSchema)
    }

    pub fn ops(self) -> &'static dyn SchemaKindOps {
        match self {
            SchemaType::JsonSchema => &json_schema::JSON_SCHEMA_OPS,
            SchemaType::Avro => &stored_only::AVRO_OPS,
            SchemaType::Protobuf => &stored_only::PROTOBUF_OPS,
            SchemaType::Thrift => &stored_only::THRIFT_OPS,
        }
    }
}

/// Confluent-style version compatibility mode, per subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compatibility {
    None,
    /// A reader using the NEW schema can read data written under the OLD.
    Backward,
    /// A reader using the OLD schema can read data written under the NEW.
    Forward,
    Full,
}

impl Compatibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Compatibility::None => "none",
            Compatibility::Backward => "backward",
            Compatibility::Forward => "forward",
            Compatibility::Full => "full",
        }
    }

    pub fn parse(s: &str) -> Option<Compatibility> {
        match s {
            "none" => Some(Compatibility::None),
            "backward" => Some(Compatibility::Backward),
            "forward" => Some(Compatibility::Forward),
            "full" => Some(Compatibility::Full),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaError {
    /// Schema text is not a valid schema of its declared type (malformed,
    /// unsupported keyword, unsupported draft, too large, remote `$ref`).
    Invalid(String),
    /// Payload does not conform to the compiled schema (publish path).
    Violation(String),
    /// `old` -> `new` is not compatible under the requested mode.
    Incompatible(String),
    /// The operation is not implemented for this schema type in this build
    /// (binary kinds until F4).
    Unsupported {
        schema_type: SchemaType,
        operation: &'static str,
    },
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaError::Invalid(m) => write!(f, "invalid schema: {m}"),
            SchemaError::Violation(m) => write!(f, "schema violation: {m}"),
            SchemaError::Incompatible(m) => write!(f, "incompatible schema change: {m}"),
            SchemaError::Unsupported {
                schema_type,
                operation,
            } => write!(
                f,
                "{operation} is not supported for {} schemas in this build",
                schema_type.as_str()
            ),
        }
    }
}

/// Registration-time compiled form, opaque to callers; produced by
/// `SchemaKindOps::compile` and consumed only by the same kind's
/// `validate`. Kept as an enum (not a trait object) so the publish path can
/// hold it in an `Arc` inside a `DashMap` without extra indirection.
#[derive(Debug)]
pub enum CompiledSchema {
    JsonSchema(json_schema::Compiled),
    /// Binary kinds carry no compiled form until F4 — the variant exists so
    /// a stored-only subject still yields a `CompiledSchema` from
    /// `compile` and can be cached uniformly.
    StoredOnly(SchemaType),
}

/// Implemented once per `SchemaType`. See the module header for the split
/// between registration-time (`compile`, `check_compatibility`,
/// `derive_subschema`) and publish-time (`validate`) responsibilities.
pub trait SchemaKindOps: Send + Sync {
    /// Parse + compile at registration time. Everything expensive or
    /// rejectable happens here, never on the publish path.
    fn compile(&self, schema_text: &str) -> Result<CompiledSchema, SchemaError>;

    /// Publish-path check; must not allocate on the success path.
    fn validate(&self, compiled: &CompiledSchema, payload: &[u8]) -> Result<(), SchemaError>;

    /// Owner decision 1: the schema describing EXACTLY the projection a
    /// field policy's `allowed` top-level field set produces (F4's binary
    /// codecs re-encode a read against it). Output must be deterministic
    /// for the same inputs so it can be memoized by content.
    fn derive_subschema(
        &self,
        schema_text: &str,
        allowed: &BTreeSet<String>,
    ) -> Result<String, SchemaError>;

    /// `Compatibility::None` is always `Ok`.
    fn check_compatibility(
        &self,
        old_schema_text: &str,
        new_schema_text: &str,
        mode: Compatibility,
    ) -> Result<(), SchemaError>;
}

/// Node-independent, content-derived id stamped into each validated
/// record's on-disk `schema_id` (`tentaflow_bus::batch::RecordInput`):
/// `blake3(org_id | 0 | subject | 0 | content_hash)` truncated to `u32`,
/// with `0` (reserved: "no schema") remapped. Deriving from `content_hash`
/// rather than the version NUMBER is deliberate: a version number is a slot,
/// not content — delete v3 and register different text and the new v3 must
/// NOT inherit the old v3's id (already stamped on old on-disk records; a
/// consumer resolving that id would then decode the wrong schema). Content
/// addressing also means two mesh nodes registering the exact same bytes
/// under the same (subject, version) converge on the same id with no
/// coordination; a collision is caught by `UNIQUE(instance_id, org_id,
/// schema_ref_id)` and surfaces as a loud registration error, never silent
/// corruption.
///
/// Deliberately takes NO `instance_id`, unlike every other id in the bus:
/// two instances in one org that register byte-identical text under the
/// same subject derive the SAME `u32`. That is not a cross-instance leak —
/// the registry tables are keyed `(instance_id, org_id, subject, …)` and
/// the uniqueness constraint above is per-instance, so neither instance can
/// read or clobber the other's row, and a stamped id is only ever resolved
/// against its own instance's rows. Adding `instance_id` to the hash would
/// buy no isolation and would break the content-addressing property that
/// lets two mesh nodes converge without coordination.
pub fn schema_ref_id_for(org_id: &str, subject: &str, content_hash: &str) -> u32 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(org_id.as_bytes());
    hasher.update(&[0]);
    hasher.update(subject.as_bytes());
    hasher.update(&[0]);
    hasher.update(content_hash.as_bytes());
    let digest = hasher.finalize();
    let bytes = digest.as_bytes();
    let id = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if id == 0 {
        1
    } else {
        id
    }
}

/// Hex blake3 of the schema text — `bus_schema_versions.content_hash`,
/// the dedup key that makes registration idempotent on content.
pub fn content_hash(schema_text: &str) -> String {
    blake3::hash(schema_text.as_bytes()).to_hex().to_string()
}

/// Process-global generation counter for the schema-registry validator
/// cache (PLAN-F3 §4.2). Bumped both by a local registry write
/// (`registry::register`/`set_compatibility`/`delete`) and by
/// `sync::core_materializer` applying a replicated `core.bus_schema_subject`
/// / `core.bus_schema_version` op — `BusService`'s `schema_cache` entry for
/// a subject is valid only as long as its captured generation matches this
/// counter's current value.
///
/// Originally lived in `sync::core_materializer` (the other of the two
/// writers, landed before this module's registration/mutation logic did)
/// and was moved here once this module existed, per the frozen contract:
/// `bus::mod::BusService` and `registry` both need to reach it, and this
/// module sits below both in the dependency graph.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Current value of [`GENERATION`] — a `BusService::schema_cache` entry is
/// still valid iff it was stamped with exactly this value.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// Bumps [`GENERATION`]. `AcqRel`/`Acquire` mirrors
/// `services::bus_authorizer::{bump_acl_generation, ACL_GENERATION}` — the
/// existing precedent for a bus-related cache-invalidation counter in this
/// codebase — rather than `Relaxed`, so a reader that observes a bumped
/// generation also observes every write that happened-before the bump (the
/// row it would recompile against).
pub fn bump_generation() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_ref_id_is_deterministic_and_never_zero() {
        let a = schema_ref_id_for("org-1", "patients", "hash-a");
        let b = schema_ref_id_for("org-1", "patients", "hash-a");
        assert_eq!(a, b);
        assert_ne!(a, 0);
        assert_ne!(a, schema_ref_id_for("org-1", "patients", "hash-b"));
        assert_ne!(a, schema_ref_id_for("org-2", "patients", "hash-a"));
    }

    #[test]
    fn schema_type_and_compatibility_round_trip_their_persisted_forms() {
        for t in [
            SchemaType::JsonSchema,
            SchemaType::Avro,
            SchemaType::Protobuf,
            SchemaType::Thrift,
        ] {
            assert_eq!(SchemaType::parse(t.as_str()), Some(t));
        }
        for c in [
            Compatibility::None,
            Compatibility::Backward,
            Compatibility::Forward,
            Compatibility::Full,
        ] {
            assert_eq!(Compatibility::parse(c.as_str()), Some(c));
        }
        assert_eq!(SchemaType::parse("xsd"), None);
    }

    #[test]
    fn only_json_schema_has_a_validator_in_this_build() {
        assert!(SchemaType::JsonSchema.has_validator());
        assert!(!SchemaType::Avro.has_validator());
        assert!(!SchemaType::Protobuf.has_validator());
        assert!(!SchemaType::Thrift.has_validator());
    }

    #[test]
    fn bump_generation_is_monotonic() {
        let before = generation();
        bump_generation();
        assert!(generation() > before);
    }
}
