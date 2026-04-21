// =============================================================================
// Plik: fuzz_targets/message_body_decode.rs
// Opis: Libfuzzer harness dla MessageBody decode. CI gate (#35): 5 min bez crasha.
// =============================================================================

#![no_main]

use libfuzzer_sys::fuzz_target;
use tentaflow_protocol::MessageBody;

fuzz_target!(|data: &[u8]| {
    let _ = rkyv::from_bytes::<MessageBody, rkyv::rancor::Error>(data);
});
