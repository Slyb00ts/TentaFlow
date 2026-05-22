// =============================================================================
// File: protocol/frame/error.rs — FrameError + FrameErrorCode (UFP/2 §11)
// Purpose: enumerate the 17 standard error codes returned by receivers in
// Control / ProtocolError envelopes. Codes 0x0001..=0x0011 are normative
// per §11; future codes append in the 0x0012..=0xFFFF reserved range.
// =============================================================================

/// Standard UFP/2 protocol error codes (§11). Values are stable u16
/// constants in the wire ProtocolError body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum FrameErrorCode {
    CanonicalEncoding = 0x0001,
    UnknownProtocolVersion = 0x0002,
    UnknownChannel = 0x0003,
    UnknownKind = 0x0004,
    InvalidSignature = 0x0005,
    ExpiredEpoch = 0x0006,
    ReplayDetected = 0x0007,
    ClockSkewExceeded = 0x0008,
    NestingTooDeep = 0x0009,
    DecryptionFailed = 0x000A,
    DecompressionFailed = 0x000B,
    FragmentAssemblyError = 0x000C,
    ForwardingLoop = 0x000D,
    BodyValidationFailed = 0x000E,
    PermissionDenied = 0x000F,
    RateLimited = 0x0010,
    UnsupportedCompression = 0x0011,
}

impl FrameErrorCode {
    pub fn as_u16(self) -> u16 {
        self as u16
    }

    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            0x0001 => Some(Self::CanonicalEncoding),
            0x0002 => Some(Self::UnknownProtocolVersion),
            0x0003 => Some(Self::UnknownChannel),
            0x0004 => Some(Self::UnknownKind),
            0x0005 => Some(Self::InvalidSignature),
            0x0006 => Some(Self::ExpiredEpoch),
            0x0007 => Some(Self::ReplayDetected),
            0x0008 => Some(Self::ClockSkewExceeded),
            0x0009 => Some(Self::NestingTooDeep),
            0x000A => Some(Self::DecryptionFailed),
            0x000B => Some(Self::DecompressionFailed),
            0x000C => Some(Self::FragmentAssemblyError),
            0x000D => Some(Self::ForwardingLoop),
            0x000E => Some(Self::BodyValidationFailed),
            0x000F => Some(Self::PermissionDenied),
            0x0010 => Some(Self::RateLimited),
            0x0011 => Some(Self::UnsupportedCompression),
            _ => None,
        }
    }
}

/// Frame-layer error carrying a code + diagnostic message + optional path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameError {
    pub code: FrameErrorCode,
    pub message: String,
    pub field_path: Option<String>,
}

impl FrameError {
    pub fn new(code: FrameErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            field_path: None,
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.field_path = Some(path.into());
        self
    }
}

impl core::fmt::Display for FrameError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match &self.field_path {
            Some(p) => write!(
                f,
                "[0x{:04X}] {}: {} (at {})",
                self.code.as_u16(),
                error_name(self.code),
                self.message,
                p
            ),
            None => write!(
                f,
                "[0x{:04X}] {}: {}",
                self.code.as_u16(),
                error_name(self.code),
                self.message
            ),
        }
    }
}

impl std::error::Error for FrameError {}

fn error_name(c: FrameErrorCode) -> &'static str {
    match c {
        FrameErrorCode::CanonicalEncoding => "CanonicalEncoding",
        FrameErrorCode::UnknownProtocolVersion => "UnknownProtocolVersion",
        FrameErrorCode::UnknownChannel => "UnknownChannel",
        FrameErrorCode::UnknownKind => "UnknownKind",
        FrameErrorCode::InvalidSignature => "InvalidSignature",
        FrameErrorCode::ExpiredEpoch => "ExpiredEpoch",
        FrameErrorCode::ReplayDetected => "ReplayDetected",
        FrameErrorCode::ClockSkewExceeded => "ClockSkewExceeded",
        FrameErrorCode::NestingTooDeep => "NestingTooDeep",
        FrameErrorCode::DecryptionFailed => "DecryptionFailed",
        FrameErrorCode::DecompressionFailed => "DecompressionFailed",
        FrameErrorCode::FragmentAssemblyError => "FragmentAssemblyError",
        FrameErrorCode::ForwardingLoop => "ForwardingLoop",
        FrameErrorCode::BodyValidationFailed => "BodyValidationFailed",
        FrameErrorCode::PermissionDenied => "PermissionDenied",
        FrameErrorCode::RateLimited => "RateLimited",
        FrameErrorCode::UnsupportedCompression => "UnsupportedCompression",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_error_code_values_match_spec() {
        assert_eq!(FrameErrorCode::CanonicalEncoding.as_u16(), 0x0001);
        assert_eq!(FrameErrorCode::UnsupportedCompression.as_u16(), 0x0011);
    }

    #[test]
    fn frame_error_code_roundtrip() {
        for code_u16 in 0x0001u16..=0x0011 {
            let c = FrameErrorCode::from_u16(code_u16).unwrap();
            assert_eq!(c.as_u16(), code_u16);
        }
    }

    #[test]
    fn frame_error_code_rejects_unknown() {
        assert!(FrameErrorCode::from_u16(0x0000).is_none());
        assert!(FrameErrorCode::from_u16(0x0012).is_none());
        assert!(FrameErrorCode::from_u16(0xFFFF).is_none());
    }

    #[test]
    fn frame_error_display_with_path() {
        let e = FrameError::new(FrameErrorCode::BodyValidationFailed, "missing field")
            .with_path("envelope.auth");
        let s = format!("{}", e);
        assert!(s.contains("[0x000E]"));
        assert!(s.contains("BodyValidationFailed"));
        assert!(s.contains("missing field"));
        assert!(s.contains("envelope.auth"));
    }

    #[test]
    fn frame_error_display_without_path() {
        let e = FrameError::new(FrameErrorCode::InvalidSignature, "bad sig");
        let s = format!("{}", e);
        assert!(s.contains("[0x0005]"));
        assert!(s.contains("InvalidSignature"));
        assert!(!s.contains("(at"));
    }
}
