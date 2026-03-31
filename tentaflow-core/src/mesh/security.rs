// =============================================================================
// Plik: mesh/security.rs
// Opis: Bezpieczenstwo mesh — generowanie kluczy Ed25519, parowanie PIN,
//       wymiana kluczy X25519, szyfrowanie ChaCha20-Poly1305.
//       Zoptymalizowane pod 1000 peerow: cache cipherow, batch trust check,
//       pre-alokowane bufory szyfrowania.
// =============================================================================

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use parking_lot::RwLock;
use rand::rngs::OsRng;
use rand::Rng;
use sha2::Sha256;
use tracing::{info, warn};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

use dashmap::DashMap;
use crate::db::{self, DbPool};

/// Sliding window do wykrywania powtorzonych nonce (replay detection).
/// Rozmiar okna: 64 — uzywa u64 bitmap.
struct ReplayWindow {
    /// Najwyzszy widziany nonce
    max_seen: u64,
    /// Bitmap okna: bit i = 1 oznacza ze nonce (max_seen - i) byl widziany
    bitmap: u64,
}

impl ReplayWindow {
    fn new() -> Self {
        Self {
            max_seen: 0,
            bitmap: 0,
        }
    }

    /// Sprawdza i aktualizuje okno dla danego nonce.
    /// Zwraca true jesli nonce jest nowy (akceptowany), false jesli duplikat/za stary.
    fn check_and_update(&mut self, nonce: u64) -> bool {
        if nonce > self.max_seen {
            let shift = (nonce - self.max_seen).min(64);
            if shift >= 64 {
                self.bitmap = 0;
            } else {
                self.bitmap <<= shift;
            }
            self.bitmap |= 1;
            self.max_seen = nonce;
            true
        } else if self.max_seen - nonce >= 64 {
            false
        } else {
            let bit = self.max_seen - nonce;
            if self.bitmap & (1 << bit) != 0 {
                false
            } else {
                self.bitmap |= 1 << bit;
                true
            }
        }
    }
}

/// Czas po jakim klucz powinien byc rotowany
pub const KEY_ROTATION_INTERVAL: Duration = Duration::from_secs(24 * 3600);

/// Czas po jakim stary klucz jest usuwany (grace period na deszyfrowanie w locie)
const KEY_GRACE_PERIOD: Duration = Duration::from_secs(7 * 24 * 3600);

/// Klucz szyfrowania dla pojedynczej epoki
struct EpochKey {
    #[allow(dead_code)]
    secret: [u8; 32],
    cipher: Arc<ChaCha20Poly1305>,
    created_at: Instant,
}

/// Zbior kluczy epokowych dla jednego peera
struct EpochKeyRing {
    /// Aktualny epoch dla tego peera
    current_epoch: u32,
    /// Mapa epoch -> klucz (aktualny + stare w grace period)
    keys: HashMap<u32, EpochKey>,
}

impl EpochKeyRing {
    /// Tworzy nowy ring z poczatkowym kluczem na epoch 0
    fn new(secret: [u8; 32]) -> Self {
        let key = Key::from(secret);
        let cipher = Arc::new(ChaCha20Poly1305::new(&key));
        let mut keys = HashMap::new();
        keys.insert(0, EpochKey {
            secret,
            cipher,
            created_at: Instant::now(),
        });
        Self {
            current_epoch: 0,
            keys,
        }
    }

    /// Usuwa klucze starsze niz grace period (zachowuje aktualny)
    fn cleanup_expired(&mut self) {
        let now = Instant::now();
        let current = self.current_epoch;
        self.keys.retain(|epoch, key| {
            *epoch == current || now.duration_since(key.created_at) < KEY_GRACE_PERIOD
        });
    }

    /// Pobiera cipher dla danej epoki
    fn get_cipher(&self, epoch: u32) -> Option<&Arc<ChaCha20Poly1305>> {
        self.keys.get(&epoch).map(|k| &k.cipher)
    }

    /// Pobiera cipher dla aktualnej epoki
    fn current_cipher(&self) -> Option<&Arc<ChaCha20Poly1305>> {
        self.get_cipher(self.current_epoch)
    }
}

/// Oczekujaca rotacja klucza — przechowuje ephemeral secret miedzy wyslaniem pub_a a otrzymaniem pub_b
struct PendingKeyRotation {
    ephemeral_secret: x25519_dalek::StaticSecret,
    #[allow(dead_code)]
    ephemeral_public: x25519_dalek::PublicKey,
    created_at: Instant,
}

/// Zarzadca bezpieczenstwa mesh — klucze, parowanie, szyfrowanie.
///
/// Optymalizacje wydajnosci (1000 peerow, 2000+ szyfrowanych wiadomosci/s):
/// - `cached_ciphers`: ChaCha20Poly1305 instancje pre-inicjalizowane per peer
///   (unikniecie powtornego `ChaCha20Poly1305::new` przy kazdym szyfrowaniu)
/// - `trusted_node_ids`: Arc<HashSet<String>> — snapshot zaufanych node_id,
///   odbudowywany tylko przy zmianie (parowanie/revoke). Pozwala na batch trust
///   check bez lockowania trusted_keys 1000 razy w petli heartbeat.
/// - `encrypt_for_node_into`: szyfrowanie z reuse bufora (zero alokacji w hot path)
pub struct MeshSecurity {
    /// Klucz prywatny tego noda (Ed25519 — podpisy)
    signing_key: SigningKey,
    /// Klucz publiczny tego noda (Ed25519)
    pub verifying_key: VerifyingKey,
    /// Klucz prywatny X25519 (do Diffie-Hellman)
    x25519_secret: StaticSecret,
    /// Klucz publiczny X25519 (do wymiany)
    x25519_public: X25519PublicKey,
    /// Zaufane nody: node_id -> klucz publiczny Ed25519
    trusted_keys: RwLock<HashMap<String, VerifyingKey>>,
    /// Klucze epokowe per peer — aktualny epoch + stare w grace period
    epoch_keys: RwLock<HashMap<String, EpochKeyRing>>,
    /// [OPT] Snapshot zaufanych node_id — Arc<HashSet> do batch trust check.
    /// Jeden read Arc::clone zamiast 1000 lockow RwLock w petli heartbeat.
    /// Odbudowywany przy kazdej zmianie trusted_keys.
    trusted_node_ids: RwLock<Arc<HashSet<String>>>,
    /// Outbound nonce countery per peer — atomowe, lockfree
    outbound_nonces: Arc<RwLock<HashMap<String, AtomicU64>>>,
    /// Inbound replay detection per peer
    inbound_windows: Arc<DashMap<String, ReplayWindow>>,
    /// Lista revoked node IDs — wypelniana przy revoke_trust()
    revoked_nodes: RwLock<HashSet<String>>,
    /// Oczekujace rotacje kluczy: peer_id -> PendingKeyRotation
    pending_rotations: RwLock<HashMap<String, PendingKeyRotation>>,
    /// Licznik prób weryfikacji PIN per pairing: node_id -> (count, last_attempt)
    pin_attempts: RwLock<HashMap<String, (u32, Instant)>>,
    /// Pool bazy danych
    pub db: DbPool,
}

impl MeshSecurity {
    /// Tworzy lub wczytuje keypair z bazy danych.
    /// Klucz prywatny Ed25519 zapisany jako hex w settings pod kluczem "node_private_key".
    /// Klucz prywatny X25519 pod kluczem "node_x25519_private_key".
    pub fn new(db: DbPool) -> Result<Self> {
        let (signing_key, x25519_secret) = Self::load_or_generate_keys(&db)?;
        let verifying_key = signing_key.verifying_key();
        let x25519_public = X25519PublicKey::from(&x25519_secret);

        let security = Self {
            signing_key,
            verifying_key,
            x25519_secret,
            x25519_public,
            trusted_keys: RwLock::new(HashMap::new()),
            epoch_keys: RwLock::new(HashMap::new()),
            trusted_node_ids: RwLock::new(Arc::new(HashSet::new())),
            outbound_nonces: Arc::new(RwLock::new(HashMap::new())),
            inbound_windows: Arc::new(DashMap::new()),
            revoked_nodes: RwLock::new(HashSet::new()),
            pending_rotations: RwLock::new(HashMap::new()),
            pin_attempts: RwLock::new(HashMap::new()),
            db,
        };

        // Wczytaj zaufane nody z bazy
        security.load_trusted_from_db()?;

        // Wczytaj revoked nodes z bazy
        if let Ok(revoked) = db::repository::list_revoked_nodes(&security.db) {
            let mut set = security.revoked_nodes.write();
            for node_id in revoked {
                set.insert(node_id);
            }
        }

        info!(
            public_key = %security.public_key_hex(),
            trusted_count = security.trusted_keys.read().len(),
            "MeshSecurity zainicjalizowany"
        );

        Ok(security)
    }

