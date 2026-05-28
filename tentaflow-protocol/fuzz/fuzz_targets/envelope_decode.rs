// =============================================================================
// Plik: fuzz_targets/envelope_decode.rs
// Opis: Libfuzzer harness dla Envelope decode. CI gate (#35): 5 min bez crasha.
//       Prowadzony: `cargo +nightly fuzz run envelope_decode -- -max_total_time=300`
// =============================================================================

#![no_main]

use libfuzzer_sys::fuzz_target;
use tentaflow_protocol::Envelope;

fuzz_target!(|data: &[u8]| {
    // Dekoder CBOR nie moze panikowac na dowolnym byte slice.
    let _ = tentaflow_protocol::cbor::decode::<Envelope>(data);
});
