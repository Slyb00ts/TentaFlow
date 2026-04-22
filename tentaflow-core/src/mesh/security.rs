// =============================================================================
// Plik: mesh/security.rs
// Opis: Tozsamosc i zaufanie mesh. Ed25519 keypair persistentny w DB (klucz
//       prywatny szyfrowany SettingsCipher), X25519 jako drugi klucz uzywany
//       przy pairing handshake. Trzyma zbior zaufanych peerow (`trusted_keys`),
//       tagi revoke oraz wpisy rate limit PIN. Calosc szyfrowania transportu
//       jest obowiazkiem iroh TLS — ten modul nie wrapuje payloadow AEAD.
// =============================================================================

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use parking_lot::RwLock;
use rand::RngExt;
use sha2::Sha256;
use tracing::{info, warn};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

use crate::db::{self, DbPool};

/// Klucz publiczny noda = konkatenacja Ed25519 (32 B, 64 hex) + X25519 (32 B,
/// 64 hex) = 128 hex znakow. Ed25519 sluzy do podpisow i tozsamosci transport
/// layer (w przyszlosci = iroh NodeId). X25519 uzywany do derywacji pairing
/// proof (HKDF(ECDH(our_x25519, remote_x25519))).
pub const PUBLIC_KEY_HEX_LEN: usize = 128;

/// Zarzadca tozsamosci i zaufania mesh.
pub struct MeshSecurity {
    /// Klucz prywatny tego noda (Ed25519).
    signing_key: SigningKey,
    /// Klucz publiczny tego noda (Ed25519).
    pub verifying_key: VerifyingKey,
    /// Klucz prywatny X25519 — wykorzystywany w pairing handshake do
    /// wyprowadzenia wspolnego sekretu uzywanego jako material dla `pin_proof`.
    x25519_secret: StaticSecret,
    /// Klucz publiczny X25519 (do wymiany w pairingu).
    x25519_public: X25519PublicKey,
    /// Zaufane nody: node_id -> klucz publiczny Ed25519.
    trusted_keys: RwLock<HashMap<String, VerifyingKey>>,
    /// Snapshot zaufanych `node_id` jako `Arc<HashSet>` — odbudowywany przy
    /// kazdej zmianie trusted_keys. Pozwala na batch trust check bez lockow.
    trusted_node_ids: RwLock<Arc<HashSet<String>>>,
    /// Aktywnie cofniete zaufanie — wypelniane przez `revoke_trust`.
    revoked_nodes: RwLock<HashSet<String>>,
    /// Nody w trakcie revoke/unpair — synchronicznie ustawiane przed async
    /// broadcastem TrustRevoked.
    revoking_nodes: RwLock<HashSet<String>>,
    /// Rate limit prob PIN per node_id: (count, last_attempt).
    pin_attempts: RwLock<HashMap<String, (u32, Instant)>>,
    /// Aktywne zaproszenie QR — (pin, expiry). Jeden naraz, rotowany co 60s.
    /// Uzywany w flow: nod A pokazuje QR z (hex, pin), nod B skanuje i inicjuje
    /// parowanie uzywajac tego pinu. Nod A gdy dostanie PairingRequest z tym
    /// pinem → auto-confirm bez user-confirmu.
    invite: RwLock<Option<(String, Instant)>>,
    /// Pool bazy danych.
    pub db: DbPool,
    /// Szyfr do szyfrowania kluczy prywatnych w `settings`.
    settings_cipher: Arc<crate::crypto::SettingsCipher>,
}

