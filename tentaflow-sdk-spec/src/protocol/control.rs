// =============================================================================
// File: protocol/control.rs — control-channel payloads (§5)
// Purpose: typed CBOR payloads for handshake (§5.1), lifecycle (§5.2) and
// flow control (§5.3). Maps use integer keys (canonical compact form);
// free-form `map<tstr, Value>` lives in `CborMap` with bytewise key sort.
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

use super::envelope::ProtocolVersion;
use super::ids::{DeviceId, Hash32, SessionId};
use super::value::Value;
use crate::protocol::ui::typed_field::assert_no_dup_tstr;

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

/// Free-form map of tstr → Value (e.g. Capability.params). Encoder sorts keys
/// by canonical CBOR encoding of the key (length-first, then bytewise) per §2.1.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CborMap(pub Vec<(String, Value)>);

fn encode_tstr_canonical(s: &str) -> Vec<u8> {
    let mut buf = Vec::with_capacity(s.len() + 9);
    let mut enc = minicbor::Encoder::new(&mut buf);
    enc.str(s).expect("encoding to Vec never fails");
    buf
}

impl<C> Encode<C> for CborMap {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical key order (§2.1 / RFC 8949 §4.2.1): bytewise-lex on the full
        // CBOR encoding of the key, including its length prefix. For tstr that
        // means shorter encodings sort before longer ones when their leading
        // header bytes differ, but full-byte comparison is used everywhere.
        let mut sorted: Vec<&(String, Value)> = self.0.iter().collect();
        sorted.sort_by(|a, b| {
            let ea = encode_tstr_canonical(&a.0);
            let eb = encode_tstr_canonical(&b.0);
            ea.cmp(&eb)
        });
        e.map(sorted.len() as u64)?;
        for (k, v) in sorted {
            e.str(k)?;
            v.encode(e, ctx)?;
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for CborMap {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut entries = Vec::with_capacity(len as usize);
        for _ in 0..len {
            let k = d.str()?.to_string();
            let v = Value::decode(d, ctx)?;
            entries.push((k, v));
        }
        Ok(CborMap(entries))
    }
}

// -----------------------------------------------------------------------------
// §5.1 Handshake — Capability, AuthContext, CreditBudget, ServerLimits, Resume*
// -----------------------------------------------------------------------------

/// Capability descriptor (§5.1).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct Capability {
    #[n(0)]
    pub name: String,
    #[n(1)]
    pub version: u32,
    #[n(2)]
    pub hash: Option<Hash32>,
    #[n(3)]
    pub params: Option<CborMap>,
}

/// Reason for capability rejection (§5.1 ProtocolWelcome.capabilities_rejected).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CapabilityRejection {
    #[n(0)]
    pub capability: String,
    #[n(1)]
    pub reason: String,
}

/// Authentication context shipped with ProtocolHello (§5.1).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct AuthContext {
    #[n(0)]
    pub bearer_token: Option<String>,
    #[n(1)]
    pub client_cert_fingerprint: Option<Vec<u8>>,
    #[n(2)]
    pub device_id: Option<DeviceId>,
    #[n(3)]
    pub origin: String,
}

/// Initial credit advertisement for flow control (§5.1 + §5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct CreditBudget {
    #[n(0)]
    pub ui: u32,
    #[n(1)]
    pub stream_per_open: u32,
}

impl Default for CreditBudget {
    fn default() -> Self {
        Self {
            ui: 256,
            stream_per_open: 32,
        }
    }
}

/// Resume context (§5.1 ProtocolHello.resume).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct Resume {
    #[n(0)]
    pub prior_session_id: SessionId,
    #[n(1)]
    pub last_received_msg_id: u64,
}

string_enum! {
    /// Resume tier mode (§5.1 ResumeStatus).
    pub enum ResumeMode {
        Replay = "replay",
        Snapshot = "snapshot",
    }
}

/// Resume status returned in ProtocolWelcome (§5.1).
#[derive(Debug, Clone, PartialEq)]
pub enum ResumeStatus {
    Fresh,
    Resumed { mode: ResumeMode, next_msg_id: u64 },
    Rejected { reason: String },
}

