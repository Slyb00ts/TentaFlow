// =============================================================================
// File: protocol/gate.rs — policy gate-check host-function ABI payloads
// Purpose: single source of truth for the CBOR request/response structs of
// `gate_check_v1`. Shared verbatim by the core host (decode input / encode
// output) and the addon SDK (encode input / decode output) so the wire format
// cannot drift. Maps use integer keys via `#[cbor(map)]` + `#[n(N)]`.
// `resource_scope` and `reason` are `Option` on the wire so a minimal payload
// can omit them.
// =============================================================================

use minicbor::{Decode, Encode};

// -----------------------------------------------------------------------------
// Input payload
// -----------------------------------------------------------------------------

/// Input for `gate_check_v1`. `resource_scope` narrows the gate to a specific
/// resource (e.g. the `faces` vector namespace) when present.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GateCheckInput {
    #[n(0)]
    pub gate_id: String,
    #[n(1)]
    pub claim_id: String,
    #[n(2)]
    pub resource_scope: Option<String>,
}

// -----------------------------------------------------------------------------
// Output payload
// -----------------------------------------------------------------------------

/// One signer that satisfied (or is required by) the gate.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GateSignerOut {
    #[n(0)]
    pub role: String,
    #[n(1)]
    pub user: String,
}

/// Output of `gate_check_v1`. `valid` always carries the inspection result;
/// `reason` is present only on the invalid path. Hard ABI errors (missing
/// permission, gate id not in manifest, malformed payload) are returned as
/// AbiError codes instead of a body with `valid = false`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GateCheckOutput {
    #[n(0)]
    pub valid: bool,
    #[n(1)]
    pub claim_id: String,
    #[n(2)]
    pub claim_type: String,
    #[n(3)]
    pub valid_until: String,
    #[n(4)]
    pub signers: Vec<GateSignerOut>,
    #[n(5)]
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: &T)
    where
        T: Encode<()> + for<'b> Decode<'b, ()> + PartialEq + core::fmt::Debug,
    {
        let mut buf = Vec::new();
        minicbor::encode(value, &mut buf).unwrap();
        let decoded: T = minicbor::decode(&buf).unwrap();
        assert_eq!(&decoded, value);
    }

    #[test]
    fn roundtrip_input_with_and_without_scope() {
        roundtrip(&GateCheckInput {
            gate_id: "d4-historical".into(),
            claim_id: "claim_1".into(),
            resource_scope: Some("faces".into()),
        });
        roundtrip(&GateCheckInput {
            gate_id: "d4-historical".into(),
            claim_id: "claim_1".into(),
            resource_scope: None,
        });
    }

    #[test]
    fn roundtrip_output_valid_and_invalid() {
        roundtrip(&GateCheckOutput {
            valid: true,
            claim_id: "claim_1".into(),
            claim_type: "dpia".into(),
            valid_until: "2026-12-31T00:00:00Z".into(),
            signers: vec![GateSignerOut {
                role: "dpo".into(),
                user: "alice".into(),
            }],
            reason: None,
        });
        roundtrip(&GateCheckOutput {
            valid: false,
            claim_id: "claim_1".into(),
            claim_type: String::new(),
            valid_until: String::new(),
            signers: vec![],
            reason: Some("claim_revoked".into()),
        });
    }
}
