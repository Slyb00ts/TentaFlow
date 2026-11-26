// =============================================================================
// File: bus/replication/forward.rs — forwarding a publish to the partition
//       leader (TB-04 K2)
// =============================================================================
//
// A node that receives a write for a partition it only follows hands the
// records to that partition's leader over `ALPN_BUS` (`ProduceForward` /
// `ProduceForwardReply`, one fresh bi-stream per request) instead of
// refusing with `NotLeader`.
//
// The leader runs the whole publish pipeline on the forwarded request —
// authorization, field policies, schema, quarantine, dedup, quotas and the
// `tf.*` provenance headers — so every decision about a record is taken where
// its log lives, exactly as for a local publish. The sender has only routed
// the records to partitions and (as for any publish) validated the batch up
// front. A forwarded request is never forwarded again: a receiver that no
// longer leads answers `NotLeader`, so a request cannot loop between nodes.
// The one write a leader makes on behalf of a forwarded request is the
// quarantine copy of a record its schema rejected: that is a broker write of
// its own and goes to the leader of the `__dlq.*` partition, whichever node
// that is, rather than being dropped.
//
// SENDER: `BusService::forward_to_leaders` (below) — one request per leader,
// one retry after `NotLeader` once the assignment moved, a bounded wait while
// an election has no leader yet. RECEIVER: `ForwardedPublishHandler`, which
// `ReplicationManager::accept_produce_forward` calls after it has checked
// the instance and environment of the request (the mesh has already admitted
// the peer as a trusted node of this environment; the ORIGINAL actor is
// authorized again on the leader, which is what decides).
//
// BLOCKING: both halves run on blocking threads (`publish` is synchronous and
// its callers already sit in `spawn_blocking`, `block_in_place` or a std
// thread). The sender never calls `block_on`; it spawns the network round
// trip onto the runtime and waits on a std channel.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_bytes::ByteBuf;

use crate::auth::actor::ActorKind;
use crate::bus::instance::BusInstanceId;
use crate::bus::replication::frames::{
    ConsumeForward, ConsumeForwardReply, ForwardAck, ForwardError, ForwardGroup, ForwardProducer,
    ForwardRecord, ForwardResult, ProduceForward, ProduceForwardReply,
};
use crate::bus::topics::RESERVED_PREFIX;
use crate::bus::{
    deny, dlq, producer::ProducerIdentity, BusAction, BusCallContext, BusService, BusServiceError,
    PartitionAck, PartitionRole, PublishBatch, PublishRecord, PublishResult,
    ReplicationCoordinator,
};

/// How long a publish waits for a partition with no known leader (an election
/// in progress), or for the assignment to move after a leader refused it,
/// before giving up with `NotLeader`.
const LEADER_WAIT: Duration = Duration::from_secs(3);

/// How often a publish whose leader refused it asks again where the partition
/// is led, whether or not this node has applied a new assignment. The role
/// this node holds changes only when the poll applies a row (up to a second
/// after the row is admitted), but the ledger row itself is there at once:
/// `refused_leader_moved` reads it, so a move is followed without that wait.
const LEADER_RECHECK: Duration = Duration::from_millis(100);

/// How long a publish first pauses before asking again a leader that knows no
/// leader for the partition yet (it has not materialized the assignment that
/// names it), and how far the pause doubles up to. A leader that waits for
/// the topic rows itself (`BusService::forwarded_topic_config`) usually has
/// them by the first answer; this is the fallback for one still behind.
const UNAWARE_LEADER_RETRY_FIRST: Duration = Duration::from_millis(50);
const UNAWARE_LEADER_RETRY_MAX: Duration = Duration::from_millis(800);

/// Slack added to the leader's own publish deadline before the sender stops
/// waiting for the reply.
const REPLY_MARGIN: Duration = Duration::from_secs(5);

/// What the leader keeps back from the time a sender waits, for the
/// reply itself and the scheduling around the work.
const REPLY_RESERVE: Duration = Duration::from_secs(1);

/// `ForwardError::code` of a refusal made by the forwarding layer itself (not
/// by the publish pipeline): `unknown_instance`, `environment_mismatch` or
/// `handler_failed`. Any member of the instance and environment may forward;
/// the sender is not checked to be a replica.
pub const CODE_FORWARD_REFUSED: &str = "bus.forward_refused";

/// Why a forwarding attempt produced no answer from the leader. What the
/// caller may do next depends on whether the request left this node.
#[derive(Debug, thiserror::Error)]
pub enum ForwardUnavailable {
    #[error("this coordinator does not forward publishes")]
    Unsupported,
    #[error("peer {0} does not accept forwarded publishes")]
    PeerCannotForward(String),
    /// Nothing was sent: no connection, no stream, a request that could not
    /// be encoded, or the leader moved before anything left. The leader
    /// cannot have appended anything, so the caller falls back to the
    /// `NotLeader` it would have returned without forwarding, or retries at
    /// the new leader.
    #[error("nothing was sent to {node}: {reason}")]
    NotSent { node: String, reason: String },
    /// The request left this node and no answer came back (stream reset,
    /// reply timeout, leader lost). The leader may have appended the
    /// records: never retried, never reported as `NotLeader`.
    #[error("no answer from {node} after the request was sent: {reason}")]
    OutcomeUnknown { node: String, reason: String },
}

/// The leader's half of forwarding: runs one forwarded publish or read.
/// Blocking; implemented by `BusService`.
pub trait ForwardedPublishHandler: Send + Sync {
    fn publish_forwarded(&self, from: &str, request: ProduceForward) -> ProduceForwardReply;
    fn consume_forwarded(&self, from: &str, request: ConsumeForward) -> ConsumeForwardReply;
    /// Why leadership of `topic` must not be moved now, as a stable machine
    /// value (`consumer_group_paused`); `None` when it may. Asked of the
    /// leader before it runs a transfer, whoever asked for it: only the node
    /// that leads a topic knows its paused groups. Defaulted to `None`.
    fn transfer_blocker(&self, org_id: &str, topic: &str) -> Option<String> {
        let _ = (org_id, topic);
        None
    }
}

/// How a publish reached this node, as far as routing and trust go.
#[derive(Debug, Clone, Copy)]
pub(in crate::bus) enum PublishSource<'a> {
    /// A caller on this node: records for partitions led elsewhere are
    /// forwarded to their leaders.
    Local,
    /// `Local`, with the instant the forwarding has to be over by: a write
    /// made on behalf of a request whose sender stops waiting then
    /// (`quarantine_by`).
    LocalWithin { deadline: Instant },
    /// Records for partitions that turned out to be led here after a
    /// forwarding attempt: never forwarded.
    LocalOnly,
    /// Forwarded here by `from`: never forwarded again. `quarantine_by` is
    /// the instant the work besides the append (the quarantine copy of a
    /// record the schema rejects) has to be over by, so the sender is
    /// answered before it gives up on a write that landed.
    Forwarded {
        from: &'a str,
        quarantine_by: Option<Instant>,
    },
}

