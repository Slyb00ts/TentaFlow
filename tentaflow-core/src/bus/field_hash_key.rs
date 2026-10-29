// =============================================================================
// File: bus/field_hash_key.rs — the per-organization key of hashed fields
// =============================================================================
// SUM/tentabus/DECYZJE-2026-09-22.md (23.09): a read rule can show a field as
// `blake3::keyed_hash(org_key, value)`, 256 bit in hex. The key belongs to the
// ORGANIZATION, so one value has one hash in every TentaBus instance of it:
//   - 32 random bytes, created lazily by the first rule that hashes a field
//     (`dispatch::bus` `field_policy_set_v1`, on a node that may replicate it).
//     A READ never creates one: a node that meets a hashing rule without the
//     key (it has not synced yet) hides the hashed fields until the key
//     arrives, so a node that was cut off or has just joined cannot mint a
//     key that disagrees with the organization's;
//   - stored in `bus_field_hash_keys` as `SettingsCipher::encrypt_bound` of
//     their hex, bound to the organization id (a row moved to another
//     organization does not decrypt);
//   - replicated to the other nodes of the mesh sealed per recipient over
//     `SharedSecretsSync` (`mesh::shared_secrets`), never through the sync
//     ledger, whose operation bodies are stored in plaintext; the newer key
//     wins: the higher `key_version` (a rotation raises it, so the version
//     never goes back), at an equal version the EARLIER `rotated_at_ms` (the
//     key that was there first; a creation is "first wins"), at an equal time
//     the smaller key — so two nodes that meet always keep the same one. A
//     stamp is trusted only inside a window around the local clock (`rank_time`);
//     outside it the key counts as having arrived just now, so a clock that is
//     far ahead or was never set can neither displace the fleet's key nor, with
//     a higher version, be refused;
//   - rotated only by an explicit request of a bus administrator
//     (`dispatch::bus`, audit `bus.field_policy.hash_key.rotate`); every hash
//     made before the rotation stops matching.
// A key is never logged, audited or returned on the wire.
//
// `FieldHashKeys` keeps the decrypted key of an organization in a cache. Every
// use still reads the stored row (one indexed point lookup, the same cost as
// resolving the rule) and compares its ciphertext with the cached one, so a
// key rotated here, adopted from a peer or replaced by another instance of
// this process is never served stale and no AES-GCM runs on the read path.
// =============================================================================

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use zeroize::Zeroizing;

use crate::crypto::SettingsCipher;
use crate::db::repository::{self, DbBusFieldHashKey};
use crate::db::DbPool;

use super::BusServiceError;

const KEY_BYTES: usize = 32;

/// The highest `key_version` a node accepts from a peer: it is a `u32` on the
/// wire of the rotation window, and a version near `i64::MAX` would make the
/// next rotation overflow and the confirmation never match.
pub const MAX_KEY_VERSION: i64 = u32::MAX as i64;

/// A key that cannot be read, or that this node does not hold yet, is logged
/// at most this often per organization.
const READ_FAILURE_LOG_INTERVAL_MS: i64 = 60_000;

/// How far ahead of the local clock a key's `rotated_at_ms` is still believed.
pub const CLOCK_LEAD_MS: i64 = 120_000;

/// A `rotated_at_ms` before this (2020-01-01) comes from a clock that was
/// never set, not from a key made then: the product did not exist.
pub const CLOCK_FLOOR_MS: i64 = 1_577_836_800_000;

/// 32 bytes of key. Zeroed when dropped.
pub struct FieldHashKey(Zeroizing<[u8; KEY_BYTES]>);

impl FieldHashKey {
    fn generate() -> Result<Self, BusServiceError> {
        let mut bytes = Zeroizing::new([0u8; KEY_BYTES]);
        getrandom::fill(bytes.as_mut())
            .map_err(|e| BusServiceError::Db(format!("no random bytes for a hash key: {e}")))?;
        Ok(Self(bytes))
    }

    fn from_hex(hex_text: &str) -> Option<Self> {
        let mut bytes = Zeroizing::new([0u8; KEY_BYTES]);
        hex::decode_to_slice(hex_text, bytes.as_mut()).ok()?;
        Some(Self(bytes))
    }

    fn to_hex(&self) -> Zeroizing<String> {
        Zeroizing::new(hex::encode(self.0.as_ref()))
    }

    /// The hash of `value` under this key: 64 lowercase hex characters.
    pub fn hash_hex(&self, value: &[u8]) -> String {
        blake3::keyed_hash(&self.0, value).to_hex().to_string()
    }
}

/// What a hash key is, without the key: for the confirmation of a rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldHashKeyInfo {
    pub key_version: u32,
    pub created_at_ms: i64,
    pub rotated_at_ms: i64,
}

