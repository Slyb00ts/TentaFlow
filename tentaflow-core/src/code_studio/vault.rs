// ===== File: code_studio/vault.rs — node-local key material for Code Studio =====
//
// The registry (§5.1) says WHAT a workspace is and travels through the Sync
// Ledger. This module holds what must never travel: the git credential of a
// workspace and the provider credential of an agent CLI, encrypted with the
// per-node `SettingsCipher` key. Neither table is listed in
// `sync/core_registry.rs`, and that omission is the design — a key encrypted
// with this node's key is meaningless anywhere else, so replicating the rows
// would only spread ciphertext nobody can open.
//
// Where material may go: the git broker (§11) and the provider adapter (§7.5),
// both of which run OUTSIDE the sandbox. Nothing else. That is why the only
// way out of this module is `SecretMaterial` — a type whose `Debug` prints
// `<redacted>` and whose `Drop` wipes the buffer, so a stray `{:?}` in a log
// line or a leftover allocation cannot become a credential leak.
//
// A missing row is `secret_missing` / `credential_missing`, never a silent
// `None`: a workspace opened on a node that does not hold its key must say so
// instead of failing later inside git with an authentication error nobody can
// attribute.

use std::fmt;

use base64::Engine as _;
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::crypto::SettingsCipher;
use crate::db::DbPool;

/// Prefix `SettingsCipher::encrypt` puts in front of every value. `decrypt`
/// accepts an unprefixed value verbatim (a migration affordance for settings),
/// which for a vault row would silently turn tampered plaintext into "material".
/// The vault therefore checks the prefix itself.
const ENC_PREFIX: &str = "enc:";

/// Prefix of a value bound to the row it lives in (`SettingsCipher::encrypt_bound`).
/// Everything this module writes carries it; `enc:` only appears on rows written
/// before binding existed, and those are rewritten the first time they are read.
const BOUND_PREFIX: &str = "encb:";

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;
const B64_NOPAD: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD_NO_PAD;

// =============================================================================
// Errors
// =============================================================================

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// The handle exists in the registry but this node holds no material for
    /// it. Distinct from every other failure so the caller can tell the user
    /// "the key lives on another node" instead of "authentication failed".
    #[error("secret_missing: no credential material for '{0}' on this node")]
    SecretMissing(String),
    /// No provider credential for this (org, node, engine) triple.
    #[error("credential_missing: no credential for engine '{engine_id}' on node '{node_id}'")]
    CredentialMissing { node_id: String, engine_id: String },
    /// A stored row is not what this module wrote — plaintext where ciphertext
    /// belongs, or an unknown kind. Never decrypted, never used.
    #[error("vault row '{0}' is corrupt")]
    Corrupt(String),
    /// Encryption or decryption failed. The message deliberately carries no
    /// detail from the cipher: an oracle is not a diagnostic.
    #[error("vault cipher failure")]
    Cipher,
    #[error("vault database error: {0}")]
    Db(String),
    #[error("{0}")]
    Invalid(String),
}

type Result<T> = std::result::Result<T, VaultError>;

fn db_err(e: impl fmt::Display) -> VaultError {
    VaultError::Db(e.to_string())
}

// =============================================================================
// Kinds and material
// =============================================================================

/// What kind of credential a workspace secret is. The strings match the CHECK
/// constraint of `code_workspace_secrets.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    GitToken,
    SshKey,
}

impl SecretKind {
    pub fn slug(self) -> &'static str {
        match self {
            SecretKind::GitToken => "git_token",
            SecretKind::SshKey => "ssh_key",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "git_token" => Some(SecretKind::GitToken),
            "ssh_key" => Some(SecretKind::SshKey),
            _ => None,
        }
    }

    /// Value of `code_workspaces.repo_auth_kind` that corresponds to this kind.
    /// The registry column names the AUTH METHOD, the vault column names the
    /// MATERIAL; they are two vocabularies for the same choice and this is the
    /// single place that maps between them.
    pub fn repo_auth_kind(self) -> &'static str {
        match self {
            SecretKind::GitToken => "token",
            SecretKind::SshKey => "ssh_key",
        }
    }

    pub fn from_repo_auth_kind(slug: &str) -> Option<Self> {
        match slug {
            "token" => Some(SecretKind::GitToken),
            "ssh_key" => Some(SecretKind::SshKey),
            _ => None,
        }
    }
}

/// Decrypted credential material. The ONLY way material leaves this module.
///
/// `Debug` prints `<redacted>` and `Drop` wipes the buffer, so the material
/// cannot reach a log line, a panic message or a freed page that someone reads
/// later. Callers hold it for exactly one operation and drop it.
pub struct SecretMaterial {
    kind: SecretKind,
    fingerprint: String,
    material: String,
}

