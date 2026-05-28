// =============================================================================
// Plik: mesh/cbor.rs
// Opis: Wspólne kodowanie i dekodowanie CBOR dla payloadów mesh.
// =============================================================================

use serde::de::DeserializeOwned;
use serde::Serialize;

pub fn encode<T: Serialize>(value: &T) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(value, &mut bytes)
        .map_err(|e| anyhow::anyhow!("CBOR encode failed: {e}"))?;
    Ok(bytes)
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> anyhow::Result<T> {
    ciborium::de::from_reader(std::io::Cursor::new(bytes))
        .map_err(|e| anyhow::anyhow!("CBOR decode failed: {e}"))
}
