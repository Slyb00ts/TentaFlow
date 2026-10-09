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
//   - `xsd` (F4 B4) and `hl7v2_profile` (F4 B5) are a hand-written XSD subset
//     (no pure-Rust validator exists; libxml2 would be a native dependency)
//     and a JSON profile over the HL7 v2 parser. Unlike the binary kinds they
//     are bound to ONE payload format each (`required_payload_format`).
//     Both are fully implemented (validate, derive, compatibility).
//
// Everything expensive or rejectable happens in `compile` (admin time).
// `validate` runs on the publish hot path for opted-in topics only: it never
// re-parses the schema, its work is bounded by the payload size and a hard
// budget, and it may allocate scratch space proportional to the payload.
// =============================================================================

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::bus::payload_format::PayloadFormat;

mod hl7v2_profile;
mod json_schema;
pub mod registry;
mod stored_only;
mod xsd;

#[cfg(test)]
mod fixtures;

/// Hard cap on registered schema text, checked before compile and before
/// insert — an admin-supplied schema is a DoS surface (PLAN-F3 R2).
pub const MAX_SCHEMA_TEXT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaType {
    JsonSchema,
    Avro,
    Protobuf,
    Thrift,
    Xsd,
    Hl7v2Profile,
}

impl SchemaType {
    /// Every kind the registry stores, in the order the UI lists them.
    pub const ALL: [SchemaType; 6] = [
        SchemaType::JsonSchema,
        SchemaType::Avro,
        SchemaType::Protobuf,
        SchemaType::Thrift,
        SchemaType::Xsd,
        SchemaType::Hl7v2Profile,
    ];