impl SecretMaterial {
    pub fn kind(&self) -> SecretKind {
        self.kind
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Hands the material to the git broker (§11) or the provider adapter
    /// (§7.5). There is no other legitimate caller: everything else works with
    /// the handle and the fingerprint.
    pub fn expose(&self) -> &str {
        &self.material
    }
}

impl Drop for SecretMaterial {
    fn drop(&mut self) {
        self.material.zeroize();
    }
}

impl fmt::Debug for SecretMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretMaterial")
            .field("kind", &self.kind.slug())
            .field("fingerprint", &self.fingerprint)
            .field("material", &"<redacted>")
            .finish()
    }
}

/// What a write produced: the handle the registry stores and the fingerprint
/// the UI shows. Neither is the material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSecret {
    pub secret_ref: String,
    pub fingerprint: String,
}

/// Outcome of a rotation. `superseded_ref` is still IN the vault on purpose
/// (§5.2): the previous key is only removed once the new one has proven itself
/// in a real operation, so a bad key cannot lock the workspace out of its
/// repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rotation {
    pub secret_ref: String,
    pub fingerprint: String,
    pub superseded_ref: Option<String>,
}

/// A provider credential plus the upstream the adapter forwards to.
pub struct AgentCredential {
    pub provider_base_url: String,
    pub material: SecretMaterial,
}

impl fmt::Debug for AgentCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentCredential")
            .field("provider_base_url", &self.provider_base_url)
            .field("material", &self.material)
            .finish()
    }
}

// =============================================================================
// Fingerprints
// =============================================================================

/// Identifies stored material WITHOUT being able to reconstruct it.
///
/// For an OpenSSH private key this is the real public-key fingerprint, byte for
/// byte what `ssh-keygen -lf` prints — the openssh-key-v1 container carries the
/// public half in the clear, so no key parsing or external binary is needed.
/// For anything else (a token, or a key in a container we cannot read) it is a
/// SHA-256 digest of the material, labelled as such so nobody mistakes it for a
/// key fingerprint.
pub fn fingerprint_of(kind: SecretKind, material: &str) -> String {
    if kind == SecretKind::SshKey {
        if let Some(blob) = openssh_public_blob(material) {
            return format!("SHA256:{}", B64_NOPAD.encode(Sha256::digest(&blob)));
        }
    }
    format!("sha256:{}", hex::encode(Sha256::digest(material.as_bytes())))
}

/// Extracts the public-key blob from an `openssh-key-v1` private key. The
/// container is: magic, ciphername, kdfname, kdfoptions, key count, then one
/// length-prefixed public key blob per key.
fn openssh_public_blob(pem: &str) -> Option<Vec<u8>> {
    const BEGIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
    const END: &str = "-----END OPENSSH PRIVATE KEY-----";
    let start = pem.find(BEGIN)? + BEGIN.len();
    let end = pem[start..].find(END)? + start;
    let body: String = pem[start..end].chars().filter(|c| !c.is_whitespace()).collect();
    let raw = B64.decode(body).ok()?;

    let mut cursor = raw.strip_prefix(b"openssh-key-v1\0".as_slice())?;
    for _ in 0..3 {
        let (_, rest) = read_ssh_string(cursor)?;
        cursor = rest;
    }
    let (count, rest) = read_ssh_u32(cursor)?;
    if count == 0 {
        return None;
    }
    let (blob, _) = read_ssh_string(rest)?;
    Some(blob.to_vec())
}

fn read_ssh_u32(buf: &[u8]) -> Option<(u32, &[u8])> {
    if buf.len() < 4 {
        return None;
    }
    let (head, rest) = buf.split_at(4);
    Some((u32::from_be_bytes(head.try_into().ok()?), rest))
}

fn read_ssh_string(buf: &[u8]) -> Option<(&[u8], &[u8])> {
    let (len, rest) = read_ssh_u32(buf)?;
    let len = len as usize;
    if rest.len() < len {
        return None;
    }
    Some(rest.split_at(len))
}

// =============================================================================
// Workspace secrets
// =============================================================================

/// Stores credential material for a workspace and returns the HANDLE the
/// registry keeps. The caller owns the plaintext it passed in — the wire buffer
/// is its responsibility, this module only guarantees what it stores itself.
pub fn put_workspace_secret(
    db: &DbPool,
    cipher: &SettingsCipher,
    workspace_id: &str,
    kind: SecretKind,
    material: &str,
    created_by: &str,
) -> Result<StoredSecret> {
    // The handle is minted BEFORE the material is sealed: the ciphertext is
    // bound to the row it is about to become, which is what stops it being
    // useful anywhere else.
    let secret_ref = new_secret_ref();
    let stored = encrypt_material(
        cipher,
        kind,
        material,
        &workspace_secret_context(workspace_id, &secret_ref),
    )?;
    let conn = db.write().map_err(db_err)?;
    conn.execute(
        "INSERT INTO code_workspace_secrets \
           (secret_ref, workspace_id, kind, material_enc, fingerprint, created_by, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))",
        params![
            secret_ref,
            workspace_id,
            kind.slug(),
            stored.ciphertext,
            stored.fingerprint,
            created_by
        ],
    )
    .map_err(db_err)?;
    Ok(StoredSecret {
        secret_ref,
        fingerprint: stored.fingerprint,
    })
}