impl FieldHashKeyInfo {
    fn of(row: &DbBusFieldHashKey) -> Self {
        Self {
            key_version: u32::try_from(row.key_version).unwrap_or(u32::MAX),
            created_at_ms: row.created_at_ms,
            rotated_at_ms: row.rotated_at_ms,
        }
    }
}

/// One organization's key with its version, in the clear, for sealing to a
/// peer (`mesh::shared_secrets`) or opening from one.
pub struct FieldHashKeyVersion {
    pub org_id: String,
    pub key_version: i64,
    pub created_at_ms: i64,
    pub rotated_at_ms: i64,
    pub key_hex: Zeroizing<String>,
}

struct CachedKey {
    /// The stored ciphertext the key was decrypted from: the cache is valid
    /// for exactly that row.
    ciphertext: String,
    key: Arc<FieldHashKey>,
}

pub struct FieldHashKeys {
    db: DbPool,
    /// The node's settings cipher; `None` when the node did not hand the bus
    /// one (no key can then be stored or opened, and a read hides what it
    /// would hash).
    cipher: Option<Arc<SettingsCipher>>,
    cache: DashMap<String, CachedKey>,
    /// When each organization's unreadable key was last logged.
    read_failure_logged_ms: DashMap<String, AtomicI64>,
    /// When each organization's missing key (rules hash, the key has not
    /// arrived) was last logged.
    key_missing_logged_ms: DashMap<String, AtomicI64>,
}

fn key_context(org_id: &str) -> Vec<u8> {
    format!("bus-field-hash-key|{org_id}").into_bytes()
}

fn db_error(org_id: &str, what: &str, e: impl std::fmt::Display) -> BusServiceError {
    BusServiceError::Db(format!(
        "field hash key of organization '{org_id}': {what}: {e}"
    ))
}

impl FieldHashKeys {
    pub fn new(db: DbPool, cipher: Arc<SettingsCipher>) -> Self {
        Self::with_cipher(db, Some(cipher))
    }

    pub fn with_cipher(db: DbPool, cipher: Option<Arc<SettingsCipher>>) -> Self {
        Self {
            db,
            cipher,
            cache: DashMap::new(),
            read_failure_logged_ms: DashMap::new(),
            key_missing_logged_ms: DashMap::new(),
        }
    }

    fn cipher(&self, org_id: &str) -> Result<&SettingsCipher, BusServiceError> {
        self.cipher.as_deref().ok_or_else(|| {
            db_error(
                org_id,
                "no settings cipher",
                "the node did not provide its master key to the bus",
            )
        })
    }

    fn encrypt(&self, org_id: &str, key: &FieldHashKey) -> Result<String, BusServiceError> {
        self.cipher(org_id)?
            .encrypt_bound(&key.to_hex(), &key_context(org_id))
            .map_err(|e| db_error(org_id, "cannot encrypt", e))
    }

    fn decrypt(&self, row: &DbBusFieldHashKey) -> Result<FieldHashKey, BusServiceError> {
        let opened = self
            .cipher(&row.org_id)?
            .decrypt_bound(&row.encrypted_key, &key_context(&row.org_id))
            .map_err(|e| db_error(&row.org_id, "cannot decrypt", e))?;
        let value = Zeroizing::new(opened.value);
        if !opened.bound {
            return Err(db_error(&row.org_id, "stored key is not bound", "refused"));
        }
        FieldHashKey::from_hex(&value)
            .ok_or_else(|| db_error(&row.org_id, "stored key is malformed", "refused"))
    }

    /// The cached key for `row`, decrypting it only when the cache holds
    /// another row's key.
    fn resolve(&self, row: &DbBusFieldHashKey) -> Result<Arc<FieldHashKey>, BusServiceError> {
        if let Some(cached) = self.cache.get(&row.org_id) {
            if cached.ciphertext == row.encrypted_key {
                return Ok(cached.key.clone());
            }
        }
        let key = Arc::new(self.decrypt(row)?);
        self.cache.insert(
            row.org_id.clone(),
            CachedKey {
                ciphertext: row.encrypted_key.clone(),
                key: key.clone(),
            },
        );
        Ok(key)
    }

    /// The key of `org_id`, `None` when this node holds none. Never creates
    /// one: a key made by a read would be a second key for the organization.
    pub fn get(&self, org_id: &str) -> Result<Option<Arc<FieldHashKey>>, BusServiceError> {
        let stored = repository::bus_field_hash_key_get(&self.db, org_id)
            .map_err(|e| db_error(org_id, "cannot read", e))?;
        stored.map(|row| self.resolve(&row)).transpose()
    }

