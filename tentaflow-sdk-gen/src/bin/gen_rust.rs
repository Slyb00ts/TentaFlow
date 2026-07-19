// =============================================================================
// File: bin/gen_rust.rs — emit Rust addon-SDK UI module source to stdout
//
// Usage:
//   tentaflow-sdk-gen-rust > components_g.rs
// =============================================================================

use std::io::Write;
use std::process::ExitCode;

use tentaflow_sdk_gen::build_manifest;
use tentaflow_sdk_gen::gen_rust;

fn main() -> ExitCode {
    let manifest = build_manifest();
    let code = gen_rust::generate(&manifest);

    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if let Err(e) = lock.write_all(code.as_bytes()) {
        eprintln!("gen_rust: stdout write failed: {e}");
        return ExitCode::from(2);
    }
    if let Err(e) = lock.flush() {
        eprintln!("gen_rust: stdout flush failed: {e}");
        return ExitCode::from(2);
    }
    eprintln!(
        "gen_rust: emitted {} bytes ({} components, {} enums, {} inline structs, {} tagged unions)",
        code.len(),
        manifest.components.len(),
        manifest.enums.len(),
        manifest.inline_structs.len(),
        manifest.tagged_unions.len(),
    );
    ExitCode::SUCCESS
}
