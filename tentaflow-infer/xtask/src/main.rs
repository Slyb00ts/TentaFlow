// ===== File: main.rs — xtask entry point: mechanical repository gates =====
//
// Usage:
//   cargo xtask lint                      report violations, non-zero exit on failure
//   cargo xtask lint --update-baseline    rewrite the allowance list from today's state
//   cargo xtask env-inventory [--write]   classify FORGE_* variables, optionally write the report

mod baseline;
mod envinv;
mod rules;
mod scan;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use scan::{Scope, SourceFile};

fn main() -> ExitCode {
    let root = repo_root();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flags: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

    match flags.first().copied() {
        Some("lint") => lint(&root, flags.contains(&"--update-baseline")),
        Some("env-inventory") => match envinv::run(&root, flags.contains(&"--write")) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("env-inventory: {err}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: cargo xtask <lint|env-inventory> [flags]");
            ExitCode::FAILURE
        }
    }
}

/// The workspace root is the directory holding this crate's parent manifest.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

struct Violation {
    rule: &'static str,
    path: String,
    metric: usize,
    allowed: usize,
    unit: &'static str,
    samples: Vec<(usize, String)>,
}

struct Stale {
    rule: &'static str,
    path: String,
    metric: usize,
    allowed: usize,
}

fn lint(root: &Path, update: bool) -> ExitCode {
    let base = baseline::load(root);
    let mut sources: BTreeMap<usize, Vec<SourceFile>> = BTreeMap::new();
    for scope in [Scope::RustAll, Scope::RustSrc, Scope::Mojo, Scope::Manifest] {
        sources.insert(scope as usize, scan::collect(root, scope));
    }

    let mut violations: Vec<Violation> = Vec::new();
    let mut stale: Vec<Stale> = Vec::new();
    let mut fresh = baseline::Baseline::new();
    let mut scanned = 0usize;

    for rule in rules::RULES {
        for scope in rule.scopes {
            let files = &sources[&(*scope as usize)];
            for file in files {
                scanned += 1;
                let measured = (rule.eval)(file);
                let allowed =
                    baseline::allowance(&base, rule.id, &file.rel, rule.default_allowance);

                if measured.metric > allowed {
                    violations.push(Violation {
                        rule: rule.id,
                        path: file.rel.clone(),
                        metric: measured.metric,
                        allowed,
                        unit: rule.unit,
                        samples: measured.samples,
                    });
                } else if measured.metric < allowed && allowed > rule.default_allowance {
                    stale.push(Stale {
                        rule: rule.id,
                        path: file.rel.clone(),
                        metric: measured.metric,
                        allowed,
                    });
                }

                if measured.metric > rule.default_allowance {
                    fresh.insert((rule.id.to_string(), file.rel.clone()), measured.metric);
                }
            }
        }
    }

    if update {
        if let Err(err) = baseline::write(root, &fresh) {
            eprintln!("nie udało się zapisać {}: {err}", baseline::FILE);
            return ExitCode::FAILURE;
        }
        println!(
            "Zapisano {} ({} wpisów) z {} sprawdzonych plikorekordów.",
            baseline::FILE,
            fresh.len(),
            scanned
        );
        summary(&fresh);
        return ExitCode::SUCCESS;
    }

    report(&violations, &stale, &fresh, scanned)
}

fn report(
    violations: &[Violation],
    stale: &[Stale],
    fresh: &baseline::Baseline,
    scanned: usize,
) -> ExitCode {
    for rule in rules::RULES {
        let hits: Vec<&Violation> = violations.iter().filter(|v| v.rule == rule.id).collect();
        if hits.is_empty() {
            continue;
        }
        println!("\n=== {} [{}] ===", rule.title, rule.id);
        println!("    dlaczego: {}", rule.why);
        if rule.review_only {
            println!(
                "    UWAGA: reguła heurystyczna — wynik wymaga przeglądu, nie jest dowodem usterki"
            );
        }
        for v in hits.iter().take(25) {
            println!(
                "  {} — {} {} (dozwolone {})",
                v.path, v.metric, v.unit, v.allowed
            );
            for (line, text) in &v.samples {
                println!("      {}:{}  {}", v.path, line, text);
            }
        }
        if hits.len() > 25 {
            println!("  ... i {} więcej", hits.len() - 25);
        }
    }

    if !stale.is_empty() {
        println!("\n=== Nieaktualne wpisy baseline (poprawa bez zaciśnięcia listy) ===");
        for s in stale.iter().take(25) {
            println!(
                "  {} [{}] — jest {}, lista dopuszcza {}",
                s.path, s.rule, s.metric, s.allowed
            );
        }
        println!("  Napraw: cargo xtask lint --update-baseline");
    }

    summary(fresh);
    println!("\nSprawdzono {scanned} par (reguła, plik).");

    if violations.is_empty() && stale.is_empty() {
        println!("Wszystkie bramki zielone.");
        return ExitCode::SUCCESS;
    }
    println!(
        "\nNARUSZENIA: {}  NIEAKTUALNE WPISY: {}",
        violations.len(),
        stale.len()
    );
    ExitCode::FAILURE
}

/// Current debt per rule. This is the number that has to fall over time, so it
/// is printed on every run rather than hidden behind a flag.
fn summary(fresh: &baseline::Baseline) {
    println!("\n=== Dług na dziś (wpisów na regułę) ===");
    for rule in rules::RULES {
        let count = fresh.keys().filter(|(r, _)| r == rule.id).count();
        let total: usize = fresh
            .iter()
            .filter(|((r, _), _)| r == rule.id)
            .map(|(_, v)| *v)
            .sum();
        println!(
            "  {:<18} plików: {:>4}   suma metryki: {:>7}",
            rule.id, count, total
        );
    }
}