impl PublishSource<'_> {
    pub(in crate::bus) fn may_forward(self) -> bool {
        matches!(
            self,
            PublishSource::Local | PublishSource::LocalWithin { .. }
        )
    }

    /// The source of the quarantine copy a publish of this source makes into
    /// the dead-letter topic: a write of its own, made by the broker, which
    /// goes to the DLQ partition's leader whoever sent the records and is
    /// bound by the request's own deadline. It cannot loop: the DLQ topic is
    /// never validated.
    pub(in crate::bus) fn for_quarantine(self) -> PublishSource<'static> {
        match self {
            PublishSource::Forwarded {
                quarantine_by: Some(deadline),
                ..
            } => PublishSource::LocalWithin { deadline },
            _ => PublishSource::Local,
        }
    }

    /// The instant forwarding must be over by, when there is one.
    pub(in crate::bus) fn deadline(self) -> Option<Instant> {
        match self {
            PublishSource::LocalWithin { deadline } => Some(deadline),
            _ => None,
        }
    }

    /// The audit detail naming the forwarding peer of a refusal.
    pub(in crate::bus) fn audit_note(self) -> Option<String> {
        match self {
            PublishSource::Forwarded { from, .. } => Some(format!("forwarded_from={from}")),
            _ => None,
        }
    }
}

// ===== Records and context ==================================================

fn record_to_wire(r: &PublishRecord) -> ForwardRecord {
    ForwardRecord {
        key: r.key.as_ref().map(|k| ByteBuf::from(k.to_vec())),
        headers: r
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), ByteBuf::from(value.to_vec())))
            .collect(),
        payload: ByteBuf::from(r.payload.to_vec()),
        timestamp_ms: r.timestamp_ms,
        schema_id: r.schema_id,
    }
}

fn record_from_wire(r: ForwardRecord) -> PublishRecord {
    PublishRecord {
        key: r.key.map(|k| Bytes::from(k.into_vec())),
        headers: r
            .headers
            .into_iter()
            .map(|(name, value)| (name, Bytes::from(value.into_vec())))
            .collect(),
        payload: Bytes::from(r.payload.into_vec()),
        timestamp_ms: r.timestamp_ms,
        schema_id: r.schema_id,
    }
}

fn producer_to_wire(p: &ProducerIdentity) -> ForwardProducer {
    ForwardProducer {
        producer_id: p.producer_id.clone(),
        epoch: p.epoch,
        base_seq: p.base_seq,
    }
}

fn producer_from_wire(p: ForwardProducer) -> ProducerIdentity {
    ProducerIdentity {
        producer_id: p.producer_id,
        epoch: p.epoch,
        base_seq: p.base_seq,
    }
}

fn ack_to_wire(a: &PartitionAck) -> ForwardAck {
    ForwardAck {
        partition: a.partition,
        base_offset: a.base_offset,
        accepted: a.accepted,
    }
}

fn ack_from_wire(a: ForwardAck) -> PartitionAck {
    PartitionAck {
        partition: a.partition,
        base_offset: a.base_offset,
        accepted: a.accepted,
    }
}

fn result_to_wire(r: &PublishResult) -> ForwardResult {
    ForwardResult {
        duplicate: r.duplicate,
        accepted: r.accepted,
        deduplicated: r.deduplicated,
        partitions: r.partitions.iter().map(ack_to_wire).collect(),
        schema_rejected: r.schema_rejected,
        schema_dropped: r.schema_dropped,
    }
}

// ===== Errors ===============================================================

fn wire_error(code: &str, message: String) -> ForwardError {
    ForwardError {
        code: code.to_string(),
        message,
        ..ForwardError::default()
    }
}

/// A refusal made by the forwarding layer (`CODE_FORWARD_REFUSED`).
pub(crate) fn refusal(reason: &str) -> ProduceForwardReply {
    ProduceForwardReply {
        result: None,
        error: Some(ForwardError {
            texts: vec![reason.to_string()],
            ..wire_error(CODE_FORWARD_REFUSED, format!("forward refused: {reason}"))
        }),
    }
}

/// `NotLeader` as the forwarding layer reports it for a partition this node
/// does not (yet) hold an assignment for.
pub(crate) fn not_leader_reply() -> ProduceForwardReply {
    ProduceForwardReply {
        result: None,
        error: Some(error_to_wire(&BusServiceError::NotLeader {
            leader_node_id: None,
            leader_epoch: 0,
        })),
    }
}

const KNOWN_STATIC: &[&str] = &[
    "messages",
    "bytes",
    "read",
    "write",
    "json",
    "xml",
    "hl7v2",
    "json_schema",
    "avro",
    "protobuf",
    "thrift",
    "xsd",
    "hl7v2_profile",
    "produce",
    "consume",
    "admin",
];

/// The `&'static str` the error variants carry, recovered from the wire: a
/// value this build does not know becomes `"unknown"` instead of a leak.
fn static_str(s: &str) -> &'static str {
    KNOWN_STATIC
        .iter()
        .copied()
        .find(|known| *known == s)
        .unwrap_or("unknown")
}

fn text(e: &ForwardError, i: usize) -> String {
    e.texts.get(i).cloned().unwrap_or_default()
}

fn number(e: &ForwardError, i: usize) -> u64 {
    e.numbers.get(i).copied().unwrap_or_default()
}

