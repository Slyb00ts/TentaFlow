// ===== File: project_studio/environments.rs — test environments with admin approval (F3) =====
//
// SQL + policy layer for `environments`. Two invariants drive everything here:
//
//   1. SECRET CONFINEMENT — the environment secret is stored SettingsCipher
//      encrypted (`enc:…`) and is decrypted at exactly one place: the run
//      submission body sent to the test runner. No read path returns it, the
//      wire only carries `has_secret`.
//   2. ADDRESS CLASS DECIDES APPROVAL — this is the reverse of the public-web
//      SSRF guard in `web_research`: a PUBLIC target is auto-approved, a
//      private/LAN/loopback one needs an explicit admin decision. The class is
//      computed at SAVE time over EVERY address the host resolves to (a name
//      that resolves to one public and one RFC1918 address counts as private)
//      and over EVERY host of the allowlist, not just `base_url` — the runner
//      may egress to any allowlisted host, so one LAN entry makes the whole
//      environment private. Changing the address OR the allowlist resets an
//      existing approval.
//   3. THE CLASS IS RE-CHECKED AT SUBMIT TIME (`recheck_private`): DNS answers
//      may move to 127.0.0.1 / 169.254.169.254 long after the admin decided.

use std::net::{IpAddr, ToSocketAddrs};

use anyhow::{anyhow, Result};
use rusqlite::{params, OptionalExtension};
use url::Url;

use super::models::EnvironmentRecord;
use crate::crypto::SettingsCipher;
use crate::db::DbPool;

pub const ENV_TYPES: &[&str] = &["web", "api"];
pub const AUTH_TYPES: &[&str] = &["none", "bearer", "api_key", "basic"];
pub const APPROVAL_STATUSES: &[&str] = &["pending", "approved", "rejected"];

/// Upper bound on the extra hosts a single environment may whitelist.
pub const MAX_HOST_ALLOWLIST: usize = 32;
/// Upper bound on the serialized extra-header object.
pub const MAX_EXTRA_HEADERS_BYTES: usize = 4096;
pub const MAX_SECRET_CHARS: usize = 8192;

fn read_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio environments read: {e}")
}

fn write_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio environments write: {e}")
}

const ENV_COLS: &str = "environment_id, name, env_type, base_url, auth_type, secret_enc, \
     extra_headers_json, host_allowlist_json, approval_status, approval_reason, \
     is_private_address, justification, requested_by, decided_by, created_at, updated_at, \
     decided_at";

fn read_environment(row: &rusqlite::Row<'_>) -> rusqlite::Result<EnvironmentRecord> {
    Ok(EnvironmentRecord {
        environment_id: row.get(0)?,
        name: row.get(1)?,
        env_type: row.get(2)?,
        base_url: row.get(3)?,
        auth_type: row.get(4)?,
        secret_enc: row.get(5)?,
        extra_headers_json: row.get(6)?,
        host_allowlist_json: row.get(7)?,
        approval_status: row.get(8)?,
        approval_reason: row.get(9)?,
        is_private_address: row.get::<_, i64>(10)? != 0,
        justification: row.get(11)?,
        requested_by: row.get(12)?,
        decided_by: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
        decided_at: row.get(16)?,
    })
}

// =============================================================================
// Address classification
// =============================================================================

/// Validated `base_url` with the address class the approval decision hinges on.
#[derive(Debug, Clone)]
pub struct AddressClass {
    /// Normalized origin-form url (scheme://host[:port]/path) stored on the row.
    pub base_url: String,
    /// Host of `base_url`, always the first allowlist entry.
    pub host: String,
    pub is_private: bool,
}

/// Port used when resolving an allowlist entry: the entry is a bare host name,
/// and `ToSocketAddrs` needs some port to answer with.
const ALLOWLIST_PROBE_PORT: u16 = 443;

/// Whether a host reaches a non-public address. A host that resolves to ANY
/// non-public address is private; a host that does not resolve at all is
/// treated as private too — an unresolvable name is exactly the case where an
/// admin must look at it, and silently auto-approving it would let a DNS entry
/// appear later and point anywhere. Blocking (DNS).
pub fn host_is_private(host: &str, port: u16) -> bool {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return !crate::web_research::security::is_public_ip(ip);
    }
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return true;
    }
    match (host, port).to_socket_addrs() {
        Ok(addrs) => {
            let resolved: Vec<IpAddr> = addrs.map(|a| a.ip()).collect();
            resolved.is_empty()
                || resolved
                    .iter()
                    .any(|ip| !crate::web_research::security::is_public_ip(*ip))
        }
        Err(_) => true,
    }
}

