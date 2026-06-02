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

    let envelope = tentaflow_protocol::cbor::decode::<Envelope>(&buf)
        .map_err(IrohStreamError::EnvelopeDecode)?;
    let body = tentaflow_protocol::cbor::decode::<MessageBody>(&envelope.body)
        .map_err(IrohStreamError::BodyDecode)?;
    Ok((envelope, body))
}
