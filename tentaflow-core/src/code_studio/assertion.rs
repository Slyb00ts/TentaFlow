// ===== File: code_studio/assertion.rs — who is acting, carried across the mesh =====
//
// A workspace runs on ONE node (§3). When the person driving it is connected
// to a different node, that node has to tell the owner node who is acting. The
// mesh connection itself only proves the NODE — the iroh handshake authenticates
// a peer, not a person — so the identity of the actor is minted here, signed
// with the issuing node's Ed25519 assertion key and verified on arrival.
//
// The five defences, none of which is decorative:
//
//   * asymmetric keys with a `kid`, rotated with an OVERLAP window, so a
//     rotation never cuts a session that is mid-flight;
//   * the issuer is bound to the CHANNEL: `iss` must equal the authenticated
//     peer of the connection the assertion arrived on, so a trusted node cannot
//     present an assertion it claims another node issued;
//   * anti-replay is PERSISTENT: `jti` lands in `session_assertion_jti` (§5.2),
//     not in a process cache. A cache dies with the process and re-opens the
//     replay window exactly when the node is most vulnerable — right after a
//     restart, when nothing else remembers what already happened;
//   * a mutating assertion carries `op_id` and the digest of the arguments, so
//     even a replay that somehow got past the `jti` table can at most re-drive
//     the operation `session_operations` already deduplicates (§13.1);
//   * `aud`, `nbf`, `exp` (≤ 120 s, enforced at issuance AND at verification)
//     and `rbac_rev` are all checked before anything happens.
//
// And then the owner node authorizes from scratch anyway (`PermissionMatrix`,
// membership, role, PEP, containment). This module transports identity; it does
// not grant anything.

use std::collections::{HashMap, HashSet};

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use parking_lot::RwLock;
use rusqlite::params;
use sha2::{Digest, Sha256};

use tentaflow_protocol::mesh::{AssertionKeyEntry, SessionAssertion};

use super::models::WorkspaceRole;
use super::pep::Capability;
use super::repository;
use crate::db::DbPool;
use crate::services::mesh_keys::now_unix_ms;
use crate::services::rbac::{resolve_org_context, OrgContext};

/// The only accepted signature algorithm. Present on the wire so an unknown
/// value is a rejection rather than a silent reinterpretation.
pub const ALG_ED25519: &str = "Ed25519";

/// Hard ceiling on an assertion's lifetime (§12.1). Enforced when issuing and
/// again when verifying — a peer that mints a longer one is refused, not
/// trusted to have been honest.
pub const MAX_LIFETIME_MS: u64 = 120_000;

/// Default lifetime. Short enough that a revocation elsewhere in the mesh is
/// bounded by sync lag plus two minutes, long enough that a slow operation does
/// not have to re-mint mid-call.
pub const DEFAULT_LIFETIME_MS: u64 = 60_000;

/// Tolerance for clock disagreement between two nodes. Deliberately small: it
/// exists for NTP jitter, not for accepting an assertion minted for the future.
const CLOCK_SKEW_MS: u64 = 2_000;

/// How long a rotated-out key stays acceptable for verification. Longer than
/// `MAX_LIFETIME_MS` by design — every assertion signed with the old key must
/// still verify until its own `exp`.
pub const KEY_OVERLAP_MS: u64 = 300_000;

/// Domain separator. Without it a signature over this structure could be
/// replayed as a signature over some other length-prefixed message.
const SIGNING_DOMAIN: &[u8] = b"tentaflow:code_studio:session_assertion:v1";

/// Sanity bounds on wire strings, applied before any of them is used as a
/// database key or a log field.
const MAX_FIELD_LEN: usize = 256;
const MAX_CAPS: usize = Capability::ALL.len();

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssertionError {
    UnsupportedAlg(String),
    Malformed(String),
    /// No verification key for (peer, kid). The caller may fetch the issuer's
    /// current key ring and retry once.
    UnknownKey {
        peer: String,
        kid: String,
    },
    IssuerMismatch {
        claimed: String,
        channel: String,
    },
    AudienceMismatch {
        claimed: String,
        local: String,
    },
    NotYetValid {
        nbf: u64,
        now: u64,
    },
    Expired {
        exp: u64,
        now: u64,
    },
    LifetimeTooLong {
        lifetime_ms: u64,
    },
    ArgsDigestMismatch,
    BadSignature,
    Replay {
        jti: String,
    },
    /// The issuer's view of the actor's authorization differs from this node's,
    /// and the local re-resolution does not cover what the assertion claims.
    RbacRevisionMismatch {
        claimed: String,
        local: String,
        missing: Vec<String>,
    },
    /// The actor does not resolve to an org member on THIS node.
    Identity(String),
    Db(String),
}

impl std::fmt::Display for AssertionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedAlg(alg) => write!(f, "unsupported assertion algorithm '{alg}'"),
            Self::Malformed(what) => write!(f, "malformed assertion: {what}"),
            Self::UnknownKey { peer, kid } => {
                write!(f, "no assertion key '{kid}' known for node '{peer}'")
            }
            Self::IssuerMismatch { claimed, channel } => write!(
                f,
                "assertion claims issuer '{claimed}' but arrived from '{channel}'"
            ),
            Self::AudienceMismatch { claimed, local } => write!(
                f,
                "assertion is addressed to '{claimed}', this node is '{local}'"
            ),
            Self::NotYetValid { nbf, now } => {
                write!(f, "assertion is not valid before {nbf} (now {now})")
            }
            Self::Expired { exp, now } => write!(f, "assertion expired at {exp} (now {now})"),
            Self::LifetimeTooLong { lifetime_ms } => write!(
                f,
                "assertion lifetime {lifetime_ms} ms exceeds the {MAX_LIFETIME_MS} ms ceiling"
            ),
            Self::ArgsDigestMismatch => {
                write!(f, "assertion does not match the payload it arrived with")
            }
            Self::BadSignature => write!(f, "assertion signature does not verify"),
            Self::Replay { jti } => write!(f, "assertion '{jti}' has already been used"),
            Self::RbacRevisionMismatch {
                claimed,
                local,
                missing,
            } => write!(
                f,
                "rbac revision '{claimed}' does not match local '{local}' and the local \
                 resolution does not grant: {}",
                missing.join(", ")
            ),
            Self::Identity(detail) => write!(f, "actor identity unusable on this node: {detail}"),
            Self::Db(detail) => write!(f, "assertion store error: {detail}"),
        }
    }
}