/// The wire form of `err`. A `PartialPublish` is flattened to its source plus
/// the partitions that landed.
pub(crate) fn error_to_wire(err: &BusServiceError) -> ForwardError {
    use BusServiceError as E;
    let msg = err.to_string();
    let with = |code: &str, texts: Vec<String>, numbers: Vec<u64>| ForwardError {
        texts,
        numbers,
        ..wire_error(code, msg.clone())
    };
    match err {
        E::PartialPublish { acked, source } => ForwardError {
            acked: acked.iter().map(ack_to_wire).collect(),
            ..error_to_wire(source)
        },
        E::PermissionDenied { action, topic } => with(
            "bus.permission_denied",
            vec![action.to_string(), topic.clone()],
            vec![],
        ),
        E::QuotaExceeded { retry_after_ms } => {
            with("bus.quota_exceeded", vec![], vec![*retry_after_ms as u64])
        }
        E::QuotaRequestTooLarge {
            unit,
            amount,
            capacity,
        } => with(
            "bus.quota_request_too_large",
            vec![unit.to_string()],
            vec![*amount, *capacity],
        ),
        E::MaxBytesTotalExceeded {
            org_id,
            max,
            current,
            requested,
        } => with(
            "bus.max_bytes_total_exceeded",
            vec![org_id.clone()],
            vec![*max, *current, *requested],
        ),
        E::Throttled { retry_after_ms } => {
            with("bus.throttled", vec![], vec![*retry_after_ms as u64])
        }
        E::PayloadTooLarge {
            len,
            max_inline_bytes,
        } => with(
            "bus.payload_too_large",
            vec![],
            vec![*len as u64, *max_inline_bytes as u64],
        ),
        E::DedupKeyRequired { topic } => {
            with("bus.dedup_key_required", vec![topic.clone()], vec![])
        }
        E::ProducerFenced { current_epoch } => {
            with("bus.producer_fenced", vec![], vec![*current_epoch as u64])
        }
        E::EnvironmentMismatch {
            topic_env,
            node_env,
        } => with(
            "bus.environment_mismatch",
            vec![
                topic_env.as_str().to_string(),
                node_env.as_str().to_string(),
            ],
            vec![],
        ),
        E::InvalidArgument(m) => with("bus.invalid_argument", vec![m.clone()], vec![]),
        E::TopicNotFound { name } => with("bus.topic_not_found", vec![name.clone()], vec![]),
        E::NotLeader {
            leader_node_id,
            leader_epoch,
        } => with(
            "bus.not_leader",
            leader_node_id.iter().cloned().collect(),
            vec![*leader_epoch as u64],
        ),
        E::NotEnoughReplicas { isr, required } => with(
            "bus.not_enough_replicas",
            vec![],
            vec![*isr as u64, *required as u64],
        ),
        E::AckTimeout { acked, required } => with(
            "bus.ack_timeout",
            vec![],
            vec![*acked as u64, *required as u64],
        ),
        E::ForwardOutcomeUnknown { leader_node_id } => with(
            "bus.forward_outcome_unknown",
            vec![leader_node_id.clone()],
            vec![],
        ),
        E::PartitionUnavailable { reason } => {
            with("bus.partition_unavailable", vec![reason.clone()], vec![])
        }
        E::FieldNotAllowed { topic, fields } => {
            let mut texts = vec![topic.clone()];
            texts.extend(fields.iter().cloned());
            with("bus.field_not_allowed", texts, vec![])
        }
        E::RequiredFieldMissing { topic, fields } => {
            let mut texts = vec![topic.clone()];
            texts.extend(fields.iter().cloned());
            with("bus.required_field_missing", texts, vec![])
        }
        E::FieldPolicyPayloadMalformed { topic, format } => with(
            "bus.field_policy_payload_malformed",
            vec![topic.clone(), format.to_string()],
            vec![],
        ),
        E::FieldPolicyChanged { topic } => {
            with("bus.field_policy_changed", vec![topic.clone()], vec![])
        }
        E::KeyNeedsTopicWideRule { topic, direction } => with(
            "bus.key_needs_topic_wide_rule",
            vec![topic.clone(), direction.to_string()],
            vec![],
        ),
        E::SchemaViolation {
            topic,
            subject,
            version,
            detail,
        } => with(
            "bus.schema_violation",
            vec![topic.clone(), subject.clone(), detail.clone()],
            vec![*version as u64],
        ),
        E::SchemaNotFound { subject } => {
            with("bus.schema_not_found", vec![subject.clone()], vec![])
        }
        E::PartitionPoisoned { topic, partition } => with(
            "bus.partition_poisoned",
            vec![topic.clone()],
            vec![*partition as u64],
        ),
        E::OffsetOutOfRange {
            topic,
            partition,
            requested,
            earliest,
            latest,
        } => with(
            "bus.offset_out_of_range",
            vec![topic.clone()],
            vec![*partition as u64, *requested, *earliest, *latest],
        ),
        E::GroupNotFound { group, topic } => with(
            "bus.group_not_found",
            vec![group.clone(), topic.clone()],
            vec![],
        ),
        E::RemoteFailure => with("bus.remote_error", vec![], vec![]),
        E::GroupPaused { group, topic } => with(
            "bus.group_paused",
            vec![group.clone(), topic.clone()],
            vec![],
        ),
        E::MaxGroupsExceeded {
            org_id,
            max,
            current,
        } => with(
            "bus.max_groups_exceeded",
            vec![org_id.clone()],
            vec![*max as u64, *current as u64],
        ),
        // Storage and engine failures of the leader: nothing the sender can
        // act on beyond "the partition cannot serve this right now". The
        // detail (it can name tables, paths or SQL) stays in the leader's
        // log and never reaches a caller on another node.
        other => {
            tracing::warn!(error = %other, "a forwarded request failed on this leader");
            ForwardError {
                texts: vec![],
                numbers: vec![],
                ..wire_error(
                    "bus.remote_error",
                    "the partition leader could not serve the request".to_string(),
                )
            }
        }
    }
}

/// The `BusServiceError` a `ForwardError` stands for. Codes this build does
/// not know, and failures internal to the leader, become `RemoteFailure`: the
/// leader's own message may hold internal detail, and neither is a state that
/// passes. A `CODE_FORWARD_REFUSED`
/// error never reaches here: `forward_to_leaders` separates it first.
pub(crate) fn error_from_wire(e: ForwardError) -> BusServiceError {
    use BusServiceError as E;
    let source = match e.code.as_str() {
        "bus.permission_denied" => E::PermissionDenied {
            action: static_str(&text(&e, 0)),
            topic: text(&e, 1),
        },
        "bus.quota_exceeded" => E::QuotaExceeded {
            retry_after_ms: number(&e, 0) as u32,
        },
        "bus.quota_request_too_large" => E::QuotaRequestTooLarge {
            unit: static_str(&text(&e, 0)),
            amount: number(&e, 0),
            capacity: number(&e, 1),
        },
        "bus.max_bytes_total_exceeded" => E::MaxBytesTotalExceeded {
            org_id: text(&e, 0),
            max: number(&e, 0),
            current: number(&e, 1),
            requested: number(&e, 2),
        },
        "bus.throttled" => E::Throttled {
            retry_after_ms: number(&e, 0) as u32,
        },
        "bus.payload_too_large" => E::PayloadTooLarge {
            len: number(&e, 0) as usize,
            max_inline_bytes: number(&e, 1) as usize,
        },
        "bus.dedup_key_required" => E::DedupKeyRequired { topic: text(&e, 0) },
        "bus.producer_fenced" => E::ProducerFenced {
            current_epoch: number(&e, 0) as u32,
        },
        "bus.environment_mismatch" => E::EnvironmentMismatch {
            topic_env: tentaflow_protocol::environment::NodeEnvironment::parse(&text(&e, 0))
                .unwrap_or_default(),
            node_env: tentaflow_protocol::environment::NodeEnvironment::parse(&text(&e, 1))
                .unwrap_or_default(),
        },
        "bus.invalid_argument" => E::InvalidArgument(text(&e, 0)),
        "bus.topic_not_found" => E::TopicNotFound { name: text(&e, 0) },
        "bus.not_leader" => E::NotLeader {
            leader_node_id: e.texts.first().cloned(),
            leader_epoch: number(&e, 0) as u32,
        },
        "bus.not_enough_replicas" => E::NotEnoughReplicas {
            isr: number(&e, 0) as u32,
            required: number(&e, 1) as u32,
        },
        "bus.ack_timeout" => E::AckTimeout {
            acked: number(&e, 0) as u32,
            required: number(&e, 1) as u32,
        },
        "bus.forward_outcome_unknown" => E::ForwardOutcomeUnknown {
            leader_node_id: text(&e, 0),
        },
        "bus.partition_unavailable" => E::PartitionUnavailable {
            reason: text(&e, 0),
        },
        "bus.field_not_allowed" => E::FieldNotAllowed {
            topic: text(&e, 0),
            fields: e.texts.iter().skip(1).cloned().collect(),
        },
        "bus.required_field_missing" => E::RequiredFieldMissing {
            topic: text(&e, 0),
            fields: e.texts.iter().skip(1).cloned().collect(),
        },
        "bus.field_policy_payload_malformed" => E::FieldPolicyPayloadMalformed {
            topic: text(&e, 0),
            format: static_str(&text(&e, 1)),
        },
        "bus.field_policy_changed" => E::FieldPolicyChanged { topic: text(&e, 0) },
        "bus.key_needs_topic_wide_rule" => E::KeyNeedsTopicWideRule {
            topic: text(&e, 0),
            direction: static_str(&text(&e, 1)),
        },
        "bus.schema_violation" => E::SchemaViolation {
            topic: text(&e, 0),
            subject: text(&e, 1),
            version: number(&e, 0) as u32,
            detail: text(&e, 2),
        },
        "bus.schema_not_found" => E::SchemaNotFound {
            subject: text(&e, 0),
        },
        "bus.partition_poisoned" => E::PartitionPoisoned {
            topic: text(&e, 0),
            partition: number(&e, 0) as u32,
        },
        "bus.offset_out_of_range" => E::OffsetOutOfRange {
            topic: text(&e, 0),
            partition: number(&e, 0) as u32,
            requested: number(&e, 1),
            earliest: number(&e, 2),
            latest: number(&e, 3),
        },
        "bus.group_not_found" => E::GroupNotFound {
            group: text(&e, 0),
            topic: text(&e, 1),
        },
        "bus.group_paused" => E::GroupPaused {
            group: text(&e, 0),
            topic: text(&e, 1),
        },
        "bus.max_groups_exceeded" => E::MaxGroupsExceeded {
            org_id: text(&e, 0),
            max: number(&e, 0) as u32,
            current: number(&e, 1) as u32,
        },
        // `bus.remote_error`, and a code a newer build sent: the leader
        // failed in a way this node cannot name. Not a state that passes.
        other => {
            if other != "bus.remote_error" {
                tracing::warn!(
                    code = other,
                    "a forwarded request failed with an unknown code"
                );
            }
            E::RemoteFailure
        }
    };
    if e.acked.is_empty() {
        source
    } else {
        E::PartialPublish {
            acked: e.acked.into_iter().map(ack_from_wire).collect(),
            source: Box::new(source),
        }
    }
}