impl<C> Encode<C> for ResumeStatus {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Keys encoded as tstr; canonical order = bytewise(encoded(key)).
        // "kind" (0x64 ...) < "mode" (0x64 ...) < "reason" (0x66 ...) < "next_msg_id" (0x6b ...)
        match self {
            ResumeStatus::Fresh => {
                e.map(1)?;
                e.str("kind")?.str("fresh")?;
            }
            ResumeStatus::Resumed { mode, next_msg_id } => {
                e.map(3)?;
                e.str("kind")?.str("resumed")?;
                e.str("mode")?;
                mode.encode(e, _ctx)?;
                e.str("next_msg_id")?.u64(*next_msg_id)?;
            }
            ResumeStatus::Rejected { reason } => {
                e.map(2)?;
                e.str("kind")?.str("rejected")?;
                e.str("reason")?.str(reason)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for ResumeStatus {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut mode: Option<ResumeMode> = None;
        let mut next_msg_id: Option<u64> = None;
        let mut reason: Option<String> = None;
        for _ in 0..len {
            let key = d.str()?;
            match key {
                "kind" => {
                    assert_no_dup_tstr(&kind, "ResumeStatus", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "mode" => {
                    assert_no_dup_tstr(&mode, "ResumeStatus", "mode")?;
                    mode = Some(ResumeMode::decode(d, ctx)?);
                }
                "next_msg_id" => {
                    assert_no_dup_tstr(&next_msg_id, "ResumeStatus", "next_msg_id")?;
                    next_msg_id = Some(d.u64()?);
                }
                "reason" => {
                    assert_no_dup_tstr(&reason, "ResumeStatus", "reason")?;
                    reason = Some(d.str()?.to_string());
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown ResumeStatus key: {other}"
                    )));
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("ResumeStatus missing kind"))?;
        match kind.as_str() {
            "fresh" => {
                if mode.is_some() || next_msg_id.is_some() || reason.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "ResumeStatus.fresh must not carry mode/next_msg_id/reason",
                    ));
                }
                Ok(ResumeStatus::Fresh)
            }
            "resumed" => {
                if reason.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "ResumeStatus.resumed must not carry reason",
                    ));
                }
                Ok(ResumeStatus::Resumed {
                    mode: mode.ok_or_else(|| {
                        minicbor::decode::Error::message("ResumeStatus.resumed missing mode")
                    })?,
                    next_msg_id: next_msg_id.ok_or_else(|| {
                        minicbor::decode::Error::message("ResumeStatus.resumed missing next_msg_id")
                    })?,
                })
            }
            "rejected" => {
                if mode.is_some() || next_msg_id.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "ResumeStatus.rejected must not carry mode/next_msg_id",
                    ));
                }
                Ok(ResumeStatus::Rejected {
                    reason: reason.ok_or_else(|| {
                        minicbor::decode::Error::message("ResumeStatus.rejected missing reason")
                    })?,
                })
            }
            other => Err(minicbor::decode::Error::message(format!(
                "unknown ResumeStatus.kind: {other}"
            ))),
        }
    }
}

/// Server-enforced limits advertised in ProtocolWelcome (§5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct ServerLimits {
    #[n(0)]
    pub max_message_bytes: u32,
    #[n(1)]
    pub max_state_path_segments: u16,
    #[n(2)]
    pub max_components_per_fragment: u16,
    #[n(3)]
    pub max_component_depth: u16,
    #[n(4)]
    pub max_state_patch_ops: u16,
    #[n(5)]
    pub max_concurrent_streams: u16,
    #[n(6)]
    pub max_queue_per_channel: u32,
    #[n(7)]
    pub default_rate_limit_actions_per_sec: u16,
    #[n(8)]
    pub server_credit_budget: CreditBudget,
}

/// ProtocolHello (0x0501): first frame sent by the client.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct ProtocolHello {
    #[n(0)]
    pub protocol_version: ProtocolVersion,
    #[n(1)]
    pub client_version: String,
    #[n(2)]
    pub capabilities_requested: Vec<Capability>,
    #[n(3)]
    pub auth: AuthContext,
    #[n(4)]
    pub resume: Option<Resume>,
    #[n(5)]
    pub client_credit_budget: CreditBudget,
}

