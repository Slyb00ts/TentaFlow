// =============================================================================
// Plik: quic/protocol.rs
// Opis: Framing dla QUIC streamow — length-prefixed rkyv. Protokol:
//       [4 bajty big-endian length][payload rkyv]. Uzywane w obu kierunkach
//       (request router→sidecar, response/chunk sidecar→router).
// =============================================================================

use anyhow::{Context, Result};
use quinn::{RecvStream, SendStream};
use rkyv::rancor::Error as RkyvError;
use rkyv::Archive;
use rkyv::Deserialize as RkyvDeserialize;
use rkyv::Serialize as RkyvSerialize;
use tokio::io::AsyncWriteExt;

/// Max rozmiar jednej ramki (16 MB) — chroni przed OOM przy uszkodzonym lencie.
pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// Zapisuje rkyv-serializable typ jako length-prefixed frame.
pub async fn write_frame<T>(send: &mut SendStream, value: &T) -> Result<()>
where
    T: for<'a> RkyvSerialize<rkyv::api::high::HighSerializer<rkyv::util::AlignedVec, rkyv::ser::allocator::ArenaHandle<'a>, RkyvError>>,
{
    let bytes = rkyv::to_bytes::<RkyvError>(value)
        .map_err(|e| anyhow::anyhow!("rkyv serialization failed: {}", e))?;
    if bytes.len() > MAX_FRAME_SIZE {
        anyhow::bail!("frame zbyt duzy: {} > {}", bytes.len(), MAX_FRAME_SIZE);
    }
    let len = (bytes.len() as u32).to_be_bytes();
    send.write_all(&len).await.context("zapis length-prefix")?;
    send.write_all(&bytes).await.context("zapis payload")?;
    Ok(())
}

/// Odczytuje jedna ramke (4-bajtowy length prefix BE + payload) i deserializuje
/// do typu T. Zwraca None jesli peer zamknal stream przed zapisem czegokolwiek.
pub async fn read_frame<T>(recv: &mut RecvStream) -> Result<Option<T>>
where
    T: Archive,
    T::Archived: RkyvDeserialize<T, rkyv::api::high::HighDeserializer<RkyvError>>
        + for<'a> rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, RkyvError>>,
{
    let mut len_buf = [0u8; 4];
    match recv.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(quinn::ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(e) => return Err(anyhow::anyhow!("read length: {}", e)),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 {
        return Ok(None);
    }
    if len > MAX_FRAME_SIZE {
        anyhow::bail!("frame zbyt duzy w naglowku: {}", len);
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await.context("read payload")?;

    let archived = rkyv::access::<T::Archived, RkyvError>(&buf)
        .map_err(|e| anyhow::anyhow!("rkyv validation: {}", e))?;
    let value = rkyv::deserialize::<T, RkyvError>(archived)
        .map_err(|e| anyhow::anyhow!("rkyv deserialization: {}", e))?;
    Ok(Some(value))
}

/// Kody bledow aplikacyjnych dla `Connection::close()` — peer zobaczy je jako
/// powod rozlaczenia. Obydwie strony uzywaja tej samej enumeracji zeby logi
/// byly czytelne.
#[derive(Debug, Clone, Copy)]
#[repr(u32)]
pub enum CloseCode {
    /// Graceful shutdown — strona konczy prace (Ctrl+C, restart, deploy).
    Shutdown = 0,
    /// Shutdown z powodu bledu wewnetrznego.
    InternalError = 1,
    /// Wersja protokolu niewspierana.
    ProtocolMismatch = 2,
    /// Autoryzacja odrzucona.
    Unauthorized = 3,
}

impl CloseCode {
    pub fn code(self) -> quinn::VarInt {
        quinn::VarInt::from_u32(self as u32)
    }
    pub fn reason(self) -> &'static [u8] {
        match self {
            CloseCode::Shutdown => b"shutdown",
            CloseCode::InternalError => b"internal_error",
            CloseCode::ProtocolMismatch => b"protocol_mismatch",
            CloseCode::Unauthorized => b"unauthorized",
        }
    }
}