impl MeshSecurity {
    /// Tworzy lub wczytuje keypair z bazy danych. Ed25519 zapisany w `settings`
    /// pod kluczem `node_private_key` (szyfrowany SettingsCipher). X25519
    /// analogicznie pod `node_x25519_private_key`.
    pub fn new(db: DbPool, settings_cipher: Arc<crate::crypto::SettingsCipher>) -> Result<Self> {
        let (signing_key, x25519_secret) = Self::load_or_generate_keys(&db, &settings_cipher)?;
        let verifying_key = signing_key.verifying_key();
        let x25519_public = X25519PublicKey::from(&x25519_secret);

        let security = Self {
            signing_key,
            verifying_key,
            x25519_secret,
            x25519_public,
            trusted_keys: RwLock::new(HashMap::new()),
            trusted_node_ids: RwLock::new(Arc::new(HashSet::new())),
            revoked_nodes: RwLock::new(HashSet::new()),
            revoking_nodes: RwLock::new(HashSet::new()),
            pin_attempts: RwLock::new(HashMap::new()),
            invite: RwLock::new(None),
            db,
            settings_cipher,
        };

        security.load_trusted_from_db()?;

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

    fn load_or_generate_keys(
        db: &DbPool,
        settings_cipher: &crate::crypto::SettingsCipher,
    ) -> Result<(SigningKey, StaticSecret)> {
        // Ed25519
        let ed_raw = db::repository::get_setting(db, "node_private_key")?;
        let signing_key = if let Some(stored) = ed_raw {
            let hex_str = settings_cipher
                .decrypt(&stored)
                .context("Blad deszyfrowania klucza Ed25519")?;
            let bytes = hex::decode(&hex_str).context("Nieprawidlowy hex klucza Ed25519")?;
            let key_bytes: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("Klucz Ed25519 ma niepoprawna dlugosc"))?;
            SigningKey::from_bytes(&key_bytes)
        } else {
            let key = SigningKey::generate(&mut rand_core_06::OsRng);
            let hex_str = hex::encode(key.to_bytes());
            db::repository::set_setting_secure(db, "node_private_key", &hex_str, settings_cipher)?;
            info!("Wygenerowano nowy klucz Ed25519 dla tego noda");
            key
        };

        // X25519
        let x_raw = db::repository::get_setting(db, "node_x25519_private_key")?;
        let x25519_secret = if let Some(stored) = x_raw {
            let hex_str = settings_cipher
                .decrypt(&stored)
                .context("Blad deszyfrowania klucza X25519")?;
            let bytes = hex::decode(&hex_str).context("Nieprawidlowy hex klucza X25519")?;
            let key_bytes: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("Klucz X25519 ma niepoprawna dlugosc"))?;
            StaticSecret::from(key_bytes)
        } else {
            let secret = StaticSecret::random_from_rng(&mut rand_core_06::OsRng);
            let hex_str = hex::encode(secret.to_bytes());
            db::repository::set_setting_secure(
                db,
                "node_x25519_private_key",
                &hex_str,
                settings_cipher,
            )?;
            info!("Wygenerowano nowy klucz X25519 dla tego noda");
            secret
        };