/// Parses and classifies an environment `base_url`. Only http/https are
/// accepted.
pub fn classify_base_url(raw: &str) -> Result<AddressClass> {
    let url = Url::parse(raw.trim()).map_err(|e| anyhow!("invalid base_url: {e}"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(anyhow!("base_url must use http or https"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("base_url has no host"))?
        .to_ascii_lowercase();
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow!("base_url has no port"))?;
    let is_private = host_is_private(&host, port);

    Ok(AddressClass {
        base_url: url.to_string(),
        host,
        is_private,
    })
}

/// The complete target of an environment: the classified `base_url` and the
/// effective host allowlist. `address.is_private` is the OR over the base url
/// AND every allowlist entry, because the runner may egress to all of them.
pub struct EnvironmentTarget {
    pub address: AddressClass,
    pub hosts: Vec<String>,
}

/// Classifies `base_url` together with the extra hosts the caller declared.
/// The allowlist is normalized (a url is reduced to its host, entries are
/// deduplicated, the base host is always first) and EVERY entry runs through
/// the same address classification as the base url — otherwise a public
/// `base_url` would auto-approve an environment whose allowlist reaches
/// 192.168.x.x or the cloud metadata address. Blocking (DNS).
pub fn classify_target(base_url: &str, extra: &[String]) -> Result<EnvironmentTarget> {
    let mut address = classify_base_url(base_url)?;
    let mut hosts = vec![address.host.clone()];
    for raw in extra {
        let candidate = raw.trim().to_ascii_lowercase();
        if candidate.is_empty() {
            continue;
        }
        let normalized = match Url::parse(&candidate) {
            Ok(u) => u
                .host_str()
                .ok_or_else(|| anyhow!("host allowlist entry '{raw}' has no host"))?
                .to_string(),
            Err(_) => candidate,
        };
        if normalized.contains('/') || normalized.contains(' ') {
            return Err(anyhow!("host allowlist entry '{raw}' is not a host name"));
        }
        if hosts.contains(&normalized) {
            continue;
        }
        if hosts.len() >= MAX_HOST_ALLOWLIST {
            return Err(anyhow!(
                "host allowlist exceeds {MAX_HOST_ALLOWLIST} entries"
            ));
        }
        address.is_private |= host_is_private(&normalized, ALLOWLIST_PROBE_PORT);
        hosts.push(normalized);
    }
    Ok(EnvironmentTarget { address, hosts })
}

/// Re-classifies a stored environment immediately before a run is submitted.
/// The approval decision is only as fresh as the DNS answer it was taken on, so
/// a name that was public at save time is resolved again here.
pub fn recheck_private(record: &EnvironmentRecord) -> Result<bool> {
    let hosts = host_allowlist_of(record);
    Ok(classify_target(&record.base_url, &hosts)?
        .address
        .is_private)
}

/// Validates the extra-header object: a flat JSON object of string values,
/// bounded in size, without the headers the runner owns itself.
pub fn validate_extra_headers(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok("{}".to_string());
    }
    if trimmed.len() > MAX_EXTRA_HEADERS_BYTES {
        return Err(anyhow!(
            "extra_headers_json exceeds {MAX_EXTRA_HEADERS_BYTES} bytes"
        ));
    }
    let value: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|e| anyhow!("invalid extra_headers_json: {e}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("extra_headers_json must be a JSON object"))?;
    for (name, header_value) in object {
        if name.trim().is_empty() || !name.bytes().all(|b| b.is_ascii_graphic() && b != b':') {
            return Err(anyhow!("invalid header name '{name}'"));
        }
        // Authorization is derived from auth_type + the stored secret; letting
        // a header override it would smuggle a second credential past the
        // secret-confinement path.
        if name.eq_ignore_ascii_case("authorization") {
            return Err(anyhow!(
                "the Authorization header is derived from auth_type, not extra_headers_json"
            ));
        }
        let text = header_value
            .as_str()
            .ok_or_else(|| anyhow!("header '{name}' must have a string value"))?;
        // A CR/LF in the value splits the request line the runner builds and
        // would let a header inject a second header (or a whole request).
        if text.chars().any(char::is_control) {
            return Err(anyhow!(
                "header '{name}' value must not contain control characters"
            ));
        }
    }
    Ok(trimmed.to_string())
}

// =============================================================================
// CRUD
// =============================================================================

pub fn list(pool: &DbPool) -> Result<Vec<EnvironmentRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {ENV_COLS} FROM environments ORDER BY name COLLATE NOCASE"
    ))?;
    let rows = stmt.query_map([], read_environment)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn list_pending(pool: &DbPool) -> Result<Vec<EnvironmentRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {ENV_COLS} FROM environments WHERE approval_status = 'pending' \
         ORDER BY created_at, environment_id"
    ))?;
    let rows = stmt.query_map([], read_environment)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn get(pool: &DbPool, environment_id: &str) -> Result<Option<EnvironmentRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {ENV_COLS} FROM environments WHERE environment_id = ?1"),
        params![environment_id],
        read_environment,
    )
    .optional()
    .map_err(Into::into)
}

