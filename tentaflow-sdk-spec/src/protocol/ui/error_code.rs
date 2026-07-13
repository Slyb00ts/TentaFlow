// =============================================================================
// File: protocol/ui/error_code.rs — canonical ErrorCode enum (§16)
// Purpose: u16-discriminated error codes used in PanelError, ActionAck.error,
// PatchRejected (where applicable). Decoder whitelist-validates the u16 value
// against the enum's known variants.
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

/// Whitelist of error codes shipped on the wire (§16). Encoded as u16.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum ErrorCode {
    // Protocol layer (0x1000–0x10FF)
    ProtocolVersionMismatch = 0x1001,
    NonCanonicalEncoding = 0x1002,
    NonCanonicalKeyOrder = 0x1003,
    NonCanonicalIntegerWidth = 0x1004,
    DuplicateMapKey = 0x1005,
    IndefiniteLengthForbidden = 0x1006,
    UnknownSemanticTag = 0x1007,
    InvalidTextString = 0x1008,
    UnknownPayloadTag = 0x1009,
    WrongChannel = 0x100A,
    MissingRequiredField = 0x100B,
    TypeMismatch = 0x100C,

    // Structural limits (0x1100–0x11FF)
    MessageTooLarge = 0x1101,
    ComponentDepthExceeded = 0x1102,
    ComponentCountExceeded = 0x1103,
    StringTooLong = 0x1104,
    ArrayTooLong = 0x1105,
    MapTooLarge = 0x1106,
    StatePatchOpsExceeded = 0x1107,
    PathDepthExceeded = 0x1108,
    CommandsPerMessageExceeded = 0x1109,
    HandlerDepthExceeded = 0x110A,
    HandlerStepsExceeded = 0x110B,
    BatchMembersExceeded = 0x110C,

    // Lifecycle (0x1200–0x12FF)
    UnknownPanel = 0x1201,
    StalePanelEpoch = 0x1202,
    PanelAlreadyOpen = 0x1203,
    PanelNotReady = 0x1204,
    RevisionMismatch = 0x1205,
    SnapshotRequired = 0x1206,

    // Sandbox (0x1300–0x13FF)
    SlotOwnershipViolation = 0x1301,
    PathOwnershipViolation = 0x1302,
    ReservedNamespace = 0x1303,
    UnknownSlot = 0x1304,
    UnknownAction = 0x1305,
    UnknownEventTopic = 0x1306,
    TopicPatternViolation = 0x1307,

    // Authorization (0x1400–0x14FF)
    PermissionDenied = 0x1401,
    CapabilityNotGranted = 0x1402,
    AuthExpired = 0x1403,
    AuthInvalid = 0x1404,
    OriginNotAllowed = 0x1405,
    UserGestureRequired = 0x1406,

    // Validation (0x1500–0x15FF)
    FieldValidationFailed = 0x1501,
    InvalidUrl = 0x1502,
    InvalidFilename = 0x1503,
    InvalidIcon = 0x1504,
    InvalidToneVariant = 0x1505,
    InvalidLocale = 0x1506,
    InvalidDuration = 0x1507,
    InvalidColorToken = 0x1508,
    InvalidStatePath = 0x1509,
    NonCanonicalFloatWidth = 0x150A,
    LocalCapabilityNotDeclared = 0x150B,

    // Resource (0x1600–0x16FF)
    RateLimited = 0x1601,
    QueueOverflowed = 0x1602,
    BackpressureBlocked = 0x1603,
    StreamLimitExceeded = 0x1604,
    FuelExhausted = 0x1605,
    MemoryExhausted = 0x1606,

    // Internal (0x1F00–0x1FFF)
    InternalError = 0x1F01,
    AddonCrashed = 0x1F02,
    AddonTimeout = 0x1F03,
    AddonUnloaded = 0x1F04,
}

impl ErrorCode {
    pub const fn as_u16(self) -> u16 {
        self as u16
    }