        Ok((signing_key, x25519_secret))
    }

    fn load_trusted_from_db(&self) -> Result<()> {
        let trusted = db::repository::list_trusted_nodes(&self.db)?;
        let mut keys = self.trusted_keys.write();

        for node in &trusted {
            match Self::parse_verifying_key(&node.public_key) {
                Ok(vk) => {
                    keys.insert(node.node_id.clone(), vk);
                }
                Err(e) => {
                    warn!(
                        node_id = %node.node_id,
                        "Nie udalo sie wczytac klucza publicznego: {}", e
                    );
                }
            }
        }

        let trusted_set: HashSet<String> = keys.keys().cloned().collect();
        drop(keys);
        *self.trusted_node_ids.write() = Arc::new(trusted_set);

        Ok(())
    }

    /// Parsuje Ed25519 public key z hex stringa (pierwsze 64 znaki hex).
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

    /// Odbudowuje snapshot `trusted_node_ids` po modyfikacji `trusted_keys`.
    fn rebuild_trusted_snapshot(&self) {
        let trusted_set: HashSet<String> = self.trusted_keys.read().keys().cloned().collect();
        *self.trusted_node_ids.write() = Arc::new(trusted_set);
    }

    /// Polaczony hex Ed25519 (64) + X25519 (64) = 128 znakow.
    pub fn public_key_hex(&self) -> String {
        let ed_hex = hex::encode(self.verifying_key.to_bytes());
        let x_hex = hex::encode(self.x25519_public.to_bytes());
        format!("{}{}", ed_hex, x_hex)
    }

    /// Sam Ed25519 public key jako 64-znakowy hex.
    pub fn ed25519_public_key_hex(&self) -> String {
        hex::encode(self.verifying_key.to_bytes())
    }

    /// Sam X25519 public key jako 64-znakowy hex.
    pub fn x25519_public_key_hex(&self) -> String {
        hex::encode(self.x25519_public.to_bytes())
    }

    /// Czy node jest zaufany? Sprawdza rowniez flage revoking.
    pub fn is_trusted(&self, node_id: &str) -> bool {
        if self.revoking_nodes.read().contains(node_id) {
            return false;
        }
        self.trusted_keys.read().contains_key(node_id)
    }

    /// Snapshot zaufanych node_id — jedno `Arc::clone` na heartbeat loop zamiast
    /// N lockow RwLock.
    pub fn trusted_node_ids_snapshot(&self) -> Arc<HashSet<String>> {
        Arc::clone(&self.trusted_node_ids.read())
    }

    // =========================================================================
    // Pairing
    // =========================================================================

    /// Generuje losowy 6-cyfrowy PIN.
    pub fn generate_pin() -> String {
        let pin: u32 = rand::rng().random_range(100_000..=999_999);
        format!("{:06}", pin)
    }

    /// Generuje (lub odswieza) QR invite PIN. Zwraca (pin, seconds_to_expiry).
    /// Expires po 60s — klient powinien co 50s odswiezac.
    pub fn generate_invite_pin(&self) -> (String, u32) {
        let pin = Self::generate_pin();
        let expiry = Instant::now() + Duration::from_secs(60);
        *self.invite.write() = Some((pin.clone(), expiry));
        (pin, 60)
    }

    /// Zwraca aktualny invite PIN (jesli wciaz wazny). Do sprawdzenia przez
    /// `handle_pairing_request` — jesli przychodzacy PIN matchuje, auto-confirm.
    pub fn peek_invite_pin(&self) -> Option<String> {
        let guard = self.invite.read();
        let (pin, expiry) = guard.as_ref()?;
        if Instant::now() < *expiry {
            Some(pin.clone())
        } else {
            None
        }
    }

    /// Skonsumuj invite PIN jesli matchuje — zapobiega reuse.
    pub fn consume_invite_pin(&self, candidate: &str) -> bool {
        let mut guard = self.invite.write();
        let matches = guard
            .as_ref()
            .map(|(p, exp)| p == candidate && Instant::now() < *exp)
            .unwrap_or(false);
        if matches {
            *guard = None;
        }
        matches
    }

    /// Backward-compat: stary `initiate_pairing` — generuje losowy PIN.
    pub fn initiate_pairing(&self, remote_node_id: &str) -> Result<String> {
        self.initiate_pairing_with_pin(remote_node_id, "")
    }

    /// Zapisuje zaproszenie z lokalnej strony (wygenerowany PIN) i zwraca go
    /// do wyswietlenia w UI. Gdy `pin_hint` niepusty — uzywamy go zamiast
    /// generowac nowy (flow QR scan: drugi nod ma PIN z QR invite).
    pub fn initiate_pairing_with_pin(
        &self,
        remote_node_id: &str,
        pin_hint: &str,
    ) -> Result<String> {
        let pending_count = db::repository::list_pending_pairings(&self.db)?;
        if pending_count.len() > 10 {
            bail!("Zbyt wiele oczekujacych parowan (max 10). Usun lub zatwierdz istniejace.");
        }

        if self.is_revoked(remote_node_id) {
            let _ = self.admin_retrust(remote_node_id);
        }

        self.pin_attempts.write().remove(remote_node_id);

        let pin = if !pin_hint.is_empty() && pin_hint.len() == 6 && pin_hint.chars().all(|c| c.is_ascii_digit()) {
            pin_hint.to_string()
        } else {
            Self::generate_pin()
        };
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

    /// Odbiera zadanie parowania od zdalnego noda i zapisuje jego klucz publiczny
    /// jako pending do czasu wprowadzenia PIN-u przez uzytkownika.
    pub fn receive_pairing_request(
        &self,
        remote_node_id: &str,
        pin: &str,
        remote_public_key: &str,
    ) -> Result<()> {
        let expires = chrono::Utc::now() + chrono::Duration::seconds(60);
        let expires_str = expires.format("%Y-%m-%d %H:%M:%S").to_string();

        if self.is_revoked(remote_node_id) {
            let _ = self.admin_retrust(remote_node_id);
        }

        self.pin_attempts.write().remove(remote_node_id);

        db::repository::create_pending_pairing(
            &self.db,
            remote_node_id,
            pin,
            "incoming",
            &expires_str,
        )?;

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

    /// Potwierdza parowanie: zapisuje klucz publiczny do `trusted_nodes`.
    /// Nie wyprowadza shared secret — transport iroh zapewnia szyfrowanie TLS.
    pub fn confirm_pairing(
        &self,
        remote_node_id: &str,
        remote_public_key_hex: &str,
        hostname: &str,
        approved_by: &str,
    ) -> Result<()> {
        let pending =
            db::repository::get_pending_pairing(&self.db, remote_node_id)?.ok_or_else(|| {
                anyhow::anyhow!("Brak oczekujacego parowania z nodem {}", remote_node_id)
            })?;

        let expires =
            chrono::NaiveDateTime::parse_from_str(&pending.expires_at, "%Y-%m-%d %H:%M:%S")
                .context("Blad parsowania daty wygasniecia")?;
        let now = chrono::Utc::now().naive_utc();
        if now > expires {
            db::repository::delete_pending_pairing(&self.db, remote_node_id)?;
            bail!("Parowanie wygaslo — wygeneruj nowy PIN");
        }

        if remote_public_key_hex.len() != PUBLIC_KEY_HEX_LEN {
            bail!(
                "Nieprawidlowa dlugosc klucza publicznego: {} (oczekiwano {})",
                remote_public_key_hex.len(),
                PUBLIC_KEY_HEX_LEN
            );
        }

        let vk = Self::parse_verifying_key(remote_public_key_hex)?;

        db::repository::add_trusted_node(
            &self.db,
            remote_node_id,
            remote_public_key_hex,
            hostname,
            approved_by,
        )?;

        self.trusted_keys
            .write()
            .insert(remote_node_id.to_string(), vk);

        db::repository::delete_pending_pairing(&self.db, remote_node_id)?;
        self.rebuild_trusted_snapshot();

        info!(
            remote_node_id = %remote_node_id,
            hostname = %hostname,
            "Parowanie zatwierdzone — node jest teraz zaufany"
        );

        Ok(())
    }

    /// Odrzuca parowanie — czysci pending wpis.
    pub fn reject_pairing(&self, remote_node_id: &str) -> Result<()> {
        db::repository::delete_pending_pairing(&self.db, remote_node_id)?;
        info!(remote_node_id = %remote_node_id, "Parowanie odrzucone");
        Ok(())
    }

    /// Wyprowadza material `pin_proof` dla pairing handshake jako
    /// `HKDF-SHA256(ECDH(our_x25519, remote_x25519), "tentaflow-pin-proof" ||
    /// pin || min(local, remote) || max(local, remote))`. Kanoniczne sortowanie
    /// id gwarantuje ze obie strony wyprowadzaja identyczny proof.
    pub fn derive_pin_proof(
        &self,
        remote_x25519_pub_hex: &str,
        pin: &str,
        local_node_id: &str,
        remote_node_id: &str,
    ) -> Result<[u8; 32]> {
        let bytes = hex::decode(remote_x25519_pub_hex).context("hex X25519")?;
        let key_bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("X25519 pub musi miec 32 bajty"))?;
        let remote_pub = X25519PublicKey::from(key_bytes);
        let shared = self.x25519_secret.diffie_hellman(&remote_pub);

        let (first, second) = if local_node_id < remote_node_id {
            (local_node_id, remote_node_id)
        } else {
            (remote_node_id, local_node_id)
        };

        let hk = Hkdf::<Sha256>::new(None, shared.as_bytes());
        let mut info_buf = Vec::with_capacity(32 + pin.len() + 64 + 64);
        info_buf.extend_from_slice(b"tentaflow-pin-proof");
        info_buf.extend_from_slice(pin.as_bytes());
        info_buf.extend_from_slice(first.as_bytes());
        info_buf.extend_from_slice(second.as_bytes());

        let mut proof = [0u8; 32];
        hk.expand(&info_buf, &mut proof)
            .map_err(|_| anyhow::anyhow!("HKDF expand nieudany"))?;
        Ok(proof)
    }

    // =========================================================================
    // Trust management
    // =========================================================================

    /// Cofniecie zaufania — usuniecie z `trusted_nodes` i zapis do `revoked_nodes`.
    pub fn revoke_trust(&self, node_id: &str) -> Result<()> {
        db::repository::remove_trusted_node(&self.db, node_id)?;
        self.trusted_keys.write().remove(node_id);
        self.revoked_nodes.write().insert(node_id.to_string());
        let _ = db::repository::add_revoked_node(&self.db, node_id, None);
        self.rebuild_trusted_snapshot();
        info!(node_id = %node_id, "Cofnieto zaufanie dla noda");
        Ok(())
    }

    /// Unpair bez revoke — usun z trusted_nodes, nie dodawaj do revoked.
    pub fn unpair(&self, node_id: &str) -> Result<()> {
        db::repository::remove_trusted_node(&self.db, node_id)?;
        self.trusted_keys.write().remove(node_id);
        self.rebuild_trusted_snapshot();
        info!(node_id = %node_id, "Odparowano node (friendly unpair)");
        Ok(())
    }

    /// Czy node jest aktywnie revoked?
    pub fn is_revoked(&self, node_id: &str) -> bool {
        self.revoked_nodes.read().contains(node_id)
    }

    /// Oznacza node jako w trakcie odparowywania (synchronicznie, przed async broadcast).
    pub fn mark_revoking(&self, node_id: &str) {
        self.revoking_nodes.write().insert(node_id.to_string());
    }

    /// Zdejmuje oznaczenie revoking po zakonczeniu operacji.
    pub fn clear_revoking(&self, node_id: &str) {
        self.revoking_nodes.write().remove(node_id);
    }

    /// Lista revokowanych node IDs — do synchronizacji przy reconnect.
    pub fn get_revoked_node_ids(&self) -> Vec<String> {
        self.revoked_nodes.read().iter().cloned().collect()
    }

    /// Usuwa node z listy revoked (admin re-trust).
    pub fn admin_retrust(&self, node_id: &str) -> Result<()> {
        self.revoked_nodes.write().remove(node_id);
        db::repository::remove_revoked_node(&self.db, node_id)?;
        info!(node_id = %node_id, "Admin re-trust — usunieto z revoked");
        Ok(())
    }

    // =========================================================================
    // Ed25519 signing
    // =========================================================================

    /// Podpisuje dane kluczem prywatnym Ed25519.
    pub fn sign(&self, data: &[u8]) -> Vec<u8> {
        self.signing_key.sign(data).to_bytes().to_vec()
    }

    /// Weryfikuje podpis od zaufanego noda.
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

    // =========================================================================
    // Trusted keys management (w tym synchronizacja po pairingu)
    // =========================================================================

    /// Wszystkie zaufane nody jako pary (node_id, public_key_hex).
    pub fn get_all_trusted_keys(&self) -> Vec<(String, String)> {
        let trusted = db::repository::list_trusted_nodes(&self.db).unwrap_or_default();
        trusted
            .iter()
            .map(|n| (n.node_id.clone(), n.public_key.clone()))
            .collect()
    }

    /// Dodaje klucz zaufanego noda otrzymany od innego noda (propagacja
    /// trusted_keys po pairingu).
    pub fn add_trusted_key(
        &self,
        node_id: &str,
        public_key_hex: &str,
        hostname: &str,
    ) -> Result<()> {
        if public_key_hex == self.public_key_hex() {
            return Ok(());
        }

        if self.is_trusted(node_id) {
            return Ok(());
        }

        if public_key_hex.len() != PUBLIC_KEY_HEX_LEN {
            bail!(
                "Nieprawidlowa dlugosc klucza publicznego: {} (oczekiwano {})",
                public_key_hex.len(),
                PUBLIC_KEY_HEX_LEN
            );
        }

        let vk = Self::parse_verifying_key(public_key_hex)?;

        db::repository::add_trusted_node(&self.db, node_id, public_key_hex, hostname, "mesh-sync")?;

        self.trusted_keys.write().insert(node_id.to_string(), vk);
        self.rebuild_trusted_snapshot();

        info!(
            node_id = %node_id,
            "Dodano zaufany klucz otrzymany z mesh sync"
        );

        Ok(())
    }

    /// Zwraca PIN z oczekujacego parowania (do wyswietlenia na UI).
    pub fn get_pending_pin(&self, remote_node_id: &str) -> Result<Option<String>> {
        let pairing = db::repository::get_pending_pairing(&self.db, remote_node_id)?;
        Ok(pairing.map(|p| p.pin_code).filter(|pin| !pin.is_empty()))
    }

    /// Sprawdza limit prob PIN (3 proby w oknie 60 s).
    pub fn check_pin_rate_limit(&self, node_id: &str) -> bool {
        let mut attempts = self.pin_attempts.write();
        let entry = attempts
            .entry(node_id.to_string())
            .or_insert((0, Instant::now()));

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

    /// Usuwa wygasle parowania z DB.
    pub fn cleanup_expired(&self) -> Result<u64> {
        db::repository::cleanup_expired_pairings(&self.db)
    }

    /// Zwraca referencje do `SettingsCipher` uzywanego do odszyfrowania kluczy
    /// prywatnych w DB. Potrzebne do konstrukcji `iroh::SecretKey` z tego
    /// samego keypair'a (iroh_manager).
    pub fn settings_cipher_ref(&self) -> &Arc<crate::crypto::SettingsCipher> {
        &self.settings_cipher
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

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
                is_active INTEGER NOT NULL DEFAULT 1,
                last_addresses TEXT NOT NULL DEFAULT ''
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

    fn test_settings_cipher() -> Arc<crate::crypto::SettingsCipher> {
        Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32]))
    }

    #[test]
    fn generowanie_klucza_i_zapis_do_db() {
        let db = setup_test_db();
        let security = MeshSecurity::new(db.clone(), test_settings_cipher()).unwrap();
        assert_eq!(security.public_key_hex().len(), PUBLIC_KEY_HEX_LEN);

        let security2 = MeshSecurity::new(db, test_settings_cipher()).unwrap();
        assert_eq!(security.public_key_hex(), security2.public_key_hex());
    }

    #[test]
    fn generowanie_pin() {
        let pin = MeshSecurity::generate_pin();
        assert_eq!(pin.len(), 6);
        assert!(pin.parse::<u32>().unwrap() >= 100_000);
        assert!(pin.parse::<u32>().unwrap() <= 999_999);
    }

    #[test]
    fn podpisywanie_i_weryfikacja() {
        let db_a = setup_test_db();
        let db_b = setup_test_db();
        let sec_a = MeshSecurity::new(db_a, test_settings_cipher()).unwrap();
        let sec_b = MeshSecurity::new(db_b, test_settings_cipher()).unwrap();

        sec_b
            .add_trusted_key("node-a", &sec_a.public_key_hex(), "host-a")
            .unwrap();

        let data = b"Wiadomosc do podpisania";
        let sig = sec_a.sign(data);

        assert!(sec_b.verify("node-a", data, &sig).unwrap());

        let mut bad_sig = sig.clone();
        bad_sig[0] ^= 0xFF;
        assert!(!sec_b.verify("node-a", data, &bad_sig).unwrap());
    }

    #[test]
    fn derive_pin_proof_symetryczny() {
        let db_a = setup_test_db();
        let db_b = setup_test_db();
        let sec_a = MeshSecurity::new(db_a, test_settings_cipher()).unwrap();
        let sec_b = MeshSecurity::new(db_b, test_settings_cipher()).unwrap();

        let pin = "123456";
        let node_a = "node-a";
        let node_b = "node-b";

        let proof_a = sec_a
            .derive_pin_proof(&sec_b.x25519_public_key_hex(), pin, node_a, node_b)
            .unwrap();
        let proof_b = sec_b
            .derive_pin_proof(&sec_a.x25519_public_key_hex(), pin, node_b, node_a)
            .unwrap();

        assert_eq!(
            proof_a, proof_b,
            "obie strony wyprowadzaja identyczny pin_proof"
        );

        let proof_wrong_pin = sec_a
            .derive_pin_proof(&sec_b.x25519_public_key_hex(), "000000", node_a, node_b)
            .unwrap();
        assert_ne!(proof_a, proof_wrong_pin);
    }

    #[test]
    fn trust_snapshot_ma_arc_clone_bez_locka() {
        let db = setup_test_db();
        let sec = MeshSecurity::new(db, test_settings_cipher()).unwrap();
        let snap1 = sec.trusted_node_ids_snapshot();
        let snap2 = sec.trusted_node_ids_snapshot();
        assert!(Arc::ptr_eq(&snap1, &snap2));
    }

    #[test]
    fn revoke_i_retrust_dziala() {
        let db = setup_test_db();
        let sec = MeshSecurity::new(db, test_settings_cipher()).unwrap();

        assert!(!sec.is_revoked("node-x"));
        sec.revoke_trust("node-x").unwrap();
        assert!(sec.is_revoked("node-x"));

        sec.admin_retrust("node-x").unwrap();
        assert!(!sec.is_revoked("node-x"));
    }
}