    /// Persisted form (`bus_schema_subjects.schema_type` CHECK constraint).
    pub fn as_str(self) -> &'static str {
        match self {
            SchemaType::JsonSchema => "json_schema",
            SchemaType::Avro => "avro",
            SchemaType::Protobuf => "protobuf",
            SchemaType::Thrift => "thrift",
            SchemaType::Xsd => "xsd",
            SchemaType::Hl7v2Profile => "hl7v2_profile",
        }
    }

    /// `json_schema|avro|…` as listed in "must be one of" request errors,
    /// built from `ALL` so a new kind cannot be missing from the message.
    pub fn accepted_list() -> String {
        Self::ALL
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join("|")
    }

    pub fn parse(s: &str) -> Option<SchemaType> {
        match s {
            "json_schema" => Some(SchemaType::JsonSchema),
            "avro" => Some(SchemaType::Avro),
            "protobuf" => Some(SchemaType::Protobuf),
            "thrift" => Some(SchemaType::Thrift),
            "xsd" => Some(SchemaType::Xsd),
            "hl7v2_profile" => Some(SchemaType::Hl7v2Profile),
            _ => None,
        }
    }

    /// Whether this build can actually evaluate payloads against schemas
    /// of this type — gates `bus_topics.validation != off` (PLAN-F3 §3
    /// rule 3). F4 flips each kind to `true` by adding its validator.
    pub fn has_validator(self) -> bool {
        matches!(
            self,
            SchemaType::JsonSchema | SchemaType::Xsd | SchemaType::Hl7v2Profile
        )
    }

    pub fn ops(self) -> &'static dyn SchemaKindOps {
        match self {
            SchemaType::JsonSchema => &json_schema::JSON_SCHEMA_OPS,
            SchemaType::Avro => &stored_only::AVRO_OPS,
            SchemaType::Protobuf => &stored_only::PROTOBUF_OPS,
            SchemaType::Thrift => &stored_only::THRIFT_OPS,
            SchemaType::Xsd => &xsd::XSD_OPS,
            SchemaType::Hl7v2Profile => &hl7v2_profile::HL7V2_PROFILE_OPS,
        }
    }

    /// The one payload format a topic must carry to bind a subject of this
    /// kind; `None` for kinds that describe their own wire encoding
    /// (the binary ones) and so bind independently of `content_type`.
    /// `json_schema` is JSON-only today, `xsd` validates XML documents and
    /// an HL7 v2 profile validates ER7 messages.
    pub fn required_payload_format(self) -> Option<PayloadFormat> {
        match self {
            SchemaType::JsonSchema => Some(PayloadFormat::Json),
            SchemaType::Xsd => Some(PayloadFormat::Xml),
            SchemaType::Hl7v2Profile => Some(PayloadFormat::Hl7V2),
            SchemaType::Avro | SchemaType::Protobuf | SchemaType::Thrift => None,
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
    /// The check gave up on a document (work budget, nesting depth) without
    /// finding it invalid, so it must not be reported as a violation: the
    /// document may well be valid.
    LimitExceeded(String),
    /// `old` -> `new` is not compatible under the requested mode.
    Incompatible(String),
    /// The operation is not implemented for this schema type in this build
    /// (kinds without a validator).
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
            SchemaError::LimitExceeded(m) => write!(f, "schema check limit exceeded: {m}"),
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
    Xsd(xsd::Compiled),
    Hl7v2Profile(hl7v2_profile::Compiled),
    /// Kinds without a validator yet carry no compiled form — the variant exists so
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

    /// Publish-path check: bounded work, no schema re-parsing. A document
    /// the check cannot decide within its budget is `LimitExceeded`, never
    /// a `Violation`.
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
///
/// R6 (accepted, `SUM/tentabus/PLAN-APP-PLATFORM.md`): this counter is
/// PROCESS-GLOBAL, not per-instance, unlike `schema_ref_id_for`'s content
/// hash above — a schema edit in instance A bumps the SAME counter instance
/// B reads, so it invalidates instance B's compiled-schema cache too, even
/// though instance B's schema tables were untouched. That is a deliberate
/// over-invalidation, not a bug: adding an instance dimension here would
/// only save a cache recompile on the next `publish`/`peek` after an
/// unrelated instance's schema write, and a stale-generation compile is
/// already the fallback path every cache miss takes.
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
        for t in SchemaType::ALL {
            assert_eq!(SchemaType::parse(t.as_str()), Some(t));
        }
        // `ALL` must list every variant: `BusCapabilitiesWire.schema_types` is
        // filtered from it by `has_validator`, so a kind missing here could
        // never reach that list.
        for t in SchemaType::ALL {
            match t {
                SchemaType::JsonSchema
                | SchemaType::Avro
                | SchemaType::Protobuf
                | SchemaType::Thrift
                | SchemaType::Xsd
                | SchemaType::Hl7v2Profile => {}
            }
        }
        assert_eq!(SchemaType::ALL.len(), 6);
        for c in [
            Compatibility::None,
            Compatibility::Backward,
            Compatibility::Forward,
            Compatibility::Full,
        ] {
            assert_eq!(Compatibility::parse(c.as_str()), Some(c));
        }
        assert_eq!(SchemaType::parse("xsd"), Some(SchemaType::Xsd));
        assert_eq!(
            SchemaType::parse("hl7v2_profile"),
            Some(SchemaType::Hl7v2Profile)
        );
        assert_eq!(SchemaType::parse("XSD"), None);
        assert_eq!(SchemaType::parse("hl7v2"), None);
    }

    #[test]
    fn payload_format_requirement_per_kind() {
        assert_eq!(
            SchemaType::JsonSchema.required_payload_format(),
            Some(PayloadFormat::Json)
        );
        assert_eq!(
            SchemaType::Xsd.required_payload_format(),
            Some(PayloadFormat::Xml)
        );
        assert_eq!(
            SchemaType::Hl7v2Profile.required_payload_format(),
            Some(PayloadFormat::Hl7V2)
        );
        for t in [SchemaType::Avro, SchemaType::Protobuf, SchemaType::Thrift] {
            assert_eq!(t.required_payload_format(), None);
        }
    }

    #[test]
    fn text_kinds_have_a_validator_and_binary_kinds_do_not_yet() {
        assert!(SchemaType::JsonSchema.has_validator());
        assert!(SchemaType::Xsd.has_validator());
        assert!(SchemaType::Hl7v2Profile.has_validator());
        assert!(!SchemaType::Avro.has_validator());
        assert!(!SchemaType::Protobuf.has_validator());
        assert!(!SchemaType::Thrift.has_validator());
    }

    /// The dashboard explains a refused XSD or HL7 profile by matching these
    /// phrases (`schema-windows.js` `textRefusalReason`, `unprocessed.js`
    /// `plainCheckError`) and the XSD compatibility reason by
    /// `requires element 'x'`. Rewording one here without the dashboard turns
    /// a plain-language refusal back into a generic one.
    #[test]
    fn schema_error_phrases_are_stable() {
        let xsd = |body: &str| {
            format!(r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">{body}</xs:schema>"#)
        };
        let element = r#"<xs:element name="a" type="xs:string"/>"#;
        let invalid_with = |kind: SchemaType, text: &str, phrase: &str| {
            let err = kind.ops().compile(text).unwrap_err();
            assert!(
                matches!(&err, SchemaError::Invalid(m) if m.contains(phrase)),
                "{text}: expected {phrase:?}, got {err:?}"
            );
        };
        for (text, phrase) in [
            (
                r#"{"required_fields":["MSH-1"]}"#,
                "'MSH-1' is the message's own field-separator",
            ),
            (
                r#"{"required_fields":["MSH-2"]}"#,
                "'MSH-2' is the message's own field-separator",
            ),
            (
                r#"{"required_fields":["pid5"]}"#,
                "'pid5' is not SEGMENT-N shaped",
            ),
            (
                r#"{"required_fields":["PID-0"]}"#,
                "is not a valid positive field number",
            ),
            (
                r#"{"required_fields":["PID-1000"]}"#,
                "exceeds the supported maximum of 999",
            ),
            (
                r#"{"required_fields":["PACJENT-3"]}"#,
                "'PACJENT' is not a valid 3-character segment id",
            ),
            (
                r#"{"required_segments":["pid"]}"#,
                "'pid' is not a valid 3-character segment id",
            ),
            (r#"{"required":[]}"#, "unknown field `required`"),
            (
                r#"{"required_fields":["PID-3","PID-3"]}"#,
                "required_fields lists 'PID-3' more than once",
            ),
            (
                r#"{"required_segments":["PID","PID"]}"#,
                "required_segments lists 'PID' more than once",
            ),
            (r#"{"required_fields":"x"}"#, "not a valid HL7 v2 profile"),
        ] {
            invalid_with(SchemaType::Hl7v2Profile, text, phrase);
        }
        let many: Vec<String> = (0..=512).map(|i| format!("\"PID-{}\"", i + 1)).collect();
        invalid_with(
            SchemaType::Hl7v2Profile,
            &format!("{{\"required_fields\":[{}]}}", many.join(",")),
            "entries, exceeding the 512-entry limit",
        );
        invalid_with(
            SchemaType::Hl7v2Profile,
            &format!("{{\"description\":\"{}\"}}", "x".repeat(1001)),
            "description exceeds 1000 characters",
        );
        for (body, phrase) in [
            (r#"<xs:element name="a"><xs:complexType><xs:sequence><xs:element ref="b"/></xs:sequence></xs:complexType></xs:element>"#.to_string(), " ref= is not supported"),
            (format!(r#"<xs:key name="k"/>{element}"#), "identity constraints are not supported"),
            (
                r#"<xs:element name="a"><xs:complexType><xs:complexContent/></xs:complexType></xs:element>"#.to_string(),
                "type derivation",
            ),
            (format!(r#"<xs:simpleType name="s"><xs:list itemType="xs:int"/></xs:simpleType>{element}"#), "list and union simple types are not supported"),
            (format!(r#"<xs:attribute name="x" type="xs:string"/>{element}"#), "global attributes are not supported"),
            (format!(r#"<xs:foo/>{element}"#), "xs:foo: this construct is not part of the supported XSD subset"),
            (format!(r#"<xs:element name="a" type="p:x" xmlns:p="urn:p"/>"#), "namespace declarations are only supported on xs:schema"),
            (format!(r#"<xs:element name="a" type="q:x"/>"#), "uses a namespace prefix that is not declared"),
            (format!(r#"<xs:include schemaLocation="x.xsd"/>{element}"#), "xs:include: schema composition is not supported"),
            (format!(r#"<xs:group name="g"><xs:sequence/></xs:group>{element}"#), "xs:group: named groups are not supported"),
            (
                r#"<xs:element name="a"><xs:complexType><xs:sequence><xs:any/></xs:sequence></xs:complexType></xs:element>"#.to_string(),
                "xs:any: wildcards are not supported",
            ),
            (format!(r#"<xs:notation name="n" public="p"/>{element}"#), "xs:notation: notations are not supported"),
            (r#"<xs:element name="a" type="xs:float"/>"#.to_string(), "built-in type xs:float is not supported"),
            (
                r#"<xs:element name="a"><xs:simpleType><xs:restriction base="xs:string"><xs:totalDigits value="2"/></xs:restriction></xs:simpleType></xs:element>"#.to_string(),
                "facet xs:totalDigits is not supported",
            ),
            (r#"<xs:simpleType name="s"><xs:restriction base="xs:string"/></xs:simpleType>"#.to_string(), "the schema declares no global element"),
            (
                r#"<xs:element name="a"><xs:complexType mixed="true"><xs:sequence/></xs:complexType></xs:element>"#.to_string(),
                "mixed content (mixed=\"true\") is not supported",
            ),
        ] {
            invalid_with(SchemaType::Xsd, &xsd(&body), phrase);
        }
        // The dashboard anchors these at the start of the sentence, so each
        // must begin with its phrase (after `invalid schema: `).
        let begins = |kind: SchemaType, text: &str, prefix: &str| {
            let err = kind.ops().compile(text).unwrap_err();
            assert!(
                matches!(&err, SchemaError::Invalid(m) if m.starts_with(prefix)),
                "{text}: expected a start of {prefix:?}, got {err:?}"
            );
        };
        for (text, prefix) in [
            (r#"{"required_fields":["MSH-1"]}"#, "hl7: "),
            (
                r#"{"required_fields":["pid5"]}"#,
                "hl7: 'pid5' is not SEGMENT-N shaped",
            ),
            (r#"{"required_fields":["PID-0"]}"#, "hl7: "),
            (r#"{"required_fields":["PID-1000"]}"#, "hl7: field number"),
            (
                r#"{"required_fields":["PID-3","PID-3"]}"#,
                "required_fields lists",
            ),
            (
                r#"{"required_segments":["PID","PID"]}"#,
                "required_segments lists",
            ),
            (r#"{"required":[]}"#, "not a valid HL7 v2 profile"),
            ("not json at all", "not a valid HL7 v2 profile"),
            (r#"["PID-3"]"#, "not a valid HL7 v2 profile"),
        ] {
            begins(SchemaType::Hl7v2Profile, text, prefix);
        }
        invalid_with(
            SchemaType::Hl7v2Profile,
            r#"["PID-3"]"#,
            "the profile must be a JSON object",
        );
        for (body, prefix) in [
            (
                r#"<xs:element name="a" type="xs:float"/>"#,
                "built-in type xs:float is not supported",
            ),
            (
                r#"<xs:element name="a"><xs:simpleType><xs:restriction base="xs:string"><xs:totalDigits value="2"/></xs:restriction></xs:simpleType></xs:element>"#,
                "facet xs:totalDigits is not supported",
            ),
            (
                r#"<xs:simpleType name="s"><xs:restriction base="xs:string"/></xs:simpleType>"#,
                "the schema declares no global element",
            ),
            (
                r#"<xs:element name="a"><xs:complexType mixed="true"><xs:sequence/></xs:complexType></xs:element>"#,
                "mixed content",
            ),
        ] {
            begins(SchemaType::Xsd, &xsd(body), prefix);
        }
        begins(
            SchemaType::Xsd,
            r#"<schema xmlns="urn:nie-xsd"><element name="a"/></schema>"#,
            "the root element must be xs:schema",
        );
        invalid_with(
            SchemaType::Xsd,
            &xsd(r#"<xs:element name="a" type="p:x" xmlns:p="urn:p"/>"#),
            "namespace declarations are only supported on xs:schema",
        );
        let other_ns = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:p="urn:other" targetNamespace="urn:mine"><xs:element name="a" type="p:x"/></xs:schema>"#;
        invalid_with(
            SchemaType::Xsd,
            other_ns,
            "belongs to namespace 'urn:other'; types from other namespaces are not supported",
        );
        invalid_with(
            SchemaType::Xsd,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><a:foo xmlns:a="urn:x"/></xs:schema>"#,
            "namespace declarations are only supported on xs:schema",
        );
        invalid_with(
            SchemaType::Xsd,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:o="urn:o"><o:foo/><xs:element name="a" type="xs:string"/></xs:schema>"#,
            "element 'foo' is not in the XML Schema namespace",
        );

        // Instance-side phrases of the XSD checker the dashboard translates
        // for a message that reached the unprocessed list.
        let instance_schema = xsd(
            r#"<xs:simpleType name="s"><xs:restriction base="xs:string"><xs:minLength value="2"/><xs:maxLength value="3"/><xs:pattern value="[a-z]+"/></xs:restriction></xs:simpleType>
               <xs:simpleType name="e"><xs:restriction base="xs:string"><xs:enumeration value="x"/></xs:restriction></xs:simpleType>
               <xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="a" type="xs:int"/>
                 <xs:element name="b" type="s" minOccurs="0"/>
                 <xs:element name="c" type="e" minOccurs="0"/>
               </xs:sequence><xs:attribute name="id" type="xs:int" use="required"/></xs:complexType></xs:element>
               <xs:element name="u"><xs:complexType><xs:all>
                 <xs:element name="a" type="xs:string"/>
                 <xs:element name="b" type="xs:string"/>
               </xs:all></xs:complexType></xs:element>"#,
        );
        let xsd_ops = SchemaType::Xsd.ops();
        let compiled = xsd_ops.compile(&instance_schema).unwrap();
        for (payload, expected) in [
            (r#"<r id="1"><x/></r>"#, "/r/x: element is not allowed here"),
            (r#"<r id="1"/>"#, "/r: required child elements are missing"),
            (
                r#"<r id="1"><a>x</a></r>"#,
                "/r/a: value is not a valid xs:int",
            ),
            (
                r#"<r id="1"><a>1</a><b>a</b></r>"#,
                "/r/b: value is shorter than minLength",
            ),
            (
                r#"<r id="1"><a>1</a><b>abcd</b></r>"#,
                "/r/b: value is longer than maxLength",
            ),
            (
                r#"<r id="1"><a>1</a><b>ab1</b></r>"#,
                "/r/b: value does not match the pattern",
            ),
            (
                r#"<r id="1"><a>1</a><c>y</c></r>"#,
                "/r/c: value is not one of the enumerated values",
            ),
            (
                r#"<r id="x"><a>1</a></r>"#,
                "/r: attribute 'id' is not a valid xs:int",
            ),
            (
                r#"<r><a>1</a></r>"#,
                "/r: required attribute 'id' is missing",
            ),
            (
                r#"<r id="1" z="2"><a>1</a></r>"#,
                "/r: attribute 'z' is not declared",
            ),
            (
                r#"<r id="1" xsi:type="x"><a>1</a></r>"#,
                "/r: attribute 'xsi:type' is not supported (xsi:type and xsi:nil are not honoured)",
            ),
            (r#"<q/>"#, "<root>: root element 'q' is not declared"),
            (
                r#"<r id="1">text<a>1</a></r>"#,
                "/r: character data is not allowed in element-only content",
            ),
            (r#"<u><a/><a/></u>"#, "/u/a: element occurs more than once"),
            (
                r#"<u><a/></u>"#,
                "/u: required child element 'b' is missing",
            ),
            (
                r#"<r id="1"><a>1</a></r><r/>"#,
                "<root>: more than one root element",
            ),
            (
                r#"<!DOCTYPE r><r/>"#,
                "<root>: DOCTYPE declarations are not allowed",
            ),
            ("", "<root>: document has no root element"),
            (r#"<r id="1"><a>1</b></r>"#, "/r/a: not well-formed XML"),
            (
                r#"<r id="1"><a>&foo;</a></r>"#,
                "/r/a: entity references other than the five predefined ones are not supported",
            ),
        ] {
            assert!(
                matches!(
                    xsd_ops.validate(&compiled, payload.as_bytes()),
                    Err(SchemaError::Violation(ref m)) if m == expected
                ),
                "{payload}: expected {expected:?}, got {:?}",
                xsd_ops.validate(&compiled, payload.as_bytes())
            );
        }

        // Instance-side phrases of the HL7 parser.
        let hl7_ops = SchemaType::Hl7v2Profile.ops();
        let msh_only = hl7_ops.compile(r#"{"required_segments":["MSH"]}"#).unwrap();
        for (payload, expected) in [
            (
                &b"PID|1"[..],
                "hl7: message does not start with an MSH segment",
            ),
            (b"MSH", "hl7: MSH segment has no field separator"),
            (b"MSH|^~\\&|a\rP!D|1", "hl7: segment id is not valid"),
            (
                b"MSH|^~\\&|a\rPIDx",
                "hl7: a segment is missing the field separator after its id",
            ),
            (b"", "hl7: empty message"),
        ] {
            assert!(
                matches!(
                    hl7_ops.validate(&msh_only, payload),
                    Err(SchemaError::Violation(ref m)) if m == expected
                ),
                "{payload:?}: expected {expected:?}, got {:?}",
                hl7_ops.validate(&msh_only, payload)
            );
        }
        assert!(matches!(
            hl7_ops.validate(&msh_only, b"\xff\xfe"),
            Err(SchemaError::Violation(m)) if m.starts_with("hl7: not valid utf-8")
        ));

        // Instance-side phrases the dashboard translates for an HL7 message.
        let compiled = SchemaType::Hl7v2Profile
            .ops()
            .compile(r#"{"required_segments":["PV1"],"required_fields":["PID-3"]}"#)
            .unwrap();
        let ops = SchemaType::Hl7v2Profile.ops();
        let no_pv1 = b"MSH|^~\\&|A|B|C|D|2026||ADT^A01|1|P|2.5\rPID|1||MRN1\r";
        assert!(matches!(
            ops.validate(&compiled, no_pv1),
            Err(SchemaError::Violation(m)) if m == "required segment 'PV1' is missing"
        ));
        let empty_pid3 = b"MSH|^~\\&|A|B|C|D|2026||ADT^A01|1|P|2.5\rPID|1||\rPV1|1\r";
        assert!(matches!(
            ops.validate(&compiled, empty_pid3),
            Err(SchemaError::Violation(m)) if m == "PID-3 is required but empty or missing (segment occurrence 1)"
        ));
    }

    #[test]
    fn bump_generation_is_monotonic() {
        let before = generation();
        bump_generation();
        assert!(generation() > before);
    }
}
