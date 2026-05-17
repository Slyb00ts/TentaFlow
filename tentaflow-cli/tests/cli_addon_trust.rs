// =============================================================================
// Plik: tentaflow-cli/tests/cli_addon_trust.rs
// Opis: End-to-end testy podkomend `tentaflow-cli addon trust-key /
//       list-trusted / untrust-key / verify-bundle` (F1c P2). Operuja na
//       tempowej bazie SQLite, weryfikuja exit code i kluczowe fragmenty
//       stdout/stderr.
// =============================================================================

use assert_cmd::Command;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use predicates::str::contains;
use sha2::{Digest, Sha256};
use std::io::Write;
use tempfile::{NamedTempFile, TempDir};

fn cli() -> Command {
    Command::cargo_bin("tentaflow-cli").expect("binary built")
}

fn keypair() -> SigningKey {
    let mut seed = [0u8; 32];
    for (i, b) in seed.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(0x37).wrapping_add(11);
    }
    SigningKey::from_bytes(&seed)
}

fn pk_b64(sk: &SigningKey) -> String {
    base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes())
}

fn sign(sk: &SigningKey, bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let sig = sk.sign(h.finalize().as_slice());
    base64::engine::general_purpose::STANDARD.encode(sig.to_bytes())
}

#[test]
fn trust_then_list_then_untrust_round_trip() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("t.db");
    let sk = keypair();
    let pk = pk_b64(&sk);

    cli()
        .args(["addon", "trust-key", &pk, "--label", "ACME Inc", "--db"])
        .arg(&db_path)
        .assert()
        .success()
        .stdout(contains("klucz dodany"));

    cli()
        .args(["addon", "list-trusted", "--db"])
        .arg(&db_path)
        .assert()
        .success()
        .stdout(contains("ACME Inc"))
        .stdout(contains(pk.as_str()));

    cli()
        .args(["addon", "untrust-key", &pk, "--db"])
        .arg(&db_path)
        .assert()
        .success()
        .stdout(contains("klucz usuniety"));

    cli()
        .args(["addon", "list-trusted", "--db"])
        .arg(&db_path)
        .assert()
        .success()
        .stdout(contains("trust store pusty"));
}

#[test]
fn trust_key_rejects_bad_base64_length() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("t.db");
    cli()
        .args(["addon", "trust-key", "too-short", "--label", "X", "--db"])
        .arg(&db_path)
        .assert()
        .failure()
        .stderr(contains("invalid length"));
}

#[test]
fn verify_bundle_happy_path() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("t.db");
    let sk = keypair();
    let pk = pk_b64(&sk);
    cli()
        .args(["addon", "trust-key", &pk, "--label", "Pub", "--db"])
        .arg(&db_path)
        .assert()
        .success();

    let bundle = b"console.log('verified');";
    let sig = sign(&sk, bundle);
    let mut f = NamedTempFile::new().expect("tmp");
    f.write_all(bundle).expect("w");
    f.flush().expect("flush");

    cli()
        .args(["addon", "verify-bundle"])
        .arg(f.path())
        .args(["--publisher-key", &pk, "--signature", &sig, "--db"])
        .arg(&db_path)
        .assert()
        .success()
        .stdout(contains("signature zweryfikowana"));
}

#[test]
fn verify_bundle_rejects_untrusted_publisher() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("t.db");
    // Open the DB once so migrations run and the trust store exists empty.
    cli()
        .args(["addon", "list-trusted", "--db"])
        .arg(&db_path)
        .assert()
        .success();

    let sk = keypair();
    let pk = pk_b64(&sk);
    let bundle = b"x";
    let sig = sign(&sk, bundle);
    let mut f = NamedTempFile::new().expect("tmp");
    f.write_all(bundle).expect("w");
    f.flush().expect("flush");

    cli()
        .args(["addon", "verify-bundle"])
        .arg(f.path())
        .args(["--publisher-key", &pk, "--signature", &sig, "--db"])
        .arg(&db_path)
        .assert()
        .failure()
        .stderr(contains("publisher key not in trust store"));
}