    pub const fn from_u16(v: u16) -> Option<Self> {
        Some(match v {
            0x1001 => Self::ProtocolVersionMismatch,
            0x1002 => Self::NonCanonicalEncoding,
            0x1003 => Self::NonCanonicalKeyOrder,
            0x1004 => Self::NonCanonicalIntegerWidth,
            0x1005 => Self::DuplicateMapKey,
            0x1006 => Self::IndefiniteLengthForbidden,
            0x1007 => Self::UnknownSemanticTag,
            0x1008 => Self::InvalidTextString,
            0x1009 => Self::UnknownPayloadTag,
            0x100A => Self::WrongChannel,
            0x100B => Self::MissingRequiredField,
            0x100C => Self::TypeMismatch,
            0x1101 => Self::MessageTooLarge,
            0x1102 => Self::ComponentDepthExceeded,
            0x1103 => Self::ComponentCountExceeded,
            0x1104 => Self::StringTooLong,
            0x1105 => Self::ArrayTooLong,
            0x1106 => Self::MapTooLarge,
            0x1107 => Self::StatePatchOpsExceeded,
            0x1108 => Self::PathDepthExceeded,
            0x1109 => Self::CommandsPerMessageExceeded,
            0x110A => Self::HandlerDepthExceeded,
            0x110B => Self::HandlerStepsExceeded,
            0x110C => Self::BatchMembersExceeded,
            0x1201 => Self::UnknownPanel,
            0x1202 => Self::StalePanelEpoch,
            0x1203 => Self::PanelAlreadyOpen,
            0x1204 => Self::PanelNotReady,
            0x1205 => Self::RevisionMismatch,
            0x1206 => Self::SnapshotRequired,
            0x1301 => Self::SlotOwnershipViolation,
            0x1302 => Self::PathOwnershipViolation,
            0x1303 => Self::ReservedNamespace,
            0x1304 => Self::UnknownSlot,
            0x1305 => Self::UnknownAction,
            0x1306 => Self::UnknownEventTopic,
            0x1307 => Self::TopicPatternViolation,
            0x1401 => Self::PermissionDenied,
            0x1402 => Self::CapabilityNotGranted,
            0x1403 => Self::AuthExpired,
            0x1404 => Self::AuthInvalid,
            0x1405 => Self::OriginNotAllowed,
            0x1406 => Self::UserGestureRequired,
            0x1501 => Self::FieldValidationFailed,
            0x1502 => Self::InvalidUrl,
            0x1503 => Self::InvalidFilename,
            0x1504 => Self::InvalidIcon,
            0x1505 => Self::InvalidToneVariant,
            0x1506 => Self::InvalidLocale,
            0x1507 => Self::InvalidDuration,
            0x1508 => Self::InvalidColorToken,
            0x1509 => Self::InvalidStatePath,
            0x150A => Self::NonCanonicalFloatWidth,
            0x150B => Self::LocalCapabilityNotDeclared,
            0x1601 => Self::RateLimited,
            0x1602 => Self::QueueOverflowed,
            0x1603 => Self::BackpressureBlocked,
            0x1604 => Self::StreamLimitExceeded,
            0x1605 => Self::FuelExhausted,
            0x1606 => Self::MemoryExhausted,
            0x1F01 => Self::InternalError,
            0x1F02 => Self::AddonCrashed,
            0x1F03 => Self::AddonTimeout,
            0x1F04 => Self::AddonUnloaded,
            _ => return None,
        })
    }
}

impl<C> Encode<C> for ErrorCode {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.u16(self.as_u16())?;
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for ErrorCode {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let v = d.u16()?;
        Self::from_u16(v).ok_or_else(|| {
            minicbor::decode::Error::message("unknown ErrorCode value (not in §16 whitelist)")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_known_codes() {
        for c in [
            ErrorCode::ProtocolVersionMismatch,
            ErrorCode::AddonTimeout,
            ErrorCode::SlotOwnershipViolation,
            ErrorCode::NonCanonicalFloatWidth,
            ErrorCode::MemoryExhausted,
        ] {
            let mut buf = Vec::new();
            minicbor::encode(&c, &mut buf).unwrap();
            let d: ErrorCode = minicbor::decode(&buf).unwrap();
            assert_eq!(d, c);
        }
    }

    #[test]
    fn unknown_value_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.u16(0x9999).unwrap();
        let res: Result<ErrorCode, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }
}
