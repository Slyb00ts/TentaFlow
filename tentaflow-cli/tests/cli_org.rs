// =============================================================================
// File: tentaflow-cli/tests/cli_org.rs
// Purpose: End-to-end coverage for `tentaflow-cli org {create,list,show,
//          invite,remove,members,delete}` + `tentaflow-cli user assign-role`
//          against a tempdir SQLite. Verifies the F2 P1.c admin surface:
//          slug-conflict exit code 2, role validation, idempotent invite /
//          remove, members listing, and delete safety guard.
// =============================================================================

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

fn cli() -> Command {
    Command::cargo_bin("tentaflow-cli").expect("binary built")
}

fn create_org(db: &std::path::Path, name: &str, slug: &str) {
    cli()
        .args(["org", "create", name, "--slug", slug, "--db"])
        .arg(db)
        .assert()
        .success()
        .stdout(contains("created"));
}

#[test]
fn create_org_roundtrip() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    create_org(&db, "Acme Corp", "acme");
    // `list` must surface the new org alongside the default seed row.
    cli()
        .args(["org", "list", "--db"])
        .arg(&db)
        .assert()
        .success()
        .stdout(contains("acme"))
        .stdout(contains("Acme Corp"))
        .stdout(contains("default"));
}

#[test]
fn create_org_slug_conflict_exit_code_2() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    create_org(&db, "First", "shared-slug");
    cli()
        .args(["org", "create", "Second", "--slug", "shared-slug", "--db"])
        .arg(&db)
        .assert()
        .failure()
        .code(2)
        .stderr(contains("already in use"));
}

#[test]
fn create_org_rejects_reserved_default_slug() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    cli()
        .args(["org", "create", "X", "--slug", "default", "--db"])
        .arg(&db)
        .assert()
        .failure()
        .code(2)
        .stderr(contains("reserved"));
}

#[test]
fn list_orgs_filters_by_status() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    create_org(&db, "Alpha", "alpha");
    create_org(&db, "Beta", "beta");
    // Soft-delete one.
    let beta_id = find_org_id(&db, "beta");
    cli()
        .args(["org", "delete", &beta_id, "--db"])
        .arg(&db)
        .assert()
        .success();
    cli()
        .args(["org", "list", "--status", "deleted", "--db"])
        .arg(&db)
        .assert()
        .success()
        .stdout(contains("beta"))
        .stdout(contains("Beta"));
    // The active list must NOT contain beta.
    let active_out = cli()
        .args(["org", "list", "--status", "active", "--db"])
        .arg(&db)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&active_out.stdout);
    assert!(stdout.contains("alpha"), "active list should include alpha");
    assert!(!stdout.contains(" beta "), "active list must not include beta row");
}

#[test]
fn invite_member_creates_membership() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    create_org(&db, "Org", "org-1");
    let org_id = find_org_id(&db, "org-1");
    cli()
        .args(["org", "invite", &org_id, "user-42", "--role", "org_admin", "--db"])
        .arg(&db)
        .assert()
        .success()
        .stdout(contains("invited"))
        .stdout(contains("org_admin"));
    cli()
        .args(["org", "members", &org_id, "--db"])
        .arg(&db)
        .assert()
        .success()
        .stdout(contains("user-42"))
        .stdout(contains("org_admin"));
}

#[test]
fn invite_member_idempotent_returns_zero() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    create_org(&db, "Org", "org-i");
    let org_id = find_org_id(&db, "org-i");
    let args = ["org", "invite", &org_id, "u-1", "--role", "org_viewer", "--db"];
    cli().args(args).arg(&db).assert().success();
    cli()
        .args(args)
        .arg(&db)
        .assert()
        .success()
        .stdout(contains("already a member"));
}

#[test]
fn invite_member_unknown_role_exit_1() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    create_org(&db, "Org", "org-r");
    let org_id = find_org_id(&db, "org-r");
    cli()
        .args(["org", "invite", &org_id, "u", "--role", "evil_admin", "--db"])
        .arg(&db)
        .assert()
        .failure()
        .code(1)
        .stderr(contains("unknown role"));
}