    /// The key a READ hashes with: `None` when the node holds none yet or
    /// cannot open the stored one — the reader then hides the hashed fields
    /// (fail closed) and the node goes on. Either is logged, rate-limited per
    /// organization: a field that vanishes from a read must leave a trace.
    /// Only called for a rule that hashes (`FieldPolicy::hash_key`).
    pub fn for_read(&self, org_id: &str) -> Option<Arc<FieldHashKey>> {
        match self.get(org_id) {
            Ok(Some(key)) => Some(key),
            Ok(None) => {
                if self.log_due(&self.key_missing_logged_ms, org_id) {
                    tracing::warn!(
                        "hashed fields of organization '{org_id}' are hidden: a rule hashes them \
                         but this node does not hold the organization's key yet (it comes from an \
                         operator node; check the connection to it)"
                    );
                }
                None
            }
            Err(e) => {
                if self.log_due(&self.read_failure_logged_ms, org_id) {
                    tracing::warn!("hashed fields are hidden, the key cannot be used: {e}");
                }
                None
            }
        }
    }

    /// Whether `org_id` may be logged now under `table`'s rate limit (and
    /// records that it was).
    fn log_due(&self, table: &DashMap<String, AtomicI64>, org_id: &str) -> bool {
        let now = super::now_ms();
        let logged = table
            .entry(org_id.to_string())
            .or_insert_with(|| AtomicI64::new(i64::MIN));
        let last = logged.load(Ordering::Relaxed);
        now.saturating_sub(last) >= READ_FAILURE_LOG_INTERVAL_MS
            && logged
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
    }

    /// The key of `org_id`, created now when the organization has none yet.
    /// For the writer of the first hashing rule, never for a read
    /// (`for_read`). Two callers that create at once end with the same key:
    /// the creation decides inside the writer transaction that stores it.
    pub fn get_or_create(&self, org_id: &str) -> Result<Arc<FieldHashKey>, BusServiceError> {
        let stored = repository::bus_field_hash_key_get(&self.db, org_id)
            .map_err(|e| db_error(org_id, "cannot read", e))?;
        if let Some(row) = stored {
            return self.resolve(&row);
        }
        let key = FieldHashKey::generate()?;
        let encrypted = self.encrypt(org_id, &key)?;
        let now = super::now_ms();
        let created = repository::bus_field_hash_key_write(&self.db, org_id, |current| {
            Ok(current.is_none().then(|| DbBusFieldHashKey {
                org_id: org_id.to_string(),
                key_version: 1,
                encrypted_key: encrypted.clone(),
                created_at_ms: now,
                rotated_at_ms: now,
            }))
        })
        .map_err(|e| db_error(org_id, "cannot create", e))?;
        if created.is_some() {
            let node_id = crate::sync::runtime::local_node_id();
            let _ = repository::log_audit(
                &self.db,
                None,
                None,
                "bus.field_policy.hash_key.create",
                Some(org_id),
                Some("key_version=1"),
                None,
                node_id.as_deref(),
            );
            let key = Arc::new(key);
            self.cache.insert(
                org_id.to_string(),
                CachedKey {
                    ciphertext: encrypted,
                    key: key.clone(),
                },
            );
            return Ok(key);
        }
        // Another writer created it between the read and the write.
        let row = repository::bus_field_hash_key_get(&self.db, org_id)
            .map_err(|e| db_error(org_id, "cannot read", e))?
            .ok_or_else(|| db_error(org_id, "key vanished after creation", "retry"))?;
        self.resolve(&row)
    }

    /// What the organization's key is, without decrypting it; `None` when no
    /// rule of the organization ever hashed a field.
    pub fn info(&self, org_id: &str) -> Result<Option<FieldHashKeyInfo>, BusServiceError> {
        Ok(repository::bus_field_hash_key_get(&self.db, org_id)
            .map_err(|e| db_error(org_id, "cannot read", e))?
            .as_ref()
            .map(FieldHashKeyInfo::of))
    }