/// Reads material back and stamps `last_used_at`. The stamp is not telemetry:
/// it is what makes "the new key has worked at least once" observable, and
/// `confirm_rotation` refuses to drop the previous key without it.
pub fn get_workspace_secret(
    db: &DbPool,
    cipher: &SettingsCipher,
    secret_ref: &str,
) -> Result<SecretMaterial> {
    let row: Option<(String, String, Vec<u8>, Option<String>)> = {
        let conn = db.read().map_err(db_err)?;
        conn.query_row(
            "SELECT workspace_id, kind, material_enc, fingerprint FROM code_workspace_secrets \
             WHERE secret_ref = ?1",
            params![secret_ref],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(db_err)?
    };
    let (workspace_id, kind_slug, ciphertext, fingerprint) =
        row.ok_or_else(|| VaultError::SecretMissing(secret_ref.to_string()))?;
    let kind = SecretKind::from_slug(&kind_slug)
        .ok_or_else(|| VaultError::Corrupt(secret_ref.to_string()))?;
    let context = workspace_secret_context(&workspace_id, secret_ref);
    let opened = decrypt_material(cipher, secret_ref, &ciphertext, &context)?;
    let material = opened.value;

    let conn = db.write().map_err(db_err)?;
    if !opened.bound {
        // Lazy migration: a row written before binding existed is rewritten to
        // its bound form the first time it is read, so the relocatable copy
        // stops existing at the first use rather than at the next rotation.
        let rebound = cipher
            .encrypt_bound(&material, &context)
            .map_err(|_| VaultError::Cipher)?;
        conn.execute(
            "UPDATE code_workspace_secrets SET material_enc = ?2 WHERE secret_ref = ?1",
            params![secret_ref, rebound.into_bytes()],
        )
        .map_err(db_err)?;
    }
    conn.execute(
        "UPDATE code_workspace_secrets SET last_used_at = datetime('now') WHERE secret_ref = ?1",
        params![secret_ref],
    )
    .map_err(db_err)?;

    Ok(SecretMaterial {
        kind,
        fingerprint: fingerprint.unwrap_or_else(|| fingerprint_of(kind, &material)),
        material,
    })
}

/// Material of the workspace's CURRENT handle. `Ok(None)` means the workspace
/// stores no credential at all (a public repository); a handle that points at
/// nothing on this node is still `secret_missing`.
pub fn get_current_workspace_secret(
    db: &DbPool,
    cipher: &SettingsCipher,
    workspace_id: &str,
) -> Result<Option<SecretMaterial>> {
    let handle: Option<String> = {
        let conn = db.read().map_err(db_err)?;
        conn.query_row(
            "SELECT secret_ref FROM code_workspaces WHERE id = ?1",
            params![workspace_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_err)?
        .flatten()
    };
    match handle {
        Some(secret_ref) => get_workspace_secret(db, cipher, &secret_ref).map(Some),
        None => Ok(None),
    }
}

/// Rotation, §5.2: write the new row, swap the registry handle atomically, and
/// LEAVE the previous row in place. The order matters — swapping first would
/// leave a window where the handle points at nothing, and deleting the old row
/// here would strand a workspace whose new key turns out to be wrong.
pub fn rotate_workspace_secret(
    db: &DbPool,
    cipher: &SettingsCipher,
    workspace_id: &str,
    kind: SecretKind,
    material: &str,
    created_by: &str,
) -> Result<Rotation> {
    let secret_ref = new_secret_ref();
    let stored = encrypt_material(
        cipher,
        kind,
        material,
        &workspace_secret_context(workspace_id, &secret_ref),
    )?;

    let mut conn = db.write().map_err(db_err)?;
    let tx = conn.transaction().map_err(db_err)?;
    let superseded: Option<String> = tx
        .query_row(
            "SELECT secret_ref FROM code_workspaces WHERE id = ?1",
            params![workspace_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_err)?
        .flatten();
    tx.execute(
        "INSERT INTO code_workspace_secrets \
           (secret_ref, workspace_id, kind, material_enc, fingerprint, created_by, created_at, \
            rotated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'), \
                 CASE WHEN ?7 IS NULL THEN NULL ELSE datetime('now') END)",
        params![
            secret_ref,
            workspace_id,
            kind.slug(),
            stored.ciphertext,
            stored.fingerprint,
            created_by,
            superseded
        ],
    )
    .map_err(db_err)?;
    let changed = tx
        .execute(
            "UPDATE code_workspaces SET secret_ref = ?2, repo_auth_kind = ?3, \
             updated_at = datetime('now') WHERE id = ?1",
            params![workspace_id, secret_ref, kind.repo_auth_kind()],
        )
        .map_err(db_err)?;
    if changed == 0 {
        return Err(VaultError::Invalid(format!(
            "workspace '{workspace_id}' does not exist"
        )));
    }
    // The HANDLE is registry, not vault: the org must learn which credential a
    // workspace now points at. The material behind it stays on this node.
    super::sync_capture::capture_workspace(&tx, workspace_id).map_err(db_err)?;
    tx.commit().map_err(db_err)?;

    Ok(Rotation {
        superseded_ref: superseded.filter(|old| old != &secret_ref),
        secret_ref,
        fingerprint: stored.fingerprint,
    })
}

/// Drops every superseded row of a workspace, but ONLY once the current handle
/// has actually been used (`last_used_at` set by `get_workspace_secret`). Until
/// then the previous key stays available, which is the whole point of keeping
/// it. Returns how many rows were removed.
pub fn confirm_rotation(db: &DbPool, workspace_id: &str) -> Result<usize> {
    let mut conn = db.write().map_err(db_err)?;
    let tx = conn.transaction().map_err(db_err)?;
    let current: Option<String> = tx
        .query_row(
            "SELECT secret_ref FROM code_workspaces WHERE id = ?1",
            params![workspace_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_err)?
        .flatten();
    let Some(current) = current else {
        return Ok(0);
    };
    let used: Option<String> = tx
        .query_row(
            "SELECT last_used_at FROM code_workspace_secrets WHERE secret_ref = ?1",
            params![current],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_err)?
        .flatten();
    if used.is_none() {
        return Ok(0);
    }
    let removed = tx
        .execute(
            "DELETE FROM code_workspace_secrets WHERE workspace_id = ?1 AND secret_ref <> ?2",
            params![workspace_id, current],
        )
        .map_err(db_err)?;
    tx.commit().map_err(db_err)?;
    Ok(removed)
}

/// Removes every piece of material a workspace owns and detaches the handle.
/// Called the moment a workspace is deleted and when its credential is cleared
/// — §13.5 gives secrets no grace period, unlike events or artifacts.
pub fn delete_workspace_secrets(db: &DbPool, workspace_id: &str) -> Result<usize> {
    let mut conn = db.write().map_err(db_err)?;
    let tx = conn.transaction().map_err(db_err)?;
    let removed = tx
        .execute(
            "DELETE FROM code_workspace_secrets WHERE workspace_id = ?1",
            params![workspace_id],
        )
        .map_err(db_err)?;
    tx.execute(
        "UPDATE code_workspaces SET secret_ref = NULL, updated_at = datetime('now') \
         WHERE id = ?1",
        params![workspace_id],
    )
    .map_err(db_err)?;
    super::sync_capture::capture_workspace(&tx, workspace_id).map_err(db_err)?;
    tx.commit().map_err(db_err)?;
    Ok(removed)
}

// =============================================================================
// Agent (provider CLI) credentials
// =============================================================================

/// Stores or replaces the provider credential of one engine on THIS node. The
/// account is organizational, the material is node-local — the key that
/// protects it is.
#[allow(clippy::too_many_arguments)]
pub fn put_agent_credential(
    db: &DbPool,
    cipher: &SettingsCipher,
    org_id: &str,
    node_id: &str,
    engine_id: &str,
    material: &str,
    provider_base_url: &str,
    created_by: &str,
) -> Result<String> {
    if provider_base_url.trim().is_empty() {
        return Err(VaultError::Invalid(
            "a provider credential needs the upstream base url the adapter forwards to".into(),
        ));
    }
    let stored = encrypt_material(
        cipher,
        SecretKind::GitToken,
        material,
        &agent_credential_context(org_id, node_id, engine_id),
    )?;
    let conn = db.write().map_err(db_err)?;
    conn.execute(
        "INSERT INTO code_agent_credentials \
           (org_id, node_id, engine_id, material_enc, provider_base_url, fingerprint, \
            created_by, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now')) \
         ON CONFLICT(org_id, node_id, engine_id) DO UPDATE SET \
            material_enc = excluded.material_enc, \
            provider_base_url = excluded.provider_base_url, \
            fingerprint = excluded.fingerprint, \
            rotated_at = datetime('now')",
        params![
            org_id,
            node_id,
            engine_id,
            stored.ciphertext,
            provider_base_url,
            stored.fingerprint,
            created_by
        ],
    )
    .map_err(db_err)?;
    Ok(stored.fingerprint)
}

/// Reads the provider credential the adapter must inject. A missing row is
/// `credential_missing`: the engine is configured for the organization but this
/// node was never given the key, and the adapter has to say that rather than
/// forward an unauthenticated request.
pub fn get_agent_credential(
    db: &DbPool,
    cipher: &SettingsCipher,
    org_id: &str,
    node_id: &str,
    engine_id: &str,
) -> Result<AgentCredential> {
    let row: Option<(Vec<u8>, String, Option<String>)> = {
        let conn = db.read().map_err(db_err)?;
        conn.query_row(
            "SELECT material_enc, provider_base_url, fingerprint FROM code_agent_credentials \
             WHERE org_id = ?1 AND node_id = ?2 AND engine_id = ?3",
            params![org_id, node_id, engine_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(db_err)?
    };
    let (ciphertext, provider_base_url, fingerprint) =
        row.ok_or_else(|| VaultError::CredentialMissing {
            node_id: node_id.to_string(),
            engine_id: engine_id.to_string(),
        })?;
    let label = format!("{org_id}/{node_id}/{engine_id}");
    let context = agent_credential_context(org_id, node_id, engine_id);
    let opened = decrypt_material(cipher, &label, &ciphertext, &context)?;
    let material = opened.value;

    let conn = db.write().map_err(db_err)?;
    if !opened.bound {
        let rebound = cipher
            .encrypt_bound(&material, &context)
            .map_err(|_| VaultError::Cipher)?;
        conn.execute(
            "UPDATE code_agent_credentials SET material_enc = ?4 \
             WHERE org_id = ?1 AND node_id = ?2 AND engine_id = ?3",
            params![org_id, node_id, engine_id, rebound.into_bytes()],
        )
        .map_err(db_err)?;
    }
    conn.execute(
        "UPDATE code_agent_credentials SET last_used_at = datetime('now') \
         WHERE org_id = ?1 AND node_id = ?2 AND engine_id = ?3",
        params![org_id, node_id, engine_id],
    )
    .map_err(db_err)?;

    Ok(AgentCredential {
        provider_base_url,
        material: SecretMaterial {
            kind: SecretKind::GitToken,
            fingerprint: fingerprint.unwrap_or_else(|| fingerprint_of(SecretKind::GitToken, &material)),
            material,
        },
    })
}

/// Removes the provider credential of one engine on this node.
pub fn delete_agent_credential(
    db: &DbPool,
    org_id: &str,
    node_id: &str,
    engine_id: &str,
) -> Result<usize> {
    let conn = db.write().map_err(db_err)?;
    conn.execute(
        "DELETE FROM code_agent_credentials \
         WHERE org_id = ?1 AND node_id = ?2 AND engine_id = ?3",
        params![org_id, node_id, engine_id],
    )
    .map_err(db_err)
}

// =============================================================================
// Internals
// =============================================================================

struct Encrypted {
    ciphertext: Vec<u8>,
    fingerprint: String,
}

/// What a workspace secret is bound to. Anything that identifies the row and
/// nothing that can change under it: moving the ciphertext to another
/// workspace, or under another handle, has to break the tag.
fn workspace_secret_context(workspace_id: &str, secret_ref: &str) -> Vec<u8> {
    format!("code-studio/workspace-secret/v1\0{workspace_id}\0{secret_ref}").into_bytes()
}

/// The same idea for a provider credential, whose identity is the triple it is
/// keyed by.
fn agent_credential_context(org_id: &str, node_id: &str, engine_id: &str) -> Vec<u8> {
    format!("code-studio/agent-credential/v1\0{org_id}\0{node_id}\0{engine_id}").into_bytes()
}

fn encrypt_material(
    cipher: &SettingsCipher,
    kind: SecretKind,
    material: &str,
    context: &[u8],
) -> Result<Encrypted> {
    if material.trim().is_empty() {
        return Err(VaultError::Invalid("credential material is empty".into()));
    }
    let fingerprint = fingerprint_of(kind, material);
    let ciphertext = cipher
        .encrypt_bound(material, context)
        .map_err(|_| VaultError::Cipher)?
        .into_bytes();
    Ok(Encrypted {
        ciphertext,
        fingerprint,
    })
}

fn decrypt_material(
    cipher: &SettingsCipher,
    label: &str,
    ciphertext: &[u8],
    context: &[u8],
) -> Result<crate::crypto::BoundPlaintext> {
    let stored = std::str::from_utf8(ciphertext).map_err(|_| VaultError::Corrupt(label.into()))?;
    if !stored.starts_with(BOUND_PREFIX) && !stored.starts_with(ENC_PREFIX) {
        // An unprefixed value is not a legacy value here: it is a row nobody in
        // this module wrote, and it is never handed back as "material".
        return Err(VaultError::Corrupt(label.into()));
    }
    // A bound row that no longer matches its own identity — a row copied from
    // another workspace by someone with database access but no master key —
    // fails the tag and comes back as a cipher failure, not as a credential.
    cipher
        .decrypt_bound(stored, context)
        .map_err(|_| VaultError::Cipher)
}

fn new_secret_ref() -> String {
    format!("cs-secret-{}", uuid::Uuid::new_v4())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_studio::models::{
        AutonomyMode, EgressEnforcement, ExecMode, NewWorkspace, WorkspaceRecord,
    };
    use crate::code_studio::repository;

    const TOKEN: &str = "ghp_thisisnotarealtokenbutlooksdangerousenough";

    fn test_db() -> (tempfile::TempDir, DbPool, SettingsCipher) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = crate::db::init(&dir.path().join("tentaflow.db")).expect("init db");
        (dir, db, SettingsCipher::new(&[7u8; 32]))
    }

    fn workspace(db: &DbPool, id: &str) -> WorkspaceRecord {
        repository::create_workspace(
            db,
            &NewWorkspace {
                id: id.to_string(),
                org_id: "org-1".into(),
                owner_user_id: "u-owner".into(),
                name: "Workspace".into(),
                slug: id.to_string(),
                node_id: "node-1".into(),
                exec_mode: ExecMode::TrustedNative,
                container_image: None,
                egress_enforcement: EgressEnforcement::Unrestricted,
                repo_kind: "git".into(),
                repo_url: Some("https://example.invalid/r.git".into()),
                repo_auth_kind: Some("none".into()),
                secret_ref: None,
                ssh_host_fingerprint: None,
                default_branch: None,
                target_branch: None,
                autonomy_ceiling: AutonomyMode::Normal,
                egress_policy: "org_approved".into(),
                index_enabled: false,
                quota_disk_bytes: None,
                quota_sessions: None,
            },
        )
        .expect("create workspace")
    }

    fn stored_blob(db: &DbPool, secret_ref: &str) -> Vec<u8> {
        let conn = db.read().unwrap();
        conn.query_row(
            "SELECT material_enc FROM code_workspace_secrets WHERE secret_ref = ?1",
            params![secret_ref],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn handle_of(db: &DbPool, workspace_id: &str) -> Option<String> {
        let conn = db.read().unwrap();
        conn.query_row(
            "SELECT secret_ref FROM code_workspaces WHERE id = ?1",
            params![workspace_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// The round trip has to survive the cipher AND leave nothing readable on
    /// disk: a vault whose table can be grepped for `ghp_` is not a vault.
    #[test]
    fn material_round_trips_and_never_lands_in_the_table_in_the_clear() {
        let (_dir, db, cipher) = test_db();
        workspace(&db, "ws-1");

        let stored =
            put_workspace_secret(&db, &cipher, "ws-1", SecretKind::GitToken, TOKEN, "u-owner")
                .expect("put");
        assert!(stored.secret_ref.starts_with("cs-secret-"));
        assert!(stored.fingerprint.starts_with("sha256:"));
        assert!(!stored.fingerprint.contains(TOKEN));

        let blob = stored_blob(&db, &stored.secret_ref);
        assert!(blob.starts_with(BOUND_PREFIX.as_bytes()));
        assert!(
            !String::from_utf8_lossy(&blob).contains(TOKEN),
            "the token is readable in the table"
        );

        let material = get_workspace_secret(&db, &cipher, &stored.secret_ref).expect("get");
        assert_eq!(material.expose(), TOKEN);
        assert_eq!(material.kind(), SecretKind::GitToken);
        assert_eq!(material.fingerprint(), stored.fingerprint);
    }

    #[test]
    fn a_row_moved_to_another_workspace_is_not_a_credential_there() {
        // The attacker in §5.2 has write access to `tentaflow.db` and no master
        // key. AES-GCM authenticates the bytes, not the place they are kept, so
        // an unbound ciphertext stays valid wherever it is pasted: the row of
        // workspace A, copied into the row of workspace B, would let B's broker
        // authenticate with A's token against a remote the attacker chose.
        let (_dir, db, cipher) = test_db();
        workspace(&db, "ws-a");
        workspace(&db, "ws-b");

        let victim =
            put_workspace_secret(&db, &cipher, "ws-a", SecretKind::GitToken, TOKEN, "u-owner")
                .expect("put a");
        let theirs = put_workspace_secret(
            &db,
            &cipher,
            "ws-b",
            SecretKind::GitToken,
            "ghp_theirownuselesstokenvaluehere",
            "u-owner",
        )
        .expect("put b");

        // The move: A's ciphertext under B's handle.
        let stolen = stored_blob(&db, &victim.secret_ref);
        {
            let conn = db.write().unwrap();
            conn.execute(
                "UPDATE code_workspace_secrets SET material_enc = ?2 WHERE secret_ref = ?1",
                params![theirs.secret_ref, stolen],
            )
            .unwrap();
        }

        let error = get_workspace_secret(&db, &cipher, &theirs.secret_ref)
            .expect_err("a relocated credential decrypted in its new home");
        assert!(matches!(error, VaultError::Cipher), "{error}");

        // The row it was taken from still works: binding is not a lock-out.
        assert_eq!(
            get_workspace_secret(&db, &cipher, &victim.secret_ref)
                .unwrap()
                .expose(),
            TOKEN
        );
    }

    #[test]
    fn a_row_written_before_binding_is_rewritten_on_first_read() {
        // Migration: rows encrypted before the binding existed still open, and
        // the first read replaces them with a bound ciphertext, so the
        // relocatable copy stops existing at first use instead of at the next
        // rotation.
        let (_dir, db, cipher) = test_db();
        workspace(&db, "ws-1");
        let stored =
            put_workspace_secret(&db, &cipher, "ws-1", SecretKind::GitToken, TOKEN, "u-owner")
                .expect("put");

        let legacy = cipher.encrypt(TOKEN).unwrap().into_bytes();
        {
            let conn = db.write().unwrap();
            conn.execute(
                "UPDATE code_workspace_secrets SET material_enc = ?2 WHERE secret_ref = ?1",
                params![stored.secret_ref, legacy],
            )
            .unwrap();
        }

        assert_eq!(
            get_workspace_secret(&db, &cipher, &stored.secret_ref)
                .unwrap()
                .expose(),
            TOKEN,
            "an existing row stopped opening"
        );
        assert!(
            stored_blob(&db, &stored.secret_ref).starts_with(BOUND_PREFIX.as_bytes()),
            "the legacy row was left relocatable"
        );
    }

    /// `Debug` is how a secret escapes without anyone meaning to: one `{:?}` in
    /// a tracing call and the token is in the log forever.
    #[test]
    fn debug_output_never_contains_the_material() {
        let material = SecretMaterial {
            kind: SecretKind::GitToken,
            fingerprint: "sha256:abc".into(),
            material: TOKEN.into(),
        };
        let rendered = format!("{material:?}");
        assert!(!rendered.contains(TOKEN), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");

        let credential = AgentCredential {
            provider_base_url: "https://api.example.invalid".into(),
            material,
        };
        let rendered = format!("{credential:?}");
        assert!(!rendered.contains(TOKEN), "{rendered}");
    }

    #[test]
    fn reading_a_handle_this_node_does_not_hold_is_secret_missing() {
        let (_dir, db, cipher) = test_db();
        let err = get_workspace_secret(&db, &cipher, "cs-secret-nowhere").unwrap_err();
        assert!(
            matches!(err, VaultError::SecretMissing(ref r) if r == "cs-secret-nowhere"),
            "{err:?}"
        );
        assert!(err.to_string().contains("secret_missing"));
    }

    /// A row that is not ciphertext is refused instead of being handed back as
    /// material — `SettingsCipher::decrypt` alone would return it verbatim.
    #[test]
    fn a_plaintext_row_is_corrupt_not_material() {
        let (_dir, db, cipher) = test_db();
        workspace(&db, "ws-1");
        {
            let conn = db.write().unwrap();
            conn.execute(
                "INSERT INTO code_workspace_secrets \
                   (secret_ref, workspace_id, kind, material_enc, created_by, created_at) \
                 VALUES ('cs-secret-raw', 'ws-1', 'git_token', ?1, 'u', datetime('now'))",
                params![TOKEN.as_bytes()],
            )
            .unwrap();
        }
        let err = get_workspace_secret(&db, &cipher, "cs-secret-raw").unwrap_err();
        assert!(matches!(err, VaultError::Corrupt(_)), "{err:?}");
    }

    /// §5.2: new row → atomic handle swap → the OLD row survives until the new
    /// key has actually worked. Dropping it earlier would leave a workspace with
    /// a bad key and no way back.
    #[test]
    fn rotation_keeps_the_previous_key_until_the_new_one_has_been_used() {
        let (_dir, db, cipher) = test_db();
        workspace(&db, "ws-1");
        let first =
            put_workspace_secret(&db, &cipher, "ws-1", SecretKind::GitToken, TOKEN, "u-owner")
                .expect("put");
        {
            let conn = db.write().unwrap();
            conn.execute(
                "UPDATE code_workspaces SET secret_ref = ?2 WHERE id = ?1",
                params!["ws-1", first.secret_ref],
            )
            .unwrap();
        }

        let rotation = rotate_workspace_secret(
            &db,
            &cipher,
            "ws-1",
            SecretKind::GitToken,
            "ghp_thenewoneentirelydifferent",
            "u-owner",
        )
        .expect("rotate");
        assert_eq!(rotation.superseded_ref.as_deref(), Some(first.secret_ref.as_str()));
        assert_eq!(handle_of(&db, "ws-1").as_deref(), Some(rotation.secret_ref.as_str()));
        assert_ne!(rotation.fingerprint, first.fingerprint);

        // Not used yet: nothing may be removed.
        assert_eq!(confirm_rotation(&db, "ws-1").expect("confirm"), 0);
        assert!(get_workspace_secret(&db, &cipher, &first.secret_ref).is_ok());

        // The new key proves itself, and only then the old one goes.
        let current = get_workspace_secret(&db, &cipher, &rotation.secret_ref).expect("get");
        assert_eq!(current.expose(), "ghp_thenewoneentirelydifferent");
        drop(current);
        assert_eq!(confirm_rotation(&db, "ws-1").expect("confirm"), 1);
        assert!(matches!(
            get_workspace_secret(&db, &cipher, &first.secret_ref).unwrap_err(),
            VaultError::SecretMissing(_)
        ));
    }

    #[test]
    fn rotating_a_workspace_that_does_not_exist_writes_nothing() {
        let (_dir, db, cipher) = test_db();
        let err = rotate_workspace_secret(
            &db,
            &cipher,
            "ws-missing",
            SecretKind::GitToken,
            TOKEN,
            "u-owner",
        )
        .unwrap_err();
        assert!(matches!(err, VaultError::Invalid(_)), "{err:?}");
        let conn = db.read().unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM code_workspace_secrets", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0, "the rolled-back insert survived");
    }

    /// §13.5: secrets have no retention window. Deleting the workspace takes the
    /// material with it, and the handle stops pointing at anything.
    #[test]
    fn deleting_a_workspace_removes_its_material_immediately() {
        let (_dir, db, cipher) = test_db();
        workspace(&db, "ws-1");
        let stored =
            put_workspace_secret(&db, &cipher, "ws-1", SecretKind::SshKey, TOKEN, "u-owner")
                .expect("put");
        {
            let conn = db.write().unwrap();
            conn.execute(
                "UPDATE code_workspaces SET secret_ref = ?2 WHERE id = ?1",
                params!["ws-1", stored.secret_ref],
            )
            .unwrap();
        }

        assert_eq!(delete_workspace_secrets(&db, "ws-1").expect("delete"), 1);
        assert!(handle_of(&db, "ws-1").is_none());
        assert!(matches!(
            get_workspace_secret(&db, &cipher, &stored.secret_ref).unwrap_err(),
            VaultError::SecretMissing(_)
        ));
        assert!(get_current_workspace_secret(&db, &cipher, "ws-1")
            .expect("current")
            .is_none());
    }

    #[test]
    fn an_agent_credential_is_addressed_by_org_node_and_engine() {
        let (_dir, db, cipher) = test_db();
        let missing = get_agent_credential(&db, &cipher, "org-1", "node-1", "codex").unwrap_err();
        assert!(
            matches!(missing, VaultError::CredentialMissing { .. }),
            "{missing:?}"
        );
        assert!(missing.to_string().contains("credential_missing"));

        put_agent_credential(
            &db,
            &cipher,
            "org-1",
            "node-1",
            "codex",
            TOKEN,
            "https://api.example.invalid",
            "u-admin",
        )
        .expect("put");

        // Another node's row is a different credential, not this one.
        assert!(get_agent_credential(&db, &cipher, "org-1", "node-2", "codex").is_err());

        let credential = get_agent_credential(&db, &cipher, "org-1", "node-1", "codex").expect("get");
        assert_eq!(credential.material.expose(), TOKEN);
        assert_eq!(credential.provider_base_url, "https://api.example.invalid");

        assert_eq!(
            delete_agent_credential(&db, "org-1", "node-1", "codex").expect("delete"),
            1
        );
        assert!(get_agent_credential(&db, &cipher, "org-1", "node-1", "codex").is_err());
    }

    #[test]
    fn empty_material_is_refused_before_anything_is_written() {
        let (_dir, db, cipher) = test_db();
        workspace(&db, "ws-1");
        let err = put_workspace_secret(&db, &cipher, "ws-1", SecretKind::GitToken, "   ", "u")
            .unwrap_err();
        assert!(matches!(err, VaultError::Invalid(_)), "{err:?}");
    }

    /// An OpenSSH private key carries its public half in the clear, so the
    /// fingerprint we show is the real `ssh-keygen -lf` one — not a digest of
    /// the private key wearing its name.
    #[test]
    fn an_openssh_key_is_fingerprinted_by_its_public_half() {
        fn ssh_string(bytes: &[u8]) -> Vec<u8> {
            let mut out = (bytes.len() as u32).to_be_bytes().to_vec();
            out.extend_from_slice(bytes);
            out
        }

        let mut public_blob = ssh_string(b"ssh-ed25519");
        public_blob.extend(ssh_string(&[9u8; 32]));

        let mut container = b"openssh-key-v1\0".to_vec();
        container.extend(ssh_string(b"none"));
        container.extend(ssh_string(b"none"));
        container.extend(ssh_string(b""));
        container.extend(1u32.to_be_bytes());
        container.extend(ssh_string(&public_blob));
        container.extend(ssh_string(b"<private section>"));

        let pem = format!(
            "-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----\n",
            B64.encode(&container)
        );

        let expected = format!("SHA256:{}", B64_NOPAD.encode(Sha256::digest(&public_blob)));
        assert_eq!(fingerprint_of(SecretKind::SshKey, &pem), expected);

        // A key in a container we cannot read falls back to a labelled digest
        // rather than pretending to be a public-key fingerprint.
        let pkcs8 = "-----BEGIN PRIVATE KEY-----\nMIIB\n-----END PRIVATE KEY-----\n";
        assert!(fingerprint_of(SecretKind::SshKey, pkcs8).starts_with("sha256:"));
    }

    #[test]
    fn different_material_yields_different_fingerprints() {
        let a = fingerprint_of(SecretKind::GitToken, TOKEN);
        let b = fingerprint_of(SecretKind::GitToken, "ghp_other");
        assert_ne!(a, b);
        assert!(!a.contains(TOKEN));
    }
}
