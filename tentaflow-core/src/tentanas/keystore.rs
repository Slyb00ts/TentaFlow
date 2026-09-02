// =============================================================================
// File: tentanas/keystore.rs — the encryption keys of native-ZFS datasets.
//
// WHY THIS IS NOT IN tentanas.db: uninstalling the app removes the whole
// instance data dir (`lifecycle::uninstall_instance` calls `remove_dir_all`
// on it), and the app database lives inside it. A key stored there would die
// with the uninstall while the encrypted dataset — which the teardown
// deliberately never touches (§5.8) — survives on the disks, permanently
// unreadable. So the keystore is a file OUTSIDE the data dir, under the
// TentaFlow home next to the master key that protects it, and the teardown
// plan lists it as consciously KEPT. Removing it is a separate, explicit act.
//
// Content: 32 random bytes per encryption root, kept as the 64 hex characters
// `keyformat=hex` expects, encrypted with `SettingsCipher::encrypt_bound` and
// bound to the dataset name — a key row moved to another dataset fails to
// decrypt instead of unlocking something it was never meant to. The plaintext
// exists only inside a `Zeroizing` buffer on its way to the helper's stdin.
// =============================================================================

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::crypto::SettingsCipher;

/// 32 bytes: the size `aes-256-gcm` takes with `keyformat=hex`.
const KEY_BYTES: usize = 32;

#[derive(Debug, Default, Serialize, Deserialize)]
struct KeyFile {
    datasets: BTreeMap<String, KeyEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct KeyEntry {
    /// `encb:` ciphertext of the 64 hex characters, bound to the dataset name.
    key_ciphertext: String,
    created_at: String,
}

/// The keystore directory, a sibling of nothing the uninstall wipes.
fn store_dir() -> PathBuf {
    crate::paths::tentaflow_home().join("tentanas-keys")
}

/// One file per instance: two TentaNas instances of different orgs never see
/// each other's keys.
pub fn store_path(addon_id: &str) -> PathBuf {
    let safe: String = addon_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    store_dir().join(format!("{safe}.json"))
}

fn load(addon_id: &str) -> Result<KeyFile> {
    let path = store_path(addon_id);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("tentanas keystore at {} is unreadable", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(KeyFile::default()),
        Err(e) => Err(anyhow!("tentanas keystore at {}: {e}", path.display())),
    }
}

fn save(addon_id: &str, file: &KeyFile) -> Result<()> {
    let path = store_path(addon_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    std::fs::write(&path, serde_json::to_string_pretty(file)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// A fresh key as the 64 hex characters `keyformat=hex` reads from stdin,
/// with the trailing newline the prompt expects.
pub fn generate() -> Zeroizing<Vec<u8>> {
    let mut raw = Zeroizing::new([0u8; KEY_BYTES]);
    getrandom::fill(raw.as_mut()).expect("OS RNG");
    let mut hex = Zeroizing::new(Vec::with_capacity(KEY_BYTES * 2 + 1));
    for byte in raw.iter() {
        hex.push(HEX[(byte >> 4) as usize]);
        hex.push(HEX[(byte & 0x0f) as usize]);
    }
    hex.push(b'\n');
    hex
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Stores the key of a new encryption root. Called only after the dataset was
/// created successfully — a key without its dataset is noise, a dataset
/// without its key is a lockout.
pub fn put(cipher: &SettingsCipher, addon_id: &str, dataset: &str, key: &[u8]) -> Result<()> {
    let plaintext = Zeroizing::new(String::from_utf8(key.to_vec())?);
    let mut file = load(addon_id)?;
    file.datasets.insert(
        dataset.to_string(),
        KeyEntry {
            key_ciphertext: cipher.encrypt_bound(plaintext.trim_end(), dataset.as_bytes())?,
            created_at: super::db::now(),
        },
    );
    save(addon_id, &file)
}

/// The key of `dataset`, ready to be written to the helper's stdin.
pub fn get(cipher: &SettingsCipher, addon_id: &str, dataset: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
    let file = load(addon_id)?;
    let Some(entry) = file.datasets.get(dataset) else {
        return Ok(None);
    };
    let plain = cipher
        .decrypt_bound(&entry.key_ciphertext, dataset.as_bytes())
        .with_context(|| format!("key of '{dataset}' cannot be decrypted"))?;
    let mut out = Zeroizing::new(plain.value.trim_end().as_bytes().to_vec());
    out.push(b'\n');
    Ok(Some(out))
}

/// Drops the keys of a destroyed dataset and, with `subtree`, of everything
/// that lived under it — a key whose dataset is gone can only be dead weight.
/// Never called by the uninstall: an uninstalled app's datasets are still on
/// the disks and still need their keys.
pub fn forget(addon_id: &str, dataset: &str, subtree: bool) -> Result<usize> {
    let mut file = load(addon_id)?;
    let prefix = format!("{dataset}/");
    let before = file.datasets.len();
    file.datasets
        .retain(|name, _| name != dataset && !(subtree && name.starts_with(&prefix)));
    let removed = before - file.datasets.len();
    if removed > 0 {
        save(addon_id, &file)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tentaflow_home()` is resolved once per process, so a test cannot move
    /// it; it writes under its own instance id and removes that file again.
    fn with_scratch_instance<T>(body: impl FnOnce(&str) -> T) -> T {
        let addon = format!("test-keystore-{}", uuid::Uuid::now_v7());
        let out = body(&addon);
        let _ = std::fs::remove_file(store_path(&addon));
        out
    }

    #[test]
    fn a_generated_key_is_64_hex_characters_and_a_newline() {
        let key = generate();
        assert_eq!(key.len(), 65);
        assert_eq!(key[64], b'\n');
        assert!(key[..64].iter().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(generate().to_vec(), key.to_vec());
    }

    #[test]
    fn keys_round_trip_and_stay_bound_to_their_dataset() {
        with_scratch_instance(|addon| {
            let cipher = SettingsCipher::new(&[7u8; 32]);
            let key = generate();
            put(&cipher, addon, "tank/secret", &key).expect("put");
            let back = get(&cipher, addon, "tank/secret").expect("get").expect("some");
            assert_eq!(back.to_vec(), key.to_vec());
            assert!(get(&cipher, addon, "tank/other").expect("get").is_none());

            // The ciphertext is bound: reading it as another dataset fails.
            let raw = std::fs::read_to_string(store_path(addon)).expect("file");
            assert!(!raw.contains(std::str::from_utf8(&key[..64]).unwrap()));
            let moved: KeyFile = serde_json::from_str(&raw).expect("json");
            let ciphertext = &moved.datasets["tank/secret"].key_ciphertext;
            assert!(cipher.decrypt_bound(ciphertext, b"tank/other").is_err());

            // A child key goes with its parent only when the destroy was
            // recursive.
            put(&cipher, addon, "tank/secret/child", &generate()).expect("put child");
            assert_eq!(forget(addon, "tank/secret", false).expect("forget"), 1);
            assert!(get(&cipher, addon, "tank/secret").expect("get").is_none());
            assert!(get(&cipher, addon, "tank/secret/child").expect("get").is_some());
            assert_eq!(forget(addon, "tank/secret", true).expect("forget"), 1);
            assert!(get(&cipher, addon, "tank/secret/child").expect("get").is_none());
            assert_eq!(forget(addon, "tank/secret", true).expect("forget"), 0);
        });
    }
}