/// ProtocolWelcome (0x0502): server response on successful handshake.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct ProtocolWelcome {
    #[n(0)]
    pub protocol_version: ProtocolVersion,
    #[n(1)]
    pub server_version: String,
    #[n(2)]
    pub session_id: SessionId,
    #[n(3)]
    pub capabilities_granted: Vec<Capability>,
    #[n(4)]
    pub capabilities_rejected: Vec<CapabilityRejection>,
    #[n(5)]
    pub resume_status: ResumeStatus,
    #[n(6)]
    pub server_limits: ServerLimits,
    #[n(7)]
    pub server_time_ms: i64,
}

/// Reason that the core refused the handshake (§5.1 RejectReason).
#[derive(Debug, Clone, PartialEq)]
pub enum RejectReason {
    VersionMismatch { supported: Vec<u16> },
    AuthRequired { method: String },
    AuthInvalid,
    AuthExpired,
    OriginBlocked,
    CapabilityRequired { capability: String },
    RateLimited,
    Maintenance,
    ServerOverloaded,
}

impl<C> Encode<C> for RejectReason {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical key order:
        //   kind(0x64..) < method(0x66..) < supported(0x69..) < capability(0x6a..)
        match self {
            RejectReason::VersionMismatch { supported } => {
                e.map(2)?;
                e.str("kind")?.str("version_mismatch")?;
                e.str("supported")?;
                e.array(supported.len() as u64)?;
                for v in supported {
                    e.u16(*v)?;
                }
            }
            RejectReason::AuthRequired { method } => {
                e.map(2)?;
                e.str("kind")?.str("auth_required")?;
                e.str("method")?.str(method)?;
            }
            RejectReason::AuthInvalid => {
                e.map(1)?;
                e.str("kind")?.str("auth_invalid")?;
            }
            RejectReason::AuthExpired => {
                e.map(1)?;
                e.str("kind")?.str("auth_expired")?;
            }
            RejectReason::OriginBlocked => {
                e.map(1)?;
                e.str("kind")?.str("origin_blocked")?;
            }
            RejectReason::CapabilityRequired { capability } => {
                e.map(2)?;
                e.str("kind")?.str("capability_required")?;
                e.str("capability")?.str(capability)?;
            }
            RejectReason::RateLimited => {
                e.map(1)?;
                e.str("kind")?.str("rate_limited")?;
            }
            RejectReason::Maintenance => {
                e.map(1)?;
                e.str("kind")?.str("maintenance")?;
            }
            RejectReason::ServerOverloaded => {
                e.map(1)?;
                e.str("kind")?.str("server_overloaded")?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for RejectReason {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut supported: Option<Vec<u16>> = None;
        let mut method: Option<String> = None;
        let mut capability: Option<String> = None;
        for _ in 0..len {
            let key = d.str()?;
            match key {
                "kind" => {
                    assert_no_dup_tstr(&kind, "RejectReason", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "supported" => {
                    assert_no_dup_tstr(&supported, "RejectReason", "supported")?;
                    let n = d.array()?.ok_or_else(|| {
                        minicbor::decode::Error::message("indefinite-length array forbidden")
                    })?;
                    let mut v = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        v.push(d.u16()?);
                    }
                    supported = Some(v);
                }
                "method" => {
                    assert_no_dup_tstr(&method, "RejectReason", "method")?;
                    method = Some(d.str()?.to_string());
                }
                "capability" => {
                    assert_no_dup_tstr(&capability, "RejectReason", "capability")?;
                    capability = Some(d.str()?.to_string());
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown RejectReason key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("RejectReason missing kind"))?;
        let no_extras = |sup: &Option<Vec<u16>>,
                         meth: &Option<String>,
                         cap: &Option<String>|
         -> Result<(), minicbor::decode::Error> {
            if sup.is_some() || meth.is_some() || cap.is_some() {
                return Err(minicbor::decode::Error::message(
                    "RejectReason variant must not carry supported/method/capability",
                ));
            }
            Ok(())
        };
        match kind.as_str() {
            "version_mismatch" => {
                if method.is_some() || capability.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "RejectReason.version_mismatch must not carry method/capability",
                    ));
                }
                Ok(RejectReason::VersionMismatch {
                    supported: supported.ok_or_else(|| {
                        minicbor::decode::Error::message(
                            "RejectReason.version_mismatch missing supported",
                        )
                    })?,
                })
            }
            "auth_required" => {
                if supported.is_some() || capability.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "RejectReason.auth_required must not carry supported/capability",
                    ));
                }
                Ok(RejectReason::AuthRequired {
                    method: method.ok_or_else(|| {
                        minicbor::decode::Error::message(
                            "RejectReason.auth_required missing method",
                        )
                    })?,
                })
            }
            "auth_invalid" => {
                no_extras(&supported, &method, &capability)?;
                Ok(RejectReason::AuthInvalid)
            }
            "auth_expired" => {
                no_extras(&supported, &method, &capability)?;
                Ok(RejectReason::AuthExpired)
            }
            "origin_blocked" => {
                no_extras(&supported, &method, &capability)?;
                Ok(RejectReason::OriginBlocked)
            }
            "capability_required" => {
                if supported.is_some() || method.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "RejectReason.capability_required must not carry supported/method",
                    ));
                }
                Ok(RejectReason::CapabilityRequired {
                    capability: capability.ok_or_else(|| {
                        minicbor::decode::Error::message(
                            "RejectReason.capability_required missing capability",
                        )
                    })?,
                })
            }
            "rate_limited" => {
                no_extras(&supported, &method, &capability)?;
                Ok(RejectReason::RateLimited)
            }
            "maintenance" => {
                no_extras(&supported, &method, &capability)?;
                Ok(RejectReason::Maintenance)
            }
            "server_overloaded" => {
                no_extras(&supported, &method, &capability)?;
                Ok(RejectReason::ServerOverloaded)
            }
            other => Err(minicbor::decode::Error::message(format!(
                "unknown RejectReason.kind: {other}"
            ))),
        }
    }
}