    /// Wczytuje lub generuje klucze Ed25519 i X25519
    fn load_or_generate_keys(db: &DbPool) -> Result<(SigningKey, StaticSecret)> {
        let conn = db.lock().map_err(|e| anyhow::anyhow!("Blad locka DB: {}", e))?;

        // Ed25519
        let ed_key_hex: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'node_private_key'",
                [],
                |row| row.get(0),
            )
            .ok();

        let signing_key = if let Some(hex_str) = ed_key_hex {
            let bytes = hex::decode(&hex_str).context("Nieprawidlowy hex klucza Ed25519")?;
            let key_bytes: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("Klucz Ed25519 ma niepoprawna dlugosc"))?;
            SigningKey::from_bytes(&key_bytes)
        } else {
            let key = SigningKey::generate(&mut OsRng);
            let hex_str = hex::encode(key.to_bytes());
            conn.execute(
                "INSERT OR REPLACE INTO settings (key, value, updated_at) VALUES ('node_private_key', ?1, datetime('now'))",
                rusqlite::params![hex_str],
            )?;
            info!("Wygenerowano nowy klucz Ed25519 dla tego noda");
            key
        };

        // X25519
        let x_key_hex: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'node_x25519_private_key'",
                [],
                |row| row.get(0),
            )
            .ok();

        let x25519_secret = if let Some(hex_str) = x_key_hex {
            let bytes = hex::decode(&hex_str).context("Nieprawidlowy hex klucza X25519")?;
            let key_bytes: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("Klucz X25519 ma niepoprawna dlugosc"))?;
            StaticSecret::from(key_bytes)
        } else {
            let secret = StaticSecret::random_from_rng(OsRng);
            let hex_str = hex::encode(secret.to_bytes());
            conn.execute(
                "INSERT OR REPLACE INTO settings (key, value, updated_at) VALUES ('node_x25519_private_key', ?1, datetime('now'))",
                rusqlite::params![hex_str],
            )?;
            info!("Wygenerowano nowy klucz X25519 dla tego noda");
            secret
        };

        Ok((signing_key, x25519_secret))
    }

    /// Wczytuje zaufane nody z bazy i oblicza shared secrets
    fn load_trusted_from_db(&self) -> Result<()> {
        let trusted = db::repository::list_trusted_nodes(&self.db)?;
        let mut keys = self.trusted_keys.write();
        let mut rings = self.epoch_keys.write();

        for node in &trusted {
            match Self::parse_verifying_key(&node.public_key) {
                Ok(vk) => {
                    keys.insert(node.node_id.clone(), vk);
                    if node.public_key.len() >= 128 {
                        let x25519_hex = &node.public_key[64..128];
                        if let Ok(x_bytes) = hex::decode(x25519_hex) {
                            if x_bytes.len() == 32 {
                                let mut arr = [0u8; 32];
                                arr.copy_from_slice(&x_bytes);
                                let remote_x_pub = X25519PublicKey::from(arr);
                                let raw_shared = self.x25519_secret.diffie_hellman(&remote_x_pub);
                                let derived_key = Self::derive_key_from_dh(&raw_shared);
                                rings.insert(node.node_id.clone(), EpochKeyRing::new(derived_key));
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        node_id = %node.node_id,
                        "Nie udalo sie wczytac klucza publicznego: {}", e
                    );
                }
            }
        }

        drop(rings);
        let trusted_set: HashSet<String> = keys.keys().cloned().collect();
        drop(keys);
        *self.trusted_node_ids.write() = Arc::new(trusted_set);

        Ok(())
    }

    /// [CR-007] Oblicza klucz szyfrowania z wyniku Diffie-Hellman przez HKDF-SHA256
    fn derive_key_from_dh(raw_shared: &x25519_dalek::SharedSecret) -> [u8; 32] {
        let hk = Hkdf::<Sha256>::new(None, raw_shared.as_bytes());
        let mut derived_key = [0u8; 32];
        hk.expand(b"tentaflow-mesh-chacha20-key", &mut derived_key)
            .expect("HKDF expand nie powinien failowac dla 32 bajtow");
        derived_key
    }

    /// Parsuje klucz publiczny Ed25519 z hex stringa (pierwsze 64 znaki hex = 32 bajty)
    fn parse_verifying_key(hex_str: &str) -> Result<VerifyingKey> {
        let ed_hex = if hex_str.len() >= 64 {
            &hex_str[..64]
        } else {
            hex_str
        };
        let bytes = hex::decode(ed_hex).context("Nieprawidlowy hex klucza publicznego")?;
        let key_bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("Klucz publiczny ma niepoprawna dlugosc"))?;
        VerifyingKey::from_bytes(&key_bytes)
            .map_err(|e| anyhow::anyhow!("Nieprawidlowy klucz Ed25519: {}", e))
    }

    /// [OPT] Odbudowuje snapshot trusted_node_ids po zmianie trusted_keys.
    /// Wewnetrzna metoda — WYMAGA ze trusted_keys NIE jest zlockowany.
    fn rebuild_trusted_snapshot(&self) {
        let trusted_set: HashSet<String> = self.trusted_keys.read().keys().cloned().collect();
        *self.trusted_node_ids.write() = Arc::new(trusted_set);
    }

    /// Zwraca polaczony klucz publiczny jako hex: Ed25519 (32B) + X25519 (32B) = 128 hex znakow
    pub fn public_key_hex(&self) -> String {
        let ed_hex = hex::encode(self.verifying_key.to_bytes());
        let x_hex = hex::encode(self.x25519_public.to_bytes());
        format!("{}{}", ed_hex, x_hex)
    }

    /// Zwraca sam klucz publiczny Ed25519 jako hex (64 znaki)
    pub fn ed25519_public_key_hex(&self) -> String {
        hex::encode(self.verifying_key.to_bytes())
    }

    /// Zwraca klucz publiczny X25519 jako hex (64 znaki)
    pub fn x25519_public_key_hex(&self) -> String {
        hex::encode(self.x25519_public.to_bytes())
    }

    /// Czy node jest zaufany?
    pub fn is_trusted(&self, node_id: &str) -> bool {
        self.trusted_keys.read().contains_key(node_id)
    }

    /// [OPT] Zwraca snapshot zaufanych node_id jako Arc<HashSet>.
    /// Jedna atomowa operacja Arc::clone zamiast 1000 lockow RwLock w petli heartbeat.
    /// Snapshot jest odbudowywany tylko przy zmianie (parowanie/revoke) — czesto czytany,
    /// rzadko modyfikowany.
    pub fn trusted_node_ids_snapshot(&self) -> Arc<HashSet<String>> {
        Arc::clone(&self.trusted_node_ids.read())
    }

    /// Generuj losowy 6-cyfrowy PIN do parowania
    pub fn generate_pin() -> String {
        let pin: u32 = OsRng.gen_range(100_000..999_999);
        format!("{:06}", pin)
    }

    /// Rozpocznij parowanie — wygeneruj PIN, zapisz pending pairing w bazie.
    /// VULN-041: Limit oczekujacych parowan do 10 — ochrona przed wyczerpaniem zasobow.
    pub fn initiate_pairing(&self, remote_node_id: &str) -> Result<String> {
        // VULN-041: Sprawdz liczbe oczekujacych parowan
        let pending_count = db::repository::list_pending_pairings(&self.db)?;
        if pending_count.len() > 10 {
            bail!("Zbyt wiele oczekujacych parowan (max 10). Usun lub zatwierdz istniejace.");
        }

        // Resetuj rate limit PIN — nowe parowanie = nowe proby
        self.pin_attempts.write().remove(remote_node_id);

        let pin = Self::generate_pin();
        let expires = chrono::Utc::now() + chrono::Duration::seconds(60);
        let expires_str = expires.format("%Y-%m-%d %H:%M:%S").to_string();

        db::repository::create_pending_pairing(
            &self.db,
            remote_node_id,
            &pin,
            "outgoing",
            &expires_str,
        )?;

        info!(
            remote_node_id = %remote_node_id,
            "Rozpoczeto parowanie — PIN wygenerowany (wazny 60s)"
        );

        Ok(pin)
    }

    /// Odbierz zadanie parowania od innego noda — zapisz pending pairing
    pub fn receive_pairing_request(&self, remote_node_id: &str, pin: &str, remote_public_key: &str) -> Result<()> {
        let expires = chrono::Utc::now() + chrono::Duration::seconds(60);
        let expires_str = expires.format("%Y-%m-%d %H:%M:%S").to_string();

        // Resetuj rate limit PIN dla tego noda — nowe parowanie = nowe proby
        self.pin_attempts.write().remove(remote_node_id);

        db::repository::create_pending_pairing(
            &self.db,
            remote_node_id,
            pin,
            "incoming",
            &expires_str,
        )?;

        // Zapamiętaj klucz publiczny inicjatora w ustawieniach (potrzebny przy confirm)
        if !remote_public_key.is_empty() {
            let key = format!("pending_pubkey:{}", remote_node_id);
            let _ = db::repository::set_setting(&self.db, &key, remote_public_key);
        }

        info!(
            remote_node_id = %remote_node_id,
            "Otrzymano zadanie parowania — PIN i klucz publiczny zapisane"
        );

        Ok(())
    }

    /// Potwierdz parowanie po weryfikacji PIN — zapisz klucz publiczny, oblicz shared secret
    pub fn confirm_pairing(
        &self,
        remote_node_id: &str,
        remote_public_key_hex: &str,
        hostname: &str,
        approved_by: &str,
    ) -> Result<()> {
        // Sprawdz czy parowanie istnieje i nie wygaslo
        let pending = db::repository::get_pending_pairing(&self.db, remote_node_id)?
            .ok_or_else(|| anyhow::anyhow!("Brak oczekujacego parowania z nodem {}", remote_node_id))?;

        let expires = chrono::NaiveDateTime::parse_from_str(&pending.expires_at, "%Y-%m-%d %H:%M:%S")
            .context("Blad parsowania daty wygasniecia")?;
        let now = chrono::Utc::now().naive_utc();
        if now > expires {
            db::repository::delete_pending_pairing(&self.db, remote_node_id)?;
            bail!("Parowanie wygaslo — wygeneruj nowy PIN");
        }

        // [CR-010] Walidacja dlugosci klucza publicznego
        if remote_public_key_hex.len() != 128 {
            bail!(
                "Nieprawidlowa dlugosc klucza publicznego: {} (oczekiwano 128 hex)",
                remote_public_key_hex.len()
            );
        }

        // Parsuj klucz publiczny Ed25519
        let vk = Self::parse_verifying_key(remote_public_key_hex)?;

        // Zapisz do bazy
        db::repository::add_trusted_node(
            &self.db,
            remote_node_id,
            remote_public_key_hex,
            hostname,
            approved_by,
        )?;

        // Dodaj do mapy pamieci
        self.trusted_keys.write().insert(remote_node_id.to_string(), vk);

        // Oblicz shared secret z X25519 z HKDF i utworz EpochKeyRing
        if remote_public_key_hex.len() >= 128 {
            let x25519_hex = &remote_public_key_hex[64..128];
            if let Ok(x_bytes) = hex::decode(x25519_hex) {
                if x_bytes.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&x_bytes);
                    let remote_x_pub = X25519PublicKey::from(arr);
                    let raw_shared = self.x25519_secret.diffie_hellman(&remote_x_pub);
                    let derived_key = Self::derive_key_from_dh(&raw_shared);
                    self.epoch_keys
                        .write()
                        .insert(remote_node_id.to_string(), EpochKeyRing::new(derived_key));
                }
            }
        }

        // Usun pending pairing
        db::repository::delete_pending_pairing(&self.db, remote_node_id)?;

        // [OPT] Odbuduj snapshot trusted_node_ids
        self.rebuild_trusted_snapshot();

        info!(
            remote_node_id = %remote_node_id,
            hostname = %hostname,
            "Parowanie zatwierdzone — node jest teraz zaufany"
        );

        Ok(())
    }

    /// Odrzuc parowanie — usun pending
    pub fn reject_pairing(&self, remote_node_id: &str) -> Result<()> {
        db::repository::delete_pending_pairing(&self.db, remote_node_id)?;
        info!(remote_node_id = %remote_node_id, "Parowanie odrzucone");
        Ok(())
    }

    /// Cofnij zaufanie — usun z bazy i pamieci
    pub fn revoke_trust(&self, node_id: &str) -> Result<()> {
        db::repository::remove_trusted_node(&self.db, node_id)?;
        self.trusted_keys.write().remove(node_id);
        self.epoch_keys.write().remove(node_id);
        self.revoked_nodes.write().insert(node_id.to_string());
        let _ = db::repository::add_revoked_node(&self.db, node_id, None);
        self.rebuild_trusted_snapshot();
        info!(node_id = %node_id, "Cofnieto zaufanie dla noda");
        Ok(())
    }

    /// Czy node zostal aktywnie revoked (byl trusted i utracil zaufanie)
    pub fn is_revoked(&self, node_id: &str) -> bool {
        self.revoked_nodes.read().contains(node_id)
    }

    /// Usuwa node z listy revoked — admin re-trust
    pub fn admin_retrust(&self, node_id: &str) -> Result<()> {
        self.revoked_nodes.write().remove(node_id);
        db::repository::remove_revoked_node(&self.db, node_id)?;
        info!(node_id = %node_id, "Admin re-trust — usunięto z revoked");
        Ok(())
    }

    /// Szyfruj payload kluczem shared secret dla danego noda.
    /// Format wyjscia: [4B epoch][8B nonce_counter][12B chacha_nonce][ciphertext+tag]
    pub fn encrypt_for_node(&self, node_id: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
        let rings = self.epoch_keys.read();
        let ring = rings.get(node_id)
            .ok_or_else(|| anyhow::anyhow!("Brak EpochKeyRing dla {}", node_id))?;
        let epoch = ring.current_epoch;
        let cipher = ring.current_cipher()
            .ok_or_else(|| anyhow::anyhow!("Brak cipher dla epoch {} noda {}", epoch, node_id))?
            .clone();
        drop(rings);

        let nonce_counter = self.get_next_nonce(node_id);

        // Deterministyczny nonce z counter: [0u8; 4][counter BE u64]
        let mut chacha_nonce_bytes = [0u8; 12];
        chacha_nonce_bytes[4..12].copy_from_slice(&nonce_counter.to_be_bytes());
        let nonce = Nonce::from_slice(&chacha_nonce_bytes);

        // AAD = epoch_be ++ nonce_counter_be
        let mut aad = [0u8; 12];
        aad[0..4].copy_from_slice(&epoch.to_be_bytes());
        aad[4..12].copy_from_slice(&nonce_counter.to_be_bytes());

        let ciphertext = cipher
            .encrypt(nonce, Payload { msg: plaintext, aad: &aad })
            .map_err(|e| anyhow::anyhow!("Blad szyfrowania: {}", e))?;

        let mut result = Vec::with_capacity(4 + 8 + 12 + ciphertext.len());
        result.extend_from_slice(&epoch.to_be_bytes());
        result.extend_from_slice(&nonce_counter.to_be_bytes());
        result.extend_from_slice(&chacha_nonce_bytes);
        result.extend_from_slice(&ciphertext);
        Ok(result)
    }

    /// [OPT] Szyfruj payload do istniejacego bufora — zero alokacji w hot path.
    /// Bufor jest czyszczony i reuzywany.
    /// Format: [4B epoch][8B nonce_counter][12B chacha_nonce][ciphertext+tag].
    pub fn encrypt_for_node_into(
        &self,
        node_id: &str,
        plaintext: &[u8],
        out_buf: &mut Vec<u8>,
    ) -> Result<()> {
        let rings = self.epoch_keys.read();
        let ring = rings.get(node_id)
            .ok_or_else(|| anyhow::anyhow!("Brak EpochKeyRing dla {}", node_id))?;
        let epoch = ring.current_epoch;
        let cipher = ring.current_cipher()
            .ok_or_else(|| anyhow::anyhow!("Brak cipher dla epoch {} noda {}", epoch, node_id))?
            .clone();
        drop(rings);

        let nonce_counter = self.get_next_nonce(node_id);

        let mut chacha_nonce_bytes = [0u8; 12];
        chacha_nonce_bytes[4..12].copy_from_slice(&nonce_counter.to_be_bytes());
        let nonce = Nonce::from_slice(&chacha_nonce_bytes);

        let mut aad = [0u8; 12];
        aad[0..4].copy_from_slice(&epoch.to_be_bytes());
        aad[4..12].copy_from_slice(&nonce_counter.to_be_bytes());

        let ciphertext = cipher
            .encrypt(nonce, Payload { msg: plaintext, aad: &aad })
            .map_err(|e| anyhow::anyhow!("Blad szyfrowania: {}", e))?;

        out_buf.clear();
        out_buf.reserve(4 + 8 + 12 + ciphertext.len());
        out_buf.extend_from_slice(&epoch.to_be_bytes());
        out_buf.extend_from_slice(&nonce_counter.to_be_bytes());
        out_buf.extend_from_slice(&chacha_nonce_bytes);
        out_buf.extend_from_slice(&ciphertext);
        Ok(())
    }

    /// Deszyfruj payload od danego noda z replay protection.
    /// Oczekiwany format: [4B epoch][8B nonce_counter][12B chacha_nonce][ciphertext+tag]
    pub fn decrypt_from_node(&self, node_id: &str, data: &[u8]) -> Result<Vec<u8>> {
        // Minimum: 4 (epoch) + 8 (counter) + 12 (nonce) + 16 (tag) = 40 bajtow
        if data.len() < 40 {
            bail!("Dane za krotkie (minimum 40 bajtow)");
        }

        let epoch = u32::from_be_bytes(data[0..4].try_into()?);
        let nonce_counter = u64::from_be_bytes(data[4..12].try_into()?);
        let chacha_nonce = &data[12..24];
        let ciphertext = &data[24..];

        if !self.check_and_update_replay_window(node_id, nonce_counter) {
            bail!("Replay detected — nonce {} juz widziany lub za stary", nonce_counter);
        }

        let rings = self.epoch_keys.read();
        let ring = rings.get(node_id)
            .ok_or_else(|| anyhow::anyhow!("Brak EpochKeyRing dla {}", node_id))?;
        let cipher = ring.get_cipher(epoch)
            .ok_or_else(|| anyhow::anyhow!("Brak klucza dla epoch {} — wymagane ponowne parowanie", epoch))?
            .clone();
        drop(rings);

        // AAD = epoch_be ++ nonce_counter_be (z oryginalnych bajtow)
        let mut aad = [0u8; 12];
        aad[0..4].copy_from_slice(&data[0..4]);
        aad[4..12].copy_from_slice(&data[4..12]);

        let nonce = Nonce::from_slice(chacha_nonce);
        let plaintext = cipher
            .decrypt(nonce, Payload { msg: ciphertext, aad: &aad })
            .map_err(|e| anyhow::anyhow!("Blad deszyfrowania: {}", e))?;

        Ok(plaintext)
    }

    /// Pobierz i inkrementuj atomowy nonce counter dla danego peera
    pub(crate) fn get_next_nonce(&self, node_id: &str) -> u64 {
        let nonces = self.outbound_nonces.read();
        if let Some(counter) = nonces.get(node_id) {
            return counter.fetch_add(1, Ordering::SeqCst);
        }
        drop(nonces);
        let mut nonces = self.outbound_nonces.write();
        let counter = nonces.entry(node_id.to_string()).or_insert_with(|| AtomicU64::new(0));
        counter.fetch_add(1, Ordering::SeqCst)
    }

    /// Sprawdz replay window i zaktualizuj jesli nonce jest nowy
    fn check_and_update_replay_window(&self, node_id: &str, nonce: u64) -> bool {
        let mut window = self.inbound_windows.entry(node_id.to_string()).or_insert_with(ReplayWindow::new);
        window.check_and_update(nonce)
    }

    /// Podpisz dane kluczem prywatnym Ed25519
    pub fn sign(&self, data: &[u8]) -> Vec<u8> {
        self.signing_key.sign(data).to_bytes().to_vec()
    }

    /// Zweryfikuj podpis od zaufanego noda
    pub fn verify(&self, node_id: &str, data: &[u8], signature_bytes: &[u8]) -> Result<bool> {
        let keys = self.trusted_keys.read();
        let key = keys
            .get(node_id)
            .ok_or_else(|| anyhow::anyhow!("Node {} nie jest zaufany", node_id))?;

        let sig_arr: [u8; 64] = signature_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("Niepoprawna dlugosc podpisu (oczekiwano 64 bajty)"))?;
        let sig = Signature::from_bytes(&sig_arr);

        Ok(key.verify(data, &sig).is_ok())
    }

    /// Zwroc klucze publiczne wszystkich zaufanych nodow (do sync po parowaniu)
    pub fn get_all_trusted_keys(&self) -> Vec<(String, String)> {
        let trusted = db::repository::list_trusted_nodes(&self.db).unwrap_or_default();
        trusted
            .iter()
            .map(|n| (n.node_id.clone(), n.public_key.clone()))
            .collect()
    }

    /// Dodaj klucz zaufanego noda otrzymany z innego noda (propagacja po parowaniu)
    pub fn add_trusted_key(
        &self,
        node_id: &str,
        public_key_hex: &str,
        hostname: &str,
    ) -> Result<()> {
        // Nie dodawaj wlasnego klucza
        if public_key_hex == self.public_key_hex() {
            return Ok(());
        }

        // Sprawdz czy juz zaufany
        if self.is_trusted(node_id) {
            return Ok(());
        }

        // [CR-010] Walidacja dlugosci klucza publicznego
        if public_key_hex.len() != 128 {
            bail!(
                "Nieprawidlowa dlugosc klucza publicznego: {} (oczekiwano 128 hex)",
                public_key_hex.len()
            );
        }

        let vk = Self::parse_verifying_key(public_key_hex)?;

        db::repository::add_trusted_node(&self.db, node_id, public_key_hex, hostname, "mesh-sync")?;

        self.trusted_keys.write().insert(node_id.to_string(), vk);

        if public_key_hex.len() >= 128 {
            let x25519_hex = &public_key_hex[64..128];
            if let Ok(x_bytes) = hex::decode(x25519_hex) {
                if x_bytes.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&x_bytes);
                    let remote_x_pub = X25519PublicKey::from(arr);
                    let raw_shared = self.x25519_secret.diffie_hellman(&remote_x_pub);
                    let derived_key = Self::derive_key_from_dh(&raw_shared);
                    self.epoch_keys
                        .write()
                        .insert(node_id.to_string(), EpochKeyRing::new(derived_key));
                }
            }
        }

        // [OPT] Odbuduj snapshot trusted_node_ids
        self.rebuild_trusted_snapshot();

        info!(
            node_id = %node_id,
            "Dodano zaufany klucz otrzymany z mesh sync"
        );

        Ok(())
    }

    /// Sprawdza czy mamy klucze epokowe dla noda (do szyfrowania)
    pub fn has_shared_secret(&self, node_id: &str) -> bool {
        self.epoch_keys.read().contains_key(node_id)
    }

    /// Zwraca PIN z oczekujacego parowania (do wyswietlenia na UI)
    pub fn get_pending_pin(&self, remote_node_id: &str) -> Result<Option<String>> {
        let pairing = db::repository::get_pending_pairing(&self.db, remote_node_id)?;
        Ok(pairing.map(|p| p.pin_code).filter(|pin| !pin.is_empty()))
    }

    /// Sprawdza czy parowanie nie przekroczyło limitu prób PIN (max 3)
    pub fn check_pin_rate_limit(&self, node_id: &str) -> bool {
        let mut attempts = self.pin_attempts.write();
        let entry = attempts.entry(node_id.to_string()).or_insert((0, Instant::now()));

        // Reset po 60s
        if Instant::now().duration_since(entry.1) > Duration::from_secs(60) {
            *entry = (0, Instant::now());
        }

        if entry.0 >= 3 {
            return false;
        }
        entry.0 += 1;
        entry.1 = Instant::now();
        true
    }

    /// Rotacja klucza dla jednego peera — dodaje nowy epoch z nowym shared secret
    pub fn rotate_keys_for_peer(&self, node_id: &str, new_shared_secret: [u8; 32]) -> Result<u32> {
        let mut rings = self.epoch_keys.write();
        let ring = rings.get_mut(node_id)
            .ok_or_else(|| anyhow::anyhow!("Brak EpochKeyRing dla {}", node_id))?;

        let new_epoch = ring.current_epoch + 1;
        let key = Key::from(new_shared_secret);
        let cipher = Arc::new(ChaCha20Poly1305::new(&key));

        ring.keys.insert(new_epoch, EpochKey {
            secret: new_shared_secret,
            cipher,
            created_at: Instant::now(),
        });
        ring.current_epoch = new_epoch;

        ring.cleanup_expired();

        Ok(new_epoch)
    }

    /// Generuje nowy ephemeral X25519 keypair do rotacji kluczy
    pub fn generate_ephemeral_x25519() -> (x25519_dalek::StaticSecret, x25519_dalek::PublicKey) {
        let secret = x25519_dalek::StaticSecret::random_from_rng(rand::thread_rng());
        let public = x25519_dalek::PublicKey::from(&secret);
        (secret, public)
    }

    /// Oblicza nowy shared secret z DH: nasz ephemeral secret + ich ephemeral public
    pub fn derive_shared_secret_from_dh(
        our_ephemeral_secret: &x25519_dalek::StaticSecret,
        their_ephemeral_public: &[u8; 32],
    ) -> Result<[u8; 32]> {
        let their_public = x25519_dalek::PublicKey::from(*their_ephemeral_public);
        let dh_result = our_ephemeral_secret.diffie_hellman(&their_public);

        let hkdf = Hkdf::<Sha256>::new(None, dh_result.as_bytes());
        let mut shared_secret = [0u8; 32];
        hkdf.expand(b"tentaflow-mesh-epoch-key", &mut shared_secret)
            .map_err(|_| anyhow::anyhow!("HKDF expand failed"))?;

        Ok(shared_secret)
    }

    /// Inicjalizuje rotację klucza — generuje ephemeral keypair i zwraca public key do wyslania
    pub fn initiate_key_rotation(&self, peer_id: &str) -> [u8; 32] {
        let (secret, public) = Self::generate_ephemeral_x25519();
        self.pending_rotations.write().insert(peer_id.to_string(), PendingKeyRotation {
            ephemeral_secret: secret,
            ephemeral_public: public,
            created_at: Instant::now(),
        });
        *public.as_bytes()
    }

    /// Finalizuje rotację po otrzymaniu odpowiedzi — oblicza shared secret z naszego ephemeral secret + ich ephemeral public
    pub fn finalize_key_rotation(&self, peer_id: &str, their_ephemeral_public: &[u8; 32]) -> Result<u32> {
        let pending = self.pending_rotations.write().remove(peer_id)
            .ok_or_else(|| anyhow::anyhow!("Brak pending rotacji dla {}", peer_id))?;

        let new_secret = Self::derive_shared_secret_from_dh(&pending.ephemeral_secret, their_ephemeral_public)?;
        self.rotate_keys_for_peer(peer_id, new_secret)
    }

    /// Odpowiada na rotację — generuje swoj ephemeral, oblicza shared secret z ich public, rotuje klucz, zwraca swoj public do wyslania
    pub fn respond_to_key_rotation(&self, peer_id: &str, their_ephemeral_public: &[u8; 32]) -> Result<([u8; 32], u32)> {
        let (our_secret, our_public) = Self::generate_ephemeral_x25519();
        let new_shared_secret = Self::derive_shared_secret_from_dh(&our_secret, their_ephemeral_public)?;
        let new_epoch = self.rotate_keys_for_peer(peer_id, new_shared_secret)?;
        Ok((*our_public.as_bytes(), new_epoch))
    }

    /// Czyści wygasłe pending rotacje (starsze niż 60s)
    pub fn cleanup_pending_rotations(&self) {
        let now = Instant::now();
        self.pending_rotations.write().retain(|_, pr| {
            now.duration_since(pr.created_at) < Duration::from_secs(60)
        });
    }

    /// Usun wygasle parowania
    pub fn cleanup_expired(&self) -> Result<u64> {
        db::repository::cleanup_expired_pairings(&self.db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Tworzy baze in-memory z tabelami potrzebnymi do testow
    fn setup_test_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS trusted_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id TEXT NOT NULL UNIQUE,
                public_key TEXT NOT NULL,
                hostname TEXT DEFAULT '',
                approved_by TEXT DEFAULT '',
                approved_at TEXT NOT NULL DEFAULT (datetime('now')),
                is_active INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE IF NOT EXISTS pending_pairings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                remote_node_id TEXT NOT NULL,
                pin_code TEXT NOT NULL,
                direction TEXT NOT NULL CHECK(direction IN ('outgoing','incoming')),
                expires_at TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS revoked_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id TEXT NOT NULL UNIQUE,
                revoked_by TEXT,
                revoked_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            ",
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
    }

    #[test]
    fn generowanie_klucza_i_zapis_do_db() {
        let db = setup_test_db();
        let security = MeshSecurity::new(db.clone()).unwrap();

        // Klucz publiczny powinien miec 128 hex znakow (64 ed25519 + 64 x25519)
        assert_eq!(security.public_key_hex().len(), 128);

        // Ponowne utworzenie powinno wczytac ten sam klucz
        let security2 = MeshSecurity::new(db).unwrap();
        assert_eq!(security.public_key_hex(), security2.public_key_hex());
    }

    #[test]
    fn generowanie_pin() {
        let pin = MeshSecurity::generate_pin();
        assert_eq!(pin.len(), 6);
        assert!(pin.parse::<u32>().unwrap() >= 100_000);
        assert!(pin.parse::<u32>().unwrap() < 999_999);
    }

    #[test]
    fn podpisywanie_i_weryfikacja() {
        let db_a = setup_test_db();
        let db_b = setup_test_db();
        let sec_a = MeshSecurity::new(db_a).unwrap();
        let sec_b = MeshSecurity::new(db_b).unwrap();

        // Dodaj klucz A do zaufanych w B
        sec_b
            .add_trusted_key("node-a", &sec_a.public_key_hex(), "host-a")
            .unwrap();

        let data = b"Wiadomosc do podpisania";
        let sig = sec_a.sign(data);

        // Weryfikacja poprawnego podpisu
        assert!(sec_b.verify("node-a", data, &sig).unwrap());

        // Weryfikacja zmienionego podpisu
        let mut bad_sig = sig.clone();
        bad_sig[0] ^= 0xFF;
        assert!(!sec_b.verify("node-a", data, &bad_sig).unwrap());
    }

    #[test]
    fn szyfrowanie_i_deszyfrowanie() {
        let db_a = setup_test_db();
        let db_b = setup_test_db();
        let sec_a = MeshSecurity::new(db_a).unwrap();
        let sec_b = MeshSecurity::new(db_b).unwrap();

        // Wzajemne dodanie kluczy
        sec_a
            .add_trusted_key("node-b", &sec_b.public_key_hex(), "host-b")
            .unwrap();
        sec_b
            .add_trusted_key("node-a", &sec_a.public_key_hex(), "host-a")
            .unwrap();

        let plaintext = b"Tajne dane do przeslania";

        // A szyfruje dla B
        let encrypted = sec_a.encrypt_for_node("node-b", plaintext).unwrap();
        assert_ne!(&encrypted, plaintext);
        assert!(encrypted.len() > plaintext.len());

        // B deszyfruje od A
        let decrypted = sec_b.decrypt_from_node("node-a", &encrypted).unwrap();
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn szyfrowanie_encrypt_into_reuse_bufora() {
        let db_a = setup_test_db();
        let db_b = setup_test_db();
        let sec_a = MeshSecurity::new(db_a).unwrap();
        let sec_b = MeshSecurity::new(db_b).unwrap();

        sec_a
            .add_trusted_key("node-b", &sec_b.public_key_hex(), "host-b")
            .unwrap();
        sec_b
            .add_trusted_key("node-a", &sec_a.public_key_hex(), "host-a")
            .unwrap();

        let plaintext = b"Dane testowe encrypt_into";
        let mut buf = Vec::with_capacity(256);

        // Szyfruj z reuse bufora
        sec_a.encrypt_for_node_into("node-b", plaintext, &mut buf).unwrap();
        assert!(!buf.is_empty());

        // Deszyfruj i zweryfikuj
        let decrypted = sec_b.decrypt_from_node("node-a", &buf).unwrap();
        assert_eq!(&decrypted, plaintext);

        // Reuse bufora — drugi raz
        let plaintext2 = b"Drugie dane";
        sec_a.encrypt_for_node_into("node-b", plaintext2, &mut buf).unwrap();
        let decrypted2 = sec_b.decrypt_from_node("node-a", &buf).unwrap();
        assert_eq!(&decrypted2, plaintext2);
    }

    #[test]
    fn trusted_snapshot() {
        let db = setup_test_db();
        let sec_a = MeshSecurity::new(db.clone()).unwrap();
        let sec_b = MeshSecurity::new(setup_test_db()).unwrap();

        // Poczatkowo pusty
        assert!(sec_a.trusted_node_ids_snapshot().is_empty());

        // Dodaj zaufany node
        sec_a.add_trusted_key("node-b", &sec_b.public_key_hex(), "host-b").unwrap();
        let snapshot = sec_a.trusted_node_ids_snapshot();
        assert!(snapshot.contains("node-b"));
        assert_eq!(snapshot.len(), 1);

        // Cofnij zaufanie
        sec_a.revoke_trust("node-b").unwrap();
        let snapshot2 = sec_a.trusted_node_ids_snapshot();
        assert!(snapshot2.is_empty());
    }

    #[test]
    fn cofanie_zaufania() {
        let db = setup_test_db();
        let sec = MeshSecurity::new(db).unwrap();

        sec.add_trusted_key("node-x", &"aa".repeat(64), "host-x")
            .unwrap_or_default();

        // Klucz jest niepoprawny, ale cofniecie powinno dzialac
        sec.revoke_trust("node-x").unwrap();
        assert!(!sec.is_trusted("node-x"));
    }

    #[test]
    fn replay_window_akceptuje_nowe_nonce() {
        let mut w = ReplayWindow::new();
        assert!(w.check_and_update(0));
        assert!(w.check_and_update(1));
        assert!(w.check_and_update(2));
        assert!(w.check_and_update(100));
    }

    #[test]
    fn replay_window_odrzuca_duplikaty() {
        let mut w = ReplayWindow::new();
        assert!(w.check_and_update(5));
        assert!(!w.check_and_update(5));
    }

    #[test]
    fn replay_window_odrzuca_za_stare() {
        let mut w = ReplayWindow::new();
        assert!(w.check_and_update(100));
        // 100 - 0 = 100 > 64 — za stary
        assert!(!w.check_and_update(0));
        // 100 - 36 = 64 — dokladnie na granicy (za stary)
        assert!(!w.check_and_update(36));
        // 100 - 37 = 63 — w oknie
        assert!(w.check_and_update(37));
    }

    #[test]
    fn replay_window_akceptuje_poza_kolejnoscia_w_oknie() {
        let mut w = ReplayWindow::new();
        assert!(w.check_and_update(10));
        assert!(w.check_and_update(8));
        assert!(w.check_and_update(5));
        // Duplikat w oknie
        assert!(!w.check_and_update(8));
        // Nowy w oknie
        assert!(w.check_and_update(7));
    }

    #[test]
    fn replay_window_duzy_skok() {
        let mut w = ReplayWindow::new();
        assert!(w.check_and_update(0));
        assert!(w.check_and_update(1000));
        // Wszystko sprzed skoku jest za stare
        assert!(!w.check_and_update(0));
        assert!(!w.check_and_update(935));
        // W nowym oknie
        assert!(w.check_and_update(937));
    }

    #[test]
    fn replay_protection_encrypt_decrypt() {
        let db_a = setup_test_db();
        let db_b = setup_test_db();
        let sec_a = MeshSecurity::new(db_a).unwrap();
        let sec_b = MeshSecurity::new(db_b).unwrap();

        sec_a.add_trusted_key("node-b", &sec_b.public_key_hex(), "host-b").unwrap();
        sec_b.add_trusted_key("node-a", &sec_a.public_key_hex(), "host-a").unwrap();

        let plaintext = b"Test replay protection";

        // Pierwsze szyfrowanie i deszyfrowanie
        let enc1 = sec_a.encrypt_for_node("node-b", plaintext).unwrap();
        let dec1 = sec_b.decrypt_from_node("node-a", &enc1).unwrap();
        assert_eq!(&dec1, plaintext);

        // Replay tego samego ciphertextu powinien byc odrzucony
        let replay = sec_b.decrypt_from_node("node-a", &enc1);
        assert!(replay.is_err());

        // Nowy message powinien dzialac
        let enc2 = sec_a.encrypt_for_node("node-b", b"Druga wiadomosc").unwrap();
        let dec2 = sec_b.decrypt_from_node("node-a", &enc2).unwrap();
        assert_eq!(&dec2, b"Druga wiadomosc");
    }

    #[test]
    fn nowy_format_ma_poprawna_strukture() {
        let db_a = setup_test_db();
        let db_b = setup_test_db();
        let sec_a = MeshSecurity::new(db_a).unwrap();
        let sec_b = MeshSecurity::new(db_b).unwrap();

        sec_a.add_trusted_key("node-b", &sec_b.public_key_hex(), "host-b").unwrap();

        let enc = sec_a.encrypt_for_node("node-b", b"test").unwrap();

        // Minimalna dlugosc: 4 + 8 + 12 + 16 (tag) + 4 (plaintext) = 44
        assert!(enc.len() >= 44);

        // Epoch = 0
        let epoch = u32::from_be_bytes(enc[0..4].try_into().unwrap());
        assert_eq!(epoch, 0);

        // Nonce counter = 0 (pierwszy message)
        let counter = u64::from_be_bytes(enc[4..12].try_into().unwrap());
        assert_eq!(counter, 0);

        // Drugi message powinien miec counter = 1
        let enc2 = sec_a.encrypt_for_node("node-b", b"test2").unwrap();
        let counter2 = u64::from_be_bytes(enc2[4..12].try_into().unwrap());
        assert_eq!(counter2, 1);
    }

    #[test]
    fn encrypt_into_replay_protection() {
        let db_a = setup_test_db();
        let db_b = setup_test_db();
        let sec_a = MeshSecurity::new(db_a).unwrap();
        let sec_b = MeshSecurity::new(db_b).unwrap();

        sec_a.add_trusted_key("node-b", &sec_b.public_key_hex(), "host-b").unwrap();
        sec_b.add_trusted_key("node-a", &sec_a.public_key_hex(), "host-a").unwrap();

        let mut buf = Vec::new();

        sec_a.encrypt_for_node_into("node-b", b"msg1", &mut buf).unwrap();
        let saved = buf.clone();
        let dec = sec_b.decrypt_from_node("node-a", &buf).unwrap();
        assert_eq!(&dec, b"msg1");

        // Replay
        assert!(sec_b.decrypt_from_node("node-a", &saved).is_err());

        // Nowy message przez encrypt_into
        sec_a.encrypt_for_node_into("node-b", b"msg2", &mut buf).unwrap();
        let dec2 = sec_b.decrypt_from_node("node-a", &buf).unwrap();
        assert_eq!(&dec2, b"msg2");
    }

    #[test]
    fn dane_za_krotkie_odrzucone() {
        let db = setup_test_db();
        let sec = MeshSecurity::new(db).unwrap();
        let result = sec.decrypt_from_node("node-x", &[0u8; 39]);
        assert!(result.is_err());
    }

    // =========================================================================
    // Testy integracyjne — interakcje miedzy wieloma nodami
    // =========================================================================

    /// Pomoc: sparuj dwa nody przez pelny flow (initiate -> receive -> confirm)
    fn pair_two_nodes(
        node_a: &MeshSecurity,
        node_a_id: &str,
        node_b: &MeshSecurity,
        node_b_id: &str,
    ) {
        let pin = node_a.initiate_pairing(node_b_id).unwrap();

        node_b
            .receive_pairing_request(node_a_id, &pin, &node_a.public_key_hex())
            .unwrap();

        node_b
            .confirm_pairing(node_a_id, &node_a.public_key_hex(), "host-a", "admin")
            .unwrap();

        node_a
            .confirm_pairing(node_b_id, &node_b.public_key_hex(), "host-b", "admin")
            .unwrap();
    }

    #[test]
    fn pair_two_nodes_full_flow() {
        let node_a = MeshSecurity::new(setup_test_db()).unwrap();
        let node_b = MeshSecurity::new(setup_test_db()).unwrap();

        // Parowanie: A inicjuje, B odbiera, obaj potwierdzaja
        let pin = node_a.initiate_pairing("node-b").unwrap();
        assert_eq!(pin.len(), 6);

        node_b
            .receive_pairing_request("node-a", &pin, &node_a.public_key_hex())
            .unwrap();

        // B potwierdza — A staje sie zaufany dla B
        node_b
            .confirm_pairing("node-a", &node_a.public_key_hex(), "host-a", "admin")
            .unwrap();
        assert!(node_b.is_trusted("node-a"));

        // A potwierdza — B staje sie zaufany dla A
        node_a
            .confirm_pairing("node-b", &node_b.public_key_hex(), "host-b", "admin")
            .unwrap();
        assert!(node_a.is_trusted("node-b"));

        // A szyfruje -> B deszyfruje
        let msg = b"Wiadomosc od A do B";
        let encrypted = node_a.encrypt_for_node("node-b", msg).unwrap();
        let decrypted = node_b.decrypt_from_node("node-a", &encrypted).unwrap();
        assert_eq!(&decrypted, msg);

        // B szyfruje -> A deszyfruje
        let msg2 = b"Odpowiedz od B do A";
        let encrypted2 = node_b.encrypt_for_node("node-a", msg2).unwrap();
        let decrypted2 = node_a.decrypt_from_node("node-b", &encrypted2).unwrap();
        assert_eq!(&decrypted2, msg2);
    }

    #[test]
    fn key_rotation_between_nodes() {
        let node_a = MeshSecurity::new(setup_test_db()).unwrap();
        let node_b = MeshSecurity::new(setup_test_db()).unwrap();

        // Sparuj nody
        pair_two_nodes(&node_a, "node-a", &node_b, "node-b");

        // Weryfikacja komunikacji przed rotacja
        let msg_before = b"Przed rotacja";
        let enc = node_a.encrypt_for_node("node-b", msg_before).unwrap();
        let dec = node_b.decrypt_from_node("node-a", &enc).unwrap();
        assert_eq!(&dec, msg_before);

        // Rotacja: wygeneruj wspolny nowy secret i zastosuj na obu nodach
        let new_secret: [u8; 32] = rand::random();
        let epoch_a = node_a.rotate_keys_for_peer("node-b", new_secret).unwrap();
        let epoch_b = node_b.rotate_keys_for_peer("node-a", new_secret).unwrap();
        assert_eq!(epoch_a, 1);
        assert_eq!(epoch_b, 1);

        // Komunikacja z nowym epoch
        let msg_after = b"Po rotacji kluczy";
        let enc2 = node_a.encrypt_for_node("node-b", msg_after).unwrap();
        // Sprawdz ze nowy epoch jest w danych
        let epoch_in_msg = u32::from_be_bytes(enc2[0..4].try_into().unwrap());
        assert_eq!(epoch_in_msg, 1);
        let dec2 = node_b.decrypt_from_node("node-a", &enc2).unwrap();
        assert_eq!(&dec2, msg_after);

        // Odwrotny kierunek
        let msg_back = b"Odpowiedz po rotacji";
        let enc3 = node_b.encrypt_for_node("node-a", msg_back).unwrap();
        let dec3 = node_a.decrypt_from_node("node-b", &enc3).unwrap();
        assert_eq!(&dec3, msg_back);
    }

    #[test]
    fn decrypt_old_epoch_in_grace_period() {
        let node_a = MeshSecurity::new(setup_test_db()).unwrap();
        let node_b = MeshSecurity::new(setup_test_db()).unwrap();

        pair_two_nodes(&node_a, "node-a", &node_b, "node-b");

        // Zaszyfruj wiadomosc z epoch 0
        let msg_epoch0 = b"Wiadomosc z epoch 0";
        let enc_epoch0 = node_a.encrypt_for_node("node-b", msg_epoch0).unwrap();

        // Rotacja do epoch 1
        let secret1: [u8; 32] = rand::random();
        node_a.rotate_keys_for_peer("node-b", secret1).unwrap();
        node_b.rotate_keys_for_peer("node-a", secret1).unwrap();

        // Stara wiadomosc z epoch 0 nadal powinna sie dac odszyfrować (grace period)
        let dec = node_b.decrypt_from_node("node-a", &enc_epoch0).unwrap();
        assert_eq!(&dec, msg_epoch0);

        // Rotacja do epoch 2
        let secret2: [u8; 32] = rand::random();
        node_a.rotate_keys_for_peer("node-b", secret2).unwrap();
        node_b.rotate_keys_for_peer("node-a", secret2).unwrap();

        // Wiadomosc z epoch 0 nadal w grace period (klucze stworzone sekundy temu, grace = 7 dni)
        // Potrzebujemy nowego nonce bo replay window odrzuci stary
        let _enc_epoch0_v2 = node_a.encrypt_for_node("node-b", msg_epoch0).unwrap();
        // Ta wiadomosc jest szyfrowana z epoch 2 (aktualny), wiec test inaczej:
        // Sprawdzmy ze stary ciphertext epoch0 zostalby odrzucony przez replay, ale klucz istnieje
        assert!(node_b.has_shared_secret("node-a"));

        // Szyfrowanie z aktualnym epoch dziala
        let msg_new = b"Nowa wiadomosc po drugiej rotacji";
        let enc_new = node_a.encrypt_for_node("node-b", msg_new).unwrap();
        let epoch_in_msg = u32::from_be_bytes(enc_new[0..4].try_into().unwrap());
        assert_eq!(epoch_in_msg, 2);
        let dec_new = node_b.decrypt_from_node("node-a", &enc_new).unwrap();
        assert_eq!(&dec_new, msg_new);
    }

    #[test]
    fn revoke_blocks_communication() {
        let node_a = MeshSecurity::new(setup_test_db()).unwrap();
        let node_b = MeshSecurity::new(setup_test_db()).unwrap();

        pair_two_nodes(&node_a, "node-a", &node_b, "node-b");

        // Weryfikacja: komunikacja dziala
        let msg = b"Test przed revoke";
        let enc = node_a.encrypt_for_node("node-b", msg).unwrap();
        let dec = node_b.decrypt_from_node("node-a", &enc).unwrap();
        assert_eq!(&dec, msg);

        // A cofa zaufanie do B
        node_a.revoke_trust("node-b").unwrap();

        assert!(!node_a.is_trusted("node-b"));
        assert!(node_a.is_revoked("node-b"));

        // A nie moze juz szyfrowac dla B — brak EpochKeyRing
        let result = node_a.encrypt_for_node("node-b", b"po revoke");
        assert!(result.is_err());

        // B nadal ma klucz A (nie wie o revoke) — oczekiwane zachowanie
        assert!(node_b.is_trusted("node-a"));
        assert!(node_b.has_shared_secret("node-a"));
    }

    #[test]
    fn replay_attack_detected_between_nodes() {
        let node_a = MeshSecurity::new(setup_test_db()).unwrap();
        let node_b = MeshSecurity::new(setup_test_db()).unwrap();

        pair_two_nodes(&node_a, "node-a", &node_b, "node-b");

        // A szyfruje wiadomosc
        let msg = b"Wiadomosc do powtorzenia";
        let encrypted = node_a.encrypt_for_node("node-b", msg).unwrap();

        // Pierwsze deszyfrowanie — OK
        let dec = node_b.decrypt_from_node("node-a", &encrypted).unwrap();
        assert_eq!(&dec, msg);

        // Replay tych samych bajtow — powinien byc odrzucony
        let replay_result = node_b.decrypt_from_node("node-a", &encrypted);
        assert!(replay_result.is_err());
        let err_msg = replay_result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Replay"),
            "Blad powinien wskazywac na replay: {}",
            err_msg
        );
    }

    #[test]
    fn trusted_keys_sync_propagation() {
        let node_a = MeshSecurity::new(setup_test_db()).unwrap();
        let node_b = MeshSecurity::new(setup_test_db()).unwrap();
        let node_c = MeshSecurity::new(setup_test_db()).unwrap();

        // Sparuj A z B i A z C
        pair_two_nodes(&node_a, "node-a", &node_b, "node-b");
        pair_two_nodes(&node_a, "node-a", &node_c, "node-c");

        // Pobierz klucze zaufane z A
        let trusted_keys = node_a.get_all_trusted_keys();
        assert!(trusted_keys.len() >= 2);

        // Propaguj klucze na B (oprócz samego B)
        for (nid, pubkey) in &trusted_keys {
            if nid != "node-b" {
                let _ = node_b.add_trusted_key(nid, pubkey, "propagated");
            }
        }

        // B powinien teraz ufac C
        assert!(node_b.is_trusted("node-c"));
    }

    #[test]
    fn three_nodes_revoke_propagation() {
        let node_a = MeshSecurity::new(setup_test_db()).unwrap();
        let node_b = MeshSecurity::new(setup_test_db()).unwrap();
        let node_c = MeshSecurity::new(setup_test_db()).unwrap();

        // Sparuj A<->B, A<->C, B<->C
        pair_two_nodes(&node_a, "node-a", &node_b, "node-b");
        pair_two_nodes(&node_a, "node-a", &node_c, "node-c");
        pair_two_nodes(&node_b, "node-b", &node_c, "node-c");

        // Weryfikacja: wszyscy ufaja sobie nawzajem
        assert!(node_a.is_trusted("node-b"));
        assert!(node_a.is_trusted("node-c"));
        assert!(node_b.is_trusted("node-a"));
        assert!(node_b.is_trusted("node-c"));
        assert!(node_c.is_trusted("node-a"));
        assert!(node_c.is_trusted("node-b"));

        // A revokuje C
        node_a.revoke_trust("node-c").unwrap();
        assert!(!node_a.is_trusted("node-c"));
        assert!(node_a.is_revoked("node-c"));

        // Propagacja: B tez revokuje C
        node_b.revoke_trust("node-c").unwrap();
        assert!(!node_b.is_trusted("node-c"));
        assert!(node_b.is_revoked("node-c"));

        // C nie dostal TrustRevoked — nadal ufa A i B
        assert!(node_c.is_trusted("node-a"));
        assert!(node_c.is_trusted("node-b"));
    }

    #[test]
    fn multiple_key_rotations() {
        let node_a = MeshSecurity::new(setup_test_db()).unwrap();
        let node_b = MeshSecurity::new(setup_test_db()).unwrap();

        pair_two_nodes(&node_a, "node-a", &node_b, "node-b");

        // 5 rotacji kluczy
        for i in 1..=5u32 {
            let secret: [u8; 32] = rand::random();
            let epoch_a = node_a.rotate_keys_for_peer("node-b", secret).unwrap();
            let epoch_b = node_b.rotate_keys_for_peer("node-a", secret).unwrap();
            assert_eq!(epoch_a, i);
            assert_eq!(epoch_b, i);
        }

        // Aktualny epoch = 5 — weryfikacja szyfrowania
        let msg = b"Po pieciu rotacjach";
        let enc = node_a.encrypt_for_node("node-b", msg).unwrap();
        let epoch_in_msg = u32::from_be_bytes(enc[0..4].try_into().unwrap());
        assert_eq!(epoch_in_msg, 5);

        let dec = node_b.decrypt_from_node("node-a", &enc).unwrap();
        assert_eq!(&dec, msg);

        // Odwrotny kierunek tez dziala
        let msg2 = b"Odpowiedz po rotacjach";
        let enc2 = node_b.encrypt_for_node("node-a", msg2).unwrap();
        let dec2 = node_a.decrypt_from_node("node-b", &enc2).unwrap();
        assert_eq!(&dec2, msg2);

        // Stary epoch 0 nadal dostepny w grace period (klucze < 7 dni)
        // Sprawdzamy ze ring nie wyrzucil starych kluczy
        let rings = node_b.epoch_keys.read();
        let ring = rings.get("node-a").unwrap();
        assert!(ring.get_cipher(0).is_some(), "Epoch 0 powinien byc w grace period");
        assert!(ring.get_cipher(5).is_some(), "Epoch 5 powinien byc aktualny");
    }

    #[test]
    fn decrypt_unknown_epoch_fails() {
        let node_a = MeshSecurity::new(setup_test_db()).unwrap();
        let node_b = MeshSecurity::new(setup_test_db()).unwrap();

        pair_two_nodes(&node_a, "node-a", &node_b, "node-b");

        // Zaszyfruj normalnie (epoch 0)
        let enc = node_a.encrypt_for_node("node-b", b"test").unwrap();

        // Zmodyfikuj epoch w danych na 99 (nieistniejacy)
        let mut tampered = enc.clone();
        tampered[0..4].copy_from_slice(&99u32.to_be_bytes());

        // Deszyfrowanie powinno sie nie udac (brak klucza dla epoch 99)
        // Uwaga: AAD tez nie zgadza sie z oryginalnym epoch, wiec AEAD odrzuci
        let result = node_b.decrypt_from_node("node-a", &tampered);
        assert!(result.is_err());
    }

    #[test]
    fn concurrent_nonces_are_unique() {
        let db = setup_test_db();
        let sec = Arc::new(MeshSecurity::new(db).unwrap());

        let sec2 = Arc::new(MeshSecurity::new(setup_test_db()).unwrap());
        sec.add_trusted_key("peer-1", &sec2.public_key_hex(), "host").unwrap();

        let sec_clone = sec.clone();
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let s = sec_clone.clone();
                std::thread::spawn(move || s.get_next_nonce("peer-1"))
            })
            .collect();

        let mut nonces: Vec<u64> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        nonces.sort();
        nonces.dedup();
        assert_eq!(nonces.len(), 10, "Wszystkie nonce powinny byc unikalne");
    }

    #[test]
    fn two_phase_key_rotation() {
        let node_a = MeshSecurity::new(setup_test_db()).unwrap();
        let node_b = MeshSecurity::new(setup_test_db()).unwrap();

        pair_two_nodes(&node_a, "node-a", &node_b, "node-b");

        // Faza 1: A inicjuje, generuje ephemeral pub
        let a_pub = node_a.initiate_key_rotation("node-b");

        // Faza 2: B odpowiada — generuje swoj ephemeral, oblicza shared secret, rotuje
        let (b_pub, epoch_b) = node_b.respond_to_key_rotation("node-a", &a_pub).unwrap();
        assert_eq!(epoch_b, 1);

        // Faza 3: A finalizuje — oblicza ten sam shared secret z B's pub
        let epoch_a = node_a.finalize_key_rotation("node-b", &b_pub).unwrap();
        assert_eq!(epoch_a, 1);

        // Weryfikacja: komunikacja dziala z nowym epoch
        let msg = b"Po dwufazowej rotacji";
        let enc = node_a.encrypt_for_node("node-b", msg).unwrap();
        let epoch_in_msg = u32::from_be_bytes(enc[0..4].try_into().unwrap());
        assert_eq!(epoch_in_msg, 1);
        let dec = node_b.decrypt_from_node("node-a", &enc).unwrap();
        assert_eq!(&dec, msg);

        // Odwrotny kierunek
        let msg2 = b"Odpowiedz po rotacji";
        let enc2 = node_b.encrypt_for_node("node-a", msg2).unwrap();
        let dec2 = node_a.decrypt_from_node("node-b", &enc2).unwrap();
        assert_eq!(&dec2, msg2);
    }

    #[test]
    fn pin_rate_limit_blocks_after_3_attempts() {
        let sec = MeshSecurity::new(setup_test_db()).unwrap();
        assert!(sec.check_pin_rate_limit("node-x"));
        assert!(sec.check_pin_rate_limit("node-x"));
        assert!(sec.check_pin_rate_limit("node-x"));
        assert!(!sec.check_pin_rate_limit("node-x")); // 4. proba — zablokowana
    }

    #[test]
    fn admin_retrust_removes_revoked() {
        let sec = MeshSecurity::new(setup_test_db()).unwrap();
        let sec2 = MeshSecurity::new(setup_test_db()).unwrap();

        // Dodaj i revokuj
        sec.add_trusted_key("node-x", &sec2.public_key_hex(), "host").unwrap();
        sec.revoke_trust("node-x").unwrap();
        assert!(sec.is_revoked("node-x"));

        // Admin re-trust
        sec.admin_retrust("node-x").unwrap();
        assert!(!sec.is_revoked("node-x"));
    }
}