    /// Replaces the key of `org_id` with a fresh one, when it is still
    /// `expected_version`. Every hash made with the old key stops matching.
    /// The new `rotated_at_ms` is above the stored one, so the rotation wins
    /// over the key it replaces wherever the two meet.
    pub fn rotate(
        &self,
        org_id: &str,
        expected_version: u32,
    ) -> Result<FieldHashKeyInfo, BusServiceError> {
        enum Refusal {
            Missing,
            Changed,
        }
        let key = FieldHashKey::generate()?;
        let encrypted = self.encrypt(org_id, &key)?;
        let now = super::now_ms();
        let mut refusal = None;
        let written = repository::bus_field_hash_key_write(&self.db, org_id, |current| {
            let Some(current) = current else {
                refusal = Some(Refusal::Missing);
                return Ok(None);
            };
            if current.key_version != i64::from(expected_version) {
                refusal = Some(Refusal::Changed);
                return Ok(None);
            }
            Ok(Some(DbBusFieldHashKey {
                org_id: org_id.to_string(),
                key_version: current.key_version + 1,
                encrypted_key: encrypted.clone(),
                created_at_ms: current.created_at_ms,
                rotated_at_ms: now.max(current.rotated_at_ms + 1),
            }))
        })
        .map_err(|e| db_error(org_id, "cannot rotate", e))?;
        match (written, refusal) {
            (Some(row), _) => {
                self.cache.insert(
                    org_id.to_string(),
                    CachedKey {
                        ciphertext: encrypted,
                        key: Arc::new(key),
                    },
                );
                Ok(FieldHashKeyInfo::of(&row))
            }
            (None, Some(Refusal::Missing)) => Err(BusServiceError::FieldHashKeyNotFound {
                org_id: org_id.to_string(),
            }),
            (None, _) => Err(BusServiceError::FieldHashKeyChanged {
                org_id: org_id.to_string(),
            }),
        }
    }

    /// Every key this node holds, in the clear, for sealing to a peer. A row
    /// that cannot be decrypted is skipped with a warning, not sent.
    pub fn export(&self) -> Result<Vec<FieldHashKeyVersion>, BusServiceError> {
        let rows = repository::bus_field_hash_key_list(&self.db)
            .map_err(|e| BusServiceError::Db(format!("field hash keys: cannot list: {e}")))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            match self.decrypt(&row) {
                Ok(key) => out.push(FieldHashKeyVersion {
                    org_id: row.org_id,
                    key_version: row.key_version,
                    created_at_ms: row.created_at_ms,
                    rotated_at_ms: row.rotated_at_ms,
                    key_hex: key.to_hex(),
                }),
                Err(e) => tracing::warn!("field hash key not shared with a peer: {e}"),
            }
        }
        Ok(out)
    }

    /// Stores `incoming` when it outranks the key held for its organization
    /// (or when none is held, or the held one cannot be decrypted),
    /// re-encrypted with this node's cipher. Reports whether it was stored.
    /// `outranks` is a total order, so two nodes that exchange keys keep the
    /// same one.
    pub fn adopt(&self, incoming: &FieldHashKeyVersion) -> Result<bool, BusServiceError> {
        self.adopt_at(incoming, super::now_ms())
    }

    /// `adopt` with the clock given, for the order's window around it.
    pub fn adopt_at(
        &self,
        incoming: &FieldHashKeyVersion,
        now_ms: i64,
    ) -> Result<bool, BusServiceError> {
        let org_id = incoming.org_id.as_str();
        let key = FieldHashKey::from_hex(&incoming.key_hex)
            .ok_or_else(|| db_error(org_id, "incoming key is malformed", "refused"))?;
        if !(1..=MAX_KEY_VERSION).contains(&incoming.key_version) {
            return Err(db_error(org_id, "incoming key has no usable version", "refused"));
        }
        let incoming_hex = key.to_hex();
        let encrypted = self.encrypt(org_id, &key)?;
        let mut kept_unreadable = None;
        let written = repository::bus_field_hash_key_write(&self.db, org_id, |current| {
            let wins = match current {
                None => true,
                Some(held) => match self.decrypt(held) {
                    Ok(held_key) => outranks(
                        (
                            incoming.key_version,
                            incoming.rotated_at_ms,
                            incoming_hex.as_str(),
                        ),
                        (
                            held.key_version,
                            held.rotated_at_ms,
                            held_key.to_hex().as_str(),
                        ),
                        now_ms,
                    ),
                    // A key this node cannot open (its master key changed) is
                    // replaced by one from the mesh — that is the way back —
                    // but never by an OLDER version: the version stays in
                    // the plain columns, and it never goes back.
                    Err(_) if incoming.key_version >= held.key_version => true,
                    Err(_) => {
                        kept_unreadable = Some(held.key_version);
                        false
                    }
                },
            };
            Ok(wins.then(|| DbBusFieldHashKey {
                org_id: org_id.to_string(),
                key_version: incoming.key_version,
                encrypted_key: encrypted.clone(),
                created_at_ms: incoming.created_at_ms,
                rotated_at_ms: incoming.rotated_at_ms,
            }))
        })
        .map_err(|e| db_error(org_id, "cannot adopt", e))?;
        if let Some(held_version) = kept_unreadable {
            tracing::warn!(
                "hash key of organization '{org_id}': this node cannot open its stored key \
                 (version {held_version}) and the key offered is older (version {}), so nothing was \
                 replaced; a key of version {held_version} or higher will be adopted",
                incoming.key_version
            );
        }
        Ok(written.is_some())
    }
}

