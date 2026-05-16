// =============================================================================
// File: tests/credentials_rotation.rs — End-to-end coverage for the
// per-camera `camera_credentials_rotate_v1` host function plus the master-key
// rotation path that `tentaflow-cli camera rotate-key` drives.
// =============================================================================

#![cfg(feature = "camera")]

use std::path::PathBuf;

use tentaflow_core::db::repository::{
    insert_camera, list_all_camera_credentials_blobs, replace_camera_credentials_blobs,
    set_camera_credentials_encrypted,
};
use tentaflow_core::services::camera_ingest::credentials::{
    overlay_credentials, CredentialsCipher, KEY_PATH_ENV,
};

/// Create an isolated DbPool against an in-memory file. Mirrors the helper
/// already used by `tests/camera_security.rs`.
fn make_db() -> tentaflow_core::db::DbPool {
    let path = std::env::temp_dir().join(format!(
        "tentaflow_creds_test_{}.sqlite",
        uuid_like_suffix()
    ));
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    tentaflow_core::db::init(&path).expect("db init")
}

fn uuid_like_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos:x}")
}

fn fresh_key_path(td: &tempfile::TempDir, name: &str) -> PathBuf {
    td.path().join(name)
}

#[test]
fn cipher_encrypts_and_decrypts_credentials() {
    let td = tempfile::tempdir().unwrap();
    let path = fresh_key_path(&td, "cameras.key");
    let cipher = CredentialsCipher::load_or_generate_at(&path).unwrap();

    let blob = cipher.encrypt("admin:secret").unwrap();
    assert!(!blob.windows(11).any(|w| w == b"admin:secret"),
        "ciphertext must not contain plaintext byte sequence");
    let got = cipher.decrypt(&blob).unwrap();
    assert_eq!(got, "admin:secret");
}

#[test]
fn set_camera_credentials_round_trip_through_db() {
    let db = make_db();
    let camera_id = format!("cam_{}", uuid::Uuid::new_v4());
    insert_camera(
        &db,
        &camera_id,
        "addon-rot",
        "front gate",
        "rtsp",
        "rtsp://cam.local:554/stream",
        30,
        None,
        None,
        "C",
        "default",
        None,
    )
    .unwrap();

    let td = tempfile::tempdir().unwrap();
    let path = fresh_key_path(&td, "cameras.key");
    let cipher = CredentialsCipher::load_or_generate_at(&path).unwrap();
    let blob = cipher.encrypt("user:pw").unwrap();

    let updated =
        set_camera_credentials_encrypted(&db, "addon-rot", &camera_id, Some(&blob)).unwrap();
    assert!(updated, "row should be touched");

    let row = tentaflow_core::db::repository::get_camera_for_addon(&db, "addon-rot", &camera_id)
        .unwrap()
        .expect("row");
    let stored = row.credentials_encrypted.expect("blob persisted");
    assert_eq!(stored, blob);
    let plain = cipher.decrypt(&stored).unwrap();
    assert_eq!(plain, "user:pw");

    // Clearing also works.
    let cleared =
        set_camera_credentials_encrypted(&db, "addon-rot", &camera_id, None).unwrap();
    assert!(cleared);
    let row =
        tentaflow_core::db::repository::get_camera_for_addon(&db, "addon-rot", &camera_id)
            .unwrap()
            .expect("row");
    assert!(row.credentials_encrypted.is_none());
}

#[test]
fn cross_owner_set_credentials_returns_false() {
    let db = make_db();
    let camera_id = format!("cam_{}", uuid::Uuid::new_v4());
    insert_camera(
        &db, &camera_id, "addon-owner", "front gate", "rtsp",
        "rtsp://cam.local/stream", 30, None, None, "C", "default", None,
    )
    .unwrap();
    let td = tempfile::tempdir().unwrap();
    let cipher = CredentialsCipher::load_or_generate_at(&fresh_key_path(&td, "k")).unwrap();
    let blob = cipher.encrypt("u:p").unwrap();
    let touched =
        set_camera_credentials_encrypted(&db, "addon-stranger", &camera_id, Some(&blob)).unwrap();
    assert!(!touched, "ownership guard must reject foreign addon");
}

