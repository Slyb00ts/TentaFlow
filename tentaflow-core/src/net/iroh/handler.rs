// =============================================================================
// Plik: net/iroh/handler.rs
// Opis: Pomocnicze typy dla implementacji `iroh::protocol::ProtocolHandler`.
//       Opakowuje `iroh::endpoint::Connection` w `IrohConnection` ktory
//       ujawnia wygodne API read/write CBOR-zakodowanych ramek MessageBody.
// =============================================================================

use iroh::endpoint::{Connection, RecvStream, SendStream};
use tentaflow_protocol::{Envelope, MessageBody};

const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Aktywne polaczenie iroh + peer id.
pub struct IrohConnection {
    pub inner: Connection,
    pub remote_id: iroh::EndpointId,
}

#[derive(Debug, thiserror::Error)]
pub enum IrohStreamError {
    #[error("iroh io: {0}")]
    Io(String),
    #[error("frame too large: {0} bajtow")]
    FrameTooLarge(usize),
    #[error("CBOR decode envelope: {0}")]
    EnvelopeDecode(String),
    #[error("schema version mismatch: peer {got}, local {expected}")]
    SchemaVersionMismatch { got: u16, expected: u16 },
    #[error("CBOR decode body: {0}")]
    BodyDecode(String),
    #[error("CBOR encode: {0}")]
    Encode(String),
}

impl IrohConnection {
    /// Otwiera bidi stream i zwraca (send, recv).
    pub async fn open_bi(&self) -> Result<(SendStream, RecvStream), IrohStreamError> {
        self.inner
            .open_bi()
            .await
            .map_err(|e| IrohStreamError::Io(format!("{e:?}")))
    }

    /// Przyjmuje przychodzacy bidi stream.
    pub async fn accept_bi(&self) -> Result<(SendStream, RecvStream), IrohStreamError> {
        self.inner
            .accept_bi()
            .await
            .map_err(|e| IrohStreamError::Io(format!("{e:?}")))
    }
}

/// Zapisuje `Envelope` na stream jako len-prefixed (u32 big-endian) CBOR.
pub async fn write_envelope(
    send: &mut SendStream,
    envelope: &Envelope,
) -> Result<(), IrohStreamError> {
    let bytes = tentaflow_protocol::cbor::encode(envelope).map_err(IrohStreamError::Encode)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(IrohStreamError::FrameTooLarge(bytes.len()));
    }
    send.write_all(&(bytes.len() as u32).to_be_bytes())
        .await
        .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
    send.write_all(&bytes)
        .await
        .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
    Ok(())
}

/// Czyta jedna ramke z streama i dekoduje `Envelope` + `MessageBody`.
pub async fn read_envelope_and_body(
    recv: &mut RecvStream,
) -> Result<(Envelope, MessageBody), IrohStreamError> {
    let mut len_bytes = [0u8; 4];
    recv.read_exact(&mut len_bytes)
        .await
        .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
    let len = u32::from_be_bytes(len_bytes) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(IrohStreamError::FrameTooLarge(len));
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf)
        .await
        .map_err(|e| IrohStreamError::Io(format!("{e}")))?;

    decode_envelope_and_body(&buf)
}

/// Decodes `Envelope` + `MessageBody` from raw frame bytes. The schema version
/// is enforced BEFORE the body is deserialized: `MessageBody` is a CBOR
/// index-tagged enum, so a frame from a peer on a different version could
/// decode "successfully" as the WRONG variant instead of being rejected
/// (mirrors the `MetaSchemaVersionCheck` gate on the dashboard WS).
pub fn decode_envelope_and_body(buf: &[u8]) -> Result<(Envelope, MessageBody), IrohStreamError> {
    let envelope = tentaflow_protocol::cbor::decode::<Envelope>(buf)
        .map_err(IrohStreamError::EnvelopeDecode)?;
    if envelope.schema_version != tentaflow_protocol::SCHEMA_VERSION {
        return Err(IrohStreamError::SchemaVersionMismatch {
            got: envelope.schema_version,
            expected: tentaflow_protocol::SCHEMA_VERSION,
        });
    }
    let body = tentaflow_protocol::cbor::decode::<MessageBody>(&envelope.body)
        .map_err(IrohStreamError::BodyDecode)?;
    Ok((envelope, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::{message_kind, SCHEMA_VERSION};

    fn frame_with_schema_version(version: u16) -> Vec<u8> {
        let body = tentaflow_protocol::cbor::encode(&MessageBody::MetaSchemaVersionCheck {
            client_version: version,
        })
        .expect("encode body");
        let mut envelope =
            Envelope::new_direct(1, 1, message_kind::META_SCHEMA_VERSION_CHECK, body);
        envelope.schema_version = version;
        tentaflow_protocol::cbor::encode(&envelope).expect("encode envelope")
    }

    #[test]
    fn decode_rejects_stale_schema_version_before_body_decode() {
        // A frame from a v20 peer with valid CBOR in the body — after the
        // variant tag shift (v21) this MUST return SchemaVersionMismatch,
        // never a misdecoded variant.
        let bytes = frame_with_schema_version(SCHEMA_VERSION - 1);
        match decode_envelope_and_body(&bytes) {
            Err(IrohStreamError::SchemaVersionMismatch { got, expected }) => {
                assert_eq!(got, SCHEMA_VERSION - 1);
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected SchemaVersionMismatch, got {other:?}"),
        }
    }

    #[test]
    fn decode_accepts_matching_schema_version() {
        let bytes = frame_with_schema_version(SCHEMA_VERSION);
        let (envelope, body) = decode_envelope_and_body(&bytes).expect("decode");
        assert_eq!(envelope.schema_version, SCHEMA_VERSION);
        assert!(matches!(
            body,
            MessageBody::MetaSchemaVersionCheck {
                client_version
            } if client_version == SCHEMA_VERSION
        ));
    }
}