/// The time a key is ranked by. A stamp is believed only when it lies between
/// `CLOCK_FLOOR_MS` and a little ahead of the local clock; anything else is a
/// clock that is wrong (or was never set), and the key then counts as having
/// arrived now — so it loses an equal-version tie against any key with a
/// believable stamp, and no skew of the sender's clock decides which key the
/// organization keeps. Both sides of a comparison go through it, so two nodes
/// that meet agree.
fn rank_time(stamp: i64, now_ms: i64) -> i64 {
    if (CLOCK_FLOOR_MS..=now_ms.saturating_add(CLOCK_LEAD_MS)).contains(&stamp) {
        stamp
    } else {
        now_ms
    }
}

/// Whether a key `(key_version, rotated_at_ms, key_hex)` replaces the held
/// one. The higher version always wins (a rotation raises it, so the version
/// never goes back and a rotation beats the key it replaced — whatever the
/// clock of the node that rotated says). At an equal version the earlier
/// ranked time (`rank_time`) wins: a key a node made lazily and a moment later
/// than the fleet's must never displace the fleet's, because the organization
/// has been hashing with the first one. Only then does the smaller key decide,
/// so a tie is broken the same way on every node.
fn outranks(incoming: (i64, i64, &str), held: (i64, i64, &str), now_ms: i64) -> bool {
    if incoming.0 != held.0 {
        return incoming.0 > held.0;
    }
    let (incoming_time, held_time) = (rank_time(incoming.1, now_ms), rank_time(held.1, now_ms));
    incoming_time < held_time || (incoming_time == held_time && incoming.2 < held.2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node() -> FieldHashKeys {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        FieldHashKeys::new(db, Arc::new(SettingsCipher::new(&[9u8; 32])))
    }

    #[test]
    fn a_hash_is_the_keyed_blake3_of_the_value_in_64_hex_characters() {
        let keys = node();
        let key = keys.get_or_create("org-1").unwrap();
        let hash = key.hash_hex(b"Kowalski");
        assert_eq!(hash.len(), 64);
        assert!(hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert_eq!(
            hash,
            blake3::keyed_hash(&key.0, b"Kowalski").to_hex().to_string()
        );
        // Deterministic, and sensitive to every byte of the value.
        assert_eq!(hash, key.hash_hex(b"Kowalski"));
        assert_ne!(hash, key.hash_hex(b"kowalski"));
        assert_ne!(hash, key.hash_hex(b"Kowalski "));
        // Not the plain hash: the key is part of it.
        assert_ne!(hash, blake3::hash(b"Kowalski").to_hex().to_string());
    }

    #[test]
    fn the_key_is_created_once_and_every_caller_gets_the_same_one() {
        let keys = node();
        assert!(keys.info("org-1").unwrap().is_none());
        let first = keys.get_or_create("org-1").unwrap().hash_hex(b"v");
        let second = keys.get_or_create("org-1").unwrap().hash_hex(b"v");
        assert_eq!(first, second);
        let info = keys.info("org-1").unwrap().expect("a key exists now");
        assert_eq!(info.key_version, 1);
        assert_eq!(info.created_at_ms, info.rotated_at_ms);

        // A second service over the same database finds the stored key.
        let other = FieldHashKeys::with_cipher(keys.db.clone(), keys.cipher.clone());
        assert_eq!(other.get_or_create("org-1").unwrap().hash_hex(b"v"), first);
    }

    #[test]
    fn two_organizations_do_not_share_a_key() {
        let keys = node();
        let a = keys.get_or_create("org-a").unwrap().hash_hex(b"same value");
        let b = keys.get_or_create("org-b").unwrap().hash_hex(b"same value");
        assert_ne!(a, b);
    }

    #[test]
    fn the_stored_key_is_encrypted_and_bound_to_its_organization() {
        let keys = node();
        let key = keys.get_or_create("org-1").unwrap();
        let row = repository::bus_field_hash_key_get(&keys.db, "org-1")
            .unwrap()
            .unwrap();
        assert!(
            row.encrypted_key.starts_with("encb:"),
            "{}",
            row.encrypted_key
        );
        assert!(!row.encrypted_key.contains(key.to_hex().as_str()));

        // The same ciphertext under another organization does not decrypt, and
        // is never replaced by a freshly made key.
        repository::bus_field_hash_key_write(&keys.db, "org-2", |_| {
            Ok(Some(DbBusFieldHashKey {
                org_id: "org-2".to_string(),
                ..row.clone()
            }))
        })
        .unwrap();
        assert!(keys.get_or_create("org-2").is_err());
    }

    #[test]
    fn rotation_replaces_the_key_and_old_hashes_stop_matching() {
        let keys = node();
        let before = keys.get_or_create("org-1").unwrap().hash_hex(b"v");
        let first = keys.info("org-1").unwrap().unwrap();

        let rotated = keys.rotate("org-1", 1).unwrap();
        assert_eq!(rotated.key_version, 2);
        assert_eq!(rotated.created_at_ms, first.created_at_ms);
        assert!(rotated.rotated_at_ms > first.rotated_at_ms);
        let after = keys.get_or_create("org-1").unwrap().hash_hex(b"v");
        assert_ne!(before, after);
        // The new key is the one a restart reads back.
        let restarted = FieldHashKeys::with_cipher(keys.db.clone(), keys.cipher.clone());
        assert_eq!(
            restarted.get_or_create("org-1").unwrap().hash_hex(b"v"),
            after
        );
    }

    #[test]
    fn a_rotation_needs_the_version_it_names_and_an_existing_key() {
        let keys = node();
        assert!(matches!(
            keys.rotate("org-1", 1),
            Err(BusServiceError::FieldHashKeyNotFound { .. })
        ));
        keys.get_or_create("org-1").unwrap();
        let hash = keys.get_or_create("org-1").unwrap().hash_hex(b"v");
        for wrong in [0, 2, 7] {
            assert!(
                matches!(
                    keys.rotate("org-1", wrong),
                    Err(BusServiceError::FieldHashKeyChanged { .. })
                ),
                "version {wrong}"
            );
        }
        assert_eq!(keys.get_or_create("org-1").unwrap().hash_hex(b"v"), hash);
        assert_eq!(keys.info("org-1").unwrap().unwrap().key_version, 1);
        // The second rotation of the same confirmation is refused.
        keys.rotate("org-1", 1).unwrap();
        assert!(matches!(
            keys.rotate("org-1", 1),
            Err(BusServiceError::FieldHashKeyChanged { .. })
        ));
    }

    fn incoming(from: &FieldHashKeys, org: &str) -> FieldHashKeyVersion {
        from.export()
            .unwrap()
            .into_iter()
            .find(|k| k.org_id == org)
            .expect("the key is held")
    }

    /// A believable stamp in the past, whatever day the test runs (above the clock floor).
    const T0: i64 = CLOCK_FLOOR_MS + 1_000_000;

    fn version_of(
        org: &str,
        key_version: i64,
        rotated_at_ms: i64,
        key_byte: &str,
    ) -> FieldHashKeyVersion {
        FieldHashKeyVersion {
            org_id: org.to_string(),
            key_version,
            created_at_ms: rotated_at_ms,
            rotated_at_ms,
            key_hex: Zeroizing::new(key_byte.repeat(KEY_BYTES)),
        }
    }

    #[test]
    fn a_fresh_key_of_this_node_gives_way_to_the_older_key_of_the_fleet() {
        // P2-1: the fleet's v1 is months old; this node made its own v1 a
        // moment ago. The older one must win here, whichever arrives first.
        let here = node();
        let fleet = version_of("org-1", 1, T0 + 1_000, "aa");
        let local = here.get_or_create("org-1").unwrap().hash_hex(b"v");
        assert!(here.adopt(&fleet).unwrap(), "the older key of the fleet is adopted");
        let after = here.get_or_create("org-1").unwrap().hash_hex(b"v");
        assert_ne!(after, local);
        assert_eq!(
            after,
            FieldHashKey::from_hex(&fleet.key_hex).unwrap().hash_hex(b"v")
        );
        assert_eq!(here.info("org-1").unwrap().unwrap().key_version, 1);
        // The fresh key does not come back over it, here or anywhere.
        let fresh = version_of("org-1", 1, super::super::now_ms(), "bb");
        assert!(!here.adopt(&fresh).unwrap());
    }

    #[test]
    fn the_older_of_two_keys_of_the_same_version_wins_on_both_nodes() {
        let (here, there) = (node(), node());
        here.get_or_create("org-1").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
        there.get_or_create("org-1").unwrap();
        let (older, newer) = (incoming(&here, "org-1"), incoming(&there, "org-1"));
        assert!(older.rotated_at_ms < newer.rotated_at_ms);

        assert!(!here.adopt(&newer).unwrap(), "the newer key does not replace the older");
        assert!(there.adopt(&older).unwrap(), "the older key replaces the newer");
        assert_eq!(
            here.get_or_create("org-1").unwrap().hash_hex(b"v"),
            there.get_or_create("org-1").unwrap().hash_hex(b"v")
        );
        // The same key twice changes nothing: the relay ends.
        assert!(!there.adopt(&older).unwrap());
    }

    #[test]
    fn a_higher_version_wins_even_when_its_key_is_older_and_a_version_never_goes_back() {
        let keys = node();
        assert!(keys.adopt(&version_of("org-1", 1, T0 + 500, "11")).unwrap());
        // A rotation made on another node: version 2, stamped LATER than v1.
        assert!(keys.adopt(&version_of("org-1", 2, T0 + 600, "22")).unwrap());
        // An independently made v1, however old, cannot take the version back.
        assert!(!keys.adopt(&version_of("org-1", 1, T0 + 100, "33")).unwrap());
        assert_eq!(keys.info("org-1").unwrap().unwrap().key_version, 2);
        // At the same version the earlier stamp wins, then the smaller key.
        assert!(keys.adopt(&version_of("org-1", 2, T0 + 550, "44")).unwrap());
        assert!(!keys.adopt(&version_of("org-1", 2, T0 + 550, "55")).unwrap());
        assert!(keys.adopt(&version_of("org-1", 2, T0 + 550, "00")).unwrap());
    }


    #[test]
    fn a_clock_that_is_far_ahead_or_never_set_decides_neither_a_tie_nor_a_rotation() {
        let keys = node();
        let now = super::super::now_ms();
        let fleet = version_of("org-1", 1, T0, "aa");
        assert!(keys.adopt_at(&fleet, now).unwrap());
        // Equal version: a stamp from the far future or from 1970 counts as "just
        // now", so the fleet's earlier key stays — in both directions.
        for stamp in [now + 10 * CLOCK_LEAD_MS, 5_000, 0, -1] {
            let odd = version_of("org-1", 1, stamp, "00");
            assert!(!keys.adopt_at(&odd, now).unwrap(), "stamp {stamp}");
            // Held the odd one first: the believable one replaces it.
            let other = node();
            assert!(other.adopt_at(&odd, now).unwrap());
            assert!(other.adopt_at(&fleet, now).unwrap(), "stamp {stamp}");
        }
        // A rotation is a higher version: its stamp does not matter, so a
        // skewed operator clock cannot make receivers refuse it.
        for stamp in [now + 10 * CLOCK_LEAD_MS, 5_000] {
            let rotated = version_of("org-1", 2, stamp, "22");
            let receiver = node();
            assert!(receiver.adopt_at(&fleet, now).unwrap());
            assert!(receiver.adopt_at(&rotated, now).unwrap(), "stamp {stamp}");
            assert_eq!(receiver.info("org-1").unwrap().unwrap().key_version, 2);
        }
    }

    #[test]
    fn an_older_version_does_not_replace_a_key_this_node_cannot_open() {
        let keys = node();
        assert!(keys.adopt(&version_of("org-1", 5, T0, "55")).unwrap());
        // The master key of the node changed: the stored v5 cannot be opened.
        let other = FieldHashKeys::new(keys.db.clone(), Arc::new(SettingsCipher::new(&[1u8; 32])));
        assert!(other.get("org-1").is_err());
        // An older version does not take its place; the row stays as it was.
        assert!(!other.adopt(&version_of("org-1", 1, T0, "11")).unwrap());
        assert!(!other.adopt(&version_of("org-1", 4, T0, "44")).unwrap());
        assert_eq!(other.info("org-1").unwrap().unwrap().key_version, 5);
        // The same or a higher version is the way back.
        assert!(other.adopt(&version_of("org-1", 5, T0, "55")).unwrap());
        assert!(other.for_read("org-1").is_some());
        assert_eq!(other.info("org-1").unwrap().unwrap().key_version, 5);
    }

    #[test]
    fn a_missing_key_is_logged_once_a_minute_not_on_every_read() {
        let keys = node();
        assert!(keys.log_due(&keys.key_missing_logged_ms, "org-1"));
        assert!(!keys.log_due(&keys.key_missing_logged_ms, "org-1"));
        assert!(keys.log_due(&keys.key_missing_logged_ms, "org-2"));
        // Missing and unreadable keys are limited separately.
        assert!(keys.log_due(&keys.read_failure_logged_ms, "org-1"));
        assert!(keys.for_read("org-1").is_none());
    }

    #[test]
    fn a_version_beyond_u32_is_refused_so_the_next_rotation_cannot_overflow() {
        let keys = node();
        for version in [0, -1, MAX_KEY_VERSION + 1, i64::MAX] {
            assert!(
                keys.adopt(&version_of("org-1", version, 1, "11")).is_err(),
                "version {version}"
            );
        }
        assert!(keys.info("org-1").unwrap().is_none());
        assert!(keys.adopt(&version_of("org-1", MAX_KEY_VERSION, 1, "11")).unwrap());
    }

    #[test]
    fn a_read_never_creates_a_key_and_hides_instead() {
        let keys = node();
        assert!(keys.for_read("org-1").is_none());
        assert!(keys.get("org-1").unwrap().is_none());
        assert!(keys.info("org-1").unwrap().is_none(), "no key was made by a read");
        // Once the creating writer has made it, a read sees it.
        let made = keys.get_or_create("org-1").unwrap().hash_hex(b"v");
        assert_eq!(keys.for_read("org-1").unwrap().hash_hex(b"v"), made);
    }

    #[test]
    fn a_node_without_a_cipher_or_with_an_unreadable_key_reads_nothing_and_does_not_fail() {
        let keys = node();
        keys.get_or_create("org-1").unwrap();
        let no_cipher = FieldHashKeys::with_cipher(keys.db.clone(), None);
        assert!(no_cipher.for_read("org-1").is_none());
        assert!(no_cipher.get("org-1").is_err());
        assert!(no_cipher.get_or_create("org-2").is_err());
        assert!(no_cipher.adopt(&version_of("org-2", 1, 1, "11")).is_err());
        // Another cipher cannot open the stored key either.
        let other = FieldHashKeys::new(keys.db.clone(), Arc::new(SettingsCipher::new(&[1u8; 32])));
        assert!(other.for_read("org-1").is_none());
        assert!(keys.for_read("org-1").is_some());
    }

    #[test]
    fn two_threads_that_create_the_key_at_once_end_with_the_same_one() {
        // A file database: concurrent readers and writers over the shared-cache
        // in-memory one lock each other out, which production never does.
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::init(&dir.path().join("keys.sqlite")).expect("file db");
        let keys = Arc::new(FieldHashKeys::new(
            db,
            Arc::new(SettingsCipher::new(&[9u8; 32])),
        ));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let hashes: Vec<String> = (0..8)
            .map(|_| {
                let (keys, barrier) = (keys.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    keys.get_or_create("org-1").unwrap().hash_hex(b"v")
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert!(hashes.windows(2).all(|pair| pair[0] == pair[1]), "{hashes:?}");
        assert_eq!(keys.get("org-1").unwrap().unwrap().hash_hex(b"v"), hashes[0]);
        assert_eq!(keys.info("org-1").unwrap().unwrap().key_version, 1);
    }

    #[test]
    fn two_nodes_that_made_a_key_each_end_with_the_same_one() {
        let (a, b) = (node(), node());
        a.get_or_create("org-1").unwrap();
        b.get_or_create("org-1").unwrap();
        // Same or neighboring millisecond, same version: the order falls to
        // the earlier stamp, then to the key itself.
        let (from_a, from_b) = (incoming(&a, "org-1"), incoming(&b, "org-1"));
        let _ = (a.adopt(&from_b).unwrap(), b.adopt(&from_a).unwrap());
        let (ha, hb) = (
            a.get_or_create("org-1").unwrap().hash_hex(b"v"),
            b.get_or_create("org-1").unwrap().hash_hex(b"v"),
        );
        assert_eq!(ha, hb);
        assert_eq!(a.info("org-1").unwrap(), b.info("org-1").unwrap());
    }

    #[test]
    fn a_rotation_wins_over_the_key_it_replaced_on_every_node() {
        let (a, b) = (node(), node());
        a.get_or_create("org-1").unwrap();
        assert!(b.adopt(&incoming(&a, "org-1")).unwrap());
        a.rotate("org-1", 1).unwrap();

        assert!(b.adopt(&incoming(&a, "org-1")).unwrap());
        assert_eq!(b.info("org-1").unwrap().unwrap().key_version, 2);
        assert_eq!(
            a.get_or_create("org-1").unwrap().hash_hex(b"v"),
            b.get_or_create("org-1").unwrap().hash_hex(b"v")
        );
        // The key that was rotated away cannot come back.
        let stale = FieldHashKeyVersion {
            org_id: "org-1".to_string(),
            key_version: 1,
            created_at_ms: 1,
            rotated_at_ms: 1,
            key_hex: Zeroizing::new("11".repeat(KEY_BYTES)),
        };
        assert!(!b.adopt(&stale).unwrap());
    }

    #[test]
    fn a_malformed_incoming_key_is_refused() {
        let keys = node();
        for bad in ["", "zz", &"ab".repeat(31), &"ab".repeat(33)] {
            let incoming = FieldHashKeyVersion {
                org_id: "org-1".to_string(),
                key_version: 1,
                created_at_ms: 1,
                rotated_at_ms: 1,
                key_hex: Zeroizing::new(bad.to_string()),
            };
            assert!(keys.adopt(&incoming).is_err(), "{bad:?}");
        }
        assert!(keys.info("org-1").unwrap().is_none());
    }
}