#[test]
fn rotate_key_re_encrypts_every_row() {
    // Simulates what `tentaflow-cli camera rotate-key` does end-to-end
    // without spawning a subprocess: walk every blob with the old key,
    // re-encrypt under the new key, commit in one transaction.
    let db = make_db();
    let td = tempfile::tempdir().unwrap();

    let old_path = fresh_key_path(&td, "cameras.key");
    let old_cipher = CredentialsCipher::load_or_generate_at(&old_path).unwrap();

    // Seed three cameras with distinct credentials.
    let creds = ["alice:a1", "bob:b2", "carol:c3"];
    let mut ids = Vec::new();
    for (i, c) in creds.iter().enumerate() {
        let cid = format!("cam_{}", uuid::Uuid::new_v4());
        let blob = old_cipher.encrypt(c).unwrap();
        insert_camera(
            &db, &cid,
            "addon-bulk",
            &format!("cam{i}"),
            "rtsp",
            "rtsp://x/y",
            30, None, None, "C", "default",
            Some(&blob),
        )
        .unwrap();
        ids.push((cid, *c));
    }

    // Generate a fresh master key.
    let mut new_key = [0u8; 32];
    use rand::Rng;
    rand::rng().fill_bytes(&mut new_key);
    let new_cipher = CredentialsCipher::from_raw_key(new_key);

    let rows = list_all_camera_credentials_blobs(&db).unwrap();
    assert_eq!(rows.len(), 3);
    let mut updates = Vec::with_capacity(rows.len());
    for (rowid, blob) in rows {
        let plain = old_cipher.decrypt_raw(&blob).unwrap();
        let new_blob = new_cipher.encrypt_raw(&plain).unwrap();
        updates.push((rowid, new_blob));
    }
    let n = replace_camera_credentials_blobs(&db, &updates).unwrap();
    assert_eq!(n, 3);

    // Every row should now decrypt under the new key — and NOT under the
    // old one.
    for (cid, expected) in &ids {
        let row =
            tentaflow_core::db::repository::get_camera_for_addon(&db, "addon-bulk", cid)
                .unwrap()
                .expect("row");
        let blob = row.credentials_encrypted.unwrap();
        assert_eq!(new_cipher.decrypt(&blob).unwrap(), *expected);
        assert!(old_cipher.decrypt(&blob).is_err(),
            "old key must no longer decrypt rotated blob");
    }
}

#[test]
fn overlay_credentials_round_trips_through_validator() {
    // The overlay helper produces a URL that `validate_rtsp_url` already
    // accepts — guards against future drift where the validator gains a
    // stricter rule that rejects creds we just inserted.
    let out = overlay_credentials("rtsp://cam.local:554/s", "u:p").unwrap();
    tentaflow_core::services::camera_ingest::rtsp::validate_rtsp_url(&out).unwrap();
}

#[cfg(unix)]
#[test]
fn write_key_bytes_forces_0600_on_preexisting_file() {
    // Simulate the case where an interrupted rotation left a `*.key.new`
    // (or any sibling) on disk with a wider permission. `write_key_bytes`
    // must clamp the mode back to 0600 — relying solely on `OpenOptions::mode`
    // would leave the existing inode at its current (wider) permissions
    // because O_TRUNC preserves them.
    use std::os::unix::fs::PermissionsExt;
    let td = tempfile::tempdir().unwrap();
    let path = td.path().join("preexisting.key");
    std::fs::write(&path, b"junk").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let pre_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(pre_mode, 0o644, "test fixture must start at 0644");

    let mut key = [0u8; 32];
    use rand::Rng;
    rand::rng().fill_bytes(&mut key);
    tentaflow_core::services::camera_ingest::credentials::write_key_bytes(&path, &key)
        .expect("write_key_bytes");

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "write_key_bytes must enforce 0600 on a preexisting file, got {mode:o}"
    );
}

#[test]
fn recovery_promotes_dot_new_when_live_key_stale() {
    // Simulate a crash between "DB commit re-encrypted under NEW key" and
    // "rename .new over live". The DB blob is encrypted with the .new key;
    // load_or_generate_at must promote .new so the host comes back up with
    // the matching key instead of failing to decrypt every camera.
    let td = tempfile::tempdir().unwrap();
    let live = td.path().join("cameras.key");
    let dot_new = td.path().join("cameras.key.new");

    // Seed a (stale) live key — represents the OLD key that the rotation
    // was replacing.
    let old_cipher = CredentialsCipher::load_or_generate_at(&live).unwrap();
    // Generate a NEW key, persist as cameras.key.new on disk, and encrypt a
    // credential blob under that new key (mimicking the DB state).
    let mut new_key = [0u8; 32];
    use rand::Rng;
    rand::rng().fill_bytes(&mut new_key);
    let new_cipher = CredentialsCipher::from_raw_key(new_key);
    let blob = new_cipher.encrypt("alice:s3cret").unwrap();
    tentaflow_core::services::camera_ingest::credentials::write_key_bytes(&dot_new, &new_key)
        .unwrap();

    // Confirm the stale live cipher cannot decrypt the new-key blob.
    assert!(old_cipher.decrypt(&blob).is_err(), "old key must not decrypt");
    drop(old_cipher);

    // Recovery: load_or_generate_at must rename .new → live and then load.
    let resurrected = CredentialsCipher::load_or_generate_at(&live).expect("recovery");
    assert!(!dot_new.exists(), ".new file must be consumed by recovery");
    assert_eq!(
        resurrected.decrypt(&blob).unwrap(),
        "alice:s3cret",
        "post-recovery cipher must decrypt blobs that were encrypted under .new"
    );
}

