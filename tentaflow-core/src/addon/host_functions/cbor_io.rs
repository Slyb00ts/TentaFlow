// =============================================================================
// File: addon/host_functions/cbor_io.rs
// Opis: Wspoldzielone helpery I/O CBOR dla host functions ABI. CBOR jest
//       jedynym dozwolonym formatem serializacji host-function ABI; te helpery
//       sa reuzywane przez wszystkie moduly host-fn (camera i kolejne), zeby
//       logika rozmiaru payloadu i retry semantics nie byla duplikowana.
// =============================================================================

use minicbor::{Decode, Encode};

use super::super::errors::AbiError;
use super::super::runtime::WasmMemory;
use super::abi_helpers::{enforce_payload_size, write_output_with_retry_semantics, PayloadKind};
use super::{read_guest_bytes, AddonState, WasmCaller};

/// Reads CBOR input from guest memory and decodes it into `T`, enforcing the
/// `kind` payload ceiling BEFORE copying any bytes onto the host heap. An
/// adversarial addon passing `input_len = i32::MAX` is rejected before a large
/// allocation is attempted. Most modules pass `PayloadKind::ServiceCall`; the
/// vector module passes `PayloadKind::VectorItem` to keep its tighter 1 MiB cap.
pub fn read_input_cbor<T>(
    memory: &WasmMemory,
    caller: &WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    kind: PayloadKind,
) -> Result<T, AbiError>
where
    T: for<'b> Decode<'b, ()>,
{
    if input_len < 0 {
        return Err(AbiError::Operation);
    }
    if enforce_payload_size(input_len as usize, kind).is_err() {
        return Err(AbiError::PayloadTooLarge);
    }
    let bytes =
        read_guest_bytes(memory, caller, input_ptr, input_len).ok_or(AbiError::Operation)?;
    decode_cbor_exact(bytes)
}

/// Decodes a single CBOR value from `bytes` and rejects any trailing bytes.
/// `minicbor::decode` happily stops at the first complete value, so a valid
/// prefix followed by garbage would otherwise be accepted; this enforces that
/// the whole input was consumed.
pub fn decode_cbor_exact<T>(bytes: &[u8]) -> Result<T, AbiError>
where
    T: for<'b> Decode<'b, ()>,
{
    let mut decoder = minicbor::Decoder::new(bytes);
    let value = decoder.decode::<T>().map_err(|_| AbiError::Operation)?;
    if decoder.position() != bytes.len() {
        return Err(AbiError::Operation);
    }
    Ok(value)
}

/// Encodes `value` to CBOR and writes it through the retry helper, re-checking
/// the `kind` ceiling so a large response shape cannot blow past the limit. Most
/// modules pass `PayloadKind::ServiceCall` (8 MiB); the vector module passes
/// `PayloadKind::VectorItem` (1 MiB).
pub fn write_cbor_capped<T: Encode<()>>(
    memory: &WasmMemory,
    caller: &mut WasmCaller<'_, AddonState>,
    value: &T,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
    kind: PayloadKind,
) -> i32 {
    let mut serialized = Vec::new();
    if minicbor::encode(value, &mut serialized).is_err() {
        return AbiError::Operation.as_i32();
    }
    if enforce_payload_size(serialized.len(), kind).is_err() {
        return AbiError::PayloadTooLarge.as_i32();
    }
    write_output_with_retry_semantics(memory, caller, &serialized, out_ptr, out_cap, out_len_ptr)
}