/// ProtocolReject (0x0503): final frame before core closes the transport.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct ProtocolReject {
    #[n(0)]
    pub reason: RejectReason,
    #[n(1)]
    pub message: String,
    #[n(2)]
    pub retry_after_ms: Option<u32>,
}

// -----------------------------------------------------------------------------
// §5.2 Lifecycle — Heartbeat, SessionEnd, CapabilityRevoked, RateLimitUpdate
// -----------------------------------------------------------------------------

/// Heartbeat (0x0510): empty payload, timing source = envelope.ts_ms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Heartbeat;

impl<C> Encode<C> for Heartbeat {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.map(0)?;
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for Heartbeat {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        if len != 0 {
            return Err(minicbor::decode::Error::message(
                "Heartbeat payload must be empty map",
            ));
        }
        Ok(Heartbeat)
    }
}

string_enum! {
    /// Reason code for SessionEnd (§5.2).
    pub enum SessionEndCode {
        UserInitiated = "user_initiated",
        ServerShutdown = "server_shutdown",
        IdleTimeout = "idle_timeout",
        ProtocolError = "protocol_error",
        AuthExpired = "auth_expired",
        Replaced = "replaced",
    }
}

/// SessionEnd (0x0511): graceful close.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct SessionEnd {
    #[n(0)]
    pub code: SessionEndCode,
    #[n(1)]
    pub reason: String,
}

/// CapabilityRevoked (0x0512): server retracts a previously-granted capability.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct CapabilityRevoked {
    #[n(0)]
    pub capability: String,
    #[n(1)]
    pub reason: String,
}

/// Scope of a RateLimitUpdate (§5.2).
#[derive(Debug, Clone, PartialEq)]
pub enum RateLimitScope {
    Global,
    Channel { channel: u8 },
    Action { action_id: String },
}