// ===== Receiver =============================================================

impl ForwardedPublishHandler for BusService {
    fn publish_forwarded(&self, from: &str, request: ProduceForward) -> ProduceForwardReply {
        match self.run_forwarded_publish(from, request) {
            Ok(result) => ProduceForwardReply {
                result: Some(result_to_wire(&result)),
                error: None,
            },
            Err(e) => ProduceForwardReply {
                result: None,
                error: Some(error_to_wire(&e)),
            },
        }
    }

    fn consume_forwarded(&self, from: &str, request: ConsumeForward) -> ConsumeForwardReply {
        self.run_forwarded_consume(from, request)
    }

    fn transfer_blocker(&self, org_id: &str, topic: &str) -> Option<String> {
        self.leader_move_blocker(org_id, topic)
    }
}

/// How long a node that took a forwarded request for a topic it has not caught
/// up with waits for the topic row (or the partitions an update added) to
/// arrive before it tells the sender it is not ready.
const TOPIC_ROWS_WAIT: Duration = Duration::from_millis(500);

/// How often that wait looks again.
const TOPIC_ROWS_POLL: Duration = Duration::from_millis(10);

impl BusService {
    /// The topic a forwarded request is for, as this node (the leader) holds
    /// it, checked against the incarnation and partition count the sender
    /// stated (`0` for either: not stated, nothing to check).
    ///
    /// A topic placed on this node a moment ago, or grown by an
    /// `update_topic`, reaches the leader through the ledger like the
    /// placement does and can lag the sender, which wrote both itself. This
    /// node then waits briefly for its own rows; one that is still behind
    /// answers `NotLeader` naming no leader, which the sender asks again
    /// after a pause (the same answer a leader gives that has the topic but
    /// not yet its placement), instead of a `TopicNotFound` or an
    /// out-of-range partition for a topic that does exist. A sender on an
    /// OLDER incarnation than this node's is a different matter: its write is
    /// for a topic that was deleted since, and never lands in the new one.
    pub(in crate::bus) fn forwarded_topic_config(
        &self,
        org_id: &str,
        topic: &str,
        sender_generation: u64,
        sender_partitions: u32,
    ) -> Result<crate::bus::topics::TopicConfig, BusServiceError> {
        let deadline = Instant::now() + TOPIC_ROWS_WAIT;
        loop {
            let behind = match self.topic_config(org_id, topic) {
                Ok(cfg) => {
                    if sender_generation != 0 && cfg.generation > sender_generation {
                        return Err(BusServiceError::PartitionUnavailable {
                            reason: format!(
                                "the forwarded write is for incarnation {sender_generation} \
                                 of topic '{topic}', which was replaced by {}",
                                cfg.generation
                            ),
                        });
                    }
                    let behind = sender_generation != 0
                        && (cfg.generation < sender_generation
                            || cfg.partitions < sender_partitions);
                    if !behind {
                        return Ok(cfg);
                    }
                    behind
                }
                Err(BusServiceError::TopicNotFound { .. }) if sender_generation != 0 => true,
                Err(e) => return Err(e),
            };
            if behind && Instant::now() >= deadline {
                return Err(BusServiceError::NotLeader {
                    leader_node_id: None,
                    leader_epoch: 0,
                });
            }
            std::thread::sleep(TOPIC_ROWS_POLL);
        }
    }

