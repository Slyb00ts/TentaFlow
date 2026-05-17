// =============================================================================
// File: tests/addon_signature_install.rs
// Purpose: Integration tests for F1c P2 — Ed25519 signature verify wired
//          into `addon::lifecycle::install`. Builds a complete addon
//          directory on disk (manifest + ui bundle + dummy WASM), signs
//          the bundle with a test key, and asserts:
//            * trusted publisher + valid sig  -> install OK, audit row written
//            * trusted publisher + wrong sig  -> install rejected
//            * untrusted publisher            -> install rejected
//            * ui_component without publisher -> manifest parser rejects
//              (cross-section validation, no install attempted)
// =============================================================================

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use std::path::Path;
use tempfile::TempDir;
use tentaflow_core::addon::lifecycle::{install, parse_manifest_toml};
use tentaflow_core::db::{self, repository, DbPool};

fn keypair(tag: u8) -> SigningKey {
    let mut seed = [0u8; 32];
    for (i, b) in seed.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(0x9d).wrapping_add(tag);
    }
    SigningKey::from_bytes(&seed)
}

fn pk_b64(sk: &SigningKey) -> String {
    base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes())
}

fn sign_bundle(sk: &SigningKey, bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let sig = sk.sign(digest.as_slice());
    base64::engine::general_purpose::STANDARD.encode(sig.to_bytes())
}

fn open_pool() -> (TempDir, DbPool) {
    let dir = TempDir::new().expect("tempdir db");
    let path = dir.path().join("core.db");
    let pool = db::init(&path).expect("init db");
    (dir, pool)
}

fn trust(pool: &DbPool, pk: &str, label: &str) {
    repository::insert_trusted_publisher(pool, pk, label, None, None)
        .expect("insert trust row");
}

/// Builds an addon directory tree:
///   <root>/manifest.toml
///   <root>/addon.wasm     (empty file ok — install only checks existence + size)
///   <root>/ui/panel.js    (the signed bundle)
fn build_addon_dir(
    bundle_bytes: &[u8],
    addon_id: &str,
    publisher_pk_b64: Option<&str>,
    component_signature: Option<&str>,
) -> TempDir {
    let dir = TempDir::new().expect("tempdir addon");
    let root = dir.path();
    std::fs::write(root.join("addon.wasm"), b"\0asm\x01\0\0\0").expect("wasm");
    std::fs::create_dir_all(root.join("ui")).expect("ui dir");
    std::fs::write(root.join("ui/panel.js"), bundle_bytes).expect("bundle");

    let mut toml = String::new();
    toml.push_str(&format!(
        "[addon]\nid = \"{addon_id}\"\nname = \"Test\"\nversion = \"1.0.0\"\nwasm_file = \"addon.wasm\"\n\n"
    ));
    if let Some(pk) = publisher_pk_b64 {
        toml.push_str("[publisher]\n");
        toml.push_str(&format!("ed25519_public_key = \"{pk}\"\n"));
        toml.push_str("label = \"Test Publisher\"\n\n");
    }
    if let Some(sig) = component_signature {
        toml.push_str("[[ui_component]]\n");
        toml.push_str("id = \"panel\"\n");
        toml.push_str("display_name = \"Panel\"\n");
        toml.push_str("slot = \"main\"\n");
        toml.push_str("src = \"ui/panel.js\"\n");
        toml.push_str(&format!("signature = \"ed25519:{sig}\"\n"));
        toml.push_str("risk = \"low\"\n");
    }
    std::fs::write(root.join("manifest.toml"), toml).expect("manifest");
    dir
}

fn count_audit(pool: &DbPool, addon_id: &str, action: &str) -> i64 {
    let conn = pool.lock().expect("lock");
    conn.query_row(
        "SELECT COUNT(*) FROM audit_log WHERE addon_id = ?1 AND action = ?2",
        rusqlite::params![addon_id, action],
        |r| r.get(0),
    )
    .expect("count audit")
}