impl std::error::Error for AssertionError {}

// =============================================================================
// Canonical signing input
// =============================================================================

/// Length-prefixed canonical encoding of every claim except the signature.
///
/// Deliberately NOT the CBOR bytes: the wire encoding may legitimately change
/// (field order, integer widths) without the signed message being allowed to
/// change with it. Length prefixes keep `("a","bc")` from colliding with
/// `("ab","c")`, the same rule `operations::op_id` follows.
pub fn signing_input(a: &SessionAssertion) -> Vec<u8> {
    let mut out = Vec::with_capacity(512);
    out.extend_from_slice(SIGNING_DOMAIN);
    let mut push = |bytes: &[u8]| {
        out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(bytes);
    };
    push(a.alg.as_bytes());
    push(a.kid.as_bytes());
    push(a.iss.as_bytes());
    push(a.sub.as_bytes());
    push(a.aud.as_bytes());
    push(a.org.as_bytes());
    push(a.workspace.as_bytes());
    push(a.session.as_bytes());
    push(&(a.caps.len() as u64).to_le_bytes());
    for cap in &a.caps {
        push(cap.as_bytes());
    }
    push(a.rbac_rev.as_bytes());
    push(&a.iat.to_le_bytes());
    push(&a.nbf.to_le_bytes());
    push(&a.exp.to_le_bytes());
    push(a.jti.as_bytes());
    push(a.op_id.as_bytes());
    push(a.args_digest.as_bytes());
    out
}

/// Lowercase hex SHA-256 — the form `args_digest` and `rbac_rev` are carried in.
pub fn digest_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

// =============================================================================
// Key ring — local signing keys and the peers' verification keys
// =============================================================================

/// The `kid` is derived from the public key, so the same key always yields the
/// same identifier on every node and a peer cannot re-advertise a different key
/// under a `kid` we already hold without the mismatch being visible.
fn kid_of(public: &VerifyingKey) -> String {
    let digest = Sha256::digest(public.as_bytes());
    hex::encode(&digest[..8])
}

struct LocalKey {
    kid: String,
    signing: SigningKey,
}

/// A key that has been rotated out but must stay verifiable until every
/// assertion signed with it has expired.
struct RetiredKey {
    kid: String,
    verifying: VerifyingKey,
    expires_unix_ms: u64,
}

/// This node's assertion signing keys: one current, optionally one retired and
/// still valid. Two acceptable `kid`s at once is the whole point — rotating
/// must not interrupt sessions that are already running.
pub struct LocalKeyRing {
    current: LocalKey,
    previous: Option<RetiredKey>,
}

impl LocalKeyRing {
    fn generate() -> Self {
        let signing = SigningKey::generate(&mut rand_core_06::OsRng);
        let kid = kid_of(&signing.verifying_key());
        Self {
            current: LocalKey { kid, signing },
            previous: None,
        }
    }

    pub fn current_kid(&self) -> &str {
        &self.current.kid
    }

    /// Generate a new signing key and keep the old one verifiable for
    /// `KEY_OVERLAP_MS`. Assertions already in flight keep verifying; new ones
    /// are signed with the new key.
    pub fn rotate(&mut self, now_unix_ms: u64) {
        let fresh = SigningKey::generate(&mut rand_core_06::OsRng);
        let replacement = LocalKey {
            kid: kid_of(&fresh.verifying_key()),
            signing: fresh,
        };
        let retiring = std::mem::replace(&mut self.current, replacement);
        self.previous = Some(RetiredKey {
            kid: retiring.kid,
            verifying: retiring.signing.verifying_key(),
            expires_unix_ms: now_unix_ms + KEY_OVERLAP_MS,
        });
    }

    /// What this node advertises to trust-paired peers: the current key with no
    /// expiry, plus the retired one while its overlap window is open.
    pub fn advertise(&self, now_unix_ms: u64) -> Vec<AssertionKeyEntry> {
        let mut entries = vec![AssertionKeyEntry {
            kid: self.current.kid.clone(),
            public_key: self.current.signing.verifying_key().as_bytes().to_vec(),
            expires_unix_ms: 0,
        }];
        if let Some(prev) = self
            .previous
            .as_ref()
            .filter(|p| now_unix_ms < p.expires_unix_ms)
        {
            entries.push(AssertionKeyEntry {
                kid: prev.kid.clone(),
                public_key: prev.verifying.as_bytes().to_vec(),
                expires_unix_ms: prev.expires_unix_ms,
            });
        }
        entries
    }
}

static LOCAL_RING: RwLock<Option<LocalKeyRing>> = RwLock::new(None);

fn with_local_ring<R>(f: impl FnOnce(&LocalKeyRing) -> R) -> R {
    {
        let guard = LOCAL_RING.read();
        if let Some(ring) = guard.as_ref() {
            return f(ring);
        }
    }
    let mut guard = LOCAL_RING.write();
    let ring = guard.get_or_insert_with(LocalKeyRing::generate);
    f(ring)
}

/// This node's advertisable assertion keys.
pub fn local_advertise() -> Vec<AssertionKeyEntry> {
    let now = now_unix_ms();
    with_local_ring(|ring| ring.advertise(now))
}

/// Rotate the local signing key. The previous key stays verifiable for
/// `KEY_OVERLAP_MS`; callers are expected to push the new advertisement to
/// trust-paired peers afterwards (`remote_proxy::broadcast_assertion_keys`).
pub fn rotate_local_key() {
    let now = now_unix_ms();
    let mut guard = LOCAL_RING.write();
    match guard.as_mut() {
        Some(ring) => ring.rotate(now),
        None => *guard = Some(LocalKeyRing::generate()),
    }
}

/// Verification keys other nodes advertised. Verifier-only public material,
/// held in memory exactly like the peer HMAC keys in `services::mesh_keys`:
/// a peer that loses trust leaves nothing behind on disk.
#[derive(Default)]
struct PeerKeyPool {
    peers: RwLock<HashMap<String, HashMap<String, PeerKey>>>,
}

struct PeerKey {
    verifying: VerifyingKey,
    /// `0` = the peer's current key, no expiry of its own.
    expires_unix_ms: u64,
}

