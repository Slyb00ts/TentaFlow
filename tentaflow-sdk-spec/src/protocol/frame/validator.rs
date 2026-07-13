// =============================================================================
// File: protocol/frame/validator.rs — UFP/2 structural validator (§11.3)
// Purpose: enforce envelope-level invariants before the receive pipeline
// runs any cryptography. The validator MUST be called BEFORE
// `verify_envelope`, `decrypt_envelope_body`, `ReplayGuard::try_observe`,
// or any other crypto / dispatch step, on every envelope arriving from
// the network.
//
// Invariants enforced (§3.4, §6, §11.3):
//   - `flags` carries no bits outside the allocated 0–6 range.
//   - Fragment flag/field consistency: IS_FRAGMENT ⇔ fragment_index +
//     fragment_count present; IS_LAST_FRAGMENT requires IS_FRAGMENT and
//     `fragment_index == fragment_count - 1`; fragment_count > 0; index <
//     count.
//   - `channel` ∈ allocated set + `kind` ∈ channel-specific valid range.
//   - `auth.kind` mandatory; per-kind sub-field presence (subject_id,
//     epoch, signature, session_id).
//   - IS_SIGNED ⇔ auth.signature present (length 64 enforced by type).
//   - IS_ENCRYPTED ⇒ auth.kind ∈ {Session, NodeIdentity, UserIdentity}
//     (no Anonymous/ApiKey under encryption — no shared key partner).
//   - source.id == auth.subject_id when auth.kind ∈ {NodeIdentity,
//     UserIdentity} (binds the signed identity to the source field —
//     addresses the binding gap flagged in 4c1b review).
//   - Anonymous auth only on channel = Control (0x05).
//   - ApiKey auth only on channel = Frontend (0x07).
//   - Per-channel auth.kind whitelist (§11.3 table).
//   - Per-channel IS_SIGNED requirement (Always / Never / PerKind).
//   - `Session + IS_SIGNED=1` is forbidden in UFP/2 v2.
//   - Reserved fragment fields absent when IS_FRAGMENT=0.
//
// What this validator does NOT enforce (deferred or out of scope):
//   - Cryptographic verification (Ed25519 sig validity, AEAD tag) — that
//     is `sign::verify_envelope` and `aead::decrypt_envelope_body`.
//   - Replay window — `replay::ReplayGuard`.
//   - Per-kind IS_SIGNED requirements where the channel is PerKind — the
//     application policy table makes that call.
//   - Explicit CBOR null rejection at the wire layer — requires a custom
//     decoder pass; the spec's strict canonical validator (§3.1) rejects
//     null in map-key positions but not map values; production receivers
//     SHOULD wrap decode with an explicit-null check before this validator.
//
// Spec ref: docs/UNIFIED_FRAME_PROTOCOL_v2.md §3.4 + §6 + §11.3.
// =============================================================================

use super::address::{NodeAddress, NodeAddressKind};
use super::auth::AuthKind;
use super::channel::{channels, Channel};
use super::envelope::{Envelope, FRAME_PROTOCOL_VERSION};
use super::error::{FrameError, FrameErrorCode};
use super::flags::Flags;

/// IS_SIGNED requirement per channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignRequirement {
    /// Every envelope on this channel MUST have IS_SIGNED=1.
    Always,
    /// Every envelope on this channel MUST have IS_SIGNED=0.
    Never,
    /// IS_SIGNED is determined per-kind by application policy; the
    /// envelope-level validator does not enforce it.
    PerKind,
}

/// Static per-channel auth policy (§11.3 table).
pub struct ChannelAuthPolicy {
    pub allowed_auth_kinds: &'static [AuthKind],
    pub sign_requirement: SignRequirement,
}

