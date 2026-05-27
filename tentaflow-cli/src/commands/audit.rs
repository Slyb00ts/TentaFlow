// =============================================================================
// File: tentaflow-cli/src/commands/audit.rs — audit chain verification CLI
// (F1b P4, DoD-15). Walks every row in `audit_log`, recomputes the Merkle
// hash chain, and reports tamper findings. Exit code 0 when the chain is
// clean, 1 when any row fails verification (so operators can wire the
// command into nightly cron / CI checks).
// =============================================================================

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Subcommand;

use tentaflow_core::audit::verify::{verify_chain, TamperKind, VerifyReport};
use tentaflow_core::paths;

#[derive(Subcommand, Debug)]
pub enum AuditCommand {
    /// Verify the Merkle hash chain stored alongside every `audit_log` row.
    /// Reports the total number of rows, how many are chained / legacy /
    /// tampered, and exits non-zero when any tampering is detected.
    Verify {
        /// Explicit path to the sqlite database (defaults to
        /// `<tentaflow_home>/data/router.db`).
        #[arg(long)]
        db_path: Option<PathBuf>,
    },
}

pub fn run(cmd: AuditCommand) -> ExitCode {
    match cmd {
        AuditCommand::Verify { db_path } => match verify(db_path) {
            Ok(report) => {
                print_report(&report);
                if report.is_clean() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                }
            }
            Err(e) => {
                eprintln!("audit verify failed: {e:#}");
                ExitCode::from(2)
            }
        },
    }
}

fn verify(db_path: Option<PathBuf>) -> anyhow::Result<VerifyReport> {
    let db_path = db_path.unwrap_or_else(paths::database_path);
    let pool = tentaflow_core::db::init(&db_path)
        .map_err(|e| anyhow::anyhow!("open db {}: {e}", db_path.display()))?;
    let conn = pool
        .lock()
        .map_err(|e| anyhow::anyhow!("acquire db lock: {e}"))?;
    let report = verify_chain(&conn)
        .map_err(|e| anyhow::anyhow!("verify_chain: {e}"))?;
    Ok(report)
}

fn print_report(report: &VerifyReport) {
    println!("Verified {} audit rows.", report.total);
    println!("  - {} chained ok", report.chained_ok);
    println!("  - {} legacy unchained (pre-P4)", report.legacy_unchained);
    println!("  - {} tampered", report.tampered.len());
    if !report.tampered.is_empty() {
        println!();
        println!("Tampered rows:");
        for t in &report.tampered {
            let reason = match t.kind {
                TamperKind::PrevHashMismatch => "prev_hash mismatch (row inserted/deleted upstream)",
                TamperKind::HashMismatch => "hash mismatch (row content modified after write)",
                TamperKind::NullHashAfterChainStart => "NULL hash after chain start (bypassed writer)",
                TamperKind::MalformedHashBlob => "malformed hash blob (not 32 bytes)",
            };
            println!("  id={} — {}", t.id, reason);
        }
    }
}