static PEER_KEYS: std::sync::OnceLock<PeerKeyPool> = std::sync::OnceLock::new();

fn peer_pool() -> &'static PeerKeyPool {
    PEER_KEYS.get_or_init(PeerKeyPool::default)
}

/// Take in an advertisement from `peer`. The caller is responsible for the
/// trust gate — the mesh command path only reaches here for trust-paired peers.
/// Returns how many entries were accepted; a malformed key is skipped, not
/// fatal, so one bad entry cannot poison a rotation.
pub fn ingest_peer_keys(peer: &str, entries: &[AssertionKeyEntry]) -> usize {
    let pool = peer_pool();
    let mut peers = pool.peers.write();
    let slot = peers.entry(peer.to_string()).or_default();
    let now = now_unix_ms();
    slot.retain(|_, key| key.expires_unix_ms == 0 || key.expires_unix_ms > now);
    let mut accepted = 0usize;
    for entry in entries {
        if entry.kid.is_empty() || entry.kid.len() > MAX_FIELD_LEN {
            continue;
        }
        let Ok(bytes) = <[u8; 32]>::try_from(entry.public_key.as_slice()) else {
            continue;
        };
        let Ok(verifying) = VerifyingKey::from_bytes(&bytes) else {
            continue;
        };
        // A `kid` is the digest of the key, so an entry whose id does not match
        // its material is either a bug or an attempt to shadow a known id.
        if kid_of(&verifying) != entry.kid {
            tracing::warn!(
                peer,
                kid = %entry.kid,
                "code studio: assertion key id does not match its material"
            );
            continue;
        }
        slot.insert(
            entry.kid.clone(),
            PeerKey {
                verifying,
                expires_unix_ms: entry.expires_unix_ms,
            },
        );
        accepted += 1;
    }
    accepted
}

/// Look up a peer's verification key. Expired overlap keys are not returned.
fn peer_key(peer: &str, kid: &str) -> Option<VerifyingKey> {
    let now = now_unix_ms();
    let pool = peer_pool();
    let peers = pool.peers.read();
    let key = peers.get(peer)?.get(kid)?;
    if key.expires_unix_ms != 0 && key.expires_unix_ms <= now {
        return None;
    }
    Some(key.verifying)
}

/// Drop every key held for `peer` — on disconnect or trust revocation.
pub fn forget_peer_keys(peer: &str) {
    peer_pool().peers.write().remove(peer);
}

/// Diagnostic: how many keys are held for a peer.
pub fn peer_key_count(peer: &str) -> usize {
    peer_pool()
        .peers
        .read()
        .get(peer)
        .map(|m| m.len())
        .unwrap_or(0)
}

// =============================================================================
// Issuing
// =============================================================================

/// Everything the issuing node knows about the actor and the call.
#[derive(Debug, Clone)]
pub struct IssueRequest<'a> {
    /// This node — becomes `iss`.
    pub local_node_id: &'a str,
    pub user_id: &'a str,
    /// Owner node — becomes `aud`.
    pub owner_node_id: &'a str,
    pub org_id: &'a str,
    pub workspace_id: &'a str,
    /// Empty for workspace-level calls that exist before any session.
    pub session_id: &'a str,
    pub caps: &'a [Capability],
    pub rbac_rev: &'a str,
    /// Set for a mutating call; empty for a read.
    pub op_id: &'a str,
    /// The exact bytes that will travel with the assertion.
    pub payload_cbor: &'a [u8],
    pub lifetime_ms: u64,
}

/// Mint an assertion. Refuses a lifetime above the ceiling here, so a bug in a
/// caller cannot produce a long-lived credential that the far side then has to
/// catch.
pub fn issue(request: &IssueRequest<'_>) -> Result<SessionAssertion, AssertionError> {
    if request.lifetime_ms == 0 || request.lifetime_ms > MAX_LIFETIME_MS {
        return Err(AssertionError::LifetimeTooLong {
            lifetime_ms: request.lifetime_ms,
        });
    }
    if request.user_id.is_empty() || request.workspace_id.is_empty() {
        return Err(AssertionError::Malformed(
            "user and workspace are required".to_string(),
        ));
    }
    let now = now_unix_ms();
    let mut assertion = SessionAssertion {
        alg: ALG_ED25519.to_string(),
        kid: String::new(),
        iss: request.local_node_id.to_string(),
        sub: request.user_id.to_string(),
        aud: request.owner_node_id.to_string(),
        org: request.org_id.to_string(),
        workspace: request.workspace_id.to_string(),
        session: request.session_id.to_string(),
        caps: request.caps.iter().map(|c| c.slug().to_string()).collect(),
        rbac_rev: request.rbac_rev.to_string(),
        iat: now,
        nbf: now,
        exp: now + request.lifetime_ms,
        jti: uuid::Uuid::new_v4().to_string(),
        op_id: request.op_id.to_string(),
        args_digest: digest_hex(request.payload_cbor),
        signature: Vec::new(),
    };
    let (kid, signature) = with_local_ring(|ring| {
        let input = {
            let mut probe = assertion.clone();
            probe.kid = ring.current.kid.clone();
            signing_input(&probe)
        };
        (
            ring.current.kid.clone(),
            ring.current.signing.sign(&input).to_bytes().to_vec(),
        )
    });
    assertion.kid = kid;
    assertion.signature = signature;
    Ok(assertion)
}

// =============================================================================
// Verification
// =============================================================================

/// Everything verification compares the assertion against, gathered by the
/// caller so the check itself stays pure and testable.
#[derive(Debug, Clone)]
pub struct VerifyInput<'a> {
    /// Authenticated peer of the mesh connection this arrived on. `iss` must
    /// equal it — this is the channel binding.
    pub channel_peer_id: &'a str,
    pub local_node_id: &'a str,
    /// The exact bytes the assertion is supposed to cover.
    pub payload_cbor: &'a [u8],
    pub now_unix_ms: u64,
}

