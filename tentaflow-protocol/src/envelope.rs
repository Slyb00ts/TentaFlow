// =============================================================================
// Plik: envelope.rs
// Opis: Framing dla binary WebSocket/QUIC. Envelope = header nad MessageBody
//       (routing, policy_hint, sequence, forwarded_session_claim). Zero-copy
//       przez rkyv, walidacja przez bytecheck na WSS input.
// Przyklad:
//   let env = Envelope::new_direct(1, 42, MSG_KIND_NODE_LIST_REQUEST, body_bytes);
//   let wire = rkyv::to_bytes::<rkyv::rancor::Error>(&env)?;
//   let decoded = rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&wire)?;
// =============================================================================

use rkyv::{Archive, Deserialize, Serialize};

// =============================================================================
// Schema version
// =============================================================================

/// Wersja schematu protokolu. Handshake porownuje wartosc klienta z serwerem;
/// mismatch = reject. Inkrementowac przy KAZDEJ breaking change w Envelope
/// lub MessageBody. Migration przez dual-version support w przejsciowym okienku.
///
/// v3 changes (2026-04-18):
///   - Envelope.sequence u32 -> u64 (overflow bug fix)
///   - SessionAuth::UserSession adds `role: Option<String>` (RBAC)
///   - Resume tokens now bind to originating_user_id (P0 fix)
/// v5 changes (2026-04-18): BREAKING — brak backward compat z nodami na v4
///   - Mesh transport zmieniony z quinn (custom TLS + ChaCha20-Poly1305 wrap +
///     epoch rotation + nonce counter + sliding window replay) na iroh
///     (QUIC z TLS 1.3 + relay fallback + LAN mDNS + DHT pkarr discovery).
///   - MeshPeerSummary.endpoint pole porzucone — iroh rozwiazuje adres po
///     EndpointId (wczesniej NodeId).
///   - ALPN zmienione na `tentaflow-mesh/v1`, `tentaflow-pairing/v1`,
///     `tentaflow-api/v1`.
///   - Custom AEAD/replay/rotation usunieta z `mesh/security.rs` — bezpieczenstwo
///     transportu zapewnia iroh TLS. Zostaje Ed25519 identity, trusted_keys,
///     PIN pairing + X25519 pin-proof derywacja, TrustRevoked broadcast.
/// v8 changes (2026-04-23):
///   - MeshNodeInfo rozszerzony o `connection` z aktywna sciezka i listą pathow
///     (p2p/relay + adresy), zeby GUI moglo pokazac realny transport mesh.
/// v9 changes (2026-04-23):
///   - `MessageBody::MeetingLiveEventBody(MeetingLiveEvent)` — unsolicited
///     broadcast dashboard GUI po każdym sukcesie `persist_meeting_event`.
///     Filtrowany server-side po owner_user_id sesji.
/// v10 changes (2026-04-24):
///   - Mesh & Network settings: `NetworkInterfacesList*`, `NetworkConfigGet*`,
///     `NetworkConfigUpdate*` — enumeracja IPv4 NIC hosta + perzistowane w DB
///     reguly bind/advertise dla iroh mesh (IPv4-only, zero v6).
pub const SCHEMA_VERSION: u16 = 10;

// =============================================================================
// Message kind discriminants
// =============================================================================

/// Klasyfikacja wiadomosci w `Envelope.message_kind`.
/// Discriminant u16 -> wariant `MessageBody`. Trzymane jako stale (a nie enum)
/// zeby dodawanie variantow bylo additive bez zmiany header layoutu.
/// Zakres 0x0000-0x0FFF = client<->node, 0x1000-0x1FFF = node<->node mesh,
/// 0x2000-0x2FFF = pairing/trust, 0x3000-0x3FFF = management, 0xF000+ = meta.
pub mod message_kind {
    /// Placeholder: meta — schema version check przy handshake.
    pub const META_SCHEMA_VERSION_CHECK: u16 = 0xF001;
    /// Placeholder: meta — bledy protokolu (malformed frame, policy reject).
    pub const META_PROTOCOL_ERROR: u16 = 0xF002;
    /// Placeholder: meta — heartbeat/ping dla WSS keepalive.
    pub const META_HEARTBEAT: u16 = 0xF003;
    /// Placeholder: meta — cancel aktywnego streama po correlation_id.
    pub const META_CANCEL_STREAM: u16 = 0xF004;
}