impl<C> Encode<C> for RateLimitScope {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical key order: kind(0x64..) < channel(0x67..) < action_id(0x69..)
        match self {
            RateLimitScope::Global => {
                e.map(1)?;
                e.str("kind")?.str("global")?;
            }
            RateLimitScope::Channel { channel } => {
                e.map(2)?;
                e.str("kind")?.str("channel")?;
                e.str("channel")?.u8(*channel)?;
            }
            RateLimitScope::Action { action_id } => {
                e.map(2)?;
                e.str("kind")?.str("action")?;
                e.str("action_id")?.str(action_id)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for RateLimitScope {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut channel: Option<u8> = None;
        let mut action_id: Option<String> = None;
        for _ in 0..len {
            let key = d.str()?;
            match key {
                "kind" => {
                    assert_no_dup_tstr(&kind, "RateLimitScope", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "channel" => {
                    assert_no_dup_tstr(&channel, "RateLimitScope", "channel")?;
                    channel = Some(d.u8()?);
                }
                "action_id" => {
                    assert_no_dup_tstr(&action_id, "RateLimitScope", "action_id")?;
                    action_id = Some(d.str()?.to_string());
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown RateLimitScope key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("RateLimitScope missing kind"))?;
        match kind.as_str() {
            "global" => {
                if channel.is_some() || action_id.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "RateLimitScope.global must not carry channel/action_id",
                    ));
                }
                Ok(RateLimitScope::Global)
            }
            "channel" => {
                if action_id.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "RateLimitScope.channel must not carry action_id",
                    ));
                }
                Ok(RateLimitScope::Channel {
                    channel: channel.ok_or_else(|| {
                        minicbor::decode::Error::message("RateLimitScope.channel missing channel")
                    })?,
                })
            }
            "action" => {
                if channel.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "RateLimitScope.action must not carry channel",
                    ));
                }
                Ok(RateLimitScope::Action {
                    action_id: action_id.ok_or_else(|| {
                        minicbor::decode::Error::message("RateLimitScope.action missing action_id")
                    })?,
                })
            }
            other => Err(minicbor::decode::Error::message(format!(
                "unknown RateLimitScope.kind: {other}"
            ))),
        }
    }
}

/// RateLimitUpdate (0x0513): server-pushed limit change.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct RateLimitUpdate {
    #[n(0)]
    pub scope: RateLimitScope,
    #[n(1)]
    pub actions_per_sec: u16,
    #[n(2)]
    pub retry_after_ms: Option<u32>,
}

// -----------------------------------------------------------------------------
// §5.3 Flow control — CreditGrant, Backpressure, QueueDepth
// -----------------------------------------------------------------------------

string_enum! {
    /// Rationale shipped with a CreditGrant (§5.3).
    pub enum GrantRationale {
        InitialAdvertise = "initial_advertise",
        Refill = "refill",
        Recovery = "recovery",
    }
}

/// CreditGrant (0x0520): peer grants additional credits to sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct CreditGrant {
    #[n(0)]
    pub channel: u8,
    #[n(1)]
    pub credits: u32,
    #[n(2)]
    pub rationale: GrantRationale,
}

string_enum! {
    /// Severity of a Backpressure signal (§5.3).
    pub enum BackpressureSeverity {
        Warn = "warn",
        Critical = "critical",
    }
}

/// Backpressure (0x0521): peer's queue near capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Backpressure {
    #[n(0)]
    pub channel: u8,
    #[n(1)]
    pub queue_depth: u32,
    #[n(2)]
    pub queue_capacity: u32,
    #[n(3)]
    pub severity: BackpressureSeverity,
}

/// QueueDepth (0x0522): diagnostic snapshot of peer queues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct QueueDepth {
    #[n(0)]
    pub channel: u8,
    #[n(1)]
    pub outbound_pending: u32,
    #[n(2)]
    pub inbound_pending: u32,
    #[n(3)]
    pub credits_available: u32,
    #[n(4)]
    pub sampled_at_ms: i64,
}

// -----------------------------------------------------------------------------
// ControlPayload — tagged union over §5 messages
// -----------------------------------------------------------------------------

/// Wire tags for control-channel payloads (§5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ControlTag {
    Hello = 0x0501,
    Welcome = 0x0502,
    Reject = 0x0503,
    Heartbeat = 0x0510,
    SessionEnd = 0x0511,
    CapabilityRevoked = 0x0512,
    RateLimitUpdate = 0x0513,
    CreditGrant = 0x0520,
    Backpressure = 0x0521,
    QueueDepth = 0x0522,
}