/// Claim and signature checks. Does NOT touch the database: replay and RBAC are
/// separate steps, because a caller that only wants to know "is this assertion
/// well formed and honestly signed" should not have to open a transaction.
pub fn verify_claims(
    assertion: &SessionAssertion,
    input: &VerifyInput<'_>,
) -> Result<(), AssertionError> {
    if assertion.alg != ALG_ED25519 {
        return Err(AssertionError::UnsupportedAlg(assertion.alg.clone()));
    }
    for (name, value) in [
        ("kid", &assertion.kid),
        ("iss", &assertion.iss),
        ("sub", &assertion.sub),
        ("aud", &assertion.aud),
        ("org", &assertion.org),
        ("workspace", &assertion.workspace),
        ("jti", &assertion.jti),
    ] {
        if value.is_empty() || value.len() > MAX_FIELD_LEN {
            return Err(AssertionError::Malformed(format!("field '{name}'")));
        }
    }
    if assertion.session.len() > MAX_FIELD_LEN
        || assertion.op_id.len() > MAX_FIELD_LEN
        || assertion.rbac_rev.len() > MAX_FIELD_LEN
    {
        return Err(AssertionError::Malformed("oversized field".to_string()));
    }
    if assertion.caps.len() > MAX_CAPS {
        return Err(AssertionError::Malformed(
            "too many capabilities".to_string(),
        ));
    }
    for cap in &assertion.caps {
        if Capability::from_slug(cap).is_none() {
            return Err(AssertionError::Malformed(format!("capability '{cap}'")));
        }
    }

    // Channel binding: a trusted node may only speak for itself.
    if assertion.iss != input.channel_peer_id {
        return Err(AssertionError::IssuerMismatch {
            claimed: assertion.iss.clone(),
            channel: input.channel_peer_id.to_string(),
        });
    }
    if assertion.aud != input.local_node_id {
        return Err(AssertionError::AudienceMismatch {
            claimed: assertion.aud.clone(),
            local: input.local_node_id.to_string(),
        });
    }

    let now = input.now_unix_ms;
    if assertion.exp <= assertion.nbf || assertion.nbf < assertion.iat {
        return Err(AssertionError::Malformed(
            "iat / nbf / exp are not ordered".to_string(),
        ));
    }
    let lifetime = assertion.exp.saturating_sub(assertion.iat);
    if lifetime > MAX_LIFETIME_MS {
        return Err(AssertionError::LifetimeTooLong {
            lifetime_ms: lifetime,
        });
    }
    if assertion.nbf > now.saturating_add(CLOCK_SKEW_MS) {
        return Err(AssertionError::NotYetValid {
            nbf: assertion.nbf,
            now,
        });
    }
    if assertion.exp <= now {
        return Err(AssertionError::Expired {
            exp: assertion.exp,
            now,
        });
    }

    if assertion.args_digest != digest_hex(input.payload_cbor) {
        return Err(AssertionError::ArgsDigestMismatch);
    }

    let key =
        peer_key(&assertion.iss, &assertion.kid).ok_or_else(|| AssertionError::UnknownKey {
            peer: assertion.iss.clone(),
            kid: assertion.kid.clone(),
        })?;
    let signature_bytes = <[u8; 64]>::try_from(assertion.signature.as_slice())
        .map_err(|_| AssertionError::BadSignature)?;
    let signature = Signature::from_bytes(&signature_bytes);
    key.verify(&signing_input(assertion), &signature)
        .map_err(|_| AssertionError::BadSignature)
}

// =============================================================================
// Persistent anti-replay
// =============================================================================

fn rfc3339_ms(unix_ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(unix_ms as i64)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Burn a `jti`. First use succeeds; every repeat — including one after a
/// restart, which is the whole reason this is a table and not a cache — fails
/// with `Replay`.
///
/// Expired rows are swept in the same call: the index on `expires_at` makes it
/// a bounded delete, and tying it to use means the table cannot grow without a
/// separate janitor running.
pub fn consume_jti(db: &DbPool, jti: &str, exp_unix_ms: u64) -> Result<(), AssertionError> {
    let mut conn = db.write().map_err(|e| AssertionError::Db(e.to_string()))?;
    let tx = conn
        .transaction()
        .map_err(|e| AssertionError::Db(e.to_string()))?;
    tx.execute(
        "DELETE FROM session_assertion_jti WHERE expires_at <= ?1",
        params![rfc3339_ms(now_unix_ms())],
    )
    .map_err(|e| AssertionError::Db(e.to_string()))?;
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO session_assertion_jti (jti, expires_at) VALUES (?1, ?2)",
        params![jti, rfc3339_ms(exp_unix_ms)],
    );
    match inserted {
        Ok(1) => {
            tx.commit().map_err(|e| AssertionError::Db(e.to_string()))?;
            Ok(())
        }
        Ok(_) => Err(AssertionError::Replay {
            jti: jti.to_string(),
        }),
        Err(e) => Err(AssertionError::Db(e.to_string())),
    }
}

// =============================================================================
// Authorization inputs: revision and capabilities
// =============================================================================

/// The actor's authorization inputs on THIS node, condensed into one value.
///
/// It covers the org role and its permission list plus the workspace
/// membership role — exactly the inputs every Code Studio gate reads. Two
/// nodes that agree on those produce the same string; a divergence (a
/// revocation that has not finished syncing, most often) produces a different
/// one and forces the owner node to re-resolve before it acts.
pub fn rbac_revision(
    db: &DbPool,
    user_id: &str,
    org_id: &str,
    workspace_id: &str,
) -> Result<String, AssertionError> {
    let org = resolve_org_context(db, user_id, Some(org_id))
        .map_err(|e| AssertionError::Identity(e.to_string()))?;
    let role = repository::role_of(db, workspace_id, user_id)
        .map_err(|e| AssertionError::Db(e.to_string()))?;
    Ok(revision_of(&org, workspace_id, role))
}

