// =============================================================================
// File: bus/schema_registry/stored_only.rs — binary kinds before F4
// =============================================================================
// `avro` / `protobuf` / `thrift` subjects can be registered and versioned in
// F3 so integrators can stage schemas ahead of F4, but nothing in this build
// can evaluate a payload against them. `compile` is a shape smoke-check
// only; every other operation returns `SchemaError::Unsupported`. F4
// replaces this file with one real implementation per kind — a pure
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_smoke_checks_and_avro_must_be_json() {
        assert!(AVRO_OPS.compile("{\"type\":\"record\",\"name\":\"X\",\"fields\":[]}").is_ok());
        assert!(AVRO_OPS.compile("not json").is_err());
        assert!(PROTOBUF_OPS.compile("syntax = \"proto3\"; message X {}").is_ok());
        assert!(THRIFT_OPS.compile("   ").is_err());
    }

    #[test]
    fn every_other_operation_is_unsupported_until_f4() {
        let compiled = PROTOBUF_OPS.compile("message X {}").unwrap();
        assert!(matches!(
            PROTOBUF_OPS.validate(&compiled, b"{}"),
            Err(SchemaError::Unsupported { .. })
        ));
        assert!(matches!(
            PROTOBUF_OPS.derive_subschema("message X {}", &BTreeSet::new()),
            Err(SchemaError::Unsupported { .. })
        ));
        assert!(PROTOBUF_OPS
            .check_compatibility("a", "b", Compatibility::None)
            .is_ok());
        assert!(matches!(
            PROTOBUF_OPS.check_compatibility("a", "b", Compatibility::Backward),
            Err(SchemaError::Unsupported { .. })
        ));
    }
}
