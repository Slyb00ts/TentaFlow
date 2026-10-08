// =============================================================================
// File: bus/schema_registry/stored_only.rs — kinds without a validator yet
// =============================================================================
// `avro` / `protobuf` / `thrift` / `xsd` / `hl7v2_profile` subjects can be
// registered and versioned so integrators can stage schemas ahead of F4, but
// nothing in this build can evaluate a payload against them. `compile` is a
// shape smoke-check only; every other operation returns
// `SchemaError::Unsupported` (a version check under any compatibility mode
// but `none` therefore refuses too — there is no comparison to pretend
// with). F4 replaces each kind's entry with a real implementation — a pure
// addition, no F3 code changes.
// =============================================================================

use std::collections::BTreeSet;

use super::{Compatibility, CompiledSchema, SchemaError, SchemaKindOps, SchemaType};

pub struct StoredOnlyOps(pub SchemaType);

impl SchemaKindOps for StoredOnlyOps {
    fn compile(&self, schema_text: &str) -> Result<CompiledSchema, SchemaError> {
        if schema_text.trim().is_empty() {
            return Err(SchemaError::Invalid("schema text is empty".to_string()));
        }
        if self.0 == SchemaType::Avro {
            // An Avro schema is JSON by definition — the one structural
            // check we can do without an Avro parser.
            serde_json::from_str::<serde_json::Value>(schema_text)
                .map_err(|e| SchemaError::Invalid(format!("avro schema is not valid JSON: {e}")))?;
        }
        Ok(CompiledSchema::StoredOnly(self.0))
    }

    fn validate(&self, _compiled: &CompiledSchema, _payload: &[u8]) -> Result<(), SchemaError> {
        Err(SchemaError::Unsupported {
            schema_type: self.0,
            operation: "validate",
        })
    }

    fn derive_subschema(
        &self,
        _schema_text: &str,
        _allowed: &BTreeSet<String>,
    ) -> Result<String, SchemaError> {
        Err(SchemaError::Unsupported {
            schema_type: self.0,
            operation: "derive_subschema",
        })
    }

    fn check_compatibility(
        &self,
        _old_schema_text: &str,
        _new_schema_text: &str,
        mode: Compatibility,
    ) -> Result<(), SchemaError> {
        if mode == Compatibility::None {
            return Ok(());
        }
        Err(SchemaError::Unsupported {
            schema_type: self.0,
            operation: "check_compatibility",
        })
    }
}

pub(super) static AVRO_OPS: StoredOnlyOps = StoredOnlyOps(SchemaType::Avro);
pub(super) static PROTOBUF_OPS: StoredOnlyOps = StoredOnlyOps(SchemaType::Protobuf);
pub(super) static THRIFT_OPS: StoredOnlyOps = StoredOnlyOps(SchemaType::Thrift);
pub(super) static XSD_OPS: StoredOnlyOps = StoredOnlyOps(SchemaType::Xsd);
pub(super) static HL7V2_PROFILE_OPS: StoredOnlyOps = StoredOnlyOps(SchemaType::Hl7v2Profile);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_smoke_checks_and_avro_must_be_json() {
        assert!(AVRO_OPS
            .compile("{\"type\":\"record\",\"name\":\"X\",\"fields\":[]}")
            .is_ok());
        assert!(AVRO_OPS.compile("not json").is_err());
        assert!(PROTOBUF_OPS
            .compile("syntax = \"proto3\"; message X {}")
            .is_ok());
        assert!(THRIFT_OPS.compile("   ").is_err());
    }

    #[test]
    fn every_other_operation_is_unsupported_until_f4() {
        for ops in [&PROTOBUF_OPS, &XSD_OPS, &HL7V2_PROFILE_OPS] {
            let compiled = ops.compile("message X {}").unwrap();
            assert!(matches!(
                ops.validate(&compiled, b"{}"),
                Err(SchemaError::Unsupported { .. })
            ));
            assert!(matches!(
                ops.derive_subschema("message X {}", &BTreeSet::new()),
                Err(SchemaError::Unsupported { .. })
            ));
            assert!(ops
                .check_compatibility("a", "b", Compatibility::None)
                .is_ok());
            assert!(matches!(
                ops.check_compatibility("a", "b", Compatibility::Backward),
                Err(SchemaError::Unsupported { .. })
            ));
        }
    }

    #[test]
    fn xsd_and_hl7_profile_smoke_check_rejects_blank_text() {
        assert!(XSD_OPS.compile("<xs:schema/>").is_ok());
        assert!(XSD_OPS.compile("  \n").is_err());
        assert!(HL7V2_PROFILE_OPS
            .compile("{\"required_segments\":[]}")
            .is_ok());
        assert!(HL7V2_PROFILE_OPS.compile("").is_err());
    }
}