/// Lookup the auth policy for a channel. Returns `None` for unallocated
/// channels (validator rejects those with `UnknownChannel`).
pub fn channel_auth_policy(channel: Channel) -> Option<ChannelAuthPolicy> {
    match channel.0 {
        0x01 => Some(ChannelAuthPolicy {
            allowed_auth_kinds: &[AuthKind::Session, AuthKind::UserIdentity],
            sign_requirement: SignRequirement::PerKind,
        }),
        0x02 => Some(ChannelAuthPolicy {
            allowed_auth_kinds: &[AuthKind::NodeIdentity, AuthKind::UserIdentity],
            sign_requirement: SignRequirement::Always,
        }),
        0x03 => Some(ChannelAuthPolicy {
            allowed_auth_kinds: &[
                AuthKind::Session,
                AuthKind::NodeIdentity,
                AuthKind::UserIdentity,
            ],
            sign_requirement: SignRequirement::PerKind,
        }),
        0x04 => Some(ChannelAuthPolicy {
            allowed_auth_kinds: &[AuthKind::NodeIdentity],
            sign_requirement: SignRequirement::Always,
        }),
        0x05 => Some(ChannelAuthPolicy {
            allowed_auth_kinds: &[
                AuthKind::Anonymous,
                AuthKind::NodeIdentity,
                AuthKind::UserIdentity,
            ],
            sign_requirement: SignRequirement::PerKind,
        }),
        0x06 => Some(ChannelAuthPolicy {
            allowed_auth_kinds: &[AuthKind::NodeIdentity],
            sign_requirement: SignRequirement::Always,
        }),
        0x07 => Some(ChannelAuthPolicy {
            allowed_auth_kinds: &[AuthKind::Session, AuthKind::ApiKey],
            sign_requirement: SignRequirement::PerKind,
        }),
        0x08 => Some(ChannelAuthPolicy {
            // ApiKey allowed here for read/inference kinds (e.g. ModelList,
            // ServiceList) per §11.3 prose. Per-kind enforcement (which kinds
            // count as "read/inference") is application policy — this
            // validator only enforces the channel-level allowlist.
            allowed_auth_kinds: &[
                AuthKind::Session,
                AuthKind::ApiKey,
                AuthKind::NodeIdentity,
                AuthKind::UserIdentity,
            ],
            sign_requirement: SignRequirement::PerKind,
        }),
        0x09 => Some(ChannelAuthPolicy {
            allowed_auth_kinds: &[AuthKind::NodeIdentity],
            sign_requirement: SignRequirement::Always,
        }),
        _ => None,
    }
}

/// Top-level structural validator. Calls every sub-check in turn and
/// returns the first failure with a precise `FrameError`.
pub fn validate_envelope(envelope: &Envelope) -> Result<(), FrameError> {
    validate_protocol_version(envelope)?;
    validate_flag_bits(envelope)?;
    validate_flag_combinations(envelope)?;
    validate_channel_kind(envelope)?;
    validate_address_invariants(envelope)?;
    validate_broadcast_invariants(envelope)?;
    validate_auth_invariants(envelope)?;
    validate_anonymous_kind_restriction(envelope)?;
    validate_source_subject_binding(envelope)?;
    validate_channel_auth_policy(envelope)?;
    Ok(())
}

fn validate_protocol_version(envelope: &Envelope) -> Result<(), FrameError> {
    if envelope.protocol_version.0 != FRAME_PROTOCOL_VERSION {
        return Err(FrameError::new(
            FrameErrorCode::UnknownProtocolVersion,
            format!(
                "validate_protocol_version: got {}, UFP/2 requires {}",
                envelope.protocol_version.0, FRAME_PROTOCOL_VERSION
            ),
        )
        .with_path("envelope.protocol_version"));
    }
    Ok(())
}

fn validate_address_invariants(envelope: &Envelope) -> Result<(), FrameError> {
    check_address(&envelope.source, "envelope.source")?;
    check_address(&envelope.destination, "envelope.destination")?;
    if let Some(hops) = &envelope.forwarded_via {
        // §3.2: forwarded_via is present iff at least one hop has forwarded
        // the envelope. An empty vec is structurally invalid — represent
        // "no hops" by omitting field 16 entirely.
        if hops.is_empty() {
            return Err(FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                "validate_address_invariants: forwarded_via present but empty (§3.2 — omit the field instead)",
            )
            .with_path("envelope.forwarded_via"));
        }
        for (i, hop) in hops.iter().enumerate() {
            check_address(hop, &format!("envelope.forwarded_via[{}]", i))?;
        }
    }
    Ok(())
}

fn check_address(addr: &NodeAddress, path: &str) -> Result<(), FrameError> {
    let is_zero = addr.id_is_zero();
    match addr.kind {
        NodeAddressKind::Anonymous | NodeAddressKind::Broadcast => {
            if !is_zero {
                return Err(FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    format!(
                        "check_address: NodeAddressKind={:?} requires all-zero id (§3.3)",
                        addr.kind
                    ),
                )
                .with_path(format!("{}.id", path)));
            }
        }
        NodeAddressKind::Node
        | NodeAddressKind::User
        | NodeAddressKind::Service
        | NodeAddressKind::Addon => {
            if is_zero {
                return Err(FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    format!(
                        "check_address: NodeAddressKind={:?} MUST NOT use the all-zero sentinel id (§3.3)",
                        addr.kind
                    ),
                )
                .with_path(format!("{}.id", path)));
            }
        }
    }
    Ok(())
}