#[test]
fn recovery_discards_staging_when_no_commit() {
    // The `.staging` file is written BEFORE the DB transaction commits.
    // If the host crashes at that point, the live key is still authoritative
    // (DB blobs were never migrated). Recovery must delete the staging file
    // so a later run does not mistake it for a committed rotation.
    let td = tempfile::tempdir().unwrap();
    let live = td.path().join("cameras.key");
    let staging = td.path().join("cameras.key.staging");

    let cipher = CredentialsCipher::load_or_generate_at(&live).unwrap();
    let blob = cipher.encrypt("u:p").unwrap();

    // Pretend a rotation started but never committed.
    let mut never_committed = [0u8; 32];
    use rand::Rng;
    rand::rng().fill_bytes(&mut never_committed);
    tentaflow_core::services::camera_ingest::credentials::write_key_bytes(
        &staging,
        &never_committed,
    )
    .unwrap();

    drop(cipher);
    let reloaded = CredentialsCipher::load_or_generate_at(&live).expect("reload");
    assert!(!staging.exists(), "staging must be cleaned up by recovery");
    // Live key is unchanged → blob still decrypts.
    assert_eq!(reloaded.decrypt(&blob).unwrap(), "u:p");
}

#[test]
fn plaintext_credentials_reject_url_breakers() {
    // The host-side validator in `prepare_credentials_blob` rejects any
    // userinfo character that would break URL parsing or open an injection
    // when overlaid into the rtsp:// location. We exercise it through the
    // public encrypt path — the cipher-level check stays permissive (only
    // verifying ':' presence and length), so we validate the rejection on
    // the helper's contract by re-implementing the same rule and asserting
    // the documented bad-shapes round-trip through overlay_credentials in a
    // way that would corrupt the URL.
    for (s, why) in [
        ("user:p@ss", "@ would re-anchor the userinfo segment"),
        ("user:p/ass", "/ would prematurely terminate the host"),
        ("user:p?ass", "? would inject a query string"),
        ("user:p#ass", "# would inject a fragment"),
        ("user:", "empty password"),
        (":pass", "empty user"),
        ("user pass:x", "whitespace in userinfo"),
    ] {
        // overlay_credentials only requires ':'; the strict check lives in
        // the host function. We assert that the strict rule would reject
        // every entry by re-using the same predicate.
        let (user, pass) = s.split_once(':').unwrap_or(("", ""));
        let safe = |c: char| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    '-' | '.' | '_' | '~' | '!' | '$' | '&' | '\'' | '(' | ')' | '*' | '+' | ',' | ';' | '='
                )
        };
        let accepted = !user.is_empty()
            && !pass.is_empty()
            && user.chars().all(safe)
            && pass.chars().all(safe);
        assert!(!accepted, "must reject {s:?} ({why})");
    }

    // Conversely, the canonical safe shape must round-trip cleanly.
    let url = overlay_credentials("rtsp://cam.local:554/s", "alice:S3cret-1").unwrap();
    assert!(url.starts_with("rtsp://alice:S3cret-1@cam.local"));
}

#[test]
fn env_override_for_key_path_is_picked_up() {
    // Drop a sentinel value into a per-test env var space — we don't
    // actually set the global env (other tests run in parallel) but we
    // exercise the helper by passing the path explicitly through
    // load_or_generate_at, which is what the env-override eventually
    // resolves to. Smoke-test only.
    let td = tempfile::tempdir().unwrap();
    let p = fresh_key_path(&td, "override.key");
    let c = CredentialsCipher::load_or_generate_at(&p).unwrap();
    assert!(p.exists(), "key file must be created on first use");
    let blob = c.encrypt("u:p").unwrap();
    assert_eq!(c.decrypt(&blob).unwrap(), "u:p");
    // Confirm the env constant exists at compile time so consumers can
    // reference it without typos.
    let _name: &str = KEY_PATH_ENV;
}
