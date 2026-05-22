// =============================================================================
// File: bin/self_test.rs — verify a `catalog-manifest/v1.cbor` payload
//
// Reads a manifest from stdin (or `--file <path>`), decodes it as a
// `ManifestEnvelope`, then checks the in-process `tentaflow-sdk-spec`
// registry produces an equivalent manifest. Exits 0 on success.
//
// Full invariant validation (wire-grammar / target resolution / handler
// consistency) lands in chunk 2d.
// =============================================================================

use std::io::Read;
use std::process::ExitCode;

use tentaflow_sdk_gen::{build_manifest, ManifestEnvelope};

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
    let expected = build_manifest();
    if decoded != expected {
        eprintln!(
            "self_test: manifest payload diverges from in-process registry \
            ({} vs {} components, {} vs {} enums, {} vs {} inline structs)",
            decoded.components.len(), expected.components.len(),
            decoded.enums.len(), expected.enums.len(),
            decoded.inline_structs.len(), expected.inline_structs.len(),
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
        "self_test: OK ({} components, {} enums, {} inline structs, {} bytes, byte-canonical)",
        decoded.components.len(), decoded.enums.len(),
        decoded.inline_structs.len(), bytes.len(),
    );
    ExitCode::SUCCESS
}
