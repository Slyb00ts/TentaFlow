// =============================================================================
// File: tentaflow-cli/tests/cli_policy.rs
// Purpose: End-to-end tests for `tentaflow-cli policy {issue,list,verify,
//          revoke,show}` against a tempdir SQLite. Verifies admin lifecycle
//          of policy claims used by the F1c P4 gate engine.
// =============================================================================

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use tempfile::TempDir;

fn cli() -> Command {
    Command::cargo_bin("tentaflow-cli").expect("binary built")
}

#[test]
fn policy_issue_creates_claim_and_signatures() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");

    cli()
        .args([
            "policy",
            "issue",
            "--claim-id",
            "claim-dpia-1",
            "--type",
            "dpia",
            "--label",
            "DPIA faces 2026",
            "--valid-until",
            "2030-01-01T00:00:00Z",
            "--signer",
            "dpo:alice",
            "--signer",
            "supervisor:bob",
            "--db",
        ])
        .arg(&db)
        .assert()
        .success()
        .stdout(contains("claim 'claim-dpia-1' issued"))
        .stdout(contains("2 signer"));

    cli()
        .args(["policy", "show", "--claim-id", "claim-dpia-1", "--db"])
        .arg(&db)
        .assert()
        .success()
        .stdout(contains("type          : dpia"))
        .stdout(contains("signers       : 2"))
        .stdout(contains("dpo"))
        .stdout(contains("supervisor"));
}

#[test]
fn policy_issue_rejects_unknown_type() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    cli()
        .args([
            "policy",
            "issue",
            "--claim-id",
            "c1",
            "--type",
            "bogus_type",
            "--label",
            "x",
            "--valid-until",
            "2030-01-01T00:00:00Z",
            "--signer",
            "dpo:a",
            "--db",
        ])
        .arg(&db)
        .assert()
        .failure()
        .stderr(contains("not allowed"));
}

#[test]
fn policy_issue_requires_signer() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    cli()
        .args([
            "policy",
            "issue",
            "--claim-id",
            "c1",
            "--type",
            "dpia",
            "--label",
            "x",
            "--valid-until",
            "2030-01-01T00:00:00Z",
            "--db",
        ])
        .arg(&db)
        .assert()
        .failure()
        .stderr(contains("at least one --signer"));
}

#[test]
fn policy_list_filters_by_type_and_active() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    let issue = |id: &str, t: &str| {
        cli()
            .args([
                "policy",
                "issue",
                "--claim-id",
                id,
                "--type",
                t,
                "--label",
                id,
                "--valid-until",
                "2030-01-01T00:00:00Z",
                "--signer",
                "dpo:a",
                "--db",
            ])
            .arg(&db)
            .assert()
            .success();
    };
    issue("c-dpia", "dpia");
    issue("c-fria", "fria");
    issue("c-consent", "consent");

    cli()
        .args(["policy", "list", "--type", "dpia", "--db"])
        .arg(&db)
        .assert()
        .success()
        .stdout(contains("c-dpia"))
        .stdout(contains("c-fria").not())
        .stdout(contains("c-consent").not());

    cli()
        .args(["policy", "revoke", "--claim-id", "c-dpia", "--reason", "test", "--db"])
        .arg(&db)
        .assert()
        .success();

    cli()
        .args(["policy", "list", "--active-only", "--db"])
        .arg(&db)
        .assert()
        .success()
        .stdout(contains("c-fria"))
        .stdout(contains("c-consent"))
        .stdout(contains("c-dpia").not());
}

#[test]
fn policy_revoke_marks_revoked_with_reason() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    cli()
        .args([
            "policy",
            "issue",
            "--claim-id",
            "c1",
            "--type",
            "dpia",
            "--label",
            "x",
            "--valid-until",
            "2030-01-01T00:00:00Z",
            "--signer",
            "dpo:a",
            "--db",
        ])
        .arg(&db)
        .assert()
        .success();
    cli()
        .args([
            "policy",
            "revoke",
            "--claim-id",
            "c1",
            "--reason",
            "audit failure 2026-05",
            "--db",
        ])
        .arg(&db)
        .assert()
        .success()
        .stdout(contains("revoked"));
    cli()
        .args(["policy", "show", "--claim-id", "c1", "--db"])
        .arg(&db)
        .assert()
        .success()
        .stdout(contains("revoked_reason: audit failure 2026-05"));
}

#[test]
fn policy_verify_ok_for_valid_claim() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    cli()
        .args([
            "policy",
            "issue",
            "--claim-id",
            "c1",
            "--type",
            "dpia",
            "--label",
            "x",
            "--valid-until",
            "2030-01-01T00:00:00Z",
            "--signer",
            "dpo:alice",
            "--db",
        ])
        .arg(&db)
        .assert()
        .success();
    cli()
        .args([
            "policy",
            "verify",
            "--claim-id",
            "c1",
            "--type",
            "dpia",
            "--addon",
            "any-addon",
            "--signer-role",
            "dpo",
            "--db",
        ])
        .arg(&db)
        .assert()
        .success()
        .stdout(contains("OK: claim 'c1' valid"));
}

#[test]
fn policy_verify_invalid_for_revoked_claim() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    cli()
        .args([
            "policy", "issue", "--claim-id", "c1", "--type", "dpia", "--label", "x",
            "--valid-until", "2030-01-01T00:00:00Z", "--signer", "dpo:a", "--db",
        ])
        .arg(&db)
        .assert()
        .success();
    cli()
        .args(["policy", "revoke", "--claim-id", "c1", "--reason", "x", "--db"])
        .arg(&db)
        .assert()
        .success();
    cli()
        .args([
            "policy", "verify", "--claim-id", "c1", "--type", "dpia",
            "--signer-role", "dpo", "--db",
        ])
        .arg(&db)
        .assert()
        .failure()
        .stderr(contains("INVALID [revoked]"));
}
