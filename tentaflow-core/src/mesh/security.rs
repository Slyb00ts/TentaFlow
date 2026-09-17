// =============================================================================
// Plik: mesh/security.rs
// Opis: Tozsamosc i zaufanie mesh. Ed25519 keypair persistentny w DB (klucz
//       prywatny szyfrowany SettingsCipher), X25519 jako drugi klucz uzywany
//       przy pairing handshake. Trzyma zbior zaufanych peerow (`trusted_keys`),
//       tagi revoke oraz wpisy rate limit PIN. Calosc szyfrowania transportu
//       jest obowiazkiem iroh TLS — ten modul nie wrapuje payloadow AEAD.
// =============================================================================

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{bail, Context, Result};
use arc_swap::ArcSwap;
use dashmap::DashMap;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use parking_lot::Mutex;
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
/// Format of `trusted_nodes.approved_at` and `revoked_nodes.revoked_at` (UTC).
/// Fixed-width, so plain string comparison orders the two.
const TRUST_TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

const SEAL_NONCE_LEN: usize = 12;
const SEAL_HKDF_LABEL: &[u8] = b"tentaflow-peer-seal";

/// A six-digit PIN has 10^6 values; these limits keep an online guess far below
/// any useful success rate while leaving room for an operator's typos.
const PIN_ATTEMPT_WINDOW: Duration = Duration::from_secs(600);
const PIN_ATTEMPTS_PER_KEY: u32 = 3;
const PIN_ATTEMPTS_ALL_KEYS: u32 = 30;
const PIN_TRACKED_KEYS: usize = 1024;

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
    /// DashMap — per-shard lock, is_trusted() i verify() nie konkuruja.
    trusted_keys: DashMap<String, VerifyingKey>,
    /// Snapshot zaufanych `node_id` jako `Arc<HashSet>` — odbudowywany przy
    /// kazdej zmianie trusted_keys. ArcSwap = zero-lock reads.
    trusted_node_ids: ArcSwap<HashSet<String>>,
    /// Aktywnie cofniete zaufanie: `node_id` -> `revoked_at` (UTC
    /// `%Y-%m-%d %H:%M:%S`) — wypelniane przez `revoke_trust`.
    revoked_nodes: DashMap<String, String>,
    /// Nody w trakcie revoke/unpair — synchronicznie ustawiane przed async
    /// broadcastem TrustRevoked.
    revoking_nodes: DashMap<String, ()>,
    /// PIN attempts per transport identity: (count, window start).
    pin_attempts: DashMap<String, (u32, Instant)>,
    /// PIN attempts across all identities: (count, window start).
    pin_global_budget: Mutex<(u32, Instant)>,
    /// Aktywne zaproszenie QR — (pin, expiry). Jeden naraz, rotowany co 60s.
    /// Mutex bo to jeden globalny slot; contention znikoma (co 50s refresh).
    invite: Mutex<Option<(String, Instant)>>,
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
            trusted_keys: DashMap::with_capacity(256),
            trusted_node_ids: ArcSwap::from_pointee(HashSet::new()),
            revoked_nodes: DashMap::with_capacity(64),
            revoking_nodes: DashMap::with_capacity(16),
            pin_attempts: DashMap::with_capacity(64),
            pin_global_budget: Mutex::new((0, Instant::now())),
            invite: Mutex::new(None),
            db,
            settings_cipher,
        };

        security.load_trusted_from_db()?;

        if let Ok(revoked) = db::repository::list_revoked_nodes(&security.db) {
            for (node_id, revoked_at) in revoked {
                security.revoked_nodes.insert(node_id, revoked_at);
            }
        }

        info!(
            target: "mesh::identity",
            ed25519_hex = %security.ed25519_public_key_hex(),
            public_key = %security.public_key_hex(),
            trusted_count = security.trusted_keys.len(),
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
            let key = crate::crypto::generate_signing_key()?;
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
            let mut seed = zeroize::Zeroizing::new([0u8; 32]);
            getrandom::fill(seed.as_mut())
                .map_err(|error| anyhow::anyhow!("Nie udalo sie wylosowac klucza X25519: {error}"))?;
            let secret = StaticSecret::from(*seed);
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

    /// Szyfr `settings` tego noda. Cross-node deploy (`handle_service_deploy_remote`)
    /// musi rozwiazac WLASNY `hf_token` z secure setting — token nigdy nie jest
    /// forwardowany przez mesh, kazdy node uzywa swojego.
    pub fn settings_cipher(&self) -> &Arc<crate::crypto::SettingsCipher> {
        &self.settings_cipher
    }

    fn load_trusted_from_db(&self) -> Result<()> {
        let trusted = db::repository::list_trusted_nodes(&self.db)?;

        for node in &trusted {
            match Self::parse_verifying_key(&node.public_key) {
                Ok(vk) => {
                    self.trusted_keys.insert(node.node_id.clone(), vk);
                }
                Err(e) => {
                    warn!(
                        node_id = %node.node_id,
                        "Nie udalo sie wczytac klucza publicznego: {}", e
                    );
                }
            }
        }

        self.rebuild_trusted_snapshot();
        Ok(())
    }

    /// VULN-M5: node_id jest pierwszymi 64 znakami hex 128-znakowego
    /// combined key (Ed25519 verifying key). Konwencja jak w
    /// `net::iroh::pairing::validate_public_key_shape`.
    fn validate_identity_binding(node_id: &str, public_key_hex: &str) -> Result<()> {
        if node_id.len() != 64 || !node_id.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("node_id musi miec 64 znaki hex");
        }
        if !public_key_hex.starts_with(node_id) {
            bail!("Ed25519 czesc klucza publicznego nie zgadza sie z node_id");
        }
        Ok(())
    }

    fn now_timestamp() -> String {
        chrono::Utc::now().format(TRUST_TIMESTAMP_FORMAT).to_string()
    }

    /// Wire timestamps are clamped to "now": a future `approved_at` would make
    /// trust expiry never fire, and a future `revoked_at` would block every
    /// later re-pairing. An unparsable value also lands on "now".
    fn clamp_wire_timestamp(ts: &str) -> String {
        match chrono::NaiveDateTime::parse_from_str(ts, TRUST_TIMESTAMP_FORMAT) {
            Ok(parsed) if parsed <= chrono::Utc::now().naive_utc() => ts.to_string(),
            _ => Self::now_timestamp(),
        }
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
    /// ArcSwap::store jest atomowy — readers widza spojny snapshot.
    fn rebuild_trusted_snapshot(&self) {
        let trusted_set: HashSet<String> =
            self.trusted_keys.iter().map(|e| e.key().clone()).collect();
        self.trusted_node_ids.store(Arc::new(trusted_set));
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
    /// Dwa DashMap contains_key — kazdy na swoim shardzie, zero read-lock
    /// contention z zapisami innych node_id.
    pub fn is_trusted(&self, node_id: &str) -> bool {
        if self.revoking_nodes.contains_key(node_id) {
            return false;
        }
        self.trusted_keys.contains_key(node_id)
    }

    /// Snapshot zaufanych node_id — ArcSwap::load_full, zero lockow.
    pub fn trusted_node_ids_snapshot(&self) -> Arc<HashSet<String>> {
        self.trusted_node_ids.load_full()
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
        *self.invite.lock() = Some((pin.clone(), expiry));
        (pin, 60)
    }

    /// Zwraca aktualny invite PIN (jesli wciaz wazny). Do sprawdzenia przez
    /// `handle_pairing_request` — jesli przychodzacy PIN matchuje, auto-confirm.
    pub fn peek_invite_pin(&self) -> Option<String> {
        let guard = self.invite.lock();
        let (pin, expiry) = guard.as_ref()?;
        if Instant::now() < *expiry {
            Some(pin.clone())
        } else {
            None
        }
    }

    /// Skonsumuj invite PIN jesli matchuje — zapobiega reuse.
    pub fn consume_invite_pin(&self, candidate: &str) -> bool {
        let mut guard = self.invite.lock();
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

        self.pin_attempts.remove(remote_node_id);

        let pin = if !pin_hint.is_empty()
            && pin_hint.len() == 6
            && pin_hint.chars().all(|c| c.is_ascii_digit())
        {
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
    /// jako pending do czasu wprowadzenia PIN-u przez uzytkownika. `environment`
    /// is the peer's OWN declared environment as carried on the pairing
    /// request (ROADMAP Z12, P1-2) — persisted alongside the pending pubkey so
    /// a LATER manual confirm (`mesh::admin_ops::confirm_pairing`, which has
    /// no direct access to the original request) can still pass it into
    /// `confirm_pairing` below, the single place that stamps
    /// `trusted_nodes.environment`.
    pub fn receive_pairing_request(
        &self,
        remote_node_id: &str,
        pin: &str,
        remote_public_key: &str,
        environment: tentaflow_protocol::environment::NodeEnvironment,
    ) -> Result<()> {
        let expires = chrono::Utc::now() + chrono::Duration::seconds(60);
        let expires_str = expires.format("%Y-%m-%d %H:%M:%S").to_string();

        // A revocation is NOT lifted here: this runs on a frame from the
        // revoked node itself. `confirm_pairing` lifts it once a local
        // operator has approved the pairing.
        // The attempt counter is not cleared here either: a new request must
        // not buy the sender a fresh set of guesses.

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
        let _ = db::repository::set_setting(
            &self.db,
            &pending_pairing_environment_key(remote_node_id),
            environment.as_str(),
        );

        info!(
            remote_node_id = %remote_node_id,
            "Otrzymano zadanie parowania — PIN i klucz publiczny zapisane"
        );

        Ok(())
    }

    /// Potwierdza parowanie: zapisuje klucz publiczny do `trusted_nodes`.
    /// Nie wyprowadza shared secret — transport iroh zapewnia szyfrowanie TLS.
    ///
    /// `environment` is the REMOTE node's declared environment (ROADMAP Z12,
    /// P1-2) and is stamped into `trusted_nodes.environment` HERE — the single
    /// place every confirm path (iroh first-contact auto-confirm, the
    /// initiator trusting a `Confirm` response, the admin manually approving
    /// a pending pairing, and a receiver's `PairingConfirm` reaching the
    /// initiator over the legacy `mesh` stream) funnels through, so none of
    /// them can forget the stamp the way only auto-confirm and the initiator
    /// path used to.
    pub fn confirm_pairing(
        &self,
        remote_node_id: &str,
        remote_public_key_hex: &str,
        hostname: &str,
        approved_by: &str,
        environment: tentaflow_protocol::environment::NodeEnvironment,
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

        // VULN-M5: tozsamosc sparowanego noda musi byc powiazana z kluczem.
        Self::validate_identity_binding(remote_node_id, remote_public_key_hex)?;

        let vk = Self::parse_verifying_key(remote_public_key_hex)?;

        db::repository::add_trusted_node(
            &self.db,
            remote_node_id,
            remote_public_key_hex,
            hostname,
            approved_by,
            None,
        )?;

        self.trusted_keys.insert(remote_node_id.to_string(), vk);

        // Confirmation is an operator decision (manual approval or a locally
        // issued invite PIN), so it overrides an earlier revocation. The fresh
        // `approved_at` then outranks that revocation on every other node.
        if self.is_revoked(remote_node_id) {
            self.admin_retrust(remote_node_id)?;
        }

        db::repository::delete_pending_pairing(&self.db, remote_node_id)?;
        let _ = db::repository::delete_setting(
            &self.db,
            &pending_pairing_environment_key(remote_node_id),
        );
        if let Err(e) =
            db::repository::set_trusted_node_environment(&self.db, remote_node_id, environment)
        {
            warn!(
                remote_node_id = %remote_node_id,
                "Zapis srodowiska peera po potwierdzeniu parowania nieudany: {}",
                e
            );
        }
        self.rebuild_trusted_snapshot();

        // Confirming a pairing is the one moment a person on this node vouches
        // for the peer, so it is also what makes the peer an operator here.
        // Nodes that become trusted through `TrustedKeysSync` never pass this
        // point and may not send node-changing commands until promoted.
        let promotion = db::repository::SyncNodeProfileUpdate {
            node_kind: None,
            operator: Some(true),
        };
        if let Err(e) =
            db::repository::update_sync_node_profile(&self.db, remote_node_id, &promotion, None)
        {
            warn!(
                remote_node_id = %remote_node_id,
                "Nadanie roli operatora po parowaniu nieudane: {}",
                e
            );
        }

        info!(
            remote_node_id = %remote_node_id,
            hostname = %hostname,
            environment = %environment,
            "Parowanie zatwierdzone — node jest teraz zaufany"
        );

        Ok(())
    }

    /// Environment the peer declared on its still-pending pairing request
    /// (`receive_pairing_request`), read back by a LATER manual confirm that
    /// has no direct access to the original request/response. `None` when no
    /// request was ever recorded for this peer (or it already expired) — the
    /// caller falls back to `NodeEnvironment::default()` (Prod), matching the
    /// same conservative default used everywhere else for a value nobody
    /// declared.
    pub fn pending_pairing_environment(
        &self,
        remote_node_id: &str,
    ) -> Option<tentaflow_protocol::environment::NodeEnvironment> {
        db::repository::get_setting(&self.db, &pending_pairing_environment_key(remote_node_id))
            .ok()
            .flatten()
            .and_then(|v| tentaflow_protocol::environment::NodeEnvironment::parse(&v))
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
    // Sealing for one trusted peer
    // =========================================================================

    /// Encrypts `plaintext` so that only `recipient_node_id` can open it.
    ///
    /// The key is `HKDF-SHA256(ECDH(our_x25519, recipient_x25519))` with both
    /// node ids in the HKDF info, sender first: a blob sealed for one peer does
    /// not open for another, and one sealed by A for B does not pass as one
    /// sealed by B for A. `context` is authenticated but not encrypted; callers
    /// bind the blob to what it describes (setting key and version) so it cannot
    /// be replanted under a different key.
    pub fn seal_for_peer(
        &self,
        recipient_node_id: &str,
        context: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        let local_node_id = self.ed25519_public_key_hex();
        let cipher = self.peer_seal_cipher(recipient_node_id, &local_node_id, recipient_node_id)?;
        let mut nonce = [0u8; SEAL_NONCE_LEN];
        getrandom::fill(&mut nonce)
            .map_err(|e| anyhow::anyhow!("OS RNG unavailable while sealing: {e}"))?;
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: plaintext, aad: context })
            .map_err(|_| anyhow::anyhow!("sealing for peer {recipient_node_id} failed"))?;
        let mut sealed = Vec::with_capacity(SEAL_NONCE_LEN + ciphertext.len());
        sealed.extend_from_slice(&nonce);
        sealed.extend_from_slice(&ciphertext);
        Ok(sealed)
    }

    /// Opens a blob produced by `seal_for_peer` on `sender_node_id` for this
    /// node. Fails when the sender is not trusted, the blob was sealed for a
    /// different recipient, or `context` differs from the one used for sealing.
    pub fn open_from_peer(
        &self,
        sender_node_id: &str,
        context: &[u8],
        sealed: &[u8],
    ) -> Result<Vec<u8>> {
        if sealed.len() < SEAL_NONCE_LEN {
            bail!("sealed blob from {sender_node_id} is shorter than its nonce");
        }
        let local_node_id = self.ed25519_public_key_hex();
        let cipher = self.peer_seal_cipher(sender_node_id, sender_node_id, &local_node_id)?;
        let (nonce, ciphertext) = sealed.split_at(SEAL_NONCE_LEN);
        cipher
            .decrypt(Nonce::from_slice(nonce), Payload { msg: ciphertext, aad: context })
            .map_err(|_| anyhow::anyhow!("sealed blob from {sender_node_id} did not authenticate"))
    }

    fn peer_seal_cipher(
        &self,
        peer_node_id: &str,
        sender_node_id: &str,
        recipient_node_id: &str,
    ) -> Result<Aes256Gcm> {
        let public_key_hex = db::repository::get_trusted_node_public_key(&self.db, peer_node_id)?
            .ok_or_else(|| anyhow::anyhow!("node {peer_node_id} is not trusted"))?;
        let x25519_hex = public_key_hex
            .get(PUBLIC_KEY_HEX_LEN / 2..PUBLIC_KEY_HEX_LEN)
            .ok_or_else(|| anyhow::anyhow!("node {peer_node_id} has no X25519 key on record"))?;
        let key_bytes: [u8; 32] = hex::decode(x25519_hex)
            .context("X25519 key of the peer is not hex")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("X25519 key of the peer must be 32 bytes"))?;
        let shared = self
            .x25519_secret
            .diffie_hellman(&X25519PublicKey::from(key_bytes));
        // A low-order peer key yields an all-zero shared secret that anyone can compute.
        if !shared.was_contributory() {
            bail!("node {peer_node_id} has a degenerate X25519 key");
        }

        let mut info = Vec::with_capacity(SEAL_HKDF_LABEL.len() + 2 * PUBLIC_KEY_HEX_LEN);
        info.extend_from_slice(SEAL_HKDF_LABEL);
        info.extend_from_slice(sender_node_id.as_bytes());
        info.extend_from_slice(recipient_node_id.as_bytes());
        let mut key = zeroize::Zeroizing::new([0u8; 32]);
        Hkdf::<Sha256>::new(None, shared.as_bytes())
            .expand(&info, key.as_mut())
            .map_err(|_| anyhow::anyhow!("HKDF expand failed"))?;
        Aes256Gcm::new_from_slice(key.as_ref()).map_err(|_| anyhow::anyhow!("AES key rejected"))
    }

    // =========================================================================
    // Trust management
    // =========================================================================

    /// Cofniecie zaufania — usuniecie z `trusted_nodes` i zapis do `revoked_nodes`.
    /// `revoked_at` is the fleet-wide revocation time when the revocation was
    /// propagated by a peer; `None` for a revocation that originates here.
    /// Returns the stored timestamp so the caller can propagate it unchanged.
    pub fn revoke_trust(&self, node_id: &str, revoked_at: Option<&str>) -> Result<String> {
        let stored = self.record_revocation_at(node_id, revoked_at)?;
        db::repository::remove_trusted_node(&self.db, node_id)?;
        self.trusted_keys.remove(node_id);
        self.rebuild_trusted_snapshot();
        info!(node_id = %node_id, revoked_at = %stored, "Cofnieto zaufanie dla noda");
        Ok(stored)
    }

    /// Persists a locally originated revocation WITHOUT dropping the trusted
    /// key yet — the admin flow still needs the key to deliver the signed
    /// TrustRevoked notification, and removes it with `unpair` afterwards.
    pub fn record_revocation(&self, node_id: &str) -> Result<String> {
        self.record_revocation_at(node_id, None)
    }

    fn record_revocation_at(&self, node_id: &str, revoked_at: Option<&str>) -> Result<String> {
        let revoked_at = revoked_at.map_or_else(Self::now_timestamp, Self::clamp_wire_timestamp);
        db::repository::add_revoked_node(&self.db, node_id, None, &revoked_at)?;
        let stored = self
            .revoked_nodes
            .entry(node_id.to_string())
            .and_modify(|current| {
                if *current < revoked_at {
                    *current = revoked_at.clone();
                }
            })
            .or_insert_with(|| revoked_at.clone())
            .clone();
        Ok(stored)
    }

    /// A propagated revocation is stale when the node was (re-)approved after
    /// it: the sender missed the re-pairing, and honouring the frame would
    /// knock a legitimately re-paired node out of the fleet again. A frame
    /// without a timestamp cannot be ordered and is never treated as stale.
    pub fn is_stale_revocation(&self, node_id: &str, revoked_at: Option<&str>) -> bool {
        let Some(revoked_at) = revoked_at else {
            return false;
        };
        let revoked_at = Self::clamp_wire_timestamp(revoked_at);
        matches!(
            db::repository::get_trusted_node_approved_at(&self.db, node_id),
            Ok(Some(approved_at)) if approved_at > revoked_at
        )
    }

    /// Unpair bez revoke — usun z trusted_nodes, nie dodawaj do revoked.
    pub fn unpair(&self, node_id: &str) -> Result<()> {
        db::repository::remove_trusted_node(&self.db, node_id)?;
        self.trusted_keys.remove(node_id);
        self.rebuild_trusted_snapshot();
        info!(node_id = %node_id, "Odparowano node (friendly unpair)");
        Ok(())
    }

    /// Czy node jest aktywnie revoked?
    pub fn is_revoked(&self, node_id: &str) -> bool {
        self.revoked_nodes.contains_key(node_id)
    }

    /// Oznacza node jako w trakcie odparowywania (synchronicznie, przed async broadcast).
    pub fn mark_revoking(&self, node_id: &str) {
        self.revoking_nodes.insert(node_id.to_string(), ());
    }

    /// Zdejmuje oznaczenie revoking po zakonczeniu operacji.
    pub fn clear_revoking(&self, node_id: &str) {
        self.revoking_nodes.remove(node_id);
    }

    /// Revokowane nody jako `(node_id, revoked_at)` — do synchronizacji przy reconnect.
    pub fn get_revoked_nodes(&self) -> Vec<(String, String)> {
        self.revoked_nodes
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect()
    }

    /// Usuwa node z listy revoked (admin re-trust).
    pub fn admin_retrust(&self, node_id: &str) -> Result<()> {
        self.revoked_nodes.remove(node_id);
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

    /// Reference to the node's Ed25519 signing key. Used by the UFP/2 mesh
    /// codec to sign outgoing envelopes via `sdk_spec::frame::sign_envelope`.
    /// Crate-internal only — never exposed across the FFI boundary.
    pub(crate) fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// Raw 32-byte Ed25519 public key for the local node. Mirrors the
    /// hex-encoded value returned by `ed25519_public_key_hex` but in the
    /// byte shape expected by `NodeAddress::node` and UFP/2 envelope
    /// signature scope (§6.3).
    pub fn verifying_key_bytes(&self) -> [u8; 32] {
        self.verifying_key.to_bytes()
    }

    /// Weryfikuje podpis od zaufanego noda.
    pub fn verify(&self, node_id: &str, data: &[u8], signature_bytes: &[u8]) -> Result<bool> {
        let key = self
            .trusted_keys
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

    /// Wszystkie zaufane nody jako trojki (node_id, public_key_hex, approved_at).
    /// `approved_at` is carried so trust mirroring can propagate the ORIGIN's approval
    /// time, keeping the trust-expiry TTL anchored to the first real pairing.
    pub fn get_all_trusted_keys(&self) -> Vec<(String, String, String)> {
        let trusted = db::repository::list_trusted_nodes(&self.db).unwrap_or_default();
        trusted
            .iter()
            .map(|n| {
                (
                    n.node_id.clone(),
                    n.public_key.clone(),
                    n.approved_at.clone(),
                )
            })
            .collect()
    }

    /// Dodaje klucz zaufanego noda otrzymany od innego noda (propagacja
    /// trusted_keys po pairingu).
    ///
    /// `origin_approved_at` is the mirrored origin's `approved_at`; it becomes the
    /// trust-expiry TTL anchor instead of "now", so a mirror re-add cannot resurrect a
    /// long-dead identity with a fresh clock. Empty/None falls back to "now" (legacy peer).
    pub fn add_trusted_key(
        &self,
        node_id: &str,
        public_key_hex: &str,
        hostname: &str,
        origin_approved_at: Option<&str>,
    ) -> Result<()> {
        if public_key_hex == self.public_key_hex() {
            return Ok(());
        }

        if self.is_trusted(node_id) {
            return Ok(());
        }

        // Transitive trust must not resurrect a revoked node: a peer that has
        // not seen the revocation yet would otherwise re-add it with its next
        // sync. The one legitimate case is a re-pairing approved elsewhere in
        // the fleet AFTER the revocation — its `approved_at` outranks it, and
        // the revocation is lifted so the node propagates normally again.
        if let Some(revoked_at) = self.revoked_nodes.get(node_id).map(|e| e.value().clone()) {
            let approved_after_revocation = origin_approved_at
                .map(Self::clamp_wire_timestamp)
                .is_some_and(|approved_at| approved_at > revoked_at);
            if !approved_after_revocation {
                bail!("node {} is revoked — mesh sync cannot re-trust it", node_id);
            }
            self.admin_retrust(node_id)?;
        }

        if public_key_hex.len() != PUBLIC_KEY_HEX_LEN {
            bail!(
                "Nieprawidlowa dlugosc klucza publicznego: {} (oczekiwano {})",
                public_key_hex.len(),
                PUBLIC_KEY_HEX_LEN
            );
        }

        // VULN-M5: node_id musi odpowiadac czesci Ed25519 klucza. Bez tego
        // TrustedKeysSync od zlosliwego peera moze wstawic dowolny klucz pod
        // cudzym node_id i podpic tozsamosc obcego noda.
        Self::validate_identity_binding(node_id, public_key_hex)?;

        // VULN-M16: przyszly origin_approved_at sprawialby, ze trust expiry
        // (max(last_seen, approved_at)) nigdy nie wygasa — clamp do "teraz".
        let approved_at_safe = origin_approved_at.map(Self::clamp_wire_timestamp);

        let vk = Self::parse_verifying_key(public_key_hex)?;

        db::repository::add_trusted_node(
            &self.db,
            node_id,
            public_key_hex,
            hostname,
            "mesh-sync",
            approved_at_safe.as_deref(),
        )?;

        self.trusted_keys.insert(node_id.to_string(), vk);
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

    /// VULN-M1: Zwraca PIN oczekujacego parowania TYLKO gdy to MY je
    /// zainicjowalismy (`direction = "outgoing"` — PIN wygenerowany lokalnie
    /// i wyswietlony naszemu uzytkownikowi). Pending "incoming" tworzy sama
    /// ramka PairingRequest od obcego noda, wiec jego PIN nie moze
    /// autoryzowac PairingConfirm (samoparowanie z PIN-em atakujacego).
    /// Pusty PIN jest odrzucany — nigdy nie pomija weryfikacji.
    pub fn get_pending_outgoing_pin(&self, remote_node_id: &str) -> Result<Option<String>> {
        let pairing = db::repository::get_pending_pairing(&self.db, remote_node_id)?;
        Ok(pairing
            .filter(|p| p.direction == "outgoing")
            .map(|p| p.pin_code)
            .filter(|pin| !pin.is_empty()))
    }

    /// Records one PIN attempt and says whether it may proceed.
    ///
    /// `key` must be something the remote side cannot choose freely (its
    /// transport identity). A fresh identity is still cheap to mint, so every
    /// attempt also draws from one budget shared by all keys: rotating
    /// identities runs into that budget instead of getting three new guesses
    /// each time.
    pub fn check_pin_rate_limit(&self, key: &str) -> bool {
        let now = Instant::now();

        {
            let mut budget = self.pin_global_budget.lock();
            if now.duration_since(budget.1) >= PIN_ATTEMPT_WINDOW {
                *budget = (0, now);
            }
            if budget.0 >= PIN_ATTEMPTS_ALL_KEYS {
                return false;
            }
            budget.0 += 1;
        }

        if self.pin_attempts.len() >= PIN_TRACKED_KEYS {
            self.pin_attempts
                .retain(|_, (_, started)| now.duration_since(*started) < PIN_ATTEMPT_WINDOW);
            if self.pin_attempts.len() >= PIN_TRACKED_KEYS {
                return false;
            }
        }

        let mut entry = self.pin_attempts.entry(key.to_string()).or_insert((0, now));
        if now.duration_since(entry.1) >= PIN_ATTEMPT_WINDOW {
            *entry = (0, now);
        }
        if entry.0 >= PIN_ATTEMPTS_PER_KEY {
            return false;
        }
        entry.0 += 1;
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

/// `settings` key holding the environment a peer declared on a still-pending
/// pairing request (ROADMAP Z12, P1-2) — see `receive_pairing_request` /
/// `pending_pairing_environment`.
fn pending_pairing_environment_key(remote_node_id: &str) -> String {
    format!("pending_pairing_env:{}", remote_node_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_test_db() -> DbPool {
        // Run the real migrations: `add_trusted_node` now materialises trusted
        // nodes into `sync_nodes` and seeds `sync_policies`, so a hand-rolled
        // partial schema would miss those tables.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn test_settings_cipher() -> Arc<crate::crypto::SettingsCipher> {
        Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32]))
    }

    /// Losowy klucz node'a w formacie wire: Ed25519 (64 hex) + X25519 (64 hex).
    /// Ed25519 czesc musi byc poprawnym punktem krzywej — losowy hex,
    /// np. "ab".repeat(64), dekoduje sie tylko w ~polowie przypadkow.
    fn random_node_key_hex() -> String {
        let mut seed = [0u8; 32];
        loop {
            getrandom::fill(&mut seed).unwrap();
            let ed: String = seed.iter().map(|b| format!("{:02x}", b)).collect();
            if MeshSecurity::parse_verifying_key(&ed).is_ok() {
                let mut x = [0u8; 32];
                getrandom::fill(&mut x).unwrap();
                let x_hex: String = x.iter().map(|b| format!("{:02x}", b)).collect();
                return format!("{ed}{x_hex}");
            }
        }
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

        // VULN-M5: node_id = pierwsze 64 hex znakow klucza (binding).
        let sec_a_node_id = &sec_a.public_key_hex()[..64];
        sec_b
            .add_trusted_key(sec_a_node_id, &sec_a.public_key_hex(), "host-a", None)
            .unwrap();

        let data = b"Wiadomosc do podpisania";
        let sig = sec_a.sign(data);

        assert!(sec_b.verify(sec_a_node_id, data, &sig).unwrap());

        let mut bad_sig = sig.clone();
        bad_sig[0] ^= 0xFF;
        assert!(!sec_b.verify(sec_a_node_id, data, &bad_sig).unwrap());
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
        sec.revoke_trust("node-x", None).unwrap();
        assert!(sec.is_revoked("node-x"));

        sec.admin_retrust("node-x").unwrap();
        assert!(!sec.is_revoked("node-x"));
    }

    /// VULN-M1: PIN z pending "incoming" (tworzonego sama ramka PairingRequest
    /// od obcego noda) NIE moze autoryzowac PairingConfirm, a pusty PIN nigdy
    /// nie pomija weryfikacji. Auto-confirm dotyczy wylacznie pending
    /// "outgoing" z PIN-em wygenerowanym lokalnie.
    #[test]
    fn pending_outgoing_pin_ignoruje_incoming_i_puste_pin() {
        let pool = setup_test_db();
        let sec = MeshSecurity::new(pool.clone(), test_settings_cipher()).unwrap();

        // Outgoing: PIN wygenerowany lokalnie — autoryzuje Confirm.
        sec.initiate_pairing_with_pin("node-a", "123456").unwrap();
        assert_eq!(
            sec.get_pending_outgoing_pin("node-a").unwrap().as_deref(),
            Some("123456")
        );

        // Incoming: PIN wybrany przez nadawce requestu.
        sec.receive_pairing_request(
            "node-b",
            "654321",
            "",
            tentaflow_protocol::environment::NodeEnvironment::Dev,
        )
        .unwrap();
        assert_eq!(sec.get_pending_outgoing_pin("node-b").unwrap(), None);

        // Pusty PIN — nigdy nie pomija weryfikacji.
        db::repository::create_pending_pairing(
            &pool,
            "node-d",
            "",
            "outgoing",
            "2099-01-01 00:00:00",
        )
        .unwrap();
        assert_eq!(sec.get_pending_outgoing_pin("node-d").unwrap(), None);
    }

    /// VULN-M5: add_trusted_key wymaga wiazania node_id <-> klucz publiczny.
    /// Bez tego TrustedKeysSync od zlosliwego peera wstawia klucz pod cudzym
    /// node_id i podpina tozsamosc obcego noda.
    #[test]
    fn add_trusted_key_wymaga_wiazania_node_id_z_kluczem() {
        let pool = setup_test_db();
        let sec = MeshSecurity::new(pool.clone(), test_settings_cipher()).unwrap();

        // Poprawne wiazanie: node_id = pierwsze 64 znakow hex klucza.
        let pubkey = random_node_key_hex();
        let node_id = pubkey[..64].to_string();
        sec.add_trusted_key(&node_id, &pubkey, "host", None).unwrap();
        assert!(sec.is_trusted(&node_id));

        // Mismatch: klucz nie zaczyna sie od node_id — reject, brak zaufania.
        let other_pubkey = random_node_key_hex();
        let wrong_node_id = "ef".repeat(32);
        assert!(sec
            .add_trusted_key(&wrong_node_id, &other_pubkey, "host", None)
            .is_err());
        assert!(!sec.is_trusted(&wrong_node_id));

        // node_id niehex / zla dlugosc — reject.
        assert!(sec.add_trusted_key("zz", &pubkey, "host", None).is_err());
        assert!(sec.add_trusted_key("gg".repeat(32).as_str(), &pubkey, "host", None).is_err());
    }

    /// VULN-M16: przyszly origin_approved_at (z wire) jest clampowany do
    /// "teraz" — inaczej trust expiry (max(last_seen, approved_at)) nigdy
    /// nie wygasa.
    #[test]
    fn add_trusted_key_clampuje_przyszly_approved_at() {
        let pool = setup_test_db();
        let sec = MeshSecurity::new(pool.clone(), test_settings_cipher()).unwrap();
        let pubkey = random_node_key_hex();
        let node_id = pubkey[..64].to_string();
        sec.add_trusted_key(&node_id, &pubkey, "host", Some("2099-01-01 00:00:00"))
            .unwrap();

        let conn = pool.read().unwrap();
        let stored: String = conn
            .query_row(
                "SELECT approved_at FROM trusted_nodes WHERE node_id = ?1",
                rusqlite::params![node_id],
                |r| r.get(0),
            )
            .unwrap();
        drop(conn);
        assert_ne!(stored, "2099-01-01 00:00:00");
        // Format datetime('now') — sparsowalny i <= teraz.
        let parsed = chrono::NaiveDateTime::parse_from_str(&stored, "%Y-%m-%d %H:%M:%S")
            .expect("approved_at po clampie w formacie DB");
        assert!(parsed <= chrono::Utc::now().naive_utc());
    }

    #[test]
    fn add_trusted_key_rejects_revoked_node_until_admin_retrust() {
        let pool = setup_test_db();
        let sec = MeshSecurity::new(pool.clone(), test_settings_cipher()).unwrap();
        let pubkey = random_node_key_hex();
        let node_id = pubkey[..64].to_string();

        sec.add_trusted_key(&node_id, &pubkey, "host", None).unwrap();
        sec.revoke_trust(&node_id, Some("2026-01-10 12:00:00")).unwrap();

        // A peer that missed the revocation still carries the old approval.
        assert!(sec.add_trusted_key(&node_id, &pubkey, "host", None).is_err());
        assert!(sec
            .add_trusted_key(&node_id, &pubkey, "host", Some("2026-01-01 00:00:00"))
            .is_err());
        assert!(!sec.is_trusted(&node_id));

        sec.admin_retrust(&node_id).unwrap();
        sec.add_trusted_key(&node_id, &pubkey, "host", None).unwrap();
        assert!(sec.is_trusted(&node_id));
    }

    #[test]
    fn repairing_after_revocation_propagates_through_sync() {
        let pool = setup_test_db();
        let sec = MeshSecurity::new(pool.clone(), test_settings_cipher()).unwrap();
        let pubkey = random_node_key_hex();
        let node_id = pubkey[..64].to_string();
        sec.revoke_trust(&node_id, Some("2026-01-10 12:00:00")).unwrap();

        sec.add_trusted_key(&node_id, &pubkey, "host", Some("2026-01-10 12:05:00"))
            .unwrap();
        assert!(sec.is_trusted(&node_id));
        assert!(!sec.is_revoked(&node_id));

        // A node that missed the re-pairing replays the old revocation.
        assert!(sec.is_stale_revocation(&node_id, Some("2026-01-10 12:00:00")));
        // A genuinely newer revocation, or one without a timestamp, still applies.
        assert!(!sec.is_stale_revocation(&node_id, Some("2026-01-10 12:30:00")));
        assert!(!sec.is_stale_revocation(&node_id, None));
    }

    #[test]
    fn revoke_trust_clamps_future_timestamp_and_survives_restart() {
        let pool = setup_test_db();
        let sec = MeshSecurity::new(pool.clone(), test_settings_cipher()).unwrap();
        let stored = sec.revoke_trust("node-future", Some("2099-01-01 00:00:00")).unwrap();
        assert_ne!(stored, "2099-01-01 00:00:00");

        let reloaded = MeshSecurity::new(pool, test_settings_cipher()).unwrap();
        assert_eq!(
            reloaded.get_revoked_nodes(),
            vec![("node-future".to_string(), stored)]
        );
    }

    #[test]
    fn pairing_request_from_revoked_node_does_not_lift_revocation() {
        let pool = setup_test_db();
        let sec = MeshSecurity::new(pool.clone(), test_settings_cipher()).unwrap();
        let pubkey = random_node_key_hex();
        let node_id = pubkey[..64].to_string();
        sec.revoke_trust(&node_id, None).unwrap();

        sec.receive_pairing_request(
            &node_id,
            "123456",
            &pubkey,
            tentaflow_protocol::environment::NodeEnvironment::default(),
        )
        .unwrap();

        assert!(sec.is_revoked(&node_id));
    }

    #[test]
    fn a_new_pairing_request_does_not_restore_pin_attempts() {
        let pool = setup_test_db();
        let sec = MeshSecurity::new(pool, test_settings_cipher()).unwrap();
        let public_key = random_node_key_hex();
        let node_id = public_key[..64].to_string();

        for _ in 0..PIN_ATTEMPTS_PER_KEY {
            assert!(sec.check_pin_rate_limit(&node_id));
        }
        assert!(!sec.check_pin_rate_limit(&node_id));

        sec.receive_pairing_request(
            &node_id,
            "123456",
            &public_key,
            tentaflow_protocol::environment::NodeEnvironment::default(),
        )
        .unwrap();

        assert!(!sec.check_pin_rate_limit(&node_id));
    }

    #[test]
    fn rotating_identities_runs_into_the_shared_pin_budget() {
        let pool = setup_test_db();
        let sec = MeshSecurity::new(pool, test_settings_cipher()).unwrap();

        let allowed = (0..PIN_ATTEMPTS_ALL_KEYS * 2)
            .filter(|i| sec.check_pin_rate_limit(&format!("identity-{i}")))
            .count();

        assert_eq!(allowed as u32, PIN_ATTEMPTS_ALL_KEYS);
        assert!(!sec.check_pin_rate_limit("never-seen-before"));
    }

    /// Three nodes on separate databases that all trust each other, the way a
    /// paired fleet does.
    fn mutually_trusting_nodes() -> [MeshSecurity; 3] {
        let nodes = [(); 3].map(|_| MeshSecurity::new(setup_test_db(), test_settings_cipher()).unwrap());
        for a in &nodes {
            for b in &nodes {
                a.add_trusted_key(&b.ed25519_public_key_hex(), &b.public_key_hex(), "peer", None)
                    .unwrap();
            }
        }
        nodes
    }

    #[test]
    fn a_sealed_blob_opens_only_for_its_recipient_and_context() {
        let [alice, bob, carol] = mutually_trusting_nodes();
        let alice_id = alice.ed25519_public_key_hex();
        let bob_id = bob.ed25519_public_key_hex();

        let sealed = alice.seal_for_peer(&bob_id, b"hf_token|7", b"secret-value").unwrap();

        assert_eq!(bob.open_from_peer(&alice_id, b"hf_token|7", &sealed).unwrap(), b"secret-value");
        assert!(carol.open_from_peer(&alice_id, b"hf_token|7", &sealed).is_err());
        assert!(bob.open_from_peer(&alice_id, b"ngc_api_key|7", &sealed).is_err());
    }

    #[test]
    fn a_sealed_blob_cannot_be_reflected_back_to_its_sender() {
        let [alice, bob, _] = mutually_trusting_nodes();
        let sealed = alice
            .seal_for_peer(&bob.ed25519_public_key_hex(), b"ctx", b"secret-value")
            .unwrap();
        assert!(alice
            .open_from_peer(&bob.ed25519_public_key_hex(), b"ctx", &sealed)
            .is_err());
    }

    #[test]
    fn sealing_for_an_untrusted_or_keyless_node_fails() {
        let db = setup_test_db();
        let sec = MeshSecurity::new(db, test_settings_cipher()).unwrap();
        assert!(sec.seal_for_peer(&"ab".repeat(32), b"ctx", b"v").is_err());

        let keyless = random_node_key_hex();
        let keyless_id = keyless[..64].to_string();
        let without_x25519 = format!("{keyless_id}{}", "00".repeat(32));
        sec.add_trusted_key(&keyless_id, &without_x25519, "peer", None).unwrap();
        assert!(sec.seal_for_peer(&keyless_id, b"ctx", b"v").is_err());
    }
}
