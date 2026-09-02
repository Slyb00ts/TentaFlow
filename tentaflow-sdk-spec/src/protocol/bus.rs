// =============================================================================
// File: protocol/bus.rs — TentaBus host-function ABI payloads (M3b, PLAN §6.4)
// Purpose: single source of truth for the CBOR request/response structs of the
// five `bus_*_v1` host functions (`bus_publish`, `bus_consume_open`,
// `bus_consume_next`, `bus_consume_commit`, `bus_consume_close`). Shared
// verbatim by the core host (decode input / encode output) and the addon SDK
// (encode input / decode output), same discipline as `protocol::streaming`.
//
// Design principle (PLAN §6.4): "nigdy per komunikat" — the WASM boundary
// crossing cost (~1-5us) is amortized over a BATCH, never paid per message.
// `bus_publish_v1` always takes a batch of records in one call; the consume
// side is a handle+batch pattern (`open` once, drain repeated `next` batches,
// `commit`/`close` when done) mirroring `stream_subscribe`/`stream_next`/
// `stream_close`, not a per-message round trip.
//
// `bus_consume_next` returns one of two variants discriminated by the `kind`
// tag (`batch` | `empty`), encoded as a single map rather than a CBOR-tagged
// enum — same reasoning as `StreamNextOutput`.
// =============================================================================

use minicbor::{Decode, Encode};

// -----------------------------------------------------------------------------
// Shared record shapes
// -----------------------------------------------------------------------------

/// One message header. Values travel as raw bytes (never assumed UTF-8) —
/// mirrors `tentaflow_core::bus::PublishRecord::headers`'s own `(String,
/// Bytes)` shape.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct BusHeader {
    #[n(0)]
    pub name: String,
    #[n(1)]
    pub value: Vec<u8>,
}

/// One record inside a `bus_publish_v1` batch.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct BusRecordIn {
    #[n(0)]
    pub key: Option<Vec<u8>>,
    #[n(1)]
    pub headers: Vec<BusHeader>,
    #[n(2)]
    pub payload: Vec<u8>,
}

/// One record returned by `bus_consume_next_v1` — carries delivery metadata
/// (`topic`/`partition`/`offset`) the addon needs to build the offsets it
/// later passes to `bus_consume_commit_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct BusRecordOut {
    #[n(0)]
    pub topic: String,
    #[n(1)]
    pub partition: u32,
    #[n(2)]
    pub offset: u64,
    #[n(3)]
    pub timestamp_ms: i64,
    #[n(4)]
    pub key: Option<Vec<u8>>,
    #[n(5)]
    pub headers: Vec<BusHeader>,
    #[n(6)]
    pub payload: Vec<u8>,
}

// -----------------------------------------------------------------------------
// bus_publish_v1
// -----------------------------------------------------------------------------

/// Input for `bus_publish_v1`. Capped at 1000 records / 8 MiB total
/// (`PayloadKind::BusBatch`) — PLAN §6.4. `create_if_missing` mirrors the
/// `bus_publish` flow node's own config field.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct BusPublishInput {
    #[n(0)]
    pub topic: String,
    #[n(1)]
    pub records: Vec<BusRecordIn>,
    #[n(2)]
    pub create_if_missing: Option<bool>,
}

/// Output of `bus_publish_v1`. `published` is `PublishResult::accepted`
/// (total records appended, summed across every partition the batch
/// touched — a batch can span more than one). `schema_rejected` is
/// `PublishResult::schema_rejected` (SUM/tentabus/PLAN-F3.md §4.5) — records
/// diverted to `__dlq.<topic>` by `validation = dlq`, quarantined rather
/// than appended. `#[cbor(default)]` so an older host that never sent this
/// key (pre-F3) decodes as `0` (no schema enforcement ran), and an older
/// addon SDK that only knows about `published` still decodes a payload from
/// a host that DOES send it (minicbor's `#[cbor(map)]` skips unrecognized
/// keys) — see `tests::old_payload_without_schema_rejected_decodes_as_zero`/
/// `tests::old_decoder_tolerates_an_unknown_schema_rejected_key`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct BusPublishOutput {
    #[n(0)]
    pub published: u32,
    #[n(1)]
    #[cbor(default)]
    pub schema_rejected: u32,
}

// -----------------------------------------------------------------------------
// bus_consume_open_v1
// -----------------------------------------------------------------------------

/// Input for `bus_consume_open_v1`. `commit_mode` mirrors
/// `bus_consume`'s flow-node config values (`"auto_after_success"` |
/// `"explicit"` | `"at_most_once"`); absent defaults to
/// `"auto_after_success"` on the host side, same as the flow node.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct BusConsumeOpenInput {
    #[n(0)]
    pub topics: Vec<String>,
    #[n(1)]
    pub group: String,
    #[n(2)]
    pub commit_mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct BusConsumeOpenOutput {
    #[n(0)]
    pub consumer_id: String,
}

// -----------------------------------------------------------------------------
// bus_consume_next_v1
// -----------------------------------------------------------------------------

/// Input for `bus_consume_next_v1`. `max_records` is clamped to 1000 on the
/// host side (PLAN §6.4 batch ceiling); `max_wait_ms` is clamped to 5000 ms
/// (same ceiling `stream_next_v1` uses).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct BusConsumeNextInput {
    #[n(0)]
    pub consumer_id: String,
    #[n(1)]
    pub max_records: u32,
    #[n(2)]
    pub max_wait_ms: u32,
}

/// Discriminator for the `bus_consume_next_v1` output variant.
pub const BUS_CONSUME_NEXT_KIND_BATCH: &str = "batch";
pub const BUS_CONSUME_NEXT_KIND_EMPTY: &str = "empty";