impl ControlTag {
    pub const fn as_u16(self) -> u16 {
        self as u16
    }

    pub const fn from_u16(v: u16) -> Option<Self> {
        match v {
            0x0501 => Some(Self::Hello),
            0x0502 => Some(Self::Welcome),
            0x0503 => Some(Self::Reject),
            0x0510 => Some(Self::Heartbeat),
            0x0511 => Some(Self::SessionEnd),
            0x0512 => Some(Self::CapabilityRevoked),
            0x0513 => Some(Self::RateLimitUpdate),
            0x0520 => Some(Self::CreditGrant),
            0x0521 => Some(Self::Backpressure),
            0x0522 => Some(Self::QueueDepth),
            _ => None,
        }
    }
}

/// Discriminated union over all §5 payloads.
///
/// Wire form: CBOR array `[tag: u16, body]` as required by Envelope.payload (§4).
#[derive(Debug, Clone, PartialEq)]
pub enum ControlPayload {
    Hello(ProtocolHello),
    Welcome(ProtocolWelcome),
    Reject(ProtocolReject),
    Heartbeat(Heartbeat),
    SessionEnd(SessionEnd),
    CapabilityRevoked(CapabilityRevoked),
    RateLimitUpdate(RateLimitUpdate),
    CreditGrant(CreditGrant),
    Backpressure(Backpressure),
    QueueDepth(QueueDepth),
}

impl ControlPayload {
    pub fn tag(&self) -> ControlTag {
        match self {
            Self::Hello(_) => ControlTag::Hello,
            Self::Welcome(_) => ControlTag::Welcome,
            Self::Reject(_) => ControlTag::Reject,
            Self::Heartbeat(_) => ControlTag::Heartbeat,
            Self::SessionEnd(_) => ControlTag::SessionEnd,
            Self::CapabilityRevoked(_) => ControlTag::CapabilityRevoked,
            Self::RateLimitUpdate(_) => ControlTag::RateLimitUpdate,
            Self::CreditGrant(_) => ControlTag::CreditGrant,
            Self::Backpressure(_) => ControlTag::Backpressure,
            Self::QueueDepth(_) => ControlTag::QueueDepth,
        }
    }
}

