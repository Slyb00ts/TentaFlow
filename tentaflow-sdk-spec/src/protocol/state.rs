// =============================================================================
// File: protocol/state.rs — shared in-memory state host-function ABI payloads
// CBOR types for the `state.*` host functions (A3) that expose the host-side
// AddonStateStore to WASM addons. All state is scoped to the calling addon_id;
// these structs only carry the cross-boundary arguments/results. `tier` is the
// persistence intent: 0 = ephemeral (RAM-only), 1 = durable (flushed to the
// backing store). Any other value is rejected by the host decoder.
// =============================================================================

use minicbor::{Decode, Encode};

/// Tier wire value for an ephemeral (RAM-only) entry.
pub const STATE_TIER_EPHEMERAL: u8 = 0;
/// Tier wire value for a durable (persisted) entry.
pub const STATE_TIER_DURABLE: u8 = 1;

/// Input for `state_set_v1` — write a value under `key` with the given `tier`.
/// `value` carries the raw bytes (CBOR byte string), so binary state needs no
/// base64 framing. `tier` MUST be `STATE_TIER_EPHEMERAL` or `STATE_TIER_DURABLE`;
/// the host rejects any other value as an operation error.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct StateSetInput {
    #[n(0)]
    pub key: String,
    #[n(1)]
    pub value: Vec<u8>,
    #[n(2)]
    pub tier: u8,
}

/// One entry's metadata returned by `state_list_v1`. `size` is the value byte
/// length (keys are not transferred as values here). `tier` uses the same wire
/// encoding as `StateSetInput::tier`.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct StateEntryMeta {
    #[n(0)]
    pub key: String,
    #[n(1)]
    pub size: u64,
    #[n(2)]
    pub tier: u8,
}

/// Output of `state_list_v1` — the metadata for every key under the requested
/// prefix (or all keys when no prefix is given). Order is unspecified.
///
/// `truncated` is `true` when the host clipped the result because the shard had
/// more matching entries than the per-call entry/byte budget allows (DoS guard:
/// a 50k-key shard must not materialise a multi-megabyte response). The addon
/// should treat the list as partial and narrow the prefix when this is set.
/// It is `#[cbor(default)]` so an older host that omits the field decodes as
/// `false` (not truncated).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct StateListOutput {
    #[n(0)]
    pub entries: Vec<StateEntryMeta>,
    #[n(1)]
    #[cbor(default)]
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_input_roundtrip_ephemeral() {
        let v = StateSetInput {
            key: "robot:1:pose".into(),
            value: vec![0x01, 0x02, 0x03, 0xff],
            tier: STATE_TIER_EPHEMERAL,
        };
        let mut buf = Vec::new();
        minicbor::encode(&v, &mut buf).unwrap();
        let back: StateSetInput = minicbor::decode(&buf).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn set_input_roundtrip_durable_empty_value() {
        let v = StateSetInput {
            key: "k".into(),
            value: Vec::new(),
            tier: STATE_TIER_DURABLE,
        };
        let mut buf = Vec::new();
        minicbor::encode(&v, &mut buf).unwrap();
        let back: StateSetInput = minicbor::decode(&buf).unwrap();
        assert_eq!(v, back);
        assert!(back.value.is_empty());
    }

    #[test]
    fn list_output_roundtrip() {
        let v = StateListOutput {
            entries: vec![
                StateEntryMeta {
                    key: "robot:1".into(),
                    size: 128,
                    tier: STATE_TIER_EPHEMERAL,
                },
                StateEntryMeta {
                    key: "config".into(),
                    size: 4096,
                    tier: STATE_TIER_DURABLE,
                },
            ],
            truncated: false,
        };
        let mut buf = Vec::new();
        minicbor::encode(&v, &mut buf).unwrap();
        let back: StateListOutput = minicbor::decode(&buf).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn list_output_truncated_roundtrip() {
        let v = StateListOutput {
            entries: vec![StateEntryMeta {
                key: "robot:1".into(),
                size: 1,
                tier: STATE_TIER_EPHEMERAL,
            }],
            truncated: true,
        };
        let mut buf = Vec::new();
        minicbor::encode(&v, &mut buf).unwrap();
        let back: StateListOutput = minicbor::decode(&buf).unwrap();
        assert_eq!(v, back);
        assert!(back.truncated);
    }

    #[test]
    fn empty_list_output_roundtrip() {
        let v = StateListOutput {
            entries: Vec::new(),
            truncated: false,
        };
        let mut buf = Vec::new();
        minicbor::encode(&v, &mut buf).unwrap();
        let back: StateListOutput = minicbor::decode(&buf).unwrap();
        assert_eq!(v, back);
        assert!(back.entries.is_empty());
        assert!(!back.truncated);
    }
}