fn validate_broadcast_invariants(envelope: &Envelope) -> Result<(), FrameError> {
    let is_broadcast_flag = envelope.flags.contains(Flags::IS_BROADCAST);
    let dest_is_broadcast = envelope.destination.kind == NodeAddressKind::Broadcast;
    if is_broadcast_flag != dest_is_broadcast {
        return Err(FrameError::new(
            FrameErrorCode::BodyValidationFailed,
            "validate_broadcast_invariants: IS_BROADCAST flag and destination.kind=Broadcast MUST agree (§3.4)",
        )
        .with_path("envelope.flags"));
    }
    if is_broadcast_flag {
        // §5.3: only Mesh and SyncLedger channels permit broadcast.
        match envelope.channel.0 {
            0x04 | 0x06 => {}
            _ => {
                return Err(FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    format!(
                        "validate_broadcast_invariants: channel 0x{:02X} does not permit broadcast (only Mesh/SyncLedger per §5.3)",
                        envelope.channel.0
                    ),
                )
                .with_path("envelope.channel"));
            }
        }
    }
    Ok(())
}

fn validate_anonymous_kind_restriction(envelope: &Envelope) -> Result<(), FrameError> {
    if envelope.auth.kind != AuthKind::Anonymous {
        return Ok(());
    }
    // Anonymous is permitted ONLY on (channel=Control, kind=Hello) per §11.3.
    if envelope.channel != channels::CONTROL || envelope.kind != channels::KIND_CONTROL_HELLO {
        return Err(FrameError::new(
            FrameErrorCode::PermissionDenied,
            format!(
                "validate_anonymous_kind_restriction: Anonymous auth only permitted on (channel=0x05 Control, kind=0x{:04X} Hello); got (channel=0x{:02X}, kind=0x{:04X})",
                channels::KIND_CONTROL_HELLO.0,
                envelope.channel.0,
                envelope.kind.0
            ),
        )
        .with_path("envelope.auth.kind"));
    }
    Ok(())
}

fn validate_flag_bits(envelope: &Envelope) -> Result<(), FrameError> {
    if !envelope.flags.reserved_bits_clear() {
        return Err(FrameError::new(
            FrameErrorCode::BodyValidationFailed,
            format!(
                "validate_flag_bits: flags value 0x{:08X} contains reserved bits (must be in 0x00..=0x7F)",
                envelope.flags.0
            ),
        )
        .with_path("envelope.flags"));
    }
    Ok(())
}

fn validate_flag_combinations(envelope: &Envelope) -> Result<(), FrameError> {
    let f = envelope.flags;
    let is_frag = f.contains(Flags::IS_FRAGMENT);
    let is_last = f.contains(Flags::IS_LAST_FRAGMENT);
    let has_index = envelope.fragment_index.is_some();
    let has_count = envelope.fragment_count.is_some();

    if is_last && !is_frag {
        return Err(FrameError::new(
            FrameErrorCode::BodyValidationFailed,
            "validate_flag_combinations: IS_LAST_FRAGMENT set without IS_FRAGMENT",
        )
        .with_path("envelope.flags"));
    }
    if is_frag && (!has_index || !has_count) {
        return Err(FrameError::new(
            FrameErrorCode::BodyValidationFailed,
            "validate_flag_combinations: IS_FRAGMENT set but fragment_index/fragment_count missing",
        )
        .with_path("envelope.fragment_index"));
    }
    if !is_frag && (has_index || has_count) {
        return Err(FrameError::new(
            FrameErrorCode::BodyValidationFailed,
            "validate_flag_combinations: fragment_index/fragment_count present without IS_FRAGMENT",
        )
        .with_path("envelope.fragment_index"));
    }
    if is_frag {
        let idx = envelope.fragment_index.unwrap();
        let count = envelope.fragment_count.unwrap();
        if count == 0 {
            return Err(FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                "validate_flag_combinations: fragment_count MUST be > 0",
            )
            .with_path("envelope.fragment_count"));
        }
        if idx >= count {
            return Err(FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                format!(
                    "validate_flag_combinations: fragment_index {} not less than fragment_count {}",
                    idx, count
                ),
            )
            .with_path("envelope.fragment_index"));
        }
        if is_last && idx != count - 1 {
            return Err(FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                "validate_flag_combinations: IS_LAST_FRAGMENT set with fragment_index != fragment_count - 1",
            )
            .with_path("envelope.flags"));
        }
        if !is_last && idx == count - 1 {
            return Err(FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                "validate_flag_combinations: last fragment_index without IS_LAST_FRAGMENT",
            )
            .with_path("envelope.flags"));
        }
    }

    Ok(())
}

