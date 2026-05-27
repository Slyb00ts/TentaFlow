// =============================================================================
// File: protocol/frame/auth.rs — Auth envelope sub-field (UFP/2 §6)
// Purpose: identity + epoch + Ed25519 signature carrier. Field 0 (kind) is
// mandatory; remaining sub-fields are conditionally present per §11.3,
// driven by `kind` + the envelope-level `IS_SIGNED` flag. This file declares
// the data carrier only; presence invariants are enforced in 4c1g.
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

use super::envelope::{NODE_ID_LEN, SIGNATURE_LEN};

/// Authentication mechanism class.
///
/// - Anonymous: bootstrap only (§11.3: Control / Hello at handshake).
/// - Session: TLS-authenticated session binding, never carries Ed25519 sig.
/// - ApiKey: edge gateway HMAC validation OUTSIDE UFP/2, read/inference only.
/// - NodeIdentity: Ed25519-signed by a node's signing key.
/// - UserIdentity: Ed25519-signed by a user's signing key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AuthKind {
    Anonymous = 0x00,
    Session = 0x01,
    ApiKey = 0x02,
    NodeIdentity = 0x03,
    UserIdentity = 0x04,
}

impl AuthKind {
    pub fn from_u8(b: u8) -> Option<Self> {
        match b {
            0x00 => Some(Self::Anonymous),
            0x01 => Some(Self::Session),
            0x02 => Some(Self::ApiKey),
            0x03 => Some(Self::NodeIdentity),
            0x04 => Some(Self::UserIdentity),
            _ => None,
        }
    }
}

impl<C> Encode<C> for AuthKind {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.u8(*self as u8)?;
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for AuthKind {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let raw = d.u8()?;
        Self::from_u8(raw)
            .ok_or_else(|| minicbor::decode::Error::message("AuthKind: unknown discriminant"))
    }
}

/// Sender authentication record carried in envelope field 13.
///
/// Wire schema (§6 + §11.3):
/// ```text
/// Auth = {
///   0: kind         u8         ; MANDATORY
///   1: subject_id   bstr(32)   ; OPTIONAL; Ed25519 pubkey when kind ∈ {Session,Node,User}
///   2: epoch        u32        ; OPTIONAL; policy_epoch when kind ∈ {Node,User}
///   3: signature    bstr(64)   ; OPTIONAL; Ed25519 sig iff IS_SIGNED=1
///   4: session_id   bstr(16)   ; OPTIONAL; when kind ∈ {Session,ApiKey}
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Auth {
    #[n(0)]
    pub kind: AuthKind,
    #[cbor(n(1), with = "minicbor::bytes")]
    pub subject_id: Option<[u8; NODE_ID_LEN]>,
    #[n(2)]
    pub epoch: Option<u32>,
    #[cbor(n(3), with = "minicbor::bytes")]
    pub signature: Option<[u8; SIGNATURE_LEN]>,
    #[cbor(n(4), with = "minicbor::bytes")]
    pub session_id: Option<[u8; 16]>,
}

impl Auth {
    /// Minimal Anonymous auth — only `kind` populated. Use for Control / Hello.
    pub fn anonymous() -> Self {
        Self {
            kind: AuthKind::Anonymous,
            subject_id: None,
            epoch: None,
            signature: None,
            session_id: None,
        }
    }

    /// Auth carrying a node identity + epoch but no signature yet. The
    /// signature is computed in 4c1b after canonical envelope encoding.
    pub fn node_unsigned(node_pubkey: [u8; NODE_ID_LEN], epoch: u32) -> Self {
        Self {
            kind: AuthKind::NodeIdentity,
            subject_id: Some(node_pubkey),
            epoch: Some(epoch),
            signature: None,
            session_id: None,
        }
    }

    /// Auth carrying a user identity + epoch but no signature yet.
    pub fn user_unsigned(user_pubkey: [u8; NODE_ID_LEN], epoch: u32) -> Self {
        Self {
            kind: AuthKind::UserIdentity,
            subject_id: Some(user_pubkey),
            epoch: Some(epoch),
            signature: None,
            session_id: None,
        }
    }

    /// Auth carrying a session_id binding. `subject_id` is the user/node
    /// behind the session; `IS_SIGNED` MUST be 0 (§11.3).
    pub fn session(subject_id: [u8; NODE_ID_LEN], session_id: [u8; 16]) -> Self {
        Self {
            kind: AuthKind::Session,
            subject_id: Some(subject_id),
            epoch: None,
            signature: None,
            session_id: Some(session_id),
        }
    }

    /// Auth carrying an ApiKey identifier (read/inference only). HMAC is
    /// validated at the edge gateway OUTSIDE UFP/2.
    pub fn api_key(api_key_id: [u8; 16]) -> Self {
        Self {
            kind: AuthKind::ApiKey,
            subject_id: None,
            epoch: None,
            signature: None,
            session_id: Some(api_key_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(v: Auth) {
        let mut b1 = Vec::new();
        minicbor::encode(&v, &mut b1).unwrap();
        let d: Auth = minicbor::decode(&b1).unwrap();
        assert_eq!(d, v);
        let mut b2 = Vec::new();
        minicbor::encode(&d, &mut b2).unwrap();
        assert_eq!(b1, b2);
    }

    #[test]
    fn auth_anonymous_roundtrip() {
        rt(Auth::anonymous());
    }

    #[test]
    fn auth_node_unsigned_roundtrip() {
        let key = [0x11u8; NODE_ID_LEN];
        rt(Auth::node_unsigned(key, 42));
    }

    #[test]
    fn auth_user_unsigned_roundtrip() {
        let key = [0x22u8; NODE_ID_LEN];
        rt(Auth::user_unsigned(key, 1));
    }

    #[test]
    fn auth_session_roundtrip() {
        let subj = [0x33u8; NODE_ID_LEN];
        let sid = [0xABu8; 16];
        rt(Auth::session(subj, sid));
    }

    #[test]
    fn auth_api_key_roundtrip() {
        let kid = [0x77u8; 16];
        rt(Auth::api_key(kid));
    }

    #[test]
    fn auth_with_signature_roundtrip() {
        let mut a = Auth::node_unsigned([0xCCu8; NODE_ID_LEN], 5);
        a.signature = Some([0x99u8; SIGNATURE_LEN]);
        rt(a);
    }

    #[test]
    fn auth_kind_rejects_unknown_discriminant() {
        let bad = [0xFFu8];
        let r: Result<AuthKind, _> = minicbor::decode(&bad);
        assert!(r.is_err());
    }
}
