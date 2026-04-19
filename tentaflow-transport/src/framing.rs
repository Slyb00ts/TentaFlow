// =============================================================================
// Plik: tentaflow-transport/src/framing.rs
// Opis: Framing bidi streamow iroh — `[u32 big-endian length][rkyv payload]`.
//       Uzywane symetrycznie w obu kierunkach. Limit 16 MiB chroni przed OOM
//       przy uszkodzonej glowie ramki.
// =============================================================================

use iroh::endpoint::{RecvStream, SendStream};
use rkyv::api::high::{HighDeserializer, HighSerializer};
use rkyv::rancor::Error as RkyvError;
use rkyv::ser::allocator::ArenaHandle;
use rkyv::util::AlignedVec;
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};

use crate::error::TransportError;

/// Maksymalny rozmiar pojedynczej ramki (16 MiB). Caller moze odrzucic ramke
/// przekraczajaca ten limit zanim zaalokuje bufor.
pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// Serializuje `value` jako rkyv i wysyla length-prefixed frame.
pub async fn write_frame<T>(send: &mut SendStream, value: &T) -> Result<(), TransportError>
where
    T: for<'a> RkyvSerialize<HighSerializer<AlignedVec, ArenaHandle<'a>, RkyvError>>,
{
    let bytes = rkyv::to_bytes::<RkyvError>(value)
        .map_err(|e| TransportError::Serialize(e.to_string()))?;

    if bytes.len() > MAX_FRAME_SIZE {
        return Err(TransportError::FrameTooLarge {
            limit: MAX_FRAME_SIZE,
            got: bytes.len(),
        });
    }

    let len = (bytes.len() as u32).to_be_bytes();
    send.write_all(&len)
        .await
        .map_err(|e| TransportError::Io(std::io::Error::other(e.to_string())))?;
    send.write_all(&bytes)
        .await
        .map_err(|e| TransportError::Io(std::io::Error::other(e.to_string())))?;
    Ok(())
}

/// Odczytuje jedna ramke i deserializuje do `T`. Zwraca `None` jesli peer
/// zamknal stream przed wyslaniem jakiegokolwiek bajtu (clean EOF).
pub async fn read_frame<T>(recv: &mut RecvStream) -> Result<Option<T>, TransportError>
where
    T: Archive,
    T::Archived: RkyvDeserialize<T, HighDeserializer<RkyvError>>
        + for<'a> rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, RkyvError>>,
{
    let mut len_buf = [0u8; 4];

    match recv.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("early eof") || msg.contains("FinishedEarly") {
                return Ok(None);
            }
            return Err(TransportError::Io(std::io::Error::other(msg)));
        }
    }

    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 {
        return Ok(None);
    }
    if len > MAX_FRAME_SIZE {
        return Err(TransportError::FrameTooLarge {
            limit: MAX_FRAME_SIZE,
            got: len,
        });
    }

    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf)
        .await
        .map_err(|e| TransportError::Io(std::io::Error::other(e.to_string())))?;

    let archived = rkyv::access::<T::Archived, RkyvError>(&buf)
        .map_err(|e| TransportError::Deserialize(e.to_string()))?;
    let value = rkyv::deserialize::<T, RkyvError>(archived)
        .map_err(|e| TransportError::Deserialize(e.to_string()))?;
    Ok(Some(value))
}