#[test]
fn invite_member_unknown_org_exit_1() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    // Ensure the DB exists / migrations ran by creating one org first.
    create_org(&db, "Org", "org-b");
    cli()
        .args(["org", "invite", "ghost-org", "u", "--role", "org_admin", "--db"])
        .arg(&db)
        .assert()
        .failure()
        .code(1)
        .stderr(contains("not found"));
}

#[test]
fn remove_member_returns_zero_even_when_absent() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    create_org(&db, "Org", "org-rm");
    let org_id = find_org_id(&db, "org-rm");
    cli()
        .args(["org", "remove", &org_id, "ghost-user", "--db"])
        .arg(&db)
        .assert()
        .success()
        .stdout(contains("was not a member"));
}

#[test]
fn members_list_shows_role_per_user() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    create_org(&db, "Org", "org-m");
    let org_id = find_org_id(&db, "org-m");
    cli()
        .args(["org", "invite", &org_id, "alice", "--role", "org_admin", "--db"])
        .arg(&db)
        .assert()
        .success();
    cli()
        .args(["org", "invite", &org_id, "bob", "--role", "org_viewer", "--db"])
        .arg(&db)
        .assert()
        .success();
    let out = cli()
        .args(["org", "members", &org_id, "--db"])
        .arg(&db)
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("alice"));
    assert!(s.contains("org_admin"));
    assert!(s.contains("bob"));
    assert!(s.contains("org_viewer"));
}

#[test]
fn delete_org_blocks_when_members_present_unless_force() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    create_org(&db, "Org", "org-d");
    let org_id = find_org_id(&db, "org-d");
    cli()
        .args(["org", "invite", &org_id, "u-1", "--role", "org_admin", "--db"])
        .arg(&db)
        .assert()
        .success();
    cli()
        .args(["org", "delete", &org_id, "--db"])
        .arg(&db)
        .assert()
        .failure()
        .code(2)
        .stderr(contains("member"));
    cli()
        .args(["org", "delete", &org_id, "--force", "--db"])
        .arg(&db)
        .assert()
        .success()
        .stdout(contains("soft-deleted"));
}

#[test]
fn delete_default_org_refused() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    // Touch DB so migrations run (and the default seed is present).
    create_org(&db, "X", "x");
    cli()
        .args(["org", "delete", "org-default", "--db"])
        .arg(&db)
        .assert()
        .failure()
        .code(2)
        .stderr(contains("default seed"));
}

#[test]
fn user_assign_role_replaces_existing_membership() {
    let d = TempDir::new().unwrap();
    let db = d.path().join("t.db");
    create_org(&db, "Org", "org-ar");
    let org_id = find_org_id(&db, "org-ar");
    cli()
        .args(["org", "invite", &org_id, "carol", "--role", "org_viewer", "--db"])
        .arg(&db)
        .assert()
        .success();
    cli()
        .args(["user", "assign-role", "carol", &org_id, "--role", "org_admin", "--db"])
        .arg(&db)
        .assert()
        .success()
        .stdout(contains("assigned role"));
    let out = cli()
        .args(["org", "members", &org_id, "--db"])
        .arg(&db)
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("carol"));
    assert!(s.contains("org_admin"));
    assert!(
        !s.contains("org_viewer"),
        "replace semantics: old role must be gone"
    );
}

// Helpers ====================================================================

/// Parse the org list output to find the org_id for a given slug. Used because
/// `create` returns the UUID but tests build the slug deterministically, so
/// this rehydrates the id -> slug mapping out of `org list` plain-text.
fn find_org_id(db: &std::path::Path, slug: &str) -> String {
    let out = cli()
        .args(["org", "list", "--db"])
        .arg(db)
        .output()
        .expect("org list");
    assert!(out.status.success(), "org list must succeed");
    let s = String::from_utf8_lossy(&out.stdout);
    // Skip header, look for first row whose second column equals slug.
    for line in s.lines().skip(1) {
        let mut parts = line.split_whitespace();
        let (Some(id), Some(found_slug)) = (parts.next(), parts.next()) else {
            continue;
        };
        if found_slug == slug {
            return id.to_string();
        }
    }
    panic!("slug '{slug}' not found in org list output:\n{s}");
}
