// =============================================================================
// File: bin/emit_manifest.rs — emit `catalog-manifest/v1.cbor` to stdout
//
// Usage:
//   tentaflow-sdk-gen-emit-manifest > catalog-manifest/v1.cbor
//
// Read by `tentaflow-sdk-gen-self-test` and by SDK generator backends
// (Krok 6: C# / Python). Encoding follows RFC 8949 §4.2.1.
// =============================================================================

use std::io::Write;
use std::process::ExitCode;

use tentaflow_sdk_gen::build_manifest;

fn main() -> ExitCode {
    let manifest = build_manifest();
    let bytes = match minicbor::to_vec(&manifest) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("emit_manifest: encode failed: {e}");
            return ExitCode::from(2);
        }
    };
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if let Err(e) = lock.write_all(&bytes) {
        eprintln!("emit_manifest: stdout write failed: {e}");
        return ExitCode::from(3);
    }
    if let Err(e) = lock.flush() {
        eprintln!("emit_manifest: stdout flush failed: {e}");
        return ExitCode::from(3);
    }
    eprintln!(
        "emit_manifest: wrote {} bytes ({} components, {} enums, {} inline structs)",
        bytes.len(),
        manifest.components.len(),
        manifest.enums.len(),
        manifest.inline_structs.len(),
    );
    ExitCode::SUCCESS
}
