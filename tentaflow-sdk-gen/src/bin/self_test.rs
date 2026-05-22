// =============================================================================
// File: bin/self_test.rs — verify a `catalog-manifest/v1.cbor` payload
//
// Reads a manifest from stdin (or `--file <path>`), decodes it as a
// `ManifestEnvelope`, then runs three checks:
//   1) standalone invariant validation (no registry — wire grammar +
//      ComponentRef / Enum / Inline target resolution + uniqueness),
//   2) equivalence vs the in-process registry,
//   3) byte-canonical re-encoding round-trip.
//
// Exit codes:
//   0 OK
//   2 read input failed
//   3 decode failed
//   4 registry mismatch
//   5 re-encode failed
//   6 not byte-canonical
//   7 standalone validation failed
// =============================================================================

use std::io::Read;
use std::process::ExitCode;

use tentaflow_sdk_gen::{build_manifest, validate_manifest, ManifestEnvelope};

fn read_input() -> std::io::Result<Vec<u8>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "--file" {
        return std::fs::read(&args[2]);
    }
    let mut buf = Vec::new();
    std::io::stdin().lock().read_to_end(&mut buf)?;
    Ok(buf)
}

fn main() -> ExitCode {
    let bytes = match read_input() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("self_test: read input failed: {e}");
            return ExitCode::from(2);
        }
    };
    let decoded: ManifestEnvelope = match minicbor::decode(&bytes) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("self_test: decode failed: {e}");
            return ExitCode::from(3);
        }
    };
    // 1) Standalone validation: simulates a downstream consumer that has
    //    only the manifest bytes (C# / Python generators in Krok 6).
    let report = match validate_manifest(&decoded) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("self_test: standalone validation failed: {e}");
            return ExitCode::from(7);
        }
    };
    let expected = build_manifest();
    if decoded != expected {
        eprintln!(
            "self_test: manifest payload diverges from in-process registry \
            ({} vs {} components, {} vs {} enums, {} vs {} inline structs, \
            {} vs {} tagged unions)",
            decoded.components.len(), expected.components.len(),
            decoded.enums.len(), expected.enums.len(),
            decoded.inline_structs.len(), expected.inline_structs.len(),
            decoded.tagged_unions.len(), expected.tagged_unions.len(),
        );
        return ExitCode::from(4);
    }
    // Byte-canonical check: re-encoding the in-process registry must produce
    // the exact input bytes. This catches non-canonical re-orderings or width
    // variations that decode-then-compare wouldn't notice.
    let re_encoded = match minicbor::to_vec(&expected) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("self_test: re-encode failed: {e}");
            return ExitCode::from(5);
        }
    };
    if re_encoded != bytes {
        eprintln!(
            "self_test: manifest bytes are NOT canonical \
            (input {} bytes, expected {} bytes after re-encode)",
            bytes.len(), re_encoded.len(),
        );
        return ExitCode::from(6);
    }
    eprintln!(
        "self_test: OK ({} components, {} enums, {} inline structs, {} tagged unions, \
        {} variants, {} bytes, byte-canonical, {} component fields + {} inline fields \
        + {} variant fields validated)",
        decoded.components.len(), decoded.enums.len(),
        decoded.inline_structs.len(), decoded.tagged_unions.len(),
        report.variants, bytes.len(),
        report.component_fields, report.inline_fields, report.variant_fields,
    );
    ExitCode::SUCCESS
}
