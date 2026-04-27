// =============================================================================
// Plik: pii.rs
// Opis: Inner-enum pack dla operacji na regulach PII. Spakowany w jednym
//       slocie `MessageBody::PiiRuleBody`, zeby zaoszczedzic miejsce w
//       enumie MessageBody (rkyv 0.8 hard limit 256 wariantow).
//       Pattern: `NsightPayload`.
// =============================================================================

use rkyv::{Archive, Deserialize, Serialize};

use crate::message_body::PiiRule;

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum PiiRulePayload {
    ListRequest,
    ListResponse { rules: Vec<PiiRule> },
}