fn validate_channel_kind(envelope: &Envelope) -> Result<(), FrameError> {
    let range = channels::valid_kind_range(envelope.channel).ok_or_else(|| {
        FrameError::new(
            FrameErrorCode::UnknownChannel,
            format!(
                "validate_channel_kind: channel 0x{:02X} is not allocated",
                envelope.channel.0
            ),
        )
        .with_path("envelope.channel")
    })?;
    if !range.contains(&envelope.kind.0) {
        return Err(FrameError::new(
            FrameErrorCode::UnknownKind,
            format!(
                "validate_channel_kind: kind 0x{:04X} not in channel 0x{:02X} range 0x{:04X}..=0x{:04X}",
                envelope.kind.0,
                envelope.channel.0,
                range.start(),
                range.end()
            ),
        )
        .with_path("envelope.kind"));
    }
    Ok(())
}

fn validate_auth_invariants(envelope: &Envelope) -> Result<(), FrameError> {
    let auth = &envelope.auth;
    let is_signed = envelope.flags.contains(Flags::IS_SIGNED);
    let is_encrypted = envelope.flags.contains(Flags::IS_ENCRYPTED);

    match auth.kind {
        AuthKind::Anonymous => {
            if is_signed {
                return Err(FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    "validate_auth_invariants: IS_SIGNED set with auth.kind=Anonymous",
                )
                .with_path("envelope.auth.kind"));
            }
            if auth.signature.is_some()
                || auth.subject_id.is_some()
                || auth.epoch.is_some()
                || auth.session_id.is_some()
            {
                return Err(FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    "validate_auth_invariants: Anonymous auth carries forbidden sub-fields",
                )
                .with_path("envelope.auth"));
            }
        }
        AuthKind::Session => {
            if is_signed {
                return Err(FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    "validate_auth_invariants: Session + IS_SIGNED=1 is forbidden in UFP/2 v2 (§11.3)",
                )
                .with_path("envelope.flags"));
            }
            if auth.subject_id.is_none() || auth.session_id.is_none() {
                return Err(FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    "validate_auth_invariants: Session auth requires subject_id and session_id",
                )
                .with_path("envelope.auth"));
            }
            if auth.signature.is_some() || auth.epoch.is_some() {
                return Err(FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    "validate_auth_invariants: Session auth MUST NOT carry signature or epoch",
                )
                .with_path("envelope.auth"));
            }
        }
        AuthKind::ApiKey => {
            if is_signed {
                return Err(FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    "validate_auth_invariants: ApiKey + IS_SIGNED=1 is forbidden (HMAC happens at edge gateway outside UFP/2 per §11.3)",
                )
                .with_path("envelope.flags"));
            }
            if auth.session_id.is_none() {
                return Err(FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    "validate_auth_invariants: ApiKey auth requires session_id (carries api_key_id)",
                )
                .with_path("envelope.auth.session_id"));
            }
            if auth.subject_id.is_some() || auth.signature.is_some() || auth.epoch.is_some() {
                return Err(FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    "validate_auth_invariants: ApiKey auth MUST NOT carry subject_id, signature, or epoch",
                )
                .with_path("envelope.auth"));
            }
        }
        AuthKind::NodeIdentity | AuthKind::UserIdentity => {
            if !is_signed {
                return Err(FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    "validate_auth_invariants: NodeIdentity/UserIdentity auth requires IS_SIGNED=1",
                )
                .with_path("envelope.flags"));
            }
            if auth.subject_id.is_none() {
                return Err(FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    "validate_auth_invariants: NodeIdentity/UserIdentity auth requires subject_id",
                )
                .with_path("envelope.auth.subject_id"));
            }
            if auth.signature.is_none() {
                return Err(FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    "validate_auth_invariants: NodeIdentity/UserIdentity auth with IS_SIGNED=1 requires signature",
                )
                .with_path("envelope.auth.signature"));
            }
            if auth.epoch.is_none() {
                return Err(FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    "validate_auth_invariants: NodeIdentity/UserIdentity auth requires epoch",
                )
                .with_path("envelope.auth.epoch"));
            }
            if auth.session_id.is_some() {
                return Err(FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    "validate_auth_invariants: NodeIdentity/UserIdentity auth MUST NOT carry session_id",
                )
                .with_path("envelope.auth.session_id"));
            }
        }
    }

    if is_signed && auth.signature.is_none() {
        return Err(FrameError::new(
            FrameErrorCode::BodyValidationFailed,
            "validate_auth_invariants: IS_SIGNED=1 without auth.signature",
        )
        .with_path("envelope.auth.signature"));
    }
    if !is_signed && auth.signature.is_some() {
        return Err(FrameError::new(
            FrameErrorCode::BodyValidationFailed,
            "validate_auth_invariants: auth.signature present with IS_SIGNED=0",
        )
        .with_path("envelope.auth.signature"));
    }

    if is_encrypted
        && !matches!(
            auth.kind,
            AuthKind::Session | AuthKind::NodeIdentity | AuthKind::UserIdentity
        )
    {
        return Err(FrameError::new(
            FrameErrorCode::BodyValidationFailed,
            "validate_auth_invariants: IS_ENCRYPTED requires auth.kind ∈ {Session, NodeIdentity, UserIdentity} (need authenticated key-exchange partner)",
        )
        .with_path("envelope.flags"));
    }

    Ok(())
}