// =============================================================================
// Routing
// =============================================================================

/// Jak zaadresowany jest ten frame.
/// `Direct` = serwer/klient obsluguje lokalnie. `Forward` = adresat to inny
/// node w mesh, obecny node ma tylko przekazac (z policy re-check po stronie B).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum Routing {
    /// Adresat = ten node/klient.
    Direct,
    /// Adresat = inny node mesh (32-byte Ed25519 public key).
    Forward { target_node_id: [u8; 32] },
}

// =============================================================================
// Envelope flags (plain u8, no bitflags dep)
// =============================================================================

/// Flagi bitowe w headerze frame. Surowy u8 zamiast `bitflags!` zeby uniknac
/// dodatkowej zaleznosci (krytyczne dla WASM bundle size).
///
/// Bit layout:
/// - 0x01 IS_ERROR — body to `MessageBody::Error`
/// - 0x02 IS_STREAM_CHUNK — jeden z frameow streamingowej odpowiedzi
/// - 0x04 IS_STREAM_END — ostatni frame streama (opcjonalnie z usage/payload)
/// - 0x08 .. 0x80 — zarezerwowane
#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvelopeFlags(pub u8);

impl EnvelopeFlags {
    pub const NONE: Self = Self(0);
    pub const IS_ERROR: Self = Self(0b0000_0001);
    pub const IS_STREAM_CHUNK: Self = Self(0b0000_0010);
    pub const IS_STREAM_END: Self = Self(0b0000_0100);

    pub fn empty() -> Self {
        Self::NONE
    }

    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    pub fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for EnvelopeFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for EnvelopeFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

// =============================================================================
// SessionAuth (reference do typu autoryzacji sesji)
// =============================================================================

/// Typ autoryzacji sesji, z ktorej wyszedl originating request.
/// Ustawiane raz na connection handshake (WSS upgrade / QUIC ALPN accept)
/// i przenoszone w `SignedSessionClaim` przy mesh forward.
///
/// Nigdy nie jest re-negotiowane per-message.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum SessionAuth {
    /// Publiczne endpointy (OpenAI-compat `/v1/*`). Brak user contextu.
    Anonymous,
    /// Zewnetrzny klient uwierzytelniony przez API key (key_id do logowania).
    ApiKey { key_id: String },
    /// Browser GUI przez WSS; user_id jako 16-byte UUID. `role` z JWT claims
    /// ("admin" / "user" / itp.); None gdy claims nie maja role field.
    UserSession {
        user_id: [u8; 16],
        role: Option<String>,
    },
    /// Mesh peer trusted przez pairing; node_id = Ed25519 pubkey, epoch = key rotation.
    MeshTrust { node_id: [u8; 32], epoch: u32 },
}

// =============================================================================
// SignedSessionClaim
// =============================================================================

/// Przenoszone w forwarded envelopes. Pozwala Node B's PermissionCheckerowi
/// re-check policy przeciwko originating user context, bez mutacji inner
/// MessageBody.
///
/// THREAT MODEL (explicit):
/// Trust mesh boundary zaklada no node compromise. Ten claim to permission-aliasing
/// primitive dla forwardowanych operacji, NIE defense-in-depth przeciwko
/// compromised forwarding node. Jesli Node A skompromitowany, attacker z kluczem A
/// moze forge claim dla dowolnego user_id — Node B zaufa bo A jest w trusted_keys.
/// Realna mitigation: timely TrustRevoked broadcast + epoch rotation (24h + 7d grace).
///
/// Signature pokrywa canonical bytes: `signing_message()` output.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct SignedSessionClaim {
    /// UUID uzytkownika ktory zainicjowal lancuch requestow (16 bajtow raw UUID).
    pub originating_user_id: [u8; 16],
    /// Typ auth z ktorego lancuch wyszedl (np. UserSession vs ApiKey).
    pub originating_session_type: SessionAuth,
    /// Unix epoch (sekundy) wystawienia claima. Sluzy do wygasania.
    pub issued_at_epoch: u64,
    /// Ed25519 public key noda ktory forwarduje (32 bajty).
    pub forwarding_node_id: [u8; 32],
    /// Ed25519 signature nad `signing_message()` (64 bajty).
    pub signature: [u8; 64],
}

