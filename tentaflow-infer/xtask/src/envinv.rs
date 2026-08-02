// ===== File: envinv.rs — inventory of FORGE_* environment variables =====
//
// Input for the configuration contract: every variable is classified by what it
// actually does, because only one of the three classes has to disappear.
// Path switches become forge.toml fields, instrumentation becomes CLI flags,
// test hooks become test attributes.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::scan::{self, Scope};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// Selects an execution path at runtime. This is the class that must reach zero.
    PathSwitch,
    /// Benchmark and profiling instrumentation.
    Instrumentation,
    /// Test-only hook or audit switch.
    TestHook,
}

impl Class {
    fn label(self) -> &'static str {
        match self {
            Class::PathSwitch => "przełącznik ścieżki",
            Class::Instrumentation => "oprzyrządowanie",
            Class::TestHook => "test",
        }
    }

    fn target(self) -> &'static str {
        match self {
            Class::PathSwitch => "forge.toml",
            Class::Instrumentation => "flaga CLI",
            Class::TestHook => "atrybut testu",
        }
    }
}

fn classify(name: &str) -> Class {
    if name.starts_with("FORGE_TEST_") || name.ends_with("_AUDIT") || name.contains("_TEST_") {
        Class::TestHook
    } else if name.starts_with("FORGE_BENCH_")
        || name.starts_with("FORGE_PROFILE_")
        || name.contains("_TRACE")
        || name.contains("_DEBUG")
    {
        Class::Instrumentation
    } else {
        Class::PathSwitch
    }
}

struct Usage {
    class: Class,
    sites: Vec<String>,
}

pub fn run(root: &Path, write_report: bool) -> std::io::Result<()> {
    let mut found: BTreeMap<String, Usage> = BTreeMap::new();

    for file in scan::collect(root, Scope::RustAll) {
        // The inventory describes the engine, not its tooling; xtask mentions
        // the FORGE_ prefixes while classifying them and would otherwise count
        // itself.
        if !file.rel.starts_with("crates/") {
            continue;
        }
        for (idx, line) in file.lines.iter().enumerate() {
            for name in extract_names(line) {
                let entry = found.entry(name.clone()).or_insert_with(|| Usage {
                    class: classify(&name),
                    sites: Vec::new(),
                });
                if entry.sites.len() < 4 {
                    entry.sites.push(format!("{}:{}", file.rel, idx + 1));
                }
            }
        }
    }

    let mut counts = [0usize; 3];
    for usage in found.values() {
        counts[usage.class as usize] += 1;
    }

    println!("Zmienne FORGE_*: {}", found.len());
    println!(
        "  przełączniki ścieżki : {}  <- do zbicia do zera",
        counts[Class::PathSwitch as usize]
    );
    println!(
        "  oprzyrządowanie      : {}",
        counts[Class::Instrumentation as usize]
    );
    println!("  testy                : {}", counts[Class::TestHook as usize]);

    if !write_report {
        return Ok(());
    }

    let mut out = String::new();
    out.push_str("# Inwentarz zmiennych środowiskowych FORGE_*\n\n");
    out.push_str(
        "Dokument **generowany** przez `cargo xtask env-inventory --write`. Nie edytuj ręcznie.\n\n",
    );
    out.push_str(
        "Podstawa pod kontrakt konfiguracji (`PLAN_NAPRAWY.md` §4.5). Klasa rozstrzyga, dokąd\n\
         zmienna trafia: przełącznik ścieżki do `forge.toml`, oprzyrządowanie do flagi CLI,\n\
         hak testowy do atrybutu testu. **Do zera musi zejść wyłącznie pierwsza klasa** — i to\n\
         ona jest mierzona bramką `env` w `cargo xtask lint`.\n\n",
    );
    out.push_str(&format!(
        "| klasa | sztuk | dokąd trafia |\n|---|--:|---|\n\
         | przełącznik ścieżki | **{}** | `forge.toml` |\n\
         | oprzyrządowanie | {} | flaga CLI |\n\
         | test | {} | atrybut testu |\n\
         | **razem** | **{}** | |\n\n",
        counts[Class::PathSwitch as usize],
        counts[Class::Instrumentation as usize],
        counts[Class::TestHook as usize],
        found.len()
    ));

    for class in [Class::PathSwitch, Class::Instrumentation, Class::TestHook] {
        out.push_str(&format!("## {} ({})\n\n", class.label(), class.target()));
        out.push_str("| zmienna | miejsca użycia |\n|---|---|\n");
        for (name, usage) in found.iter().filter(|(_, u)| u.class == class) {
            out.push_str(&format!("| `{}` | {} |\n", name, usage.sites.join("<br>")));
        }
        out.push('\n');
    }

    let path = root.join("docs/INWENTARZ_ENV.md");
    fs::write(&path, out)?;
    println!("Raport: {}", scan::rel_path(&path, root));
    Ok(())
}

/// Pulls FORGE_* identifiers out of a line, whether they appear as a string
/// literal or bare in a macro argument.
fn extract_names(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while let Some(rel) = line[i..].find("FORGE_") {
        let start = i + rel;
        let mut end = start;
        while end < bytes.len() {
            let c = bytes[end] as char;
            if c.is_ascii_alphanumeric() || c == '_' {
                end += 1;
            } else {
                break;
            }
        }
        let name = &line[start..end];
        // A trailing underscore means the literal is a prefix used to build a
        // name, not a variable that anyone reads.
        let complete = name.len() > "FORGE_".len() && !name.ends_with('_');
        if complete && !out.iter().any(|n: &String| n == name) {
            out.push(name.to_string());
        }
        i = end.max(start + 1);
    }
    out
}