fn validate_source_subject_binding(envelope: &Envelope) -> Result<(), FrameError> {
    match envelope.auth.kind {
        AuthKind::NodeIdentity | AuthKind::UserIdentity => {
            let expected_subject = envelope.auth.subject_id.ok_or_else(|| {
                FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    "validate_source_subject_binding: missing auth.subject_id",
                )
            })?;
            if envelope.source.id != expected_subject {
                return Err(FrameError::new(
                    FrameErrorCode::PermissionDenied,
                    "validate_source_subject_binding: source.id != auth.subject_id (§11.3 binding rule — attacker may be signing on behalf of another identity)",
                )
                .with_path("envelope.source.id"));
            }
            // Source address kind must match the auth kind for coherence:
            // NodeIdentity -> source.kind = Node; UserIdentity -> source.kind = User.
            let expected_addr_kind = match envelope.auth.kind {
                AuthKind::NodeIdentity => NodeAddressKind::Node,
                AuthKind::UserIdentity => NodeAddressKind::User,
                _ => unreachable!(),
            };
            if envelope.source.kind != expected_addr_kind {
                return Err(FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    format!(
                        "validate_source_subject_binding: source.kind {:?} does not match auth.kind {:?} (expected source.kind {:?})",
                        envelope.source.kind, envelope.auth.kind, expected_addr_kind
                    ),
                )
                .with_path("envelope.source.kind"));
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_channel_auth_policy(envelope: &Envelope) -> Result<(), FrameError> {
    let policy = channel_auth_policy(envelope.channel).ok_or_else(|| {
        FrameError::new(
            FrameErrorCode::UnknownChannel,
            format!(
                "validate_channel_auth_policy: no policy for channel 0x{:02X}",
                envelope.channel.0
            ),
        )
        .with_path("envelope.channel")
    })?;
    if !policy.allowed_auth_kinds.contains(&envelope.auth.kind) {
        return Err(FrameError::new(
            FrameErrorCode::PermissionDenied,
            format!(
                "validate_channel_auth_policy: auth.kind {:?} not allowed on channel 0x{:02X} (allowed: {:?})",
                envelope.auth.kind, envelope.channel.0, policy.allowed_auth_kinds
            ),
        )
        .with_path("envelope.auth.kind"));
    }
    let is_signed = envelope.flags.contains(Flags::IS_SIGNED);
    match policy.sign_requirement {
        SignRequirement::Always => {
            if !is_signed {
                return Err(FrameError::new(
                    FrameErrorCode::InvalidSignature,
                    format!(
                        "validate_channel_auth_policy: channel 0x{:02X} REQUIRES IS_SIGNED=1",
                        envelope.channel.0
                    ),
                )
                .with_path("envelope.flags"));
            }
        }
        SignRequirement::Never => {
            if is_signed {
                return Err(FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    format!(
                        "validate_channel_auth_policy: channel 0x{:02X} FORBIDS IS_SIGNED=1",
                        envelope.channel.0
                    ),
                )
                .with_path("envelope.flags"));
            }
        }
        SignRequirement::PerKind => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::frame::address::NodeAddress;
    use crate::protocol::frame::auth::{Auth, AuthKind};
    use crate::protocol::frame::channel::{channels, Kind};
    use crate::protocol::frame::envelope::{
        MessageId, Priority, MESSAGE_ID_LEN, NODE_ID_LEN, SIGNATURE_LEN,
    };

    fn base_envelope(channel: Channel, kind: Kind) -> Envelope {
        let mut mid = [0u8; MESSAGE_ID_LEN];
        mid[0] = 1;
        Envelope::minimal(
            NodeAddress::node([0x11u8; NODE_ID_LEN]),
            NodeAddress::node([0x22u8; NODE_ID_LEN]),
            channel,
            kind,
            Priority::Normal,
            Flags::NONE,
            MessageId(mid),
            1_700_000_000_000,
        )
    }

    fn mesh_envelope_signed_valid() -> Envelope {
        let pubkey = [0x11u8; NODE_ID_LEN];
        let mut env = base_envelope(channels::MESH, Kind(0x0010));
        env.source = NodeAddress::node(pubkey);
        env.flags = env.flags.with(Flags::IS_SIGNED);
        env.auth = Auth {
            kind: AuthKind::NodeIdentity,
            subject_id: Some(pubkey),
            epoch: Some(0),
            signature: Some([0u8; SIGNATURE_LEN]),
            session_id: None,
        };
        env
    }

    #[test]
    fn validate_passes_for_valid_mesh_envelope() {
        let env = mesh_envelope_signed_valid();
        validate_envelope(&env).unwrap();
    }

    #[test]
    fn rejects_reserved_flag_bits() {
        let mut env = mesh_envelope_signed_valid();
        env.flags = Flags(env.flags.0 | 0x0080); // bit 7 reserved
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn rejects_last_fragment_without_fragment_flag() {
        let mut env = mesh_envelope_signed_valid();
        env.flags = env.flags.with(Flags::IS_LAST_FRAGMENT);
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn rejects_fragment_flag_without_index_count() {
        let mut env = mesh_envelope_signed_valid();
        env.flags = env.flags.with(Flags::IS_FRAGMENT);
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn rejects_fragment_index_without_fragment_flag() {
        let mut env = mesh_envelope_signed_valid();
        env.fragment_index = Some(0);
        env.fragment_count = Some(2);
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn rejects_fragment_count_zero() {
        let mut env = mesh_envelope_signed_valid();
        env.flags = env.flags.with(Flags::IS_FRAGMENT);
        env.fragment_index = Some(0);
        env.fragment_count = Some(0);
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn rejects_fragment_index_out_of_range() {
        let mut env = mesh_envelope_signed_valid();
        env.flags = env.flags.with(Flags::IS_FRAGMENT);
        env.fragment_index = Some(5);
        env.fragment_count = Some(3);
        let r = validate_envelope(&env);
        assert!(r.is_err());
    }

    #[test]
    fn rejects_unknown_channel() {
        let mut env = mesh_envelope_signed_valid();
        env.channel = Channel(0xAB);
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::UnknownChannel);
    }

    #[test]
    fn rejects_kind_outside_channel_range() {
        let mut env = mesh_envelope_signed_valid();
        env.kind = Kind(0xFFFF);
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::UnknownKind);
    }

    #[test]
    fn rejects_anonymous_with_is_signed() {
        let mut env = base_envelope(channels::CONTROL, Kind(0x0001));
        env.flags = env.flags.with(Flags::IS_SIGNED);
        env.auth = Auth::anonymous();
        env.auth.signature = Some([0u8; SIGNATURE_LEN]);
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn rejects_session_with_is_signed() {
        let mut env = base_envelope(channels::UI, Kind(0x0001));
        env.flags = env.flags.with(Flags::IS_SIGNED);
        env.auth = Auth::session([0x11u8; NODE_ID_LEN], [0xAAu8; 16]);
        env.auth.signature = Some([0u8; SIGNATURE_LEN]);
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn rejects_apikey_with_is_signed() {
        let mut env = base_envelope(channels::FRONTEND, Kind(0x0001));
        env.flags = env.flags.with(Flags::IS_SIGNED);
        env.auth = Auth::api_key([0x77u8; 16]);
        env.auth.signature = Some([0u8; SIGNATURE_LEN]);
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn rejects_node_identity_without_subject_id() {
        let mut env = mesh_envelope_signed_valid();
        env.auth.subject_id = None;
        let r = validate_envelope(&env);
        assert!(r.is_err());
    }

    #[test]
    fn rejects_node_identity_without_signature() {
        let mut env = mesh_envelope_signed_valid();
        env.auth.signature = None;
        let r = validate_envelope(&env);
        assert!(r.is_err());
    }

    #[test]
    fn rejects_node_identity_without_epoch() {
        let mut env = mesh_envelope_signed_valid();
        env.auth.epoch = None;
        let r = validate_envelope(&env);
        assert!(r.is_err());
    }

    #[test]
    fn rejects_signature_without_is_signed_flag() {
        let mut env = base_envelope(channels::MESH, Kind(0x0010));
        env.auth = Auth::node_unsigned([0x11u8; NODE_ID_LEN], 0);
        env.auth.signature = Some([0u8; SIGNATURE_LEN]);
        // IS_SIGNED not set
        let r = validate_envelope(&env);
        assert!(r.is_err());
    }

    #[test]
    fn rejects_source_subject_mismatch() {
        let mut env = mesh_envelope_signed_valid();
        env.source = NodeAddress::node([0xFFu8; NODE_ID_LEN]);
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::PermissionDenied);
    }

    #[test]
    fn rejects_user_identity_in_node_address() {
        // auth.kind = UserIdentity but source.kind = Node
        let pubkey = [0x33u8; NODE_ID_LEN];
        let mut env = base_envelope(channels::DOMAIN, Kind(0x0001));
        env.source = NodeAddress::node(pubkey);
        env.flags = env.flags.with(Flags::IS_SIGNED);
        env.auth = Auth {
            kind: AuthKind::UserIdentity,
            subject_id: Some(pubkey),
            epoch: Some(0),
            signature: Some([0u8; SIGNATURE_LEN]),
            session_id: None,
        };
        let r = validate_envelope(&env);
        assert!(r.is_err());
    }

    #[test]
    fn rejects_anonymous_outside_control_channel() {
        let env = base_envelope(channels::UI, Kind(0x0001));
        // env.auth defaults to Anonymous; channel UI does not permit it.
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::PermissionDenied);
    }

    #[test]
    fn accepts_anonymous_on_control_channel() {
        let env = base_envelope(channels::CONTROL, channels::KIND_CONTROL_HELLO);
        validate_envelope(&env).unwrap();
    }

    #[test]
    fn rejects_apikey_on_mesh_channel() {
        let mut env = base_envelope(channels::MESH, Kind(0x0010));
        env.auth = Auth::api_key([0x77u8; 16]);
        let r = validate_envelope(&env);
        assert!(r.is_err());
    }

    #[test]
    fn accepts_apikey_on_domain_channel_for_read_kinds() {
        // §11.3 prose explicitly mentions Domain/ModelList and
        // Domain/ServiceList as ApiKey-reachable read/inference kinds.
        let mut env = base_envelope(channels::DOMAIN, Kind(0x0001));
        env.auth = Auth::api_key([0x77u8; 16]);
        validate_envelope(&env).unwrap();
    }

    #[test]
    fn rejects_unsigned_mesh_envelope() {
        let mut env = base_envelope(channels::MESH, Kind(0x0010));
        env.auth = Auth::node_unsigned([0x11u8; NODE_ID_LEN], 0);
        // No IS_SIGNED flag — NodeIdentity auth + missing IS_SIGNED fails
        // validate_auth_invariants before the channel policy check runs.
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn channel_policy_rejects_anonymous_mesh_envelope() {
        // Mesh channel policy: Always requires signing AND auth.kind must be
        // NodeIdentity. An Anonymous envelope hits the policy check (since
        // auth invariants for Anonymous pass on their own) and is denied.
        let env = base_envelope(channels::MESH, Kind(0x0010));
        // env.auth is Anonymous; not in Mesh's allowed_auth_kinds.
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::PermissionDenied);
    }

    #[test]
    fn rejects_encrypted_with_anonymous_auth() {
        let mut env = base_envelope(channels::CONTROL, Kind(0x0001));
        env.flags = env.flags.with(Flags::IS_ENCRYPTED);
        // Anonymous auth — no key-exchange partner.
        let r = validate_envelope(&env);
        assert!(r.is_err());
    }

    #[test]
    fn rejects_encrypted_with_apikey_auth() {
        let mut env = base_envelope(channels::FRONTEND, Kind(0x0001));
        env.flags = env.flags.with(Flags::IS_ENCRYPTED);
        env.auth = Auth::api_key([0x77u8; 16]);
        let r = validate_envelope(&env);
        assert!(r.is_err());
    }

    #[test]
    fn channel_auth_policy_returns_none_for_unallocated() {
        assert!(channel_auth_policy(Channel(0x0A)).is_none());
        assert!(channel_auth_policy(Channel(0xFF)).is_none());
    }

    #[test]
    fn rejects_wrong_protocol_version() {
        let mut env = mesh_envelope_signed_valid();
        env.protocol_version = crate::protocol::frame::envelope::FrameProtocolVersion(1);
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::UnknownProtocolVersion);
    }

    #[test]
    fn rejects_node_address_with_zero_id() {
        let mut env = mesh_envelope_signed_valid();
        env.source = NodeAddress {
            kind: NodeAddressKind::Node,
            id: NodeAddress::ZERO_ID,
            name: None,
        };
        // subject_id also zero so binding doesn't trip first; the address check
        // catches the zero-id sentinel violation.
        env.auth.subject_id = Some(NodeAddress::ZERO_ID);
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn rejects_anonymous_address_with_non_zero_id() {
        let mut env = base_envelope(channels::CONTROL, channels::KIND_CONTROL_HELLO);
        env.source = NodeAddress {
            kind: NodeAddressKind::Anonymous,
            id: [0x42u8; NODE_ID_LEN],
            name: None,
        };
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn rejects_empty_forwarded_via_vec() {
        let mut env = mesh_envelope_signed_valid();
        env.forwarded_via = Some(Vec::new());
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn rejects_forwarded_via_hop_with_invalid_address() {
        let mut env = mesh_envelope_signed_valid();
        env.forwarded_via = Some(vec![NodeAddress {
            kind: NodeAddressKind::Node,
            id: NodeAddress::ZERO_ID,
            name: None,
        }]);
        let r = validate_envelope(&env);
        assert!(r.is_err());
    }

    #[test]
    fn rejects_is_broadcast_flag_with_node_destination() {
        let mut env = mesh_envelope_signed_valid();
        env.flags = env.flags.with(Flags::IS_BROADCAST);
        // destination still kind=Node — mismatch
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn rejects_broadcast_destination_without_flag() {
        let mut env = mesh_envelope_signed_valid();
        env.destination = NodeAddress::broadcast();
        // IS_BROADCAST flag not set
        let r = validate_envelope(&env);
        assert!(r.is_err());
    }

    #[test]
    fn accepts_broadcast_on_mesh_channel() {
        let mut env = mesh_envelope_signed_valid();
        env.flags = env.flags.with(Flags::IS_BROADCAST);
        env.destination = NodeAddress::broadcast();
        validate_envelope(&env).unwrap();
    }

    #[test]
    fn rejects_broadcast_on_non_mesh_channel() {
        let pubkey = [0x11u8; NODE_ID_LEN];
        let mut env = base_envelope(channels::FRONTEND, Kind(0x0001));
        env.source = NodeAddress::node(pubkey);
        env.flags = env.flags.with(Flags::IS_BROADCAST);
        env.destination = NodeAddress::broadcast();
        env.auth = Auth::session(pubkey, [0xAAu8; 16]);
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn rejects_anonymous_on_non_hello_control_kind() {
        // Anonymous on Control channel but with a non-Hello kind — rejected.
        let env = base_envelope(channels::CONTROL, Kind(0x0042));
        // env.auth defaults to Anonymous
        let r = validate_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::PermissionDenied);
    }

    #[test]
    fn accepts_anonymous_on_control_hello_kind() {
        let env = base_envelope(channels::CONTROL, channels::KIND_CONTROL_HELLO);
        validate_envelope(&env).unwrap();
    }

    #[test]
    fn channel_auth_policy_covers_all_allocated_channels() {
        for c in &[
            channels::UI,
            channels::HOST_FUNCTION,
            channels::STREAM,
            channels::MESH,
            channels::CONTROL,
            channels::SYNC_LEDGER,
            channels::FRONTEND,
            channels::DOMAIN,
            channels::FRAME_BLOB,
        ] {
            assert!(
                channel_auth_policy(*c).is_some(),
                "channel 0x{:02X} missing auth policy",
                c.0
            );
        }
    }
}