    fn run_forwarded_publish(
        &self,
        from: &str,
        request: ProduceForward,
    ) -> Result<PublishResult, BusServiceError> {
        let instance_id = BusInstanceId::parse(&request.instance_id)
            .map_err(|e| BusServiceError::InvalidArgument(format!("instance id: {e}")))?;
        let actor_kind = ActorKind::parse(&request.actor_kind).ok_or_else(|| {
            BusServiceError::InvalidArgument("unknown actor kind in a forwarded publish".into())
        })?;
        let ctx = BusCallContext {
            instance_id,
            org_id: request.org_id,
            actor: request.actor,
            actor_kind,
            correlation_id: request.correlation_id,
            origin: request.origin,
        };
        let topic = request.topic;
        // The sender waits `reply_within_ms` for the answer. Besides the
        // append of its own groups, the quarantine copy of a record the
        // schema rejects must be over before then.
        let quarantine_by = (request.reply_within_ms > 0).then(|| {
            let waits = Duration::from_millis(u64::from(request.reply_within_ms));
            let append = self.publish_ack_timeout * request.groups.len().max(1) as u32;
            (Instant::now() + waits)
                .checked_sub(append + REPLY_RESERVE)
                .unwrap_or_else(Instant::now)
        });
        let source = PublishSource::Forwarded {
            from,
            quarantine_by,
        };

        // The `SYSTEM_ACTOR` sentinel is the broker's own writer of `__*`
        // topics: the authorizer lets it through there without asking. A peer
        // vouching for it anywhere else is refused whatever the authorizer
        // would say. Any other actor — a system component named by its own id
        // included — is judged by the authorizer like a local caller of that
        // kind.
        let system = ctx.actor.as_deref() == Some(crate::services::bus_authorizer::SYSTEM_ACTOR);
        if system && !topic.starts_with(RESERVED_PREFIX) {
            self.audit_windowed(
                &ctx,
                "bus.produce.denied",
                Some(&topic),
                source.audit_note().as_deref(),
            );
            return Err(deny(BusAction::Produce, &topic));
        }

        self.check_instance(&ctx)?;
        let cfg = self.forwarded_topic_config(
            &ctx.org_id,
            &topic,
            request.topic_generation,
            request.partitions,
        )?;
        if request.groups.is_empty() {
            return Err(BusServiceError::InvalidArgument(
                "forwarded publish has no partition groups".into(),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for group in &request.groups {
            if group.partition >= cfg.partitions || !seen.insert(group.partition) {
                return Err(BusServiceError::InvalidArgument(format!(
                    "forwarded publish names partition {} of '{topic}', which has {}",
                    group.partition, cfg.partitions
                )));
            }
        }
        let broker_dead_letter =
            request.broker_dead_letter && topic.starts_with(dlq::DLQ_TOPIC_PREFIX);
        let producer = request.producer.map(producer_from_wire);

        // One call for the whole request, so the leader's checks and its
        // quota charge happen once, exactly as for a local batch that
        // spans these partitions; the sender chose each record's partition.
        let mut records = Vec::new();
        let mut tags = Vec::new();
        for ForwardGroup {
            partition,
            records: group,
        } in request.groups
        {
            for record in group {
                records.push(record_from_wire(record));
                tags.push(partition);
            }
        }
        self.publish_routed(
            &ctx,
            &topic,
            PublishBatch {
                partition: None,
                producer,
                records,
            },
            None,
            broker_dead_letter,
            source,
            Some(tags),
        )
    }
}

// ===== Sender ===============================================================

/// What a publish carries to every leader it forwards to.
pub(in crate::bus) struct ForwardJob<'a> {
    pub ctx: &'a BusCallContext,
    pub topic: &'a str,
    pub producer: Option<&'a ProducerIdentity>,
    pub broker_dead_letter: bool,
    /// The instant the forwarding has to be over by, when the publish serves
    /// a request that is only waited for so long.
    pub deadline: Option<Instant>,
    /// The incarnation and partition count of the topic as the sender
    /// knows them, for the leader to check against its own
    /// (`BusService::forwarded_topic_config`).
    pub topic_generation: u64,
    pub partitions: u32,
}

/// The totals the forwarded groups add to the publish's own.
#[derive(Debug, Default)]
pub(in crate::bus) struct ForwardTotals {
    pub accepted: u32,
    pub deduplicated: u32,
    pub duplicate: bool,
    pub schema_rejected: u32,
    pub schema_dropped: u32,
}

/// A group of records waiting for its leader.
struct PendingGroup {
    partition: u32,
    records: Vec<PublishRecord>,
    /// How many times it was sent to a leader so far.
    sends: u8,
    /// The leader that answered `NotLeader` for it (or could not be reached
    /// while the partition moved): not asked again until the assignment
    /// names another one.
    refused_by: Option<String>,
    /// The leader it was last sent to answered `NotLeader` naming no leader
    /// at all: it has not caught up with the assignment yet (a topic placed
    /// on it a moment ago) or is in an election. It is asked again after
    /// a pause that doubles from `UNAWARE_LEADER_RETRY_FIRST`, until the wait
    /// for a leader runs out.
    target_unaware: bool,
    /// The leader it is sent to was named by the ledger row, not by this
    /// node's role (which still names the one that refused): that leader may
    /// not have applied the row itself yet.
    via_row: bool,
}

enum Reply {
    Landed(ForwardResult),
    Failed(ForwardError),
}

/// Where the records of one partition go.
pub(super) enum Route {
    Here,
    Remote { leader: String, epoch: u32 },
    Unknown,
}

/// What became of one request.
enum Sent {
    /// The leader answered and the groups are settled.
    Done,
    /// The groups go round again: the leader refused them or the assignment
    /// moved before anything was sent.
    Again(Vec<PendingGroup>),
}

/// The largest request body a forwarded write may take: a frame is capped at
/// `MAX_FRAME_BYTES`, and what the estimate below leaves out (field names,
/// the request envelope) is small next to this margin.
const REQUEST_BUDGET: usize = crate::bus::replication::frames::MAX_FRAME_BYTES - 256 * 1024;

/// A generous upper bound of the bytes `records` take in a `ForwardGroup`.
fn group_size(records: &[PublishRecord]) -> usize {
    records
        .iter()
        .map(|r| {
            r.payload.len()
                + r.key.as_ref().map_or(0, |k| k.len())
                + r.headers
                    .iter()
                    .map(|(name, value)| name.len() + value.len() + 24)
                    .sum::<usize>()
                + 96
        })
        .sum()
}

/// Refuses a write whose records for one partition are too big to forward in
/// one request, before any of the call has been appended. Several groups for
/// one leader are split across requests; one group is not, because it is one
/// partition's append.
pub(in crate::bus) fn ensure_forwardable(
    groups: &[(u32, Vec<PublishRecord>)],
) -> Result<(), BusServiceError> {
    for (partition, records) in groups {
        let size = group_size(records);
        if size > REQUEST_BUDGET {
            return Err(BusServiceError::InvalidArgument(format!(
                "the {} records for partition {partition} take about {size} bytes, more than \
                 the {REQUEST_BUDGET} a write handed to the partition's leader may carry",
                records.len()
            )));
        }
    }
    Ok(())
}

/// Greedily packs `groups` into requests of at most `REQUEST_BUDGET`.
fn chunk_groups(groups: Vec<PendingGroup>) -> Vec<Vec<PendingGroup>> {
    let mut chunks: Vec<Vec<PendingGroup>> = Vec::new();
    let mut used = 0usize;
    for group in groups {
        let size = group_size(&group.records);
        match chunks.last_mut() {
            Some(chunk) if used + size <= REQUEST_BUDGET => {
                used += size;
                chunk.push(group);
            }
            _ => {
                used = size;
                chunks.push(vec![group]);
            }
        }
    }
    chunks
}

impl BusService {
    /// Delivers `foreign` — the partition groups of a publish this node does
    /// not lead, as they arrived — to the leaders, and appends what landed to
    /// `acks`.
    ///
    /// One request per leader (several when the groups do not fit one
    /// frame). A leader that answers `NotLeader` before writing anything is
    /// asked again once, after the assignment moved; a partition with no
    /// known leader waits for one up to `LEADER_WAIT`, and so does a leader
    /// that answers it knows none (it has not materialized the assignment
    /// yet), which is asked again after a pause doubling from
    /// `UNAWARE_LEADER_RETRY_FIRST` to `UNAWARE_LEADER_RETRY_MAX`. A request
    /// that never
    /// left this node (no connection, leader moved meanwhile) falls back to
    /// the `NotLeader` this node would have returned without forwarding, or
    /// is retried at the new leader. A request that left and got no answer
    /// ends as `ForwardOutcomeUnknown` and is NOT retried: the leader may
    /// have appended it. The first failure ends the call: groups already
    /// delivered stay in `acks` for the caller's `PartialPublish`.
    pub(in crate::bus) fn forward_to_leaders(
        &self,
        coordinator: &Arc<dyn ReplicationCoordinator>,
        job: &ForwardJob<'_>,
        foreign: Vec<(u32, Vec<PublishRecord>)>,
        acks: &mut Vec<PartitionAck>,
    ) -> Result<ForwardTotals, BusServiceError> {
        let mut deadline = Instant::now() + LEADER_WAIT;
        if let Some(limit) = job.deadline {
            deadline = deadline.min(limit);
        }
        let local_node = coordinator.local_node_id();
        let mut totals = ForwardTotals::default();
        let mut pending: Vec<PendingGroup> = foreign
            .into_iter()
            .map(|(partition, records)| PendingGroup {
                partition,
                records,
                sends: 0,
                refused_by: None,
                target_unaware: false,
                via_row: false,
            })
            .collect();
        let mut last_refusal: Option<BusServiceError> = None;
        let mut unaware_pause = UNAWARE_LEADER_RETRY_FIRST;

        while !pending.is_empty() {
            // Read before the roles it has to explain, so a change landing
            // in between is not waited for.
            let generation = coordinator.assignment_generation();
            let mut by_leader: BTreeMap<String, (u32, Vec<PendingGroup>)> = BTreeMap::new();
            let mut waiting: Vec<PendingGroup> = Vec::new();
            for group in std::mem::take(&mut pending) {
                let (routed, via_row) =
                    target_of(coordinator, &local_node, &job.ctx.org_id, job.topic, &group);
                let mut group = group;
                group.via_row = via_row;
                match routed {
                    Route::Here => {
                        let outcome = self.publish_routed(
                            job.ctx,
                            job.topic,
                            PublishBatch {
                                partition: Some(group.partition),
                                producer: job.producer.cloned(),
                                records: group.records,
                            },
                            None,
                            job.broker_dead_letter,
                            PublishSource::LocalOnly,
                            None,
                        );
                        absorb_local(outcome, acks, &mut totals)?;
                    }
                    Route::Remote { leader, epoch }
                        if group.refused_by.as_deref() != Some(leader.as_str()) =>
                    {
                        by_leader
                            .entry(leader)
                            .or_insert_with(|| (epoch, Vec::new()))
                            .1
                            .push(group);
                    }
                    Route::Remote { .. } | Route::Unknown => waiting.push(group),
                }
            }

            for (leader, (epoch, groups)) in by_leader {
                for chunk in chunk_groups(groups) {
                    match self.send_chunk(
                        coordinator,
                        &local_node,
                        job,
                        &leader,
                        epoch,
                        chunk,
                        acks,
                        &mut totals,
                        &mut last_refusal,
                    )? {
                        Sent::Done => {}
                        Sent::Again(groups) => waiting.extend(groups),
                    }
                }
            }

            pending = waiting;
            if pending.is_empty() {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let unaware = pending.iter().any(|g| g.target_unaware);
            let pause = if unaware {
                let pause = remaining.min(unaware_pause);
                unaware_pause = (unaware_pause * 2).min(UNAWARE_LEADER_RETRY_MAX);
                pause
            } else {
                remaining.min(LEADER_RECHECK)
            };
            if remaining.is_zero() {
                return Err(last_refusal.unwrap_or(BusServiceError::NotLeader {
                    leader_node_id: None,
                    leader_epoch: 0,
                }));
            }
            coordinator.wait_for_assignment_change(generation, pause);
        }
        Ok(totals)
    }

    /// One request to `leader` for `groups`.
    #[allow(clippy::too_many_arguments)]
    fn send_chunk(
        &self,
        coordinator: &Arc<dyn ReplicationCoordinator>,
        local_node: &str,
        job: &ForwardJob<'_>,
        leader: &str,
        epoch: u32,
        mut groups: Vec<PendingGroup>,
        acks: &mut Vec<PartitionAck>,
        totals: &mut ForwardTotals,
        last_refusal: &mut Option<BusServiceError>,
    ) -> Result<Sent, BusServiceError> {
        for g in &mut groups {
            g.sends += 1;
            g.target_unaware = false;
        }
        let fallback = BusServiceError::NotLeader {
            leader_node_id: Some(leader.to_string()),
            leader_epoch: epoch,
        };
        let mut timeout = self.publish_ack_timeout * groups.len().max(1) as u32 + REPLY_MARGIN;
        if let Some(limit) = job.deadline {
            let left = limit.saturating_duration_since(Instant::now());
            if left.is_zero() {
                tracing::debug!(
                    leader,
                    topic = job.topic,
                    "publish: no time left to forward, answering NotLeader"
                );
                return Err(fallback);
            }
            timeout = timeout.min(left);
        }
        let request = self.forward_request(job, &groups, timeout);
        // The wait for the reply ends early once the partitions have moved to
        // another leader or this one is gone: a leader that crashed must not
        // hold a producer for the whole ack timeout.
        let led_by_target = || {
            groups.iter().all(|g| {
                match target_of(coordinator, local_node, &job.ctx.org_id, job.topic, g).0 {
                    Route::Remote { leader: l, .. } => l == leader,
                    Route::Here => false,
                    Route::Unknown => true,
                }
            })
        };
        let still_led_by_target = || led_by_target() && coordinator.node_reachable(leader);
        let reply =
            match coordinator.forward_publish(leader, request, timeout, &still_led_by_target) {
                Ok(reply) => reply,
                Err(ForwardUnavailable::OutcomeUnknown { node, reason }) => {
                    tracing::warn!(
                        leader = %node, topic = job.topic, %reason,
                        "publish: a forwarded write got no answer; its outcome is unknown"
                    );
                    return Err(BusServiceError::ForwardOutcomeUnknown {
                        leader_node_id: leader.to_string(),
                    });
                }
                Err(ForwardUnavailable::NotSent { reason, .. })
                    if groups.iter().all(|g| g.sends < 2) && !led_by_target() =>
                {
                    tracing::debug!(
                        leader = %leader, topic = job.topic, %reason,
                        "publish: the leader moved before the write was sent, trying the new one"
                    );
                    for g in &mut groups {
                        g.refused_by = Some(leader.to_string());
                    }
                    *last_refusal = Some(fallback);
                    return Ok(Sent::Again(groups));
                }
                Err(e) => {
                    tracing::debug!(
                        leader = %leader, topic = job.topic, error = %e,
                        "publish: forwarding unavailable, answering NotLeader"
                    );
                    return Err(fallback);
                }
            };
        match classify_reply(reply) {
            Reply::Landed(result) => {
                absorb_remote(result, acks, totals);
                Ok(Sent::Done)
            }
            Reply::Failed(err) if err.code == CODE_FORWARD_REFUSED => {
                tracing::warn!(
                    leader = %leader, topic = job.topic, reason = %text(&err, 0),
                    "publish: the leader refused a forwarded publish, answering NotLeader"
                );
                Err(fallback)
            }
            Reply::Failed(err) => {
                let landed = err.acked.clone();
                let decoded = error_from_wire(err);
                let (not_leader, source) = match decoded {
                    BusServiceError::PartialPublish { source, .. } => (false, *source),
                    other => (matches!(other, BusServiceError::NotLeader { .. }), other),
                };
                let retryable = not_leader && landed.is_empty();
                // A refusal naming nobody, or a leader this node sent a write
                // to by the ledger row and which still sees an older term: not
                // a leader that moved, one that has not caught up.
                let unaware = match &source {
                    BusServiceError::NotLeader {
                        leader_node_id: None,
                        ..
                    } => true,
                    BusServiceError::NotLeader { leader_epoch, .. } => {
                        groups.iter().all(|g| g.via_row) && *leader_epoch < epoch
                    }
                    _ => false,
                };
                if retryable && unaware {
                    // Not a leader that moved: the one that was named does
                    // not know yet that it leads. Asking it again is not a
                    // second attempt at another leader, so it is not counted.
                    for g in &mut groups {
                        g.sends -= 1;
                        g.target_unaware = true;
                    }
                    *last_refusal = Some(source);
                    return Ok(Sent::Again(groups));
                }
                if retryable && groups.iter().all(|g| g.sends < 2) {
                    for g in &mut groups {
                        g.refused_by = Some(leader.to_string());
                    }
                    *last_refusal = Some(source);
                    return Ok(Sent::Again(groups));
                }
                acks.extend(landed.into_iter().map(ack_from_wire));
                Err(source)
            }
        }
    }

    fn forward_request(
        &self,
        job: &ForwardJob<'_>,
        groups: &[PendingGroup],
        timeout: Duration,
    ) -> ProduceForward {
        ProduceForward {
            instance_id: job.ctx.instance_id.as_str().to_string(),
            environment: self.cached_environment(),
            org_id: job.ctx.org_id.clone(),
            topic: job.topic.to_string(),
            actor: job.ctx.actor.clone(),
            actor_kind: job.ctx.actor_kind.as_str().to_string(),
            correlation_id: job.ctx.correlation_id.clone(),
            origin: job.ctx.origin.clone(),
            broker_dead_letter: job.broker_dead_letter,
            producer: job.producer.map(producer_to_wire),
            reply_within_ms: u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX),
            topic_generation: job.topic_generation,
            partitions: job.partitions,
            groups: groups
                .iter()
                .map(|g| ForwardGroup {
                    partition: g.partition,
                    records: g.records.iter().map(record_to_wire).collect(),
                })
                .collect(),
        }
    }
}

/// Where the records of `partition` go, as this node knows it. A node that
/// holds no replica of the partition has no role for it; the cluster-wide
/// assignment still names the leader.
pub(super) fn route(
    coordinator: &Arc<dyn ReplicationCoordinator>,
    local_node: &str,
    org_id: &str,
    topic: &str,
    partition: u32,
) -> Route {
    match coordinator.role(org_id, topic, partition) {
        PartitionRole::Leader { .. } => Route::Here,
        PartitionRole::Follower {
            leader_node_id,
            epoch,
        } if !leader_node_id.is_empty() => Route::Remote {
            leader: leader_node_id,
            epoch,
        },
        PartitionRole::Unavailable { .. } => {
            match coordinator.assigned_leader(org_id, topic, partition) {
                Some((leader, epoch)) if leader != local_node => Route::Remote { leader, epoch },
                _ => Route::Unknown,
            }
        }
        PartitionRole::Follower { .. } => Route::Unknown,
    }
}

/// Where a group goes now, and whether the ledger row decided it. Where this
/// node's role still names the leader that refused the group (or answered that
/// it does not know its leadership yet), the move that made it refuse is in
/// the ledger before the poll applies it here: the row is followed if it names
/// another remote leader at a higher epoch than the role's. A row that names
/// this node is not followed: the role here has not caught up, and the publish
/// waits for it. The same answer decides whether a write already sent is still
/// going to the right leader (`send_chunk`).
fn target_of(
    coordinator: &Arc<dyn ReplicationCoordinator>,
    local_node: &str,
    org_id: &str,
    topic: &str,
    group: &PendingGroup,
) -> (Route, bool) {
    let routed = route(coordinator, local_node, org_id, topic, group.partition);
    if let Route::Remote { leader, epoch } = &routed {
        let refused = group.refused_by.as_deref() == Some(leader.as_str());
        if refused || group.target_unaware {
            if let Some((row_leader, row_epoch)) =
                coordinator.assigned_leader(org_id, topic, group.partition)
            {
                if row_leader != local_node && row_leader != *leader && row_epoch > *epoch {
                    return (
                        Route::Remote {
                            leader: row_leader,
                            epoch: row_epoch,
                        },
                        true,
                    );
                }
            }
        }
    }
    (routed, false)
}

fn classify_reply(reply: ProduceForwardReply) -> Reply {
    match (reply.result, reply.error) {
        (Some(result), None) => Reply::Landed(result),
        (_, Some(error)) => Reply::Failed(error),
        (None, None) => Reply::Failed(wire_error(
            CODE_FORWARD_REFUSED,
            "forward reply carries neither a result nor an error".to_string(),
        )),
    }
}

fn absorb_remote(result: ForwardResult, acks: &mut Vec<PartitionAck>, totals: &mut ForwardTotals) {
    totals.accepted += result.accepted;
    totals.deduplicated += result.deduplicated;
    totals.duplicate |= result.duplicate;
    totals.schema_rejected += result.schema_rejected;
    totals.schema_dropped += result.schema_dropped;
    acks.extend(result.partitions.into_iter().map(ack_from_wire));
}

/// Folds the local re-run of a group that turned out to be led here into the
/// publish's own totals; its failure is the publish's failure, with the
/// partitions it landed before failing kept in `acks`.
fn absorb_local(
    outcome: Result<PublishResult, BusServiceError>,
    acks: &mut Vec<PartitionAck>,
    totals: &mut ForwardTotals,
) -> Result<(), BusServiceError> {
    match outcome {
        Ok(r) => {
            totals.accepted += r.accepted;
            totals.deduplicated += r.deduplicated;
            totals.duplicate |= r.duplicate;
            totals.schema_rejected += r.schema_rejected;
            totals.schema_dropped += r.schema_dropped;
            acks.extend(r.partitions);
            Ok(())
        }
        Err(BusServiceError::PartialPublish { acked, source }) => {
            acks.extend(acked);
            Err(*source)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::environment::NodeEnvironment;

    fn round_trip(err: BusServiceError) -> BusServiceError {
        error_from_wire(error_to_wire(&err))
    }

    /// Every error the publish pipeline of a leader can answer with survives
    /// the wire as the same variant with the same details, so a forwarded
    /// publish fails exactly like a local one.
    #[test]
    fn publish_errors_round_trip_through_the_wire() {
        let cases = vec![
            BusServiceError::PermissionDenied {
                action: "produce",
                topic: "orders".into(),
            },
            BusServiceError::QuotaExceeded {
                retry_after_ms: 250,
            },
            BusServiceError::QuotaRequestTooLarge {
                unit: "bytes",
                amount: 2048,
                capacity: 1024,
            },
            BusServiceError::MaxBytesTotalExceeded {
                org_id: "org-1".into(),
                max: 10,
                current: 9,
                requested: 5,
            },
            BusServiceError::Throttled { retry_after_ms: 7 },
            BusServiceError::PayloadTooLarge {
                len: 99,
                max_inline_bytes: 10,
            },
            BusServiceError::DedupKeyRequired {
                topic: "orders".into(),
            },
            BusServiceError::ProducerFenced { current_epoch: 4 },
            BusServiceError::EnvironmentMismatch {
                topic_env: NodeEnvironment::Dev,
                node_env: NodeEnvironment::Prod,
            },
            BusServiceError::InvalidArgument("bad".into()),
            BusServiceError::TopicNotFound {
                name: "orders".into(),
            },
            BusServiceError::NotLeader {
                leader_node_id: Some("node-b".into()),
                leader_epoch: 3,
            },
            BusServiceError::NotLeader {
                leader_node_id: None,
                leader_epoch: 0,
            },
            BusServiceError::NotEnoughReplicas {
                isr: 1,
                required: 2,
            },
            BusServiceError::AckTimeout {
                acked: 1,
                required: 2,
            },
            BusServiceError::PartitionUnavailable {
                reason: "no isr".into(),
            },
            BusServiceError::FieldNotAllowed {
                topic: "orders".into(),
                fields: vec!["ssn".into(), "dob".into()],
            },
            BusServiceError::RequiredFieldMissing {
                topic: "orders".into(),
                fields: vec!["id".into()],
            },
            BusServiceError::FieldPolicyPayloadMalformed {
                topic: "orders".into(),
                format: "json",
            },
            BusServiceError::FieldPolicyChanged {
                topic: "orders".into(),
            },
            BusServiceError::KeyNeedsTopicWideRule {
                topic: "orders".into(),
                direction: "write",
            },
            BusServiceError::SchemaViolation {
                topic: "orders".into(),
                subject: "orders-v".into(),
                version: 2,
                detail: "missing id".into(),
            },
            BusServiceError::SchemaNotFound {
                subject: "orders-v".into(),
            },
            BusServiceError::PartitionPoisoned {
                topic: "orders".into(),
                partition: 3,
            },
            BusServiceError::GroupPaused {
                group: "g".into(),
                topic: "orders".into(),
            },
            BusServiceError::MaxGroupsExceeded {
                org_id: "org-1".into(),
                max: 4,
                current: 4,
            },
            BusServiceError::OffsetOutOfRange {
                topic: "orders".into(),
                partition: 2,
                requested: 10,
                earliest: 500,
                latest: 900,
            },
            BusServiceError::GroupNotFound {
                group: "g".into(),
                topic: "orders".into(),
            },
            BusServiceError::RemoteFailure,
        ];
        for case in cases {
            let expected = format!("{case:?}");
            assert_eq!(
                format!("{:?}", round_trip(case)),
                expected,
                "{expected} must survive the wire unchanged"
            );
        }
    }

    #[test]
    fn a_partial_publish_keeps_the_partitions_that_landed() {
        let err = BusServiceError::PartialPublish {
            acked: vec![PartitionAck {
                partition: 2,
                base_offset: 40,
                accepted: 3,
            }],
            source: Box::new(BusServiceError::NotLeader {
                leader_node_id: Some("node-b".into()),
                leader_epoch: 5,
            }),
        };
        match round_trip(err) {
            BusServiceError::PartialPublish { acked, source } => {
                assert_eq!(acked.len(), 1);
                assert_eq!(
                    (acked[0].partition, acked[0].base_offset, acked[0].accepted),
                    (2, 40, 3)
                );
                assert!(matches!(
                    *source,
                    BusServiceError::NotLeader {
                        leader_node_id: Some(ref n),
                        leader_epoch: 5,
                    } if n == "node-b"
                ));
            }
            other => panic!("expected PartialPublish, got {other:?}"),
        }
    }

    /// A leader on a newer build may answer with a code this one has never
    /// heard of: the sender still gets an error, never a panic or a silent
    /// success, and one that does not invite a retry loop.
    #[test]
    fn an_unknown_error_code_degrades_to_a_remote_failure() {
        let err = error_from_wire(ForwardError {
            code: "bus.from_the_future".into(),
            message: "the leader is unhappy".into(),
            ..ForwardError::default()
        });
        assert!(matches!(err, BusServiceError::RemoteFailure), "{err:?}");
        assert!(
            !err.to_string().contains("the leader is unhappy"),
            "the leader's own text is not passed on: {err}"
        );
    }

    /// A storage failure of the leader is not a partition that is
    /// unavailable for a moment: the sender must not take it for one and
    /// retry for ever.
    #[test]
    fn an_engine_failure_of_the_leader_is_a_remote_failure_not_an_unavailable_partition() {
        let wire = error_to_wire(&BusServiceError::Db("disk on fire".into()));
        assert_eq!(wire.code, "bus.remote_error");
        assert!(wire.message.find("disk").is_none());
        assert!(matches!(
            error_from_wire(wire),
            BusServiceError::RemoteFailure
        ));
    }

    #[test]
    fn records_round_trip_byte_for_byte() {
        let record = PublishRecord {
            key: Some(Bytes::from_static(b"k\x00ey")),
            headers: vec![("h".into(), Bytes::from_static(&[0, 255, 7]))],
            payload: Bytes::from_static(&[0, 1, 2, 254, 255]),
            timestamp_ms: -42,
            schema_id: 9,
        };
        let back = record_from_wire(record_to_wire(&record));
        assert_eq!(back.key, record.key);
        assert_eq!(back.headers, record.headers);
        assert_eq!(back.payload, record.payload);
        assert_eq!((back.timestamp_ms, back.schema_id), (-42, 9));
    }

    #[test]
    fn the_static_strings_of_errors_are_recovered_only_from_known_values() {
        assert_eq!(static_str("bytes"), "bytes");
        assert_eq!(static_str("write"), "write");
        assert_eq!(static_str("something-else"), "unknown");
    }
}
