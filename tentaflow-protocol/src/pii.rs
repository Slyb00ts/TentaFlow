// =============================================================================
// Plik: pii.rs
// Opis: Inner-enum pack dla operacji na regulach PII. Spakowany w jednym
//       slocie `MessageBody::PiiRuleBody`, zeby zaoszczedzic miejsce w
//       enumie MessageBody (CBOR 0.8 hard limit 256 wariantow).
//       Pattern: `ProfilingPayload`.
// =============================================================================

use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

use crate::message_body::PiiRule;

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum PiiRulePayload {
    ListRequest,
    ListResponse { rules: Vec<PiiRule> },
}
