// ===== File: build.rs — Dynamic kernel artifact embedding per available architecture =====
//
// For each GPU architecture with a complete manifest (all referenced artifacts exist),
// generates Rust code to embed those artifacts. Missing architectures are silently omitted
// and runtime falls back to FORGE_KERNEL_DIR or returns an error.
//
// This allows developers on different rigs to build binaries with only their own GPU's
// artifacts embedded, while the git repo accumulates artifacts from all machines.

use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=../../kernels/mojo/build");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let out_path = Path::new(&out_dir).join("embedded_artifacts.rs");

    let kernel_build_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../kernels/mojo/build");

    let architectures = vec!["sm_89", "sm_121a", "gfx1030", "gfx1100", "gfx1201"];
    let mut generated_code = String::new();
    let mut embedded_sets_entries = Vec::new();

    // For each architecture, check completeness and generate code
    for arch in &architectures {
        let arch_dir = kernel_build_dir.join(arch);
        let manifest_path = arch_dir.join("manifest.json");

        if !manifest_path.exists() {
            eprintln!(
                "warning: manifest not found for {}: {}",
                arch,
                manifest_path.display()
            );
            continue;
        }

        let manifest_str = match fs::read_to_string(&manifest_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("warning: failed to read manifest for {}: {}", arch, e);
                continue;
            }
        };

        // Parse manifest to get the list of kernels
        let manifest: serde_json::Value = match serde_json::from_str(&manifest_str) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("warning: failed to parse manifest for {}: {}", arch, e);
                continue;
            }
        };

        // Check if all referenced artifacts exist
        let kernels = match manifest.get("kernels") {
            Some(serde_json::Value::Object(k)) => k,
            _ => {
                eprintln!(
                    "warning: malformed manifest for {}: no kernels object",
                    arch
                );
                continue;
            }
        };

        let mut all_exist = true;
        let mut kernel_files = Vec::new();

        for (kernel_name, kernel_entry) in kernels {
            let file_name = match kernel_entry.get("file") {
                Some(serde_json::Value::String(f)) => f.clone(),
                _ => {
                    eprintln!(
                        "warning: kernel {} in {} has no file field",
                        kernel_name, arch
                    );
                    all_exist = false;
                    break;
                }
            };

            let artifact_path = arch_dir.join(&file_name);
            if !artifact_path.exists() {
                eprintln!(
                    "warning: artifact missing for {} in {}: {}",
                    kernel_name,
                    arch,
                    artifact_path.display()
                );
                all_exist = false;
                break;
            }
            kernel_files.push(file_name);
        }

        // Only embed if all artifacts exist
        if !all_exist {
            eprintln!(
                "info: skipping architecture {} — incomplete artifact set",
                arch
            );
            continue;
        }

        // Determine file extension based on architecture
        let ext = if arch.starts_with("sm_") {
            ".ptx"
        } else {
            ".hsaco"
        };

        // Generate the include_bytes! calls and const definition
        let const_name = format!("EMBEDDED_{}", arch.to_uppercase().replace(".", "_"));
        let manifest_const = format!(
            "EMBEDDED_MANIFEST_{}",
            arch.to_uppercase().replace(".", "_")
        );

        generated_code.push_str(&format!(
            r#"
const {}: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../kernels/mojo/build/{}/manifest.json"
));

const {}: &[EmbeddedArtifact] = embedded_arch![
    "{}",
    "{}",
"#,
            manifest_const, arch, const_name, arch, ext
        ));

        // Add all kernel names to the embedded_arch! macro
        for kernel_name in &kernel_files {
            generated_code.push_str(&format!("    \"{}\",\n", kernel_name.replace(ext, "")));
        }

        generated_code.push_str("];\n\n");

        // Record this architecture for EMBEDDED_SETS
        embedded_sets_entries.push((arch.to_string(), manifest_const, const_name));
    }

    // Generate EMBEDDED_SETS array
    generated_code.push_str("\nconst EMBEDDED_SETS: &[EmbeddedSet] = &[\n");
    for (arch, manifest_const, const_name) in embedded_sets_entries {
        let set_name = format!("EMBEDDED_{}", arch.to_uppercase().replace(".", "_"));
        generated_code.push_str(&format!(
            "    EmbeddedSet {{\n        arch: \"{}\",\n        manifest: {},\n        artifacts: {},\n        name: \"{}\",\n    }},\n",
            arch, manifest_const, const_name, set_name
        ));
    }
    generated_code.push_str("];\n");

    // Write generated code
    fs::write(&out_path, generated_code).expect("failed to write embedded_artifacts.rs");
}
