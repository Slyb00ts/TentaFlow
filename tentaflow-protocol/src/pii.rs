// =============================================================================
// Plik: pii.rs
// Opis: Inner-enum pack dla operacji na regulach PII. Spakowany w jednym
//       slocie `MessageBody::PiiRuleBody`, zeby jedna funkcja zajmowala
//       jeden wariant i enum MessageBody zostal czytelny (limitu 256
//       wariantow NIE ma — ciborium taguje po NAZWIE).
//       Pattern: `ProfilingPayload`.
// =============================================================================

use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

use crate::message_body::PiiRule;

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum PiiRulePayload {
    ListRequest,
    ListResponse { rules: Vec<PiiRule> },
}