/// Output of `bus_consume_next_v1`. `kind == "empty"` means the long-poll
/// window elapsed with nothing new (a normal, expected outcome — the addon
/// should call `next` again), never an error.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct BusConsumeNextOutput {
    #[n(0)]
    pub kind: String,
    #[n(1)]
    pub records: Vec<BusRecordOut>,
}

impl BusConsumeNextOutput {
    pub fn batch(records: Vec<BusRecordOut>) -> Self {
        Self {
            kind: BUS_CONSUME_NEXT_KIND_BATCH.to_string(),
            records,
        }
    }

    pub fn empty() -> Self {
        Self {
            kind: BUS_CONSUME_NEXT_KIND_EMPTY.to_string(),
            records: Vec::new(),
        }
    }
}

// -----------------------------------------------------------------------------
// bus_consume_commit_v1
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct BusOffsetEntry {
    #[n(0)]
    pub topic: String,
    #[n(1)]
    pub partition: u32,
    #[n(2)]
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct BusConsumeCommitInput {
    #[n(0)]
    pub consumer_id: String,
    #[n(1)]
    pub offsets: Vec<BusOffsetEntry>,
}

#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct BusConsumeCommitOutput {
    #[n(0)]
    pub committed: bool,
}

// -----------------------------------------------------------------------------
// bus_consume_close_v1
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct BusConsumeCloseInput {
    #[n(0)]
    pub consumer_id: String,
}

#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct BusConsumeCloseOutput {
    #[n(0)]
    pub closed: bool,
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
    fn roundtrip_publish_input_and_output() {
        roundtrip(&BusPublishInput {
            topic: "orders.created".into(),
            records: vec![BusRecordIn {
                key: Some(b"order-1".to_vec()),
                headers: vec![BusHeader {
                    name: "content-type".into(),
                    value: b"application/json".to_vec(),
                }],
                payload: b"{\"id\":1}".to_vec(),
            }],
            create_if_missing: Some(true),
        });
        roundtrip(&BusPublishOutput {
            published: 1,
            schema_rejected: 2,
        });
    }

    /// Mirror of the pre-schema-registry `BusPublishOutput` (key 0 only),
    /// used only to prove forward-compatible decode of an old host's
    /// payload (finding #4).
    #[derive(Encode, Decode)]
    #[cbor(map)]
    struct BusPublishOutputLegacy {
        #[n(0)]
        published: u32,
    }

    #[test]
    fn old_payload_without_schema_rejected_decodes_as_zero() {
        let old = BusPublishOutputLegacy { published: 7 };
        let mut buf = Vec::new();
        minicbor::encode(&old, &mut buf).unwrap();

        let back: BusPublishOutput = minicbor::decode(&buf).unwrap();
        assert_eq!(back.published, 7);
        assert_eq!(back.schema_rejected, 0);
    }

    #[test]
    fn old_decoder_tolerates_an_unknown_schema_rejected_key() {
        // An addon SDK built against the pre-F3 wire only knows key 0; a
        // NEW host that sends `schema_rejected` (key 1) too must not break
        // that old decoder — minicbor's `#[cbor(map)]` skips unrecognized
        // keys rather than erroring.
        let new = BusPublishOutput {
            published: 3,
            schema_rejected: 1,
        };
        let mut buf = Vec::new();
        minicbor::encode(&new, &mut buf).unwrap();

        let back: BusPublishOutputLegacy = minicbor::decode(&buf).unwrap();
        assert_eq!(back.published, 3);
    }

    #[test]
    fn roundtrip_publish_input_minimal() {
        roundtrip(&BusPublishInput {
            topic: "orders.created".into(),
            records: vec![BusRecordIn {
                key: None,
                headers: Vec::new(),
                payload: b"x".to_vec(),
            }],
            create_if_missing: None,
        });
    }

    #[test]
    fn roundtrip_consume_open() {
        roundtrip(&BusConsumeOpenInput {
            topics: vec!["orders.created".into()],
            group: "billing".into(),
            commit_mode: Some("explicit".into()),
        });
        roundtrip(&BusConsumeOpenOutput {
            consumer_id: "busc_00000000-0000-0000-0000-000000000000".into(),
        });
    }

    #[test]
    fn roundtrip_consume_next_input() {
        roundtrip(&BusConsumeNextInput {
            consumer_id: "busc_00000000-0000-0000-0000-000000000000".into(),
            max_records: 1000,
            max_wait_ms: 1000,
        });
    }

    #[test]
    fn roundtrip_consume_next_output_batch_and_empty() {
        roundtrip(&BusConsumeNextOutput::batch(vec![BusRecordOut {
            topic: "orders.created".into(),
            partition: 0,
            offset: 42,
            timestamp_ms: 1_700_000_000_000,
            key: Some(b"order-1".to_vec()),
            headers: Vec::new(),
            payload: b"{\"id\":1}".to_vec(),
        }]));
        roundtrip(&BusConsumeNextOutput::empty());
    }

    #[test]
    fn roundtrip_consume_commit_and_close() {
        roundtrip(&BusConsumeCommitInput {
            consumer_id: "busc_00000000-0000-0000-0000-000000000000".into(),
            offsets: vec![BusOffsetEntry {
                topic: "orders.created".into(),
                partition: 0,
                offset: 43,
            }],
        });
        roundtrip(&BusConsumeCommitOutput { committed: true });
        roundtrip(&BusConsumeCloseInput {
            consumer_id: "busc_00000000-0000-0000-0000-000000000000".into(),
        });
        roundtrip(&BusConsumeCloseOutput { closed: true });
    }
}
