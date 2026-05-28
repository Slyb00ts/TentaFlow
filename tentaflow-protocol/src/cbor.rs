// =============================================================================
// Plik: cbor.rs
// Opis: Wspólny codec CBOR dla ramek protokołu TentaFlow.
// =============================================================================

use serde::de::DeserializeOwned;
use serde::Serialize;

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    ciborium::ser::into_writer(value, &mut out).map_err(|e| format!("CBOR encode failed: {e}"))?;
    Ok(out)
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    ciborium::de::from_reader(std::io::Cursor::new(bytes))
        .map_err(|e| format!("CBOR decode failed: {e}"))
}
