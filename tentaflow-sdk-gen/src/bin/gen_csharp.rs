// =============================================================================
// File: bin/gen_csharp.rs — emit C# SDK source to stdout
//
// Usage:
//   tentaflow-sdk-gen-csharp > Components.g.cs
// =============================================================================

use std::io::Write;
use std::process::ExitCode;

use tentaflow_sdk_gen::build_manifest;
use tentaflow_sdk_gen::gen_csharp;

fn main() -> ExitCode {
    let manifest = build_manifest();
    let code = gen_csharp::generate(&manifest);

    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if let Err(e) = lock.write_all(code.as_bytes()) {
        eprintln!("gen_csharp: stdout write failed: {e}");
        return ExitCode::from(2);
    }
    if let Err(e) = lock.flush() {
        eprintln!("gen_csharp: stdout flush failed: {e}");
        return ExitCode::from(2);
    }
    eprintln!(
        "gen_csharp: emitted {} bytes ({} components, {} enums, {} inline structs, {} tagged unions)",
        code.len(),
        manifest.components.len(),
        manifest.enums.len(),
        manifest.inline_structs.len(),
        manifest.tagged_unions.len(),
    );
    ExitCode::SUCCESS
}