/// Validated payload of an environment save.
pub struct EnvironmentInput<'a> {
    pub name: &'a str,
    pub env_type: &'a str,
    pub auth_type: &'a str,
    pub extra_headers_json: &'a str,
    pub justification: &'a str,
    pub address: &'a AddressClass,
    pub host_allowlist: &'a [String],
    /// `None` keeps the stored secret, `Some("")` clears it, `Some(v)` replaces
    /// it. Already encrypted by the caller.
    pub secret_enc: Option<&'a str>,
}

/// Encrypts a plaintext secret for storage. An empty secret stays empty (the
/// cipher would otherwise turn "" into a non-empty ciphertext and `has_secret`
/// would lie).
pub fn encrypt_secret(cipher: &SettingsCipher, plaintext: &str) -> Result<String> {
    if plaintext.is_empty() {
        return Ok(String::new());
    }
    if plaintext.chars().count() > MAX_SECRET_CHARS {
        return Err(anyhow!("secret exceeds {MAX_SECRET_CHARS} characters"));
    }
    cipher.encrypt(plaintext)
}

/// Decrypts the stored secret for the ONE consumer that may see it (the run
/// submission body). Never call this on a read path.
pub fn decrypt_secret(cipher: &SettingsCipher, record: &EnvironmentRecord) -> Result<String> {
    if record.secret_enc.is_empty() {
        return Ok(String::new());
    }
    cipher.decrypt(&record.secret_enc)
}

/// Inserts a new environment. Public addresses are auto-approved, private ones
/// start pending. Returns the resulting approval status.
pub fn insert(
    pool: &DbPool,
    environment_id: &str,
    input: &EnvironmentInput<'_>,
    requested_by: &str,
) -> Result<String> {
    let status = if input.address.is_private {
        "pending"
    } else {
        "approved"
    };
    let decided_by = if status == "approved" { "system" } else { "" };
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO environments (environment_id, name, env_type, base_url, auth_type, \
            secret_enc, extra_headers_json, host_allowlist_json, approval_status, \
            is_private_address, justification, requested_by, decided_by, decided_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
            CASE WHEN ?9 = 'approved' THEN datetime('now') ELSE NULL END)",
        params![
            environment_id,
            input.name,
            input.env_type,
            input.address.base_url,
            input.auth_type,
            input.secret_enc.unwrap_or(""),
            input.extra_headers_json,
            serde_json::to_string(input.host_allowlist)?,
            status,
            input.address.is_private as i64,
            input.justification,
            requested_by,
            decided_by,
        ],
    )?;
    Ok(status.to_string())
}

