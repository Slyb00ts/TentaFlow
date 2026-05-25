// =============================================================================
// File: bin/gen_python.rs — emit Python SDK source to stdout
//
// Usage:
//   tentaflow-sdk-gen-python > components.py
// =============================================================================

use std::io::Write;
use std::process::ExitCode;

use tentaflow_sdk_gen::build_manifest;
use tentaflow_sdk_gen::gen_python;

fn main() -> ExitCode {
    let manifest = build_manifest();
    let code = gen_python::generate(&manifest);

    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if let Err(e) = lock.write_all(code.as_bytes()) {
        eprintln!("gen_python: stdout write failed: {e}");
        return ExitCode::from(2);
    }
    if let Err(e) = lock.flush() {
        eprintln!("gen_python: stdout flush failed: {e}");
        return ExitCode::from(2);
    }
    eprintln!(
        "gen_python: emitted {} bytes ({} components, {} enums, {} inline structs, {} tagged unions)",
        code.len(),
        manifest.components.len(),
        manifest.enums.len(),
        manifest.inline_structs.len(),
        manifest.tagged_unions.len(),
    );
    ExitCode::SUCCESS
}
