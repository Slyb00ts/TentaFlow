// =============================================================================
// File: protocol/flow.rs — flow invoke/status/cancel host-function ABI payloads
// Purpose: single source of truth for the CBOR request/response structs of the
// three `flow_*_v1` host functions. Shared verbatim by the core host (decode
// input / encode output) and the addon SDK (encode input / decode output) so
// the wire format cannot drift. Maps use integer keys via `#[cbor(map)]` +
// `#[n(N)]`.
//
// Note on the flow data plane: `input_toml` and `result_toml` carry the opaque
// flow-operator payload as raw TOML text. That text is the flow runtime's own
// data-plane contract (operators read `OperatorContext.input_toml`); it is not
// the host-function ABI serialization format. The ABI itself is CBOR — these
// strings are just an opaque blob inside the CBOR map.
// =============================================================================

use minicbor::{Decode, Encode};

// -----------------------------------------------------------------------------
// Input payloads
// -----------------------------------------------------------------------------

/// Input for `flow_invoke_v1`. `input_toml` is the opaque operator payload
/// forwarded verbatim to every operator; `None` collapses to an empty TOML
/// table. `wait_ms = 0` returns immediately; `> 0` waits up to the host cap.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct FlowInvokeInput {
    #[n(0)]
    pub flow_id: String,
    #[n(1)]
    pub input_toml: Option<String>,
    #[n(2)]
    pub wait_ms: u32,
}

impl FlowInvokeInput {
    /// `input_toml` text with the empty-table default applied when absent.
    pub fn input_toml_or_empty(&self) -> &str {
        self.input_toml.as_deref().unwrap_or("")
    }
}

/// Input carrying a single `invocation_id` — shared by `status` / `cancel`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct FlowInvocationIdInput {
    #[n(0)]
    pub invocation_id: String,
}

// -----------------------------------------------------------------------------
// Output payloads
// -----------------------------------------------------------------------------

/// Output of `flow_invoke_v1` / `flow_status_v1`. Mirrors the scheduler's
/// `InvocationStatus`. `result_toml` carries the opaque operator output as raw
/// TOML text (see the module note).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct FlowInvocationOutput {
    #[n(0)]
    pub invocation_id: String,
    #[n(1)]
    pub status: String,
    #[n(2)]
    pub started_at: String,
    #[n(3)]
    pub finished_at: Option<String>,
    #[n(4)]
    pub operators_completed: i64,
    #[n(5)]
    pub operators_total: i64,
    #[n(6)]
    pub error: Option<String>,
    #[n(7)]
    pub result_toml: Option<String>,
}

/// Output of `flow_cancel_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct FlowCancelOutput {
    #[n(0)]
    pub cancelled: bool,
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
    fn invoke_input_default_input_resolves_to_empty() {
        let minimal = FlowInvokeInput {
            flow_id: "f1".into(),
            input_toml: None,
            wait_ms: 0,
        };
        let mut buf = Vec::new();
        minicbor::encode(&minimal, &mut buf).unwrap();
        let decoded: FlowInvokeInput = minicbor::decode(&buf).unwrap();
        assert_eq!(decoded.input_toml_or_empty(), "");
    }

    #[test]
    fn roundtrip_invocation_output() {
        roundtrip(&FlowInvocationOutput {
            invocation_id: "inv_1".into(),
            status: "running".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            finished_at: None,
            operators_completed: 0,
            operators_total: 3,
            error: None,
            result_toml: None,
        });
    }

    #[test]
    fn roundtrip_cancel_output() {
        roundtrip(&FlowCancelOutput { cancelled: true });
    }
}