impl SignedSessionClaim {
    /// Kanoniczne bajty do podpisu/weryfikacji. Deterministyczne, little-endian,
    /// bez paddingu rkyv. Format (134+ bajtow):
    ///   [16] originating_user_id
    ///   [8]  issued_at_epoch (LE)
    ///   [32] forwarding_node_id
    ///   [1]  session_type discriminant (0=Anon, 1=ApiKey, 2=UserSession, 3=MeshTrust)
    ///   [..] session_type extra bytes (key_id utf8 / user_id / node_id+epoch)
    ///
    /// UWAGA: format MUSI byc stable — zmiana = invalidacja wszystkich podpisanych
    /// claimow. Gdy potrzeba rozszerzenia, nowy discriminant a nie mutacja istniejacego.
    pub fn signing_message(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(128);
        buf.extend_from_slice(&self.originating_user_id);
        buf.extend_from_slice(&self.issued_at_epoch.to_le_bytes());
        buf.extend_from_slice(&self.forwarding_node_id);
        match &self.originating_session_type {
            SessionAuth::Anonymous => {
                buf.push(0);
            }
            SessionAuth::ApiKey { key_id } => {
                buf.push(1);
                let bytes = key_id.as_bytes();
                buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                buf.extend_from_slice(bytes);
            }
            SessionAuth::UserSession { user_id, role } => {
                buf.push(2);
                buf.extend_from_slice(user_id);
                let role_bytes = role.as_deref().unwrap_or("").as_bytes();
                buf.extend_from_slice(&(role_bytes.len() as u32).to_le_bytes());
                buf.extend_from_slice(role_bytes);
            }
            SessionAuth::MeshTrust { node_id, epoch } => {
                buf.push(3);
                buf.extend_from_slice(node_id);
                buf.extend_from_slice(&epoch.to_le_bytes());
            }
        }
        buf
    }
}

// =============================================================================
// Envelope
// =============================================================================

/// Framing header dla jednej wiadomosci WSS/QUIC.
///
/// Body jest trzymane jako `Vec<u8>` (rkyv-serializowane MessageBody). Taki
/// layered pattern pozwala:
/// 1. Zdekodowac tylko envelope (policy check) zanim tkniemy drozszy body.
/// 2. Forwardowac frame bit-for-bit przez mesh bez re-encode payloadu.
/// 3. Wyabstrahowac opaque body = MessageBody trzymany w osobnym module.
///
/// Wire-level: caly `Envelope` jest rkyv-encoded w jeden WSS binary frame.
/// `rkyv::access` z bytecheck NA KAZDYM WSS input (nigdy `access_unchecked`).
#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// Wersja schematu. Klient wysyla swoja, serwer odrzuca mismatch.
    pub schema_version: u16,
    /// Linkuje request z response/stream chunkami. Per-connection unikalny.
    pub correlation_id: u64,
    /// Per-connection counter. Replay window (sliding) na serwerze. u64 zeby
    /// uniknac overflow na long-lived connections (4B+ frames).
    pub sequence: u64,
    /// Discriminant wariantu MessageBody (patrz `message_kind` module).
    pub message_kind: u16,
    /// Flagi bitowe (error / stream chunk / stream end).
    pub flags: EnvelopeFlags,
    /// Direct (local) vs Forward (do innego noda mesh).
    pub routing: Routing,
    /// Wypelniane gdy Node A forwarduje do Node B. Node B re-checks policy.
    pub forwarded_session_claim: Option<SignedSessionClaim>,
    /// rkyv-serializowany MessageBody jako opaque bytes.
    pub body: Vec<u8>,
}

