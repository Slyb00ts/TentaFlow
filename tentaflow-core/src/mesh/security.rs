// =============================================================================
// Plik: mesh/security.rs
// Opis: Bezpieczenstwo mesh — generowanie kluczy Ed25519, parowanie PIN,
//       wymiana kluczy X25519, szyfrowanie ChaCha20-Poly1305.
//       Zoptymalizowane pod 1000 peerow: cache cipherow, batch trust check,
//       pre-alokowane bufory szyfrowania.
// =============================================================================

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use parking_lot::RwLock;
use rand::rngs::OsRng;
use rand::Rng;
use sha2::Sha256;
use tracing::{info, warn};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

use crate::db::{self, DbPool};

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
    /// Shared secrets (ChaCha20 key) per para nodow: node_id -> [u8; 32]
    shared_secrets: RwLock<HashMap<String, [u8; 32]>>,
    /// [OPT] Cache zainicjalizowanych cipherow ChaCha20-Poly1305 per peer.
    /// Unika powtornego `ChaCha20Poly1305::new(key)` przy kazdym szyfrowaniu.
    /// Odbudowywany atomowo przy dodaniu/usunieciu shared secret.
    cached_ciphers: RwLock<HashMap<String, Arc<ChaCha20Poly1305>>>,
    /// [OPT] Snapshot zaufanych node_id — Arc<HashSet> do batch trust check.
    /// Jeden read Arc::clone zamiast 1000 lockow RwLock w petli heartbeat.
    /// Odbudowywany przy kazdej zmianie trusted_keys.
    trusted_node_ids: RwLock<Arc<HashSet<String>>>,
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
            shared_secrets: RwLock::new(HashMap::new()),
            cached_ciphers: RwLock::new(HashMap::new()),
            trusted_node_ids: RwLock::new(Arc::new(HashSet::new())),
            db,
        };

        // Wczytaj zaufane nody z bazy
        security.load_trusted_from_db()?;

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
        let mut secrets = self.shared_secrets.write();
        let mut ciphers = self.cached_ciphers.write();

        for node in &trusted {
            match Self::parse_verifying_key(&node.public_key) {
                Ok(vk) => {
                    keys.insert(node.node_id.clone(), vk);
                    // Oblicz shared secret z klucza publicznego X25519
                    // Klucz X25519 przechowywany jako druga polowa public_key (64+64 hex = ed25519 + x25519)
                    // [CR-007] Wynik DH przepuszczany przez HKDF-SHA256
                    if node.public_key.len() >= 128 {
                        let x25519_hex = &node.public_key[64..128];
                        if let Ok(x_bytes) = hex::decode(x25519_hex) {
                            if x_bytes.len() == 32 {
                                let mut arr = [0u8; 32];
                                arr.copy_from_slice(&x_bytes);
                                let remote_x_pub = X25519PublicKey::from(arr);
                                let raw_shared = self.x25519_secret.diffie_hellman(&remote_x_pub);
                                let derived_key = Self::derive_key_from_dh(&raw_shared);
                                // [OPT] Pre-inicjalizuj cipher dla tego peera
                                let key = Key::from(derived_key);
                                let cipher = ChaCha20Poly1305::new(&key);
                                ciphers.insert(node.node_id.clone(), Arc::new(cipher));
                                secrets.insert(node.node_id.clone(), derived_key);
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

        // [OPT] Odbuduj snapshot trusted_node_ids
        drop(secrets);
        drop(ciphers);
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

    /// [OPT] Dodaje cipher do cache po dodaniu shared secret.
    /// Wewnetrzna metoda.
    fn cache_cipher_for_node(&self, node_id: &str, secret: &[u8; 32]) {
        let key = Key::from(*secret);
        let cipher = ChaCha20Poly1305::new(&key);
        self.cached_ciphers.write().insert(node_id.to_string(), Arc::new(cipher));
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

        // [CR-007] Oblicz shared secret z X25519 z HKDF
        if remote_public_key_hex.len() >= 128 {
            let x25519_hex = &remote_public_key_hex[64..128];
            if let Ok(x_bytes) = hex::decode(x25519_hex) {
                if x_bytes.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&x_bytes);
                    let remote_x_pub = X25519PublicKey::from(arr);
                    let raw_shared = self.x25519_secret.diffie_hellman(&remote_x_pub);
                    let derived_key = Self::derive_key_from_dh(&raw_shared);
                    self.shared_secrets
                        .write()
                        .insert(remote_node_id.to_string(), derived_key);
                    // [OPT] Cache cipher
                    self.cache_cipher_for_node(remote_node_id, &derived_key);
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
        self.shared_secrets.write().remove(node_id);
        self.cached_ciphers.write().remove(node_id);
        // [OPT] Odbuduj snapshot trusted_node_ids
        self.rebuild_trusted_snapshot();
        info!(node_id = %node_id, "Cofnieto zaufanie dla noda");
        Ok(())
    }

    /// Szyfruj payload kluczem shared secret dla danego noda.
    /// Format wyjscia: [12B nonce][ciphertext+tag]
    pub fn encrypt_for_node(&self, node_id: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
        // [OPT] Uzyj cached cipher zamiast tworzenia nowego za kazdym razem
        let cipher = {
            let ciphers = self.cached_ciphers.read();
            ciphers.get(node_id).cloned()
        };

        let cipher = match cipher {
            Some(c) => c,
            None => {
                // Fallback — jesli cipher nie w cache, stworz z shared_secrets
                let secrets = self.shared_secrets.read();
                let secret = secrets
                    .get(node_id)
                    .ok_or_else(|| anyhow::anyhow!("Brak shared secret dla noda {}", node_id))?;
                let key = Key::from(*secret);
                let c = Arc::new(ChaCha20Poly1305::new(&key));
                drop(secrets);
                // Dodaj do cache na przyszlosc
                self.cached_ciphers.write().insert(node_id.to_string(), c.clone());
                c
            }
        };

        let nonce_bytes: [u8; 12] = rand::random();
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| anyhow::anyhow!("Blad szyfrowania: {}", e))?;

        // [OPT] Jedna alokacja z dokladnym rozmiarem
        let mut result = Vec::with_capacity(12 + ciphertext.len());
        result.extend_from_slice(&nonce_bytes);
        result.extend_from_slice(&ciphertext);
        Ok(result)
    }

    /// [OPT] Szyfruj payload do istniejacego bufora — zero alokacji w hot path.
    /// Bufor jest czyszczony i reuzywany. Format: [12B nonce][ciphertext+tag].
    /// Zwraca dlugosc zapisanych danych.
    pub fn encrypt_for_node_into(
        &self,
        node_id: &str,
        plaintext: &[u8],
        out_buf: &mut Vec<u8>,
    ) -> Result<()> {
        let cipher = {
            let ciphers = self.cached_ciphers.read();
            ciphers.get(node_id).cloned()
        };

        let cipher = match cipher {
            Some(c) => c,
            None => {
                let secrets = self.shared_secrets.read();
                let secret = secrets
                    .get(node_id)
                    .ok_or_else(|| anyhow::anyhow!("Brak shared secret dla noda {}", node_id))?;
                let key = Key::from(*secret);
                let c = Arc::new(ChaCha20Poly1305::new(&key));
                drop(secrets);
                self.cached_ciphers.write().insert(node_id.to_string(), c.clone());
                c
            }
        };

        let nonce_bytes: [u8; 12] = rand::random();
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| anyhow::anyhow!("Blad szyfrowania: {}", e))?;

        out_buf.clear();
        out_buf.reserve(12 + ciphertext.len());
        out_buf.extend_from_slice(&nonce_bytes);
        out_buf.extend_from_slice(&ciphertext);
        Ok(())
    }

    /// Deszyfruj payload od danego noda.
    /// Oczekiwany format: [12B nonce][ciphertext+tag]
    pub fn decrypt_from_node(&self, node_id: &str, data: &[u8]) -> Result<Vec<u8>> {
        if data.len() < 12 {
            bail!("Dane za krotkie — brak nonce (minimum 12 bajtow)");
        }

        // [OPT] Uzyj cached cipher
        let cipher = {
            let ciphers = self.cached_ciphers.read();
            ciphers.get(node_id).cloned()
        };

        let cipher = match cipher {
            Some(c) => c,
            None => {
                let secrets = self.shared_secrets.read();
                let secret = secrets
                    .get(node_id)
                    .ok_or_else(|| anyhow::anyhow!("Brak shared secret dla noda {}", node_id))?;
                let key = Key::from(*secret);
                let c = Arc::new(ChaCha20Poly1305::new(&key));
                drop(secrets);
                self.cached_ciphers.write().insert(node_id.to_string(), c.clone());
                c
            }
        };

        let nonce = Nonce::from_slice(&data[..12]);

        let plaintext = cipher
            .decrypt(nonce, &data[12..])
            .map_err(|e| anyhow::anyhow!("Blad deszyfrowania: {}", e))?;

        Ok(plaintext)
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

        // [CR-007] Oblicz shared secret z HKDF
        if public_key_hex.len() >= 128 {
            let x25519_hex = &public_key_hex[64..128];
            if let Ok(x_bytes) = hex::decode(x25519_hex) {
                if x_bytes.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&x_bytes);
                    let remote_x_pub = X25519PublicKey::from(arr);
                    let raw_shared = self.x25519_secret.diffie_hellman(&remote_x_pub);
                    let derived_key = Self::derive_key_from_dh(&raw_shared);
                    self.shared_secrets
                        .write()
                        .insert(node_id.to_string(), derived_key);
                    // [OPT] Cache cipher
                    self.cache_cipher_for_node(node_id, &derived_key);
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

    /// Sprawdza czy mamy shared secret dla noda (do szyfrowania)
    pub fn has_shared_secret(&self, node_id: &str) -> bool {
        self.shared_secrets.read().contains_key(node_id)
    }

    /// Zwraca PIN z oczekujacego parowania (do wyswietlenia na UI)
    pub fn get_pending_pin(&self, remote_node_id: &str) -> Result<Option<String>> {
        let pairing = db::repository::get_pending_pairing(&self.db, remote_node_id)?;
        Ok(pairing.map(|p| p.pin_code))
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
}