fn revision_of(org: &OrgContext, workspace_id: &str, role: Option<WorkspaceRole>) -> String {
    let mut permissions: Vec<&str> = org.permissions.iter().map(String::as_str).collect();
    permissions.sort_unstable();
    let mut hasher = Sha256::new();
    let mut push = |bytes: &[u8]| {
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    push(b"tentaflow:code_studio:rbac_rev:v1");
    push(org.org_id.as_bytes());
    push(org.role_id.as_bytes());
    push(workspace_id.as_bytes());
    push(role.map(|r| r.slug()).unwrap_or("none").as_bytes());
    push(&(permissions.len() as u64).to_le_bytes());
    for permission in permissions {
        push(permission.as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// What the actor may hold on this workspace, resolved from live local state.
///
/// This is the RBAC matrix of §9.2 read through `Capability::minimum_role` —
/// the one place that mapping lives. System capabilities are excluded: they
/// belong to the coordinator, never to an actor, so they must not be claimable
/// through an assertion.
pub fn resolve_caps(
    db: &DbPool,
    user_id: &str,
    workspace_id: &str,
) -> Result<Vec<Capability>, AssertionError> {
    let role = repository::role_of(db, workspace_id, user_id)
        .map_err(|e| AssertionError::Db(e.to_string()))?;
    Ok(caps_for_role(role))
}

/// The capability set of a workspace role. `None` (no membership) holds
/// nothing at all.
pub fn caps_for_role(role: Option<WorkspaceRole>) -> Vec<Capability> {
    let Some(role) = role else {
        return Vec::new();
    };
    Capability::ALL
        .into_iter()
        .filter(|cap| !cap.is_system() && role >= cap.minimum_role())
        .collect()
}

/// Outcome of a fully checked assertion.
#[derive(Debug, Clone)]
pub struct VerifiedAssertion {
    pub user_id: String,
    pub org_id: String,
    pub workspace_id: String,
    pub session_id: String,
    pub issuer_node_id: String,
    pub op_id: String,
    /// Capabilities the LOCAL resolution grants — never the claimed list.
    pub local_caps: Vec<Capability>,
    /// True when the issuer's revision differed and the claim had to be
    /// re-resolved against local state.
    pub revalidated: bool,
}

/// The complete owner-side check: claims and signature, then the persistent
/// replay gate, then the RBAC revision.
///
/// The order matters. Claims first, so an unsigned or misaddressed assertion
/// never reaches the database. The `jti` next, so a replay is refused before
/// any authorization work is repeated. The revision last, because a mismatch
/// means re-resolving permissions, and there is no point re-resolving for an
/// assertion that was never going to be accepted.
///
/// A revision mismatch is not itself fatal — it means the issuer and this node
/// disagree, so this node re-resolves from its own database and refuses only
/// when the local answer does not cover what the assertion claims. That is what
/// makes a role revoked elsewhere land: the claim survives, the local resolution
/// does not, and the operation is denied.
pub fn verify(
    db: &DbPool,
    assertion: &SessionAssertion,
    input: &VerifyInput<'_>,
) -> Result<VerifiedAssertion, AssertionError> {
    verify_claims(assertion, input)?;
    consume_jti(db, &assertion.jti, assertion.exp)?;

    let org = resolve_org_context(db, &assertion.sub, Some(&assertion.org))
        .map_err(|e| AssertionError::Identity(e.to_string()))?;
    let role = repository::role_of(db, &assertion.workspace, &assertion.sub)
        .map_err(|e| AssertionError::Db(e.to_string()))?;
    let local_rev = revision_of(&org, &assertion.workspace, role);
    let local_caps = caps_for_role(role);

    // The claim is measured against the LOCAL resolution unconditionally, not
    // only when the revisions disagree. Matching revisions imply matching sets
    // — both sides run this same function over the same inputs — so the check
    // is free in the common case, and it means a peer cannot claim a capability
    // by reporting a revision that happens to agree.
    let granted: HashSet<&str> = local_caps.iter().map(|c| c.slug()).collect();
    let missing: Vec<String> = assertion
        .caps
        .iter()
        .filter(|cap| !granted.contains(cap.as_str()))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(AssertionError::RbacRevisionMismatch {
            claimed: assertion.rbac_rev.clone(),
            local: local_rev,
            missing,
        });
    }

    let revalidated = local_rev != assertion.rbac_rev;
    if revalidated {
        tracing::info!(
            user = %assertion.sub,
            workspace = %assertion.workspace,
            issuer = %assertion.iss,
            "code studio: assertion rbac revision diverged, re-resolved locally"
        );
    }

    Ok(VerifiedAssertion {
        user_id: assertion.sub.clone(),
        org_id: assertion.org.clone(),
        workspace_id: assertion.workspace.clone(),
        session_id: assertion.session.clone(),
        issuer_node_id: assertion.iss.clone(),
        op_id: assertion.op_id.clone(),
        local_caps,
        revalidated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::org::repo as org_repo;
    use std::path::PathBuf;
    use tempfile::TempDir;

    const NODE_A: &str = "node-a";
    const NODE_B: &str = "node-b";

    fn open_pool(path: &PathBuf) -> DbPool {
        crate::db::init(path).expect("init db")
    }

    struct Fixture {
        _dir: TempDir,
        path: PathBuf,
        pool: DbPool,
        user_id: String,
        org_id: String,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("assertion_test.db");
        let pool = open_pool(&path);
        let user_id = uuid::Uuid::new_v4().to_string();
        let org_id = seed_member(&pool, &user_id, WorkspaceRole::Owner);
        Fixture {
            _dir: dir,
            path,
            pool,
            user_id,
            org_id,
        }
    }

    /// Seeds an org, a user with an org role, a workspace owned by node B and a
    /// workspace membership at `role`. Returns the org id.
    fn seed_member(pool: &DbPool, user_id: &str, role: WorkspaceRole) -> String {
        let org = org_repo::create_organization(pool, "Acme", "acme", None, None, None, None)
            .expect("create org");
        let role_id = role_id_named(pool, "org_admin");
        org_repo::add_membership(pool, &org.org_id, user_id, &role_id, user_id)
            .expect("org membership");

        let conn = pool.write().expect("db");
        conn.execute(
            "INSERT OR IGNORE INTO user_accounts \
               (id, username, password_hash, display_name, email, is_active, is_admin, \
                created_at, updated_at, role) \
             VALUES (?1, ?1, 'x', ?1, ?1, 1, 0, datetime('now'), datetime('now'), 'user')",
            params![user_id],
        )
        .expect("seed user");
        conn.execute(
            "INSERT OR IGNORE INTO code_workspaces \
               (id, org_id, owner_user_id, name, slug, node_id, exec_mode, \
                egress_enforcement, repo_kind, autonomy_ceiling, egress_policy, \
                index_enabled, status, created_at, updated_at) \
             VALUES ('ws-1', ?2, ?1, 'W', 'w', ?3, 'trusted_native', \
                'unrestricted', 'empty', 'normal', 'org_approved', 0, 'active', \
                datetime('now'), datetime('now'))",
            params![user_id, org.org_id, NODE_B],
        )
        .expect("seed workspace");
        conn.execute(
            "INSERT OR REPLACE INTO code_workspace_members \
               (workspace_id, user_id, role, added_by, added_at) \
             VALUES ('ws-1', ?1, ?2, ?1, datetime('now'))",
            params![user_id, role.slug()],
        )
        .expect("seed membership");
        org.org_id
    }

    fn role_id_named(pool: &DbPool, name: &str) -> String {
        org_repo::list_roles(pool)
            .expect("roles")
            .into_iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("role '{name}' must be seeded by the migrations"))
            .role_id
    }

    fn set_workspace_role(pool: &DbPool, user_id: &str, role: WorkspaceRole) {
        let conn = pool.write().expect("db");
        conn.execute(
            "UPDATE code_workspace_members SET role = ?2 WHERE workspace_id = 'ws-1' \
             AND user_id = ?1",
            params![user_id, role.slug()],
        )
        .expect("update role");
    }

    /// Mints an assertion as node A would and publishes A's keys to the local
    /// (node B) verifier pool, the way an advertisement over the mesh does.
    fn mint(
        f: &Fixture,
        payload: &[u8],
        caps: &[Capability],
        lifetime_ms: u64,
    ) -> SessionAssertion {
        let rev = rbac_revision(&f.pool, &f.user_id, &f.org_id, "ws-1").expect("revision");
        let assertion = issue(&IssueRequest {
            local_node_id: NODE_A,
            user_id: &f.user_id,
            owner_node_id: NODE_B,
            org_id: &f.org_id,
            workspace_id: "ws-1",
            session_id: "sess-1",
            caps,
            rbac_rev: &rev,
            op_id: "op-1",
            payload_cbor: payload,
            lifetime_ms,
        })
        .expect("issue");
        ingest_peer_keys(NODE_A, &local_advertise());
        assertion
    }

    fn input<'a>(payload: &'a [u8]) -> VerifyInput<'a> {
        VerifyInput {
            channel_peer_id: NODE_A,
            local_node_id: NODE_B,
            payload_cbor: payload,
            now_unix_ms: now_unix_ms(),
        }
    }

    /// §12.2 — a stream call is verified through THIS function, so it inherits
    /// the channel binding, the persistent `jti` and the lifetime ceiling. What
    /// it adds is the binding to its own parameters: an assertion minted to
    /// read one stream cannot be re-aimed at another person's session, because
    /// the digest covers the request bytes.
    #[test]
    fn a_stream_pull_assertion_is_bound_to_its_parameters() {
        use tentaflow_protocol::mesh::CodeStudioStreamPullRequest;

        let f = fixture();
        let mine = crate::mesh::cbor::encode(&CodeStudioStreamPullRequest {
            session_id: "sess-1".into(),
            stream_id: "timeline".into(),
            after_seq: 0,
            ack_seq: 0,
            credits: 64,
        })
        .expect("encode");
        let theirs = crate::mesh::cbor::encode(&CodeStudioStreamPullRequest {
            session_id: "sess-2".into(),
            stream_id: "timeline".into(),
            after_seq: 0,
            ack_seq: 0,
            credits: 64,
        })
        .expect("encode");

        let assertion = mint(&f, &mine, &[Capability::FsRead], 30_000);
        assert!(
            matches!(
                verify(&f.pool, &assertion, &input(&theirs)),
                Err(AssertionError::ArgsDigestMismatch)
            ),
            "a pull signed for one stream must not read another"
        );
        // The unmodified call still works, and burns its `jti` doing so.
        verify(&f.pool, &assertion, &input(&mine)).expect("verify");
        assert!(matches!(
            verify(&f.pool, &assertion, &input(&mine)),
            Err(AssertionError::Replay { .. })
        ));
    }

    /// A stream is read for hours and an assertion lives at most 120 s, so the
    /// consumer mints one per call. An expired one is refused rather than
    /// tolerated because the stream was authorized once already.
    #[test]
    fn an_expired_stream_assertion_is_refused() {
        let f = fixture();
        let payload = b"pull".to_vec();
        let assertion = mint(&f, &payload, &[Capability::FsRead], 30_000);
        let mut late = input(&payload);
        late.now_unix_ms = assertion.exp + 1;
        assert!(matches!(
            verify(&f.pool, &assertion, &late),
            Err(AssertionError::Expired { .. })
        ));
    }

    #[test]
    fn valid_assertion_verifies_and_reports_local_caps() {
        let f = fixture();
        let payload = b"payload".to_vec();
        let assertion = mint(&f, &payload, &[Capability::FsRead], 30_000);
        let verified = verify(&f.pool, &assertion, &input(&payload)).expect("verify");
        assert_eq!(verified.user_id, f.user_id);
        assert!(!verified.revalidated);
        assert!(verified.local_caps.contains(&Capability::GitPush));
        assert!(!verified.local_caps.contains(&Capability::GitWorktree));
    }

    /// A replayed `jti` is refused, and STAYS refused once the pool has been
    /// closed and reopened — the point of storing it instead of caching it.
    #[test]
    fn replayed_jti_is_rejected_before_and_after_restart() {
        let f = fixture();
        let payload = b"payload".to_vec();
        let assertion = mint(&f, &payload, &[Capability::FsRead], 60_000);
        verify(&f.pool, &assertion, &input(&payload)).expect("first use");
        assert!(matches!(
            verify(&f.pool, &assertion, &input(&payload)),
            Err(AssertionError::Replay { .. })
        ));

        // The "restart": close every handle to the database and open the same
        // FILE again. `_dir` stays bound so the directory outlives the reopen —
        // dropping it here would silently give the test a brand new database
        // and prove nothing.
        let Fixture {
            _dir, path, pool, ..
        } = f;
        drop(pool);
        let reopened = open_pool(&path);
        assert!(
            matches!(
                verify(&reopened, &assertion, &input(&payload)),
                Err(AssertionError::Replay { .. })
            ),
            "a jti burned before the restart must still be burned after it"
        );
        drop(_dir);
    }

    #[test]
    fn assertion_from_another_issuer_on_this_channel_is_rejected() {
        let f = fixture();
        let payload = b"payload".to_vec();
        let mut assertion = mint(&f, &payload, &[Capability::FsRead], 30_000);
        assertion.iss = "node-c".to_string();
        assert!(matches!(
            verify(&f.pool, &assertion, &input(&payload)),
            Err(AssertionError::IssuerMismatch { .. })
        ));
    }

    #[test]
    fn assertion_for_another_audience_is_rejected() {
        let f = fixture();
        let payload = b"payload".to_vec();
        let assertion = mint(&f, &payload, &[Capability::FsRead], 30_000);
        let mut verify_input = input(&payload);
        verify_input.local_node_id = "node-z";
        assert!(matches!(
            verify(&f.pool, &assertion, &verify_input),
            Err(AssertionError::AudienceMismatch { .. })
        ));
    }

    #[test]
    fn lifetime_above_the_ceiling_is_refused_at_issuance_and_at_verification() {
        let f = fixture();
        let rev = rbac_revision(&f.pool, &f.user_id, &f.org_id, "ws-1").expect("revision");
        let request = IssueRequest {
            local_node_id: NODE_A,
            user_id: &f.user_id,
            owner_node_id: NODE_B,
            org_id: &f.org_id,
            workspace_id: "ws-1",
            session_id: "sess-1",
            caps: &[Capability::FsRead],
            rbac_rev: &rev,
            op_id: "",
            payload_cbor: b"payload",
            lifetime_ms: MAX_LIFETIME_MS + 1,
        };
        assert!(matches!(
            issue(&request),
            Err(AssertionError::LifetimeTooLong { .. })
        ));

        // And a peer that mints one anyway — signature and all — is refused on
        // arrival, not trusted to have respected the ceiling.
        let payload = b"payload".to_vec();
        let mut assertion = mint(&f, &payload, &[Capability::FsRead], 30_000);
        assertion.exp = assertion.iat + MAX_LIFETIME_MS + 5_000;
        resign_as_node_a(&mut assertion);
        assert!(matches!(
            verify(&f.pool, &assertion, &input(&payload)),
            Err(AssertionError::LifetimeTooLong { .. })
        ));
    }

    #[test]
    fn nbf_in_the_future_is_rejected() {
        let f = fixture();
        let payload = b"payload".to_vec();
        let mut assertion = mint(&f, &payload, &[Capability::FsRead], 60_000);
        assertion.nbf = assertion.iat + 30_000;
        resign_as_node_a(&mut assertion);
        assert!(matches!(
            verify(&f.pool, &assertion, &input(&payload)),
            Err(AssertionError::NotYetValid { .. })
        ));
    }

    #[test]
    fn payload_swapped_after_signing_is_rejected() {
        let f = fixture();
        let payload = b"payload".to_vec();
        let assertion = mint(&f, &payload, &[Capability::FsRead], 30_000);
        assert!(matches!(
            verify(&f.pool, &assertion, &input(b"different payload")),
            Err(AssertionError::ArgsDigestMismatch)
        ));
    }

    /// The role is taken away on the verifying node after the assertion was
    /// minted. The signature is still perfectly good; the claim is not.
    #[test]
    fn assertion_is_rejected_after_the_rbac_revision_changed() {
        let f = fixture();
        let payload = b"payload".to_vec();
        let assertion = mint(&f, &payload, &[Capability::GitPush], 60_000);
        set_workspace_role(&f.pool, &f.user_id, WorkspaceRole::Viewer);
        match verify(&f.pool, &assertion, &input(&payload)) {
            Err(AssertionError::RbacRevisionMismatch { missing, .. }) => {
                assert!(missing.contains(&"git_push".to_string()));
            }
            other => panic!("expected an rbac mismatch, got {other:?}"),
        }
    }

    /// A revision that moved without narrowing the claim re-resolves and passes:
    /// the divergence forces a re-resolution, it does not by itself deny.
    #[test]
    fn diverged_revision_revalidates_instead_of_denying() {
        let f = fixture();
        let payload = b"payload".to_vec();
        let assertion = mint(&f, &payload, &[Capability::FsRead], 60_000);
        let viewer_role = role_id_named(&f.pool, "org_viewer");
        {
            let conn = f.pool.write().expect("db");
            conn.execute(
                "UPDATE org_memberships SET role_id = ?1 WHERE user_id = ?2",
                params![viewer_role, f.user_id],
            )
            .expect("re-role");
        }
        let verified = verify(&f.pool, &assertion, &input(&payload)).expect("verify");
        assert!(verified.revalidated);
        // The workspace role is untouched, so the capability set is unchanged.
        assert!(verified.local_caps.contains(&Capability::GitPush));
    }

    /// Rotating the signing key must not cut a session that is mid-flight: the
    /// assertion signed with the retired key still verifies until its own `exp`,
    /// and a fresh one is signed with the new key.
    #[test]
    fn key_rotation_keeps_live_assertions_valid() {
        let f = fixture();
        let payload = b"payload".to_vec();
        let before = mint(&f, &payload, &[Capability::FsRead], 60_000);
        let kid_before = before.kid.clone();

        rotate_local_key();
        ingest_peer_keys(NODE_A, &local_advertise());
        let after = mint(&f, &payload, &[Capability::FsRead], 60_000);

        assert_ne!(kid_before, after.kid, "rotation must change the key id");
        assert_eq!(peer_key_count(NODE_A), 2, "both keys stay verifiable");
        verify(&f.pool, &before, &input(&payload)).expect("pre-rotation assertion still verifies");
        verify(&f.pool, &after, &input(&payload)).expect("post-rotation assertion verifies");
    }

    #[test]
    fn key_id_that_does_not_match_its_material_is_refused() {
        let peer = "node-liar";
        forget_peer_keys(peer);
        let signing = SigningKey::generate(&mut rand_core_06::OsRng);
        let accepted = ingest_peer_keys(
            peer,
            &[AssertionKeyEntry {
                kid: "0000000000000000".to_string(),
                public_key: signing.verifying_key().as_bytes().to_vec(),
                expires_unix_ms: 0,
            }],
        );
        assert_eq!(accepted, 0);
        assert_eq!(peer_key_count(peer), 0);
        forget_peer_keys(peer);
    }

    /// A workspace owned by node B, driven from node A: two separate databases,
    /// the same synced registry rows. The assertion minted against A's view of
    /// the actor has to verify against B's — and stop verifying the moment B's
    /// view says the actor lost the role.
    #[test]
    fn workspace_on_node_b_is_driven_from_node_a() {
        let dir = TempDir::new().expect("tempdir");
        let pool_a = open_pool(&dir.path().join("node-a.db"));
        let pool_b = open_pool(&dir.path().join("node-b.db"));
        let user_id = uuid::Uuid::new_v4().to_string();
        let org_id = "org-shared";
        for pool in [&pool_a, &pool_b] {
            seed_shared_registry(pool, org_id, &user_id, WorkspaceRole::Owner);
        }

        let payload = b"{\"op\":\"git_status\"}".to_vec();
        let rev = rbac_revision(&pool_a, &user_id, org_id, "ws-1").expect("revision on A");
        let caps = resolve_caps(&pool_a, &user_id, "ws-1").expect("caps on A");
        let assertion = issue(&IssueRequest {
            local_node_id: NODE_A,
            user_id: &user_id,
            owner_node_id: NODE_B,
            org_id,
            workspace_id: "ws-1",
            session_id: "sess-1",
            caps: &caps,
            rbac_rev: &rev,
            op_id: "op-1",
            payload_cbor: &payload,
            lifetime_ms: DEFAULT_LIFETIME_MS,
        })
        .expect("issue on A");
        ingest_peer_keys(NODE_A, &local_advertise());

        // Node B accepts it: same registry, same revision.
        let verified = verify(&pool_b, &assertion, &input(&payload)).expect("verify on B");
        assert_eq!(verified.workspace_id, "ws-1");
        assert!(verified.local_caps.contains(&Capability::GitPush));
        assert!(!verified.revalidated);

        // The membership is taken away on B only — the revocation has not made
        // it back to A yet, so A would still happily mint the same claim.
        {
            let conn = pool_b.write().expect("db");
            conn.execute(
                "DELETE FROM code_workspace_members WHERE workspace_id = 'ws-1' AND user_id = ?1",
                params![user_id],
            )
            .expect("revoke on B");
        }
        let rev = rbac_revision(&pool_a, &user_id, org_id, "ws-1").expect("revision on A");
        let replacement = issue(&IssueRequest {
            local_node_id: NODE_A,
            user_id: &user_id,
            owner_node_id: NODE_B,
            org_id,
            workspace_id: "ws-1",
            session_id: "sess-1",
            caps: &caps,
            rbac_rev: &rev,
            op_id: "op-2",
            payload_cbor: &payload,
            lifetime_ms: DEFAULT_LIFETIME_MS,
        })
        .expect("issue on A");
        assert!(matches!(
            verify(&pool_b, &replacement, &input(&payload)),
            Err(AssertionError::RbacRevisionMismatch { .. })
        ));
    }

    /// Seeds the rows the Sync Ledger would have carried to every node of the
    /// org, with ids fixed so two pools describe the SAME workspace.
    fn seed_shared_registry(pool: &DbPool, org_id: &str, user_id: &str, role: WorkspaceRole) {
        let admin_role = role_id_named(pool, "org_admin");
        let conn = pool.write().expect("db");
        conn.execute(
            "INSERT OR IGNORE INTO organizations (org_id, name, slug, status, created_at) \
             VALUES (?1, 'Shared', ?1, 'active', datetime('now'))",
            params![org_id],
        )
        .expect("seed org");
        conn.execute(
            "INSERT OR IGNORE INTO user_accounts \
               (id, username, password_hash, display_name, email, is_active, is_admin, \
                created_at, updated_at, role) \
             VALUES (?1, ?1, 'x', ?1, ?1, 1, 0, datetime('now'), datetime('now'), 'user')",
            params![user_id],
        )
        .expect("seed user");
        conn.execute(
            "INSERT OR IGNORE INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
             VALUES (?1, ?2, ?3, datetime('now'), ?2)",
            params![org_id, user_id, admin_role],
        )
        .expect("seed org membership");
        conn.execute(
            "INSERT OR IGNORE INTO code_workspaces \
               (id, org_id, owner_user_id, name, slug, node_id, exec_mode, \
                egress_enforcement, repo_kind, autonomy_ceiling, egress_policy, \
                index_enabled, status, created_at, updated_at) \
             VALUES ('ws-1', ?2, ?1, 'W', 'w', ?3, 'trusted_native', 'unrestricted', 'empty', \
                'normal', 'org_approved', 0, 'active', datetime('now'), datetime('now'))",
            params![user_id, org_id, NODE_B],
        )
        .expect("seed workspace");
        conn.execute(
            "INSERT OR REPLACE INTO code_workspace_members \
               (workspace_id, user_id, role, added_by, added_at) \
             VALUES ('ws-1', ?1, ?2, ?1, datetime('now'))",
            params![user_id, role.slug()],
        )
        .expect("seed membership");
    }

    #[test]
    fn caps_follow_the_role_matrix_and_never_include_system_ones() {
        let owner = caps_for_role(Some(WorkspaceRole::Owner));
        let editor = caps_for_role(Some(WorkspaceRole::Editor));
        let viewer = caps_for_role(Some(WorkspaceRole::Viewer));
        assert!(owner.contains(&Capability::GitPush));
        assert!(!editor.contains(&Capability::GitPush));
        assert!(editor.contains(&Capability::FsWrite));
        assert!(!viewer.contains(&Capability::FsWrite));
        assert!(viewer.contains(&Capability::FsRead));
        for set in [&owner, &editor, &viewer] {
            assert!(!set.contains(&Capability::GitWorktree));
        }
        assert!(caps_for_role(None).is_empty());
    }

    /// Re-signs a tampered assertion with node A's CURRENT key, so the test
    /// exercises the claim check rather than the signature check.
    fn resign_as_node_a(assertion: &mut SessionAssertion) {
        with_local_ring(|ring| {
            assertion.kid = ring.current.kid.clone();
            let input = signing_input(assertion);
            assertion.signature = ring.current.signing.sign(&input).to_bytes().to_vec();
        });
        ingest_peer_keys(NODE_A, &local_advertise());
    }
}