impl Envelope {
    /// Nowy direct-routed envelope bez session claima (zwykly client->node).
    pub fn new_direct(
        correlation_id: u64,
        sequence: u64,
        message_kind: u16,
        body: Vec<u8>,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            correlation_id,
            sequence,
            message_kind,
            flags: EnvelopeFlags::empty(),
            routing: Routing::Direct,
            forwarded_session_claim: None,
            body,
        }
    }

    /// Nowy forward envelope z session claim (node A -> node B).
    pub fn new_forward(
        correlation_id: u64,
        sequence: u64,
        message_kind: u16,
        target_node_id: [u8; 32],
        claim: SignedSessionClaim,
        body: Vec<u8>,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            correlation_id,
            sequence,
            message_kind,
            flags: EnvelopeFlags::empty(),
            routing: Routing::Forward { target_node_id },
            forwarded_session_claim: Some(claim),
            body,
        }
    }

    /// Helper dla response z is_error set.
    pub fn with_error_flag(mut self) -> Self {
        self.flags.insert(EnvelopeFlags::IS_ERROR);
        self
    }

    /// Helper dla stream chunka.
    pub fn with_stream_chunk(mut self) -> Self {
        self.flags.insert(EnvelopeFlags::IS_STREAM_CHUNK);
        self
    }

    /// Helper dla koncowego frame streama.
    pub fn with_stream_end(mut self) -> Self {
        self.flags.insert(EnvelopeFlags::IS_STREAM_END);
        self
    }
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_claim() -> SignedSessionClaim {
        SignedSessionClaim {
            originating_user_id: [7u8; 16],
            originating_session_type: SessionAuth::UserSession {
                user_id: [7u8; 16],
                role: Some("user".to_string()),
            },
            issued_at_epoch: 1_700_000_000,
            forwarding_node_id: [9u8; 32],
            signature: [0u8; 64],
        }
    }

    #[test]
    fn envelope_direct_round_trip() {
        let env = Envelope::new_direct(42, 1, message_kind::META_HEARTBEAT, vec![1, 2, 3, 4]);
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&env).expect("encode");
        let decoded: Envelope =
            rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&bytes).expect("decode");
        assert_eq!(decoded, env);
        assert_eq!(decoded.schema_version, SCHEMA_VERSION);
        assert!(matches!(decoded.routing, Routing::Direct));
        assert!(decoded.forwarded_session_claim.is_none());
    }

    #[test]
    fn envelope_forward_with_claim_round_trip() {
        let claim = sample_claim();
        let env = Envelope::new_forward(
            99,
            7,
            message_kind::META_PROTOCOL_ERROR,
            [9u8; 32],
            claim.clone(),
            vec![0xAA, 0xBB],
        );
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&env).expect("encode");
        let decoded: Envelope =
            rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&bytes).expect("decode");
        assert_eq!(decoded, env);
        match decoded.routing {
            Routing::Forward { target_node_id } => assert_eq!(target_node_id, [9u8; 32]),
            _ => panic!("expected Forward routing"),
        }
        assert_eq!(decoded.forwarded_session_claim.as_ref().unwrap(), &claim);
    }

    #[test]
    fn envelope_flags_helpers_work() {
        let mut f = EnvelopeFlags::empty();
        assert!(!f.contains(EnvelopeFlags::IS_ERROR));
        f.insert(EnvelopeFlags::IS_STREAM_CHUNK);
        f.insert(EnvelopeFlags::IS_STREAM_END);
        assert!(f.contains(EnvelopeFlags::IS_STREAM_CHUNK));
        assert!(f.contains(EnvelopeFlags::IS_STREAM_END));
        assert!(!f.contains(EnvelopeFlags::IS_ERROR));
        f.remove(EnvelopeFlags::IS_STREAM_CHUNK);
        assert!(!f.contains(EnvelopeFlags::IS_STREAM_CHUNK));
        assert!(f.contains(EnvelopeFlags::IS_STREAM_END));
    }

    #[test]
    fn envelope_flags_persist_through_rkyv() {
        let mut env = Envelope::new_direct(1, 1, message_kind::META_HEARTBEAT, vec![]);
        env = env.with_stream_chunk().with_stream_end();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&env).expect("encode");
        let decoded: Envelope =
            rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&bytes).expect("decode");
        assert!(decoded.flags.contains(EnvelopeFlags::IS_STREAM_CHUNK));
        assert!(decoded.flags.contains(EnvelopeFlags::IS_STREAM_END));
    }

    #[test]
    fn truncated_bytes_rejected() {
        let env = Envelope::new_direct(1, 1, message_kind::META_HEARTBEAT, vec![1, 2, 3, 4]);
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&env).expect("encode");
        let truncated = &bytes[..bytes.len() / 2];
        let result = rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(truncated);
        assert!(
            result.is_err(),
            "truncated bytes must fail bytecheck validation"
        );
    }

    #[test]
    fn empty_bytes_rejected() {
        let result = rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&[]);
        assert!(result.is_err(), "empty bytes must fail");
    }

    #[test]
    fn corrupted_tail_rejected() {
        let env = Envelope::new_direct(1, 1, message_kind::META_HEARTBEAT, vec![1, 2, 3, 4]);
        let mut bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&env)
            .expect("encode")
            .to_vec();
        // Uszkodzenie ostatniego bajtu (relative pointer / len trailer w rkyv)
        if let Some(last) = bytes.last_mut() {
            *last = last.wrapping_add(0x7F);
        }
        let result = rkyv::from_bytes::<Envelope, rkyv::rancor::Error>(&bytes);
        assert!(result.is_err(), "corrupted tail must fail bytecheck");
    }

    #[test]
    fn signing_message_stable_layout() {
        let claim = sample_claim();
        let msg = claim.signing_message();
        // [16] user_id + [8] epoch + [32] node_id + [1] discriminant
        // + [16] UserSession.user_id + [4] role_len + [4] "user" bytes
        assert_eq!(msg.len(), 16 + 8 + 32 + 1 + 16 + 4 + 4);
        assert_eq!(&msg[0..16], &[7u8; 16]);
        assert_eq!(&msg[16..24], &1_700_000_000u64.to_le_bytes(), "epoch LE");
        assert_eq!(&msg[24..56], &[9u8; 32]);
        assert_eq!(msg[56], 2, "UserSession discriminant");
        assert_eq!(&msg[57..73], &[7u8; 16], "user_id");
        assert_eq!(&msg[73..77], &4u32.to_le_bytes(), "role len");
        assert_eq!(&msg[77..81], b"user", "role bytes");
    }

    #[test]
    fn signing_message_differs_across_session_types() {
        let mut c1 = sample_claim();
        c1.originating_session_type = SessionAuth::Anonymous;
        let m1 = c1.signing_message();

        let mut c2 = sample_claim();
        c2.originating_session_type = SessionAuth::ApiKey {
            key_id: "k1".into(),
        };
        let m2 = c2.signing_message();

        let c3 = sample_claim();
        let m3 = c3.signing_message();

        let mut c4 = sample_claim();
        c4.originating_session_type = SessionAuth::MeshTrust {
            node_id: [1u8; 32],
            epoch: 42,
        };
        let m4 = c4.signing_message();

        assert_ne!(m1, m2);
        assert_ne!(m1, m3);
        assert_ne!(m2, m3);
        assert_ne!(m3, m4);
    }

    #[test]
    fn signing_message_sensitive_to_epoch() {
        let mut c1 = sample_claim();
        let m1 = c1.signing_message();
        c1.issued_at_epoch += 1;
        let m2 = c1.signing_message();
        assert_ne!(m1, m2, "epoch bump must change signing bytes");
    }

    #[test]
    fn session_auth_round_trip_all_variants() {
        for auth in [
            SessionAuth::Anonymous,
            SessionAuth::ApiKey {
                key_id: "abc123".to_string(),
            },
            SessionAuth::UserSession {
                user_id: [3u8; 16],
                role: Some("admin".to_string()),
            },
            SessionAuth::UserSession {
                user_id: [4u8; 16],
                role: None,
            },
            SessionAuth::MeshTrust {
                node_id: [5u8; 32],
                epoch: 7,
            },
        ] {
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&auth).expect("encode");
            let decoded: SessionAuth =
                rkyv::from_bytes::<SessionAuth, rkyv::rancor::Error>(&bytes).expect("decode");
            assert_eq!(decoded, auth);
        }
    }
}