#[test]
fn install_succeeds_with_trusted_publisher_and_valid_signature() {
    let (_dir, pool) = open_pool();
    let sk = keypair(1);
    let pk = pk_b64(&sk);
    trust(&pool, &pk, "Test Publisher");

    let bundle = b"export const Panel = () => 'hi';";
    let sig = sign_bundle(&sk, bundle);
    let addon = build_addon_dir(bundle, "ok-addon", Some(&pk), Some(&sig));

    install(addon.path(), &pool).expect("install must succeed");

    let conn = pool.lock().expect("lock");
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM addons WHERE addon_id = 'ok-addon'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(n, 1, "addon row must be written");
    drop(conn);

    let audit_ok = count_audit(&pool, "ok-addon", "addon.ui_signature_verify");
    assert_eq!(
        audit_ok, 1,
        "exactly one ui_signature_verify audit row per component"
    );
}

#[test]
fn install_rejected_when_signature_is_invalid() {
    let (_dir, pool) = open_pool();
    let sk_real = keypair(2);
    let sk_attacker = keypair(99);
    let pk = pk_b64(&sk_real);
    trust(&pool, &pk, "Real Publisher");

    let bundle = b"trusted-content";
    // Signature is from a DIFFERENT key — must fail verify.
    let bad_sig = sign_bundle(&sk_attacker, bundle);
    let addon = build_addon_dir(bundle, "bad-sig-addon", Some(&pk), Some(&bad_sig));

    let err = install(addon.path(), &pool).expect_err("install must reject");
    let msg = format!("{err}");
    assert!(
        msg.contains("signature verify failed"),
        "unexpected error: {msg}"
    );

    let conn = pool.lock().expect("lock");
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM addons WHERE addon_id = 'bad-sig-addon'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(n, 0, "addon row MUST NOT be written when signature fails");
    drop(conn);

    let audit_denied = count_audit(&pool, "bad-sig-addon", "addon.ui_signature_verify");
    assert!(
        audit_denied >= 1,
        "denial must produce at least one audit row"
    );
}

#[test]
fn install_rejected_when_publisher_not_in_trust_store() {
    let (_dir, pool) = open_pool();
    let sk = keypair(3);
    let pk = pk_b64(&sk);
    // intentionally NOT trusted

    let bundle = b"any";
    let sig = sign_bundle(&sk, bundle);
    let addon = build_addon_dir(bundle, "untrusted-addon", Some(&pk), Some(&sig));

    let err = install(addon.path(), &pool).expect_err("install must reject");
    let msg = format!("{err}");
    assert!(
        msg.contains("publisher key not in trust store"),
        "unexpected error: {msg}"
    );
}

#[test]
fn manifest_with_ui_component_but_no_publisher_block_is_rejected_at_parse_time() {
    // No publisher + ui_component → cross-section validator inside
    // parse_manifest_toml refuses the manifest, install never starts.
    let bundle = b"x";
    // Use the canonical 86+2 base64 placeholder so the signature format
    // check passes and we hit the publisher-presence rule we want to test.
    let placeholder_sig =
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==";
    let addon = build_addon_dir(bundle, "no-pub-addon", None, Some(placeholder_sig));
    let toml = std::fs::read_to_string(addon.path().join("manifest.toml")).expect("read");
    let err = parse_manifest_toml(&toml).expect_err("parser must reject");
    assert!(
        err.to_string().contains("no [publisher] block"),
        "unexpected error: {err}"
    );
}

#[test]
fn manifest_with_publisher_and_no_ui_components_parses_ok() {
    // Publisher pre-declared but no UI bundles — allowed (addon may add
    // signed UI components in a later version without rotating identity).
    let sk = keypair(7);
    let pk = pk_b64(&sk);
    let toml = format!(
        "[addon]\nid=\"pub-only\"\nname=\"PO\"\nversion=\"1.0.0\"\nwasm_file=\"a.wasm\"\n\n\
         [publisher]\ned25519_public_key=\"{pk}\"\nlabel=\"Future\"\n"
    );
    let m = parse_manifest_toml(&toml).expect("parse ok");
    assert!(m.publisher.is_some());
    assert!(m.ui_components.is_empty());
}

// Sanity: ensure the helper Path import isn't dead.
#[test]
fn _path_import_is_used() {
    let _: &Path = Path::new("/");
}
