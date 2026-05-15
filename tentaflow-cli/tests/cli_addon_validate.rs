// =============================================================================
// Plik: tentaflow-cli/tests/cli_addon_validate.rs
// Opis: Testy end-to-end komendy `tentaflow-cli addon validate`. Sprawdza
//       exit code i kluczowe fragmenty raportu na 4 manifestach:
//       test-app-addon (OK), teams-bot (OK), broken_duplicate_alias (FAIL),
//       broken_invalid_signature (FAIL).
// =============================================================================

use assert_cmd::Command;
use predicates::str::contains;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR -> tentaflow-cli; .. -> root repo
    let cli_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    cli_dir
        .parent()
        .expect("repo root")
        .to_path_buf()
}

fn cli() -> Command {
    Command::cargo_bin("tentaflow-cli").expect("binary built")
}

#[test]
fn validate_test_app_addon_ok() {
    let path = repo_root().join("tentaflow-core/addons/test-app-addon");
    cli()
        .arg("addon")
        .arg("validate")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("Manifest wczytany: test-app-addon"))
        .stdout(contains("manifest poprawny"));
}

#[test]
fn validate_teams_bot_addon_ok() {
    let path = repo_root().join("tentaflow-core/addons-pro/teams-bot");
    cli()
        .arg("addon")
        .arg("validate")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("Manifest wczytany: teams-bot"));
}

#[test]
fn validate_duplicate_alias_fails() {
    let path = repo_root()
        .join("tentaflow-core/tests/fixtures/broken_manifest_duplicate_alias.toml");
    cli()
        .arg("addon")
        .arg("validate")
        .arg(&path)
        .assert()
        .failure()
        .stdout(contains("Duplicate alias id: tentavision-yolo"));
}

#[test]
fn validate_invalid_signature_fails() {
    let path = repo_root()
        .join("tentaflow-core/tests/fixtures/broken_manifest_invalid_signature.toml");
    cli()
        .arg("addon")
        .arg("validate")
        .arg(&path)
        .assert()
        .failure()
        .stdout(contains("invalid signature format"));
}

#[test]
fn validate_path_traversal_fails() {
    let path = repo_root()
        .join("tentaflow-core/tests/fixtures/broken_manifest_path_traversal.toml");
    cli()
        .arg("addon")
        .arg("validate")
        .arg(&path)
        .assert()
        .failure()
        .stdout(contains("path traversal"));
}

#[test]
fn validate_nonexistent_path_fails() {
    cli()
        .arg("addon")
        .arg("validate")
        .arg("/nonexistent/path/xyz123")
        .assert()
        .failure();
}
