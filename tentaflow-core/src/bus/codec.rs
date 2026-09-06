// =============================================================================
// File: bus/codec.rs — shared CBOR encode/decode for fjall-backed bus metadata
// =============================================================================
//
// Small helper shared by `groups.rs` (consumer offsets) and `producer.rs`
// (producer sequence idempotency, PLAN §3.1 layer 1) — both store small
// CBOR-encoded structs in fjall keyspaces under `<bus_dir>/_meta`, mirroring
// `sync/ledger/types.rs`'s own private `encode`/`decode` pair. Not shared
// with that module directly (its helpers are `pub(crate)` to `sync::ledger`,
// not `bus`), so this is a deliberate, tiny duplication rather than a new
// cross-module dependency for two five-line functions.

use serde::{Deserialize, Serialize};

use super::BusServiceError;

pub(super) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, BusServiceError> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(value, &mut bytes)
        .map_err(|e| BusServiceError::Codec(e.to_string()))?;
    Ok(bytes)
}

pub(super) fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, BusServiceError> {
    ciborium::de::from_reader(std::io::Cursor::new(bytes))
        .map_err(|e| BusServiceError::Codec(e.to_string()))
}