impl<C> Encode<C> for ControlPayload {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.array(2)?;
        e.u16(self.tag().as_u16())?;
        match self {
            Self::Hello(v) => v.encode(e, ctx)?,
            Self::Welcome(v) => v.encode(e, ctx)?,
            Self::Reject(v) => v.encode(e, ctx)?,
            Self::Heartbeat(v) => v.encode(e, ctx)?,
            Self::SessionEnd(v) => v.encode(e, ctx)?,
            Self::CapabilityRevoked(v) => v.encode(e, ctx)?,
            Self::RateLimitUpdate(v) => v.encode(e, ctx)?,
            Self::CreditGrant(v) => v.encode(e, ctx)?,
            Self::Backpressure(v) => v.encode(e, ctx)?,
            Self::QueueDepth(v) => v.encode(e, ctx)?,
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for ControlPayload {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .array()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length array forbidden"))?;
        if len != 2 {
            return Err(minicbor::decode::Error::message(
                "Envelope payload tuple MUST be [tag, body]",
            ));
        }
        let tag_raw = d.u16()?;
        let tag = ControlTag::from_u16(tag_raw)
            .ok_or_else(|| minicbor::decode::Error::message("unknown control-channel tag"))?;
        Ok(match tag {
            ControlTag::Hello => Self::Hello(ProtocolHello::decode(d, ctx)?),
            ControlTag::Welcome => Self::Welcome(ProtocolWelcome::decode(d, ctx)?),
            ControlTag::Reject => Self::Reject(ProtocolReject::decode(d, ctx)?),
            ControlTag::Heartbeat => Self::Heartbeat(Heartbeat::decode(d, ctx)?),
            ControlTag::SessionEnd => Self::SessionEnd(SessionEnd::decode(d, ctx)?),
            ControlTag::CapabilityRevoked => {
                Self::CapabilityRevoked(CapabilityRevoked::decode(d, ctx)?)
            }
            ControlTag::RateLimitUpdate => Self::RateLimitUpdate(RateLimitUpdate::decode(d, ctx)?),
            ControlTag::CreditGrant => Self::CreditGrant(CreditGrant::decode(d, ctx)?),
            ControlTag::Backpressure => Self::Backpressure(Backpressure::decode(d, ctx)?),
            ControlTag::QueueDepth => Self::QueueDepth(QueueDepth::decode(d, ctx)?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::envelope::{Channel, Envelope, Flags, Priority, ProtocolVersion};
    use crate::protocol::ids::{Hash32, SessionId, TraceId};

    fn envelope_with(payload: ControlPayload) -> Envelope<ControlPayload> {
        Envelope {
            protocol_version: ProtocolVersion::V1,
            channel: Channel::Control,
            msg_id: 1,
            correlation_id: None,
            ts_ms: 1_700_000_000_000,
            session_id: SessionId::from_bytes([1; 16]),
            trace_id: Some(TraceId::from_bytes([2; 16])),
            deadline_ms: None,
            priority: Priority::Control,
            flags: Flags::RELIABLE,
            payload,
        }
    }

    fn roundtrip(env: &Envelope<ControlPayload>) {
        let mut buf1 = Vec::new();
        minicbor::encode(env, &mut buf1).unwrap();
        let decoded: Envelope<ControlPayload> = minicbor::decode(&buf1).unwrap();
        assert_eq!(&decoded, env);
        let mut buf2 = Vec::new();
        minicbor::encode(&decoded, &mut buf2).unwrap();
        assert_eq!(buf1, buf2, "re-encode must be bit-identical");
    }

    #[test]
    fn roundtrip_hello() {
        let hello = ProtocolHello {
            protocol_version: ProtocolVersion::V1,
            client_version: "tentaflow-web/1.0.0".into(),
            capabilities_requested: vec![
                Capability {
                    name: "ui_v1".into(),
                    version: 1,
                    hash: Some(Hash32::from_bytes([0xAA; 32])),
                    params: None,
                },
                Capability {
                    name: "compression_zstd".into(),
                    version: 1,
                    hash: None,
                    params: Some(CborMap(vec![("level".into(), Value::U64(7))])),
                },
            ],
            auth: AuthContext {
                bearer_token: Some("token".into()),
                client_cert_fingerprint: None,
                device_id: None,
                origin: "https://app.tentaflow.io".into(),
            },
            resume: None,
            client_credit_budget: CreditBudget::default(),
        };
        roundtrip(&envelope_with(ControlPayload::Hello(hello)));
    }

    #[test]
    fn roundtrip_welcome_resumed() {
        let welcome = ProtocolWelcome {
            protocol_version: ProtocolVersion::V1,
            server_version: "tentaflow-core/0.5.0".into(),
            session_id: SessionId::from_bytes([9; 16]),
            capabilities_granted: vec![],
            capabilities_rejected: vec![CapabilityRejection {
                capability: "webtransport_datagrams".into(),
                reason: "not_available".into(),
            }],
            resume_status: ResumeStatus::Resumed {
                mode: ResumeMode::Snapshot,
                next_msg_id: 42,
            },
            server_limits: ServerLimits {
                max_message_bytes: 1_048_576,
                max_state_path_segments: 32,
                max_components_per_fragment: 1000,
                max_component_depth: 64,
                max_state_patch_ops: 256,
                max_concurrent_streams: 32,
                max_queue_per_channel: 1024,
                default_rate_limit_actions_per_sec: 100,
                server_credit_budget: CreditBudget::default(),
            },
            server_time_ms: 1_700_000_000_000,
        };
        roundtrip(&envelope_with(ControlPayload::Welcome(welcome)));
    }

    #[test]
    fn roundtrip_reject_with_supported_versions() {
        let reject = ProtocolReject {
            reason: RejectReason::VersionMismatch {
                supported: vec![1, 2],
            },
            message: "unsupported version".into(),
            retry_after_ms: Some(1000),
        };
        roundtrip(&envelope_with(ControlPayload::Reject(reject)));
    }

    #[test]
    fn roundtrip_heartbeat() {
        roundtrip(&envelope_with(ControlPayload::Heartbeat(Heartbeat)));
    }

    #[test]
    fn roundtrip_session_end() {
        roundtrip(&envelope_with(ControlPayload::SessionEnd(SessionEnd {
            code: SessionEndCode::IdleTimeout,
            reason: "no traffic 90s".into(),
        })));
    }

    #[test]
    fn roundtrip_rate_limit_update_action_scope() {
        roundtrip(&envelope_with(ControlPayload::RateLimitUpdate(
            RateLimitUpdate {
                scope: RateLimitScope::Action {
                    action_id: "tentavision.snapshot".into(),
                },
                actions_per_sec: 5,
                retry_after_ms: Some(2000),
            },
        )));
    }

    #[test]
    fn roundtrip_credit_grant() {
        roundtrip(&envelope_with(ControlPayload::CreditGrant(CreditGrant {
            channel: Channel::Ui.as_u8(),
            credits: 128,
            rationale: GrantRationale::Refill,
        })));
    }

    #[test]
    fn roundtrip_backpressure_and_queue_depth() {
        roundtrip(&envelope_with(ControlPayload::Backpressure(Backpressure {
            channel: 0xFF,
            queue_depth: 800,
            queue_capacity: 1024,
            severity: BackpressureSeverity::Warn,
        })));
        roundtrip(&envelope_with(ControlPayload::QueueDepth(QueueDepth {
            channel: Channel::Stream.as_u8(),
            outbound_pending: 5,
            inbound_pending: 0,
            credits_available: 32,
            sampled_at_ms: 1_700_000_001_000,
        })));
    }

    #[test]
    fn cbormap_encode_sorts_keys_canonically() {
        let m = CborMap(vec![
            ("zeta".into(), Value::U64(1)),
            ("a".into(), Value::U64(2)),
            ("mm".into(), Value::U64(3)),
        ]);
        let mut buf = Vec::new();
        minicbor::encode(&m, &mut buf).unwrap();
        // After canonical sort: shorter-first then bytewise → "a" (1), "mm" (2), "zeta" (4).
        // Decode and verify ordering preserved.
        let decoded: CborMap = minicbor::decode(&buf).unwrap();
        let keys: Vec<&str> = decoded.0.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["a", "mm", "zeta"]);
    }

    #[test]
    fn unknown_control_tag_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap().u16(0x05FE).unwrap().map(0).unwrap();
        let res: Result<ControlPayload, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn resume_status_fresh_rejects_extra_fields() {
        // {kind:"fresh", next_msg_id:1} must NOT decode as Fresh.
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("fresh")
            .unwrap()
            .str("next_msg_id")
            .unwrap()
            .u64(1)
            .unwrap();
        let res: Result<ResumeStatus, _> = minicbor::decode(&buf);
        assert!(
            res.is_err(),
            "extra fields must be rejected for fresh variant"
        );
    }

    #[test]
    fn reject_reason_auth_invalid_rejects_extra_fields() {
        // {kind:"auth_invalid", method:"x"} must NOT decode.
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("auth_invalid")
            .unwrap()
            .str("method")
            .unwrap()
            .str("x")
            .unwrap();
        let res: Result<RejectReason, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn rate_limit_scope_global_rejects_extra_fields() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("global")
            .unwrap()
            .str("channel")
            .unwrap()
            .u8(1)
            .unwrap();
        let res: Result<RateLimitScope, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn protocol_version_other_than_one_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.u16(2).unwrap();
        let res: Result<ProtocolVersion, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn capability_hash_wrong_length_rejected() {
        // Construct a Capability map with hash of 31 bytes instead of 32.
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(3)
            .unwrap()
            .u8(0)
            .unwrap()
            .str("ui_v1")
            .unwrap()
            .u8(1)
            .unwrap()
            .u32(1)
            .unwrap()
            .u8(2)
            .unwrap()
            .bytes(&[0xAA; 31])
            .unwrap();
        let res: Result<Capability, _> = minicbor::decode(&buf);
        assert!(res.is_err(), "31-byte hash must be rejected as not bstr 32");
    }
}