/// Updates an environment in place. The approval RESETS to pending whenever the
/// target moves to a private address or the address class changes — an already
/// approved public environment must not be able to become a LAN target behind
/// the admin's back. The allowlist counts as part of the target: appending a
/// host to an approved environment is a new egress destination and needs the
/// same decision as changing `base_url`. Returns the resulting approval status.
pub fn update(
    pool: &DbPool,
    existing: &EnvironmentRecord,
    input: &EnvironmentInput<'_>,
) -> Result<String> {
    let host_allowlist_json = serde_json::to_string(input.host_allowlist)?;
    let class_changed = existing.is_private_address != input.address.is_private
        || existing.base_url != input.address.base_url
        || existing.host_allowlist_json != host_allowlist_json;
    let status = if input.address.is_private {
        if class_changed || existing.approval_status != "approved" {
            "pending"
        } else {
            "approved"
        }
    } else {
        "approved"
    };
    let (decided_by, reason) = if status == "pending" {
        (String::new(), String::new())
    } else if status == "approved" && class_changed {
        ("system".to_string(), String::new())
    } else {
        (
            existing.decided_by.clone(),
            existing.approval_reason.clone(),
        )
    };
    let secret_enc = match input.secret_enc {
        Some(value) => value.to_string(),
        None => existing.secret_enc.clone(),
    };
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "UPDATE environments SET name = ?1, env_type = ?2, base_url = ?3, auth_type = ?4, \
            secret_enc = ?5, extra_headers_json = ?6, host_allowlist_json = ?7, \
            approval_status = ?8, approval_reason = ?9, is_private_address = ?10, \
            justification = ?11, decided_by = ?12, updated_at = datetime('now'), \
            decided_at = CASE WHEN ?8 = 'pending' THEN NULL ELSE decided_at END \
         WHERE environment_id = ?13",
        params![
            input.name,
            input.env_type,
            input.address.base_url,
            input.auth_type,
            secret_enc,
            input.extra_headers_json,
            host_allowlist_json,
            status,
            reason,
            input.address.is_private as i64,
            input.justification,
            decided_by,
            existing.environment_id,
        ],
    )?;
    Ok(status.to_string())
}

/// Records an admin decision. Guarded on `approval_status = 'pending'`, so two
/// admins deciding concurrently produce exactly one transition.
pub fn decide(
    pool: &DbPool,
    environment_id: &str,
    approve: bool,
    reason: &str,
    decided_by: &str,
) -> Result<bool> {
    let status = if approve { "approved" } else { "rejected" };
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE environments SET approval_status = ?1, approval_reason = ?2, decided_by = ?3, \
            decided_at = datetime('now'), updated_at = datetime('now') \
         WHERE environment_id = ?4 AND approval_status = 'pending'",
        params![status, reason, decided_by, environment_id],
    )?;
    Ok(n > 0)
}

/// How many runs reference the environment. A referenced environment cannot be
/// deleted — historical runs must stay resolvable to the target they ran on.
pub fn run_reference_count(pool: &DbPool, environment_id: &str) -> Result<u32> {
    let conn = pool.read().map_err(read_err)?;
    let n: i64 = conn.query_row(
        "SELECT (SELECT COUNT(*) FROM test_runs WHERE environment_id = ?1) \
              + (SELECT COUNT(*) FROM auto_run_meta WHERE environment_id = ?1)",
        params![environment_id],
        |row| row.get(0),
    )?;
    Ok(n as u32)
}

pub fn delete(pool: &DbPool, environment_id: &str) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "DELETE FROM environments WHERE environment_id = ?1",
        params![environment_id],
    )?;
    Ok(n > 0)
}

