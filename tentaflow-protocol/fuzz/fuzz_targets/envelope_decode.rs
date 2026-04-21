// =============================================================================
// Plik: fuzz_targets/envelope_decode.rs
// Opis: Libfuzzer harness dla Envelope decode. CI gate (#35): 5 min bez crasha.
//       Prowadzony: `cargo +nightly fuzz run envelope_decode -- -max_total_time=300`
// =============================================================================

#![no_main]

use libfuzzer_sys::fuzz_target;
use tentaflow_protocol::Envelope;

fuzz_target!(|data: &[u8]| {
    // rkyv::from_bytes z bytecheck walidacja — nigdy nie powinno panic na
    // dowolnym byte slice. Malformed data MUSI zwrocic Err, nie unwind.
    let _ = rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(data);
});