/// Parses the stored host allowlist JSON, falling back to the base_url host
/// when the column holds junk (never fail a run on a cosmetic column).
pub fn host_allowlist_of(record: &EnvironmentRecord) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(&record.host_allowlist_json).unwrap_or_else(|_| {
        Url::parse(&record.base_url)
            .ok()
            .and_then(|u| u.host_str().map(|h| vec![h.to_string()]))
            .unwrap_or_default()
    })
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn pool() -> DbPool {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).expect("dir");
        let (pool, _) = super::super::project_db::open_pool_at(&dir).expect("open");
        std::mem::forget(tmp);
        pool
    }

    fn address(base_url: &str, is_private: bool) -> AddressClass {
        AddressClass {
            base_url: base_url.to_string(),
            host: Url::parse(base_url)
                .expect("url")
                .host_str()
                .expect("host")
                .to_string(),
            is_private,
        }
    }

    /// (a) A private/LAN target starts 'pending'; a public one is auto-approved
    /// and the encrypted secret never appears in the record's plaintext form.
    #[test]
    fn private_address_goes_pending_public_is_auto_approved() {
        let pool = pool();
        let cipher = SettingsCipher::new(&[7u8; 32]);

        let lan = address("http://192.168.1.10:8080/", true);
        let secret = encrypt_secret(&cipher, "super-tajne").expect("encrypt");
        assert!(
            secret.starts_with("enc:"),
            "secret must be encrypted at rest"
        );
        assert!(!secret.contains("super-tajne"));
        let status = insert(
            &pool,
            "env-lan",
            &EnvironmentInput {
                name: "LAN",
                env_type: "api",
                auth_type: "bearer",
                extra_headers_json: "{}",
                justification: "wewnetrzny staging",
                address: &lan,
                host_allowlist: &["192.168.1.10".to_string()],
                secret_enc: Some(&secret),
            },
            "requester",
        )
        .expect("insert lan");
        assert_eq!(status, "pending");

        let public = address("https://example.com/", false);
        let status = insert(
            &pool,
            "env-pub",
            &EnvironmentInput {
                name: "Public",
                env_type: "web",
                auth_type: "none",
                extra_headers_json: "{}",
                justification: "",
                address: &public,
                host_allowlist: &["example.com".to_string()],
                secret_enc: Some(""),
            },
            "requester",
        )
        .expect("insert public");
        assert_eq!(status, "approved");

        let stored = get(&pool, "env-lan").expect("get").expect("row");
        assert!(stored.is_private_address);
        assert_eq!(
            decrypt_secret(&cipher, &stored).expect("decrypt"),
            "super-tajne"
        );
        let public_row = get(&pool, "env-pub").expect("get").expect("row");
        assert!(public_row.secret_enc.is_empty(), "empty secret stays empty");

        // Admin decision is idempotent: the guarded UPDATE fires exactly once.
        assert!(decide(&pool, "env-lan", true, "", "admin").expect("decide"));
        assert!(!decide(&pool, "env-lan", true, "", "admin2").expect("second decide"));
        let decided = get(&pool, "env-lan").expect("get").expect("row");
        assert_eq!(decided.approval_status, "approved");
        assert_eq!(decided.decided_by, "admin");

        // Moving an approved LAN environment to another LAN address resets it.
        let other_lan = address("http://10.0.0.5:9000/", true);
        let status = update(
            &pool,
            &decided,
            &EnvironmentInput {
                name: "LAN",
                env_type: "api",
                auth_type: "bearer",
                extra_headers_json: "{}",
                justification: "przeniesione",
                address: &other_lan,
                host_allowlist: &["10.0.0.5".to_string()],
                secret_enc: None,
            },
        )
        .expect("update lan");
        assert_eq!(status, "pending", "address change resets the approval");
        let moved = get(&pool, "env-lan").expect("get").expect("row");
        assert_eq!(
            decrypt_secret(&cipher, &moved).expect("decrypt"),
            "super-tajne",
            "secret_enc: None keeps the stored secret"
        );
    }

    /// Loopback, RFC1918 and link-local literals classify as private; a public
    /// literal does not. (No DNS in the test — literals take the parse branch.)
    #[test]
    fn classify_base_url_flags_local_targets() {
        for private in [
            "http://127.0.0.1:8080",
            "http://10.1.2.3",
            "http://192.168.0.1:3000",
            "http://172.16.5.9",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]:8080",
            "http://localhost:5173",
            "http://my-box.local",
        ] {
            let class = classify_base_url(private).expect(private);
            assert!(class.is_private, "{private} must classify as private");
        }
        let public = classify_base_url("https://1.1.1.1/").expect("public literal");
        assert!(!public.is_private);
        assert!(classify_base_url("ftp://example.com").is_err());
        assert!(classify_base_url("not a url").is_err());
    }

    #[test]
    fn host_allowlist_and_headers_are_normalized_and_bounded() {
        let target = classify_target(
            "https://1.1.1.1/",
            &[
                "https://1.0.0.1/assets".to_string(),
                "1.1.1.1".to_string(),
                "  ".to_string(),
            ],
        )
        .expect("allowlist");
        assert_eq!(target.hosts, vec!["1.1.1.1", "1.0.0.1"]);
        assert!(!target.address.is_private);
        let many: Vec<String> = (0..MAX_HOST_ALLOWLIST + 1)
            .map(|i| format!("{}.{}.0.1", 1 + i / 250, i % 250))
            .collect();
        assert!(classify_target("https://1.1.1.1/", &many).is_err());
        assert!(classify_target("https://1.1.1.1/", &["a b".to_string()]).is_err());

        assert_eq!(validate_extra_headers("").expect("empty"), "{}");
        assert!(validate_extra_headers(r#"{"X-Tenant":"acme"}"#).is_ok());
        assert!(validate_extra_headers(r#"{"Authorization":"Bearer x"}"#).is_err());
        assert!(validate_extra_headers(r#"{"X-N":5}"#).is_err());
        assert!(validate_extra_headers("[1,2]").is_err());
        assert!(
            validate_extra_headers("{\"X-Tenant\":\"acme\\r\\nX-Admin: 1\"}").is_err(),
            "a CRLF in a header value must be rejected"
        );
    }

    /// CR-001: a public `base_url` must NOT auto-approve an environment whose
    /// allowlist reaches the LAN or the cloud metadata address — the extra
    /// hosts are egress targets of the runner exactly like the base url.
    #[test]
    fn a_private_host_in_the_allowlist_makes_the_environment_pending() {
        let target = classify_target(
            "https://1.1.1.1/",
            &[
                "192.168.10.5".to_string(),
                "169.254.169.254".to_string(),
                "localhost".to_string(),
            ],
        )
        .expect("classify");
        assert!(
            target.address.is_private,
            "one private allowlist entry classifies the whole target as private"
        );
        assert_eq!(target.hosts.len(), 4);

        let pool = pool();
        let status = insert(
            &pool,
            "env-mixed",
            &EnvironmentInput {
                name: "Mixed",
                env_type: "api",
                auth_type: "none",
                extra_headers_json: "{}",
                justification: "internal probes",
                address: &target.address,
                host_allowlist: &target.hosts,
                secret_enc: Some(""),
            },
            "requester",
        )
        .expect("insert");
        assert_eq!(status, "pending");

        // An approved PUBLIC environment that later grows a LAN entry must go
        // back to the queue instead of silently widening its egress.
        let public = classify_target("https://1.1.1.1/", &[]).expect("public");
        let status = insert(
            &pool,
            "env-public",
            &EnvironmentInput {
                name: "Public",
                env_type: "api",
                auth_type: "none",
                extra_headers_json: "{}",
                justification: "",
                address: &public.address,
                host_allowlist: &public.hosts,
                secret_enc: Some(""),
            },
            "requester",
        )
        .expect("insert public");
        assert_eq!(status, "approved");
        let existing = get(&pool, "env-public").expect("get").expect("row");
        let widened =
            classify_target("https://1.1.1.1/", &["10.0.0.5".to_string()]).expect("widened");
        let status = update(
            &pool,
            &existing,
            &EnvironmentInput {
                name: "Public",
                env_type: "api",
                auth_type: "none",
                extra_headers_json: "{}",
                justification: "reach the internal api too",
                address: &widened.address,
                host_allowlist: &widened.hosts,
                secret_enc: None,
            },
        )
        .expect("update");
        assert_eq!(status, "pending");

        // Adding a PUBLIC host to an approved private environment also resets
        // the decision: the allowlist is part of the approved target.
        assert!(decide(&pool, "env-public", true, "", "admin").expect("decide"));
        let approved = get(&pool, "env-public").expect("get").expect("row");
        assert_eq!(approved.approval_status, "approved");
        let more = classify_target(
            "https://1.1.1.1/",
            &["10.0.0.5".to_string(), "1.0.0.1".to_string()],
        )
        .expect("more");
        let status = update(
            &pool,
            &approved,
            &EnvironmentInput {
                name: "Public",
                env_type: "api",
                auth_type: "none",
                extra_headers_json: "{}",
                justification: "one more host",
                address: &more.address,
                host_allowlist: &more.hosts,
                secret_enc: None,
            },
        )
        .expect("update again");
        assert_eq!(status, "pending", "allowlist change resets the approval");
    }
}
