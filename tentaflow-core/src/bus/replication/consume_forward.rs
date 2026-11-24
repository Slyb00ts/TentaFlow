// =============================================================================
// File: bus/replication/consume_forward.rs — forwarding a stateless read to
//       the topic's leader (TB-04 K3)
// =============================================================================
//
// The REST `GET .../records` is stateless: open a consumer, fetch, commit,
// answer. A node that does not lead the topic hands the WHOLE request to the
// node that does (`ConsumeForward` / `ConsumeForwardReply`, one fresh
// bi-stream per request, the same pooled connections and `bus_features`
// gating as `ProduceForward`) instead of refusing with `NotLeader`. A topic is
// led by one node (TB-04 P1), so there is one target; a topic whose
// partitions are led by several nodes is not forwarded and is refused like
// before.
//
// The leader runs open, fetch and commit itself and returns the records,
// committed before the reply is written. It authorizes the ORIGINAL actor
// again (the broker's own sentinel actor is never accepted on a forwarded
// read, and a user must still belong to the organisation on the leader's own
// view of it) and never forwards the request further: a receiver that no
// longer leads answers `NotLeader`. The reply is built and sized to fit one
// frame BEFORE the commit, and the commit covers only the records the reply
// carries, so a commit never covers records the frame cannot hold.
//
// DELIVERY: a read served locally commits the records it returns, which is
// at-least-once only in that a failed commit lets them be read again. A
// forwarded read is at-most-once when its reply is lost: the commit is made
// before the reply is written, so records of a reply that never arrives are
// consumed and not delivered.
//
// OUTCOME UNKNOWN: a request that left this node and got no answer may have
// been fetched AND committed by the leader, so the caller may have skipped
// records it never saw. It ends as `ForwardOutcomeUnknown`, is never retried
// and is never reported as a read that came back empty. Only a request that
// never left is the `NotLeader` it would have been without forwarding.
//
// The stateful consumer an addon holds (`open_consumer`) is NOT forwarded
// (TB-04 P7): its handle lives on one node and `open_consumer` on a node
// that does not lead the topic is refused, naming the leader.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_bytes::ByteBuf;

use crate::auth::actor::ActorKind;
use crate::bus::groups::CommitMode;
use crate::bus::instance::BusInstanceId;
use crate::bus::replication::forward::{
    error_from_wire, error_to_wire, route, ForwardUnavailable, Route, CODE_FORWARD_REFUSED,
};
use crate::bus::replication::frames::{
    self, ConsumeForward, ConsumeForwardReply, ForwardError, ForwardFetched,
};
use crate::bus::{
    deny, BusAction, BusCallContext, BusService, BusServiceError, ConsumerConfig, ConsumerHandle,
    FetchedRecordMeta, ReplicationCoordinator, TopicPartition,
};

/// The most a forwarded read may fetch: a reply is one frame, capped at
/// `frames::MAX_FRAME_BYTES`, and a larger answer would never reach its
/// sender.
const MAX_FORWARDED_FETCH_BYTES: usize = 8 * 1024 * 1024;

/// What a forwarded reply may take of its frame at most, whatever the caller
/// asks for: the rest is left for the envelope and for the estimate of a
/// record being an estimate.
const REPLY_BUDGET: usize = frames::MAX_FRAME_BYTES - 1024 * 1024;

/// The longest a forwarded read may wait for records on the leader.
const MAX_FORWARDED_WAIT_MS: u32 = 30_000;

/// Slack added to the read's own wait before the sender stops waiting for the
/// reply: dialing, authorization, open and commit on the leader.
const REPLY_MARGIN: Duration = Duration::from_secs(5);

/// A refusal made by the forwarding layer (`CODE_FORWARD_REFUSED`).
pub(crate) fn refusal(reason: &str) -> ConsumeForwardReply {
    ConsumeForwardReply {
        records: None,
        error: Some(ForwardError {
            code: CODE_FORWARD_REFUSED.to_string(),
            message: format!("forward refused: {reason}"),
            texts: vec![reason.to_string()],
            ..ForwardError::default()
        }),
    }
}

fn fetched_to_wire(r: FetchedRecordMeta) -> ForwardFetched {
    ForwardFetched {
        partition: r.partition,
        offset: r.offset,
        timestamp_ms: r.timestamp_ms,
        key: r.key.map(|k| ByteBuf::from(k.to_vec())),
        headers: r
            .headers
            .into_iter()
            .map(|(name, value)| (ByteBuf::from(name.to_vec()), ByteBuf::from(value.to_vec())))
            .collect(),
        payload: ByteBuf::from(r.payload.to_vec()),
        schema_id: r.schema_id,
    }
}

fn fetched_from_wire(topic: &str, r: ForwardFetched) -> FetchedRecordMeta {
    FetchedRecordMeta {
        topic: topic.to_string(),
        partition: r.partition,
        offset: r.offset,
        timestamp_ms: r.timestamp_ms,
        key: r.key.map(|k| Bytes::from(k.into_vec())),
        headers: r
            .headers
            .into_iter()
            .map(|(name, value)| (Bytes::from(name.into_vec()), Bytes::from(value.into_vec())))
            .collect(),
        payload: Bytes::from(r.payload.into_vec()),
        schema_id: r.schema_id,
    }
}

/// The bytes of a record's own content: key, headers and payload. The
/// caller's `max_bytes` counts payloads alone when a read is served here; a
/// forwarded reply counts all three, so that headers and keys cannot grow it
/// past what was asked for.
fn content_size(r: &FetchedRecordMeta) -> usize {
    r.key.as_ref().map_or(0, Bytes::len)
        + r.payload.len()
        + r.headers
            .iter()
            .map(|(name, value)| name.len() + value.len())
            .sum::<usize>()
}

/// An upper bound of the bytes a record takes in a `ConsumeForwardReply`:
/// its content, and per record the CBOR that is not content (the field
/// names, integers at their widest, the string and container headers).
fn wire_size(r: &FetchedRecordMeta) -> usize {
    const PER_RECORD: usize = 160;
    const PER_HEADER: usize = 20;
    PER_RECORD + content_size(r) + PER_HEADER * r.headers.len()
}

/// The longest prefix of `records` the reply may carry: its content
/// (`content_size`) stays within `wanted`, the caller's own byte bound,
/// except that the first record is always kept, so a read makes progress
/// however small the bound is; and its encoded size (`wire_size`) stays within
/// `frame`, what one reply can hold, whatever the caller wants. The reply
/// carries exactly these and the commit covers exactly these: what is left
/// off is read by the next request. A first record that does not fit `frame`
/// is an error, since no reply could ever carry it.
fn fit_to_reply(
    records: Vec<FetchedRecordMeta>,
    wanted: usize,
    frame: usize,
) -> Result<Vec<FetchedRecordMeta>, BusServiceError> {
    let mut encoded = 0usize;
    let mut content = 0usize;
    let mut fit = 0usize;
    for r in &records {
        let (wire, own) = (wire_size(r), content_size(r));
        if encoded + wire > frame || (fit > 0 && content + own > wanted) {
            break;
        }
        encoded += wire;
        content += own;
        fit += 1;
    }
    if fit == 0 && !records.is_empty() {
        return Err(BusServiceError::PayloadTooLarge {
            len: wire_size(&records[0]),
            max_inline_bytes: frame,
        });
    }
    let mut records = records;
    records.truncate(fit);
    Ok(records)
}

/// The partitions of one topic that one node leads: this node (`leader:
/// None`) or another.
struct ReadLeg {
    leader: Option<String>,
    epoch: u32,
    partitions: Vec<u32>,
}

impl BusService {
    /// One stateless read of `topic` for `group`: opens a consumer, fetches up
    /// to `max_bytes` and `max_records` (waiting up to `max_wait_ms` for
    /// records) and commits what it returns, as the REST `GET .../records`
    /// does. Served on this node, a commit that fails lets the records be read
    /// again (at-least-once); forwarded, the leader commits before it
    /// answers, so a reply that is lost loses the records for this group
    /// (at-most-once, reported as `ForwardOutcomeUnknown`).
    ///
    /// On a node that does not lead the topic the whole read runs on its
    /// leader (`ConsumeForward`) when that is one node and can take it. A
    /// topic whose partitions are led by several nodes is read one leader at
    /// a time, each for the partitions it leads (`consume_split`). Otherwise,
    /// when some partition has no known leader, the read is refused with
    /// `NotLeader` naming the leader.
    /// Blocking, like `open_consumer` and `fetch`: callers on an async
    /// runtime hand it to `block_in_place` or `spawn_blocking`.
    pub fn consume_once(
        &self,
        ctx: &BusCallContext,
        group: &str,
        topic: &str,
        max_bytes: usize,
        max_records: usize,
        max_wait_ms: u32,
    ) -> Result<Vec<FetchedRecordMeta>, BusServiceError> {
        if let Some(coordinator) = self.replication() {
            if let Some(legs) = self.split_read_legs(&coordinator, ctx, topic) {
                if let Err(e) = self.check_consume_access(ctx, topic, group) {
                    if matches!(e, BusServiceError::PermissionDenied { .. }) {
                        self.audit_windowed(
                            ctx,
                            "bus.consume.denied",
                            Some(topic),
                            Some(&format!("group={group}")),
                        );
                    }
                    return Err(e);
                }
                return self.consume_split(
                    &coordinator,
                    ctx,
                    group,
                    topic,
                    max_bytes,
                    max_records,
                    max_wait_ms,
                    legs,
                );
            }
            if let Some((leader, epoch)) = self.sole_foreign_leader(&coordinator, ctx, topic) {
                // The refusals `open_consumer` makes first, so a caller who
                // may not read the topic learns nothing about where it
                // lives, and the leader is not asked for what is refused here.
                if let Err(e) = self.check_consume_access(ctx, topic, group) {
                    if matches!(e, BusServiceError::PermissionDenied { .. }) {
                        self.audit_windowed(
                            ctx,
                            "bus.consume.denied",
                            Some(topic),
                            Some(&format!("group={group}")),
                        );
                    }
                    return Err(e);
                }
                return self.forward_consume(
                    &coordinator,
                    ctx,
                    group,
                    topic,
                    max_bytes,
                    max_records,
                    max_wait_ms,
                    &leader,
                    epoch,
                    &[],
                );
            }
        }
        let (handle, records) =
            self.read_batch(ctx, group, topic, max_bytes, max_records, max_wait_ms, None)?;
        self.commit_read(&handle, group, topic, &records);
        Ok(records)
    }

    /// A topic whose partitions are led by several nodes, every one of them
    /// known: the partitions each leader serves, this node's own first. `None`
    /// when one node leads them all (this one, or another: `sole_foreign_leader`
    /// and the plain read cover those) or when some partition has no known
    /// leader, which the read refuses as it always has.
    fn split_read_legs(
        &self,
        coordinator: &Arc<dyn ReplicationCoordinator>,
        ctx: &BusCallContext,
        topic: &str,
    ) -> Option<Vec<ReadLeg>> {
        let cfg = self.topic_config(&ctx.org_id, topic).ok()?;
        let local_node = coordinator.local_node_id();
        let mut here: Vec<u32> = Vec::new();
        let mut remote: Vec<ReadLeg> = Vec::new();
        for partition in 0..cfg.partitions {
            match route(coordinator, &local_node, &ctx.org_id, topic, partition) {
                Route::Here => here.push(partition),
                Route::Remote { leader, epoch } => {
                    match remote
                        .iter_mut()
                        .find(|leg| leg.leader.as_deref() == Some(&leader))
                    {
                        Some(leg) => leg.partitions.push(partition),
                        None => remote.push(ReadLeg {
                            leader: Some(leader),
                            epoch,
                            partitions: vec![partition],
                        }),
                    }
                }
                Route::Unknown => return None,
            }
        }
        if remote.len() + usize::from(!here.is_empty()) < 2 {
            return None;
        }
        let mut legs = Vec::with_capacity(remote.len() + 1);
        if !here.is_empty() {
            legs.push(ReadLeg {
                leader: None,
                epoch: 0,
                partitions: here,
            });
        }
        legs.extend(remote);
        Some(legs)
    }

    /// The read of a split topic: every leg asked without waiting, over and
    /// over until something comes back or `max_wait_ms` is spent, so that a
    /// partition with records is not held up by one without. A leg that cannot
    /// be served fails the read as the whole topic's read would have.
    #[allow(clippy::too_many_arguments)]
    fn consume_split(
        &self,
        coordinator: &Arc<dyn ReplicationCoordinator>,
        ctx: &BusCallContext,
        group: &str,
        topic: &str,
        max_bytes: usize,
        max_records: usize,
        max_wait_ms: u32,
        legs: Vec<ReadLeg>,
    ) -> Result<Vec<FetchedRecordMeta>, BusServiceError> {
        let deadline = std::time::Instant::now() + Duration::from_millis(u64::from(max_wait_ms));
        // Each leg's share of one pass: a leg listed first must not use up the
        // pass and leave the others never asked while it has records to give.
        let share_records = max_records.div_ceil(legs.len()).max(1);
        let share_bytes = max_bytes.div_ceil(legs.len()).max(1);
        // Opened once for the whole call: opening a consumer writes the group
        // row and replicates it, which every 50 ms pass must not repeat.
        let mut local: Option<ConsumerHandle> = None;
        loop {
            let mut collected: Vec<FetchedRecordMeta> = Vec::new();
            let mut bytes_left = max_bytes;
            for leg in &legs {
                let records_left = max_records.saturating_sub(collected.len());
                if records_left == 0 || bytes_left == 0 {
                    break;
                }
                let leg_records = records_left.min(share_records);
                let leg_bytes = bytes_left.min(share_bytes);
                // A leg that fails after another returned records ends the
                // pass with what was read: those records are committed by
                // their leaders already, and an error would drop them.
                let served = (|| match &leg.leader {
                    None => {
                        if local.is_none() {
                            local = Some(self.open_consumer_scoped(
                                ctx,
                                group,
                                &[topic.to_string()],
                                ConsumerConfig {
                                    commit_mode: CommitMode::Explicit,
                                },
                                Some(&leg.partitions),
                            )?);
                        }
                        let handle = local.as_ref().expect("opened above");
                        let records = handle.fetch_up_to(leg_bytes, leg_records, 0)?.records;
                        self.commit_read(handle, group, topic, &records);
                        Ok(records)
                    }
                    Some(leader) => self.forward_consume(
                        coordinator,
                        ctx,
                        group,
                        topic,
                        leg_bytes,
                        leg_records,
                        0,
                        leader,
                        leg.epoch,
                        &leg.partitions,
                    ),
                })();
                let got = match served {
                    Ok(got) => got,
                    Err(e) if collected.is_empty() => return Err(e),
                    Err(e) => {
                        tracing::warn!(topic, error = %e, "bus read: a leg of a split topic failed after another returned records");
                        break;
                    }
                };
                let used: usize = got.iter().map(|r| r.payload.len()).sum();
                bytes_left = bytes_left.saturating_sub(used.max(1));
                collected.extend(got);
            }
            if !collected.is_empty() || std::time::Instant::now() >= deadline {
                return Ok(collected);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The open and fetch of `consume_once`, on this node's own partitions:
    /// `open_consumer` refuses with `NotLeader` for a topic this node does not
    /// lead. The caller commits what it goes on to return (`commit_read`).
    fn read_batch(
        &self,
        ctx: &BusCallContext,
        group: &str,
        topic: &str,
        max_bytes: usize,
        max_records: usize,
        max_wait_ms: u32,
        only: Option<&[u32]>,
    ) -> Result<(ConsumerHandle, Vec<FetchedRecordMeta>), BusServiceError> {
        let handle = self.open_consumer_scoped(
            ctx,
            group,
            &[topic.to_string()],
            ConsumerConfig {
                commit_mode: CommitMode::Explicit,
            },
            only,
        )?;
        let batch = handle.fetch_up_to(max_bytes, max_records, max_wait_ms)?;
        Ok((handle, batch.records))
    }

    /// Commits `records`, which are about to be returned: to one past the
    /// highest offset per partition. A failed commit must not drop them; it
    /// only means they may be delivered again on the next read.
    fn commit_read(
        &self,
        handle: &ConsumerHandle,
        group: &str,
        topic: &str,
        records: &[FetchedRecordMeta],
    ) {
        if records.is_empty() {
            return;
        }
        let mut next: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
        for r in records {
            next.entry(r.partition)
                .and_modify(|o| *o = (*o).max(r.offset + 1))
                .or_insert(r.offset + 1);
        }
        let offsets: Vec<(TopicPartition, u64)> = next
            .into_iter()
            .map(|(partition, offset)| {
                (
                    TopicPartition {
                        topic: topic.to_string(),
                        partition,
                    },
                    offset,
                )
            })
            .collect();
        if let Err(e) = handle.commit(&offsets) {
            tracing::warn!(
                topic, group, error = %e,
                "bus read: post-fetch commit failed, records already returned to the caller"
            );
        }
    }

    /// The one other node that leads every partition of `topic`, with the
    /// epoch, when this node leads none of them and a single node leads them
    /// all. `None` leaves the read to `consume_here`: this node leads
    /// (some of) it, or its partitions are split between nodes, or nobody is
    /// known to lead it, or the topic is not known here, and `open_consumer`
    /// then answers in its usual order (authorization before anything else).
    fn sole_foreign_leader(
        &self,
        coordinator: &Arc<dyn ReplicationCoordinator>,
        ctx: &BusCallContext,
        topic: &str,
    ) -> Option<(String, u32)> {
        let cfg = self.topic_config(&ctx.org_id, topic).ok()?;
        let local_node = coordinator.local_node_id();
        let mut sole: Option<(String, u32)> = None;
        for partition in 0..cfg.partitions {
            match route(coordinator, &local_node, &ctx.org_id, topic, partition) {
                Route::Remote { leader, epoch } => match &sole {
                    None => sole = Some((leader, epoch)),
                    Some((first, _)) if *first == leader => {}
                    Some(_) => return None,
                },
                Route::Here | Route::Unknown => return None,
            }
        }
        sole
    }

    #[allow(clippy::too_many_arguments)]
    fn forward_consume(
        &self,
        coordinator: &Arc<dyn ReplicationCoordinator>,
        ctx: &BusCallContext,
        group: &str,
        topic: &str,
        max_bytes: usize,
        max_records: usize,
        max_wait_ms: u32,
        leader: &str,
        epoch: u32,
        only_partitions: &[u32],
    ) -> Result<Vec<FetchedRecordMeta>, BusServiceError> {
        let fallback = BusServiceError::NotLeader {
            leader_node_id: Some(leader.to_string()),
            leader_epoch: epoch,
        };
        let sender_cfg = self.topic_config(&ctx.org_id, topic)?;
        let request = ConsumeForward {
            instance_id: ctx.instance_id.as_str().to_string(),
            environment: self.cached_environment(),
            org_id: ctx.org_id.clone(),
            topic: topic.to_string(),
            group: group.to_string(),
            actor: ctx.actor.clone(),
            actor_kind: ctx.actor_kind.as_str().to_string(),
            correlation_id: ctx.correlation_id.clone(),
            origin: ctx.origin.clone(),
            max_bytes: max_bytes as u64,
            max_records: Some(max_records as u64),
            max_wait_ms,
            topic_generation: sender_cfg.generation,
            partitions: sender_cfg.partitions,
            only_partitions: only_partitions.to_vec(),
        };
        let timeout = Duration::from_millis(u64::from(max_wait_ms)) + REPLY_MARGIN;
        let still_led_by_target = || coordinator.node_reachable(leader);
        let reply =
            match coordinator.forward_consume(leader, request, timeout, &still_led_by_target) {
                Ok(reply) => reply,
                Err(ForwardUnavailable::OutcomeUnknown { node, reason }) => {
                    tracing::warn!(
                        leader = %node, topic, %reason,
                        "bus read: a forwarded read got no answer; its outcome is unknown"
                    );
                    return Err(BusServiceError::ForwardOutcomeUnknown {
                        leader_node_id: leader.to_string(),
                    });
                }
                Err(e) => {
                    tracing::debug!(
                        leader, topic, error = %e,
                        "bus read: forwarding unavailable, answering NotLeader"
                    );
                    return Err(fallback);
                }
            };
        match (reply.records, reply.error) {
            (Some(records), None) => Ok(records
                .into_iter()
                .map(|r| fetched_from_wire(topic, r))
                .collect()),
            (_, Some(error)) if error.code == CODE_FORWARD_REFUSED => {
                tracing::warn!(
                    leader, topic, reason = ?error.texts.first(),
                    "bus read: the leader refused a forwarded read, answering NotLeader"
                );
                Err(fallback)
            }
            (_, Some(error)) => Err(error_from_wire(error)),
            (None, None) => {
                tracing::warn!(
                    leader,
                    topic,
                    "bus read: the leader's reply carries neither records nor an error"
                );
                Err(fallback)
            }
        }
    }

    /// The leader's half of `forward_consume`: authorizes the original actor
    /// and runs the read on this node's own partitions, never forwarding it.
    pub(in crate::bus) fn run_forwarded_consume(
        &self,
        from: &str,
        request: ConsumeForward,
    ) -> ConsumeForwardReply {
        match self.forwarded_consume(from, request) {
            Ok(records) => ConsumeForwardReply {
                records: Some(records),
                error: None,
            },
            Err(e) => ConsumeForwardReply {
                records: None,
                error: Some(error_to_wire(&e)),
            },
        }
    }

    fn forwarded_consume(
        &self,
        from: &str,
        request: ConsumeForward,
    ) -> Result<Vec<ForwardFetched>, BusServiceError> {
        let instance_id = BusInstanceId::parse(&request.instance_id)
            .map_err(|e| BusServiceError::InvalidArgument(format!("instance id: {e}")))?;
        let actor_kind = ActorKind::parse(&request.actor_kind).ok_or_else(|| {
            BusServiceError::InvalidArgument("unknown actor kind in a forwarded read".into())
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
        let group = request.group;
        let refuse = |reason: &str| {
            self.audit_windowed(
                &ctx,
                "bus.consume.denied",
                Some(&topic),
                Some(&format!(
                    "group={group} forwarded_from={from} reason={reason}"
                )),
            );
            deny(BusAction::Consume, &topic)
        };
        // A forwarded read is a REST read: made by a user or by an API key.
        // The broker's own sentinel actor reads `__*` topics of any
        // organisation without a check, and an addon's consumer is never
        // forwarded, so a peer vouching for either is refused whatever topic
        // it names.
        let system = ctx.actor.as_deref() == Some(crate::services::bus_authorizer::SYSTEM_ACTOR);
        if system || !matches!(ctx.actor_kind, ActorKind::User | ActorKind::ApiKey) {
            return Err(refuse("actor_not_forwardable"));
        }
        // The sender checked that a user belongs to the organisation; the
        // leader asks again of its own copy of the memberships, so a peer
        // cannot name an organisation its user is not in.
        if ctx.actor_kind == ActorKind::User {
            let member = ctx.actor.as_deref().is_some_and(|user| {
                matches!(
                    crate::services::org::repo::get_user_role_in_org(&self.db, user, &ctx.org_id),
                    Ok(Some(_))
                )
            });
            if !member {
                return Err(refuse("not_an_org_member"));
            }
        }
        if let Err(e) = self.check_consume_access(&ctx, &topic, &group) {
            if matches!(e, BusServiceError::PermissionDenied { .. }) {
                return Err(refuse("acl"));
            }
            return Err(e);
        }
        // After the checks, so that a caller who may not read learns nothing
        // of the topic, and a leader still behind the sender's rows is told
        // so only to one who may.
        self.forwarded_topic_config(
            &ctx.org_id,
            &topic,
            request.topic_generation,
            request.partitions,
        )?;
        let max_bytes = usize::try_from(request.max_bytes)
            .unwrap_or(usize::MAX)
            .min(MAX_FORWARDED_FETCH_BYTES);
        let max_records = request
            .max_records
            .map_or(usize::MAX, |n| usize::try_from(n).unwrap_or(usize::MAX));
        let only = (!request.only_partitions.is_empty()).then_some(&request.only_partitions[..]);
        let (handle, records) = self.read_batch(
            &ctx,
            &group,
            &topic,
            max_bytes,
            max_records,
            request.max_wait_ms.min(MAX_FORWARDED_WAIT_MS),
            only,
        )?;
        // Sized and converted before the commit: the commit covers what the
        // reply carries, no more.
        let records = fit_to_reply(records, max_bytes, REPLY_BUDGET)?;
        self.commit_read(&handle, &group, &topic, &records);
        Ok(records.into_iter().map(fetched_to_wire).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fetched_record_round_trips_byte_for_byte() {
        let record = FetchedRecordMeta {
            topic: "orders".into(),
            partition: 3,
            offset: 41,
            timestamp_ms: -7,
            key: Some(Bytes::from_static(b"k\x00ey")),
            headers: vec![(
                Bytes::from_static(&[0xFF, 0x00]),
                Bytes::from_static(&[1, 2, 3]),
            )],
            payload: Bytes::from_static(&[0, 1, 254, 255]),
            schema_id: 9,
        };
        let back = fetched_from_wire("orders", fetched_to_wire(record.clone()));
        assert_eq!(
            (
                back.topic.as_str(),
                back.partition,
                back.offset,
                back.timestamp_ms,
                back.schema_id
            ),
            ("orders", 3, 41, -7, 9)
        );
        assert_eq!(back.key, record.key);
        assert_eq!(back.headers, record.headers);
        assert_eq!(back.payload, record.payload);
    }

    fn fetched(
        partition: u32,
        offset: u64,
        key: usize,
        header: usize,
        payload: usize,
    ) -> FetchedRecordMeta {
        FetchedRecordMeta {
            topic: "orders".into(),
            partition,
            offset,
            timestamp_ms: i64::MIN,
            key: (key > 0).then(|| Bytes::from(vec![7u8; key])),
            headers: if header > 0 {
                vec![(Bytes::from_static(b"tf.h"), Bytes::from(vec![1u8; header]))]
            } else {
                Vec::new()
            },
            payload: Bytes::from(vec![9u8; payload]),
            schema_id: u32::MAX,
        }
    }

    /// The estimate is what the cut rests on, so it must never fall below the
    /// bytes the frame really takes: records with nothing but a key, or only
    /// headers, or only a tiny payload, at their widest integers.
    #[tokio::test]
    async fn the_size_estimate_is_never_below_the_encoded_reply() {
        let records: Vec<FetchedRecordMeta> = vec![
            fetched(0, 0, 0, 0, 0),
            fetched(u32::MAX, u64::MAX, 0, 0, 1),
            fetched(3, 1 << 40, 4_096, 0, 0),
            fetched(3, 1 << 40, 0, 8_192, 1),
            fetched(7, 9, 17, 33, 1_000),
        ];
        let estimate: usize = records.iter().map(wire_size).sum();
        let reply = ConsumeForwardReply {
            records: Some(records.into_iter().map(fetched_to_wire).collect()),
            error: None,
        };
        let mut encoded = Vec::new();
        frames::write_frame(&mut encoded, &frames::ReplFrame::ConsumeForwardReply(reply))
            .await
            .unwrap();
        assert!(
            estimate >= encoded.len(),
            "estimate {estimate} < encoded {}",
            encoded.len()
        );
    }

    /// Headers and keys count: records with a one-byte payload and a large
    /// header would pass a payload-only bound and overrun the frame.
    #[test]
    fn a_reply_is_cut_to_the_records_that_fit_counting_keys_and_headers() {
        let records: Vec<FetchedRecordMeta> = (0..10u32)
            .map(|n| fetched(n % 3, 100 + u64::from(n), 50, 4_000, 1))
            .collect();
        let own = content_size(&records[0]);
        assert_eq!(
            own,
            50 + 4_000 + 4 + 1,
            "key, header (name and value), payload"
        );
        let cut = fit_to_reply(records.clone(), own * 4 + own / 2, usize::MAX).unwrap();
        assert_eq!(cut.len(), 4);
        assert_eq!(
            cut.iter().map(|r| r.offset).collect::<Vec<_>>(),
            records.iter().take(4).map(|r| r.offset).collect::<Vec<_>>(),
            "a prefix, in order: the commit that follows covers exactly these"
        );
        let all = fit_to_reply(records.clone(), own * 10, usize::MAX).unwrap();
        assert_eq!(all.len(), 10);
        assert!(fit_to_reply(Vec::new(), 1, 1).unwrap().is_empty());
    }

    /// The frame is a hard bound the caller's wish cannot exceed, and a bound
    /// smaller than one record still returns that record, or a read could
    /// never make progress.
    #[test]
    fn the_frame_bounds_a_reply_and_a_tiny_wish_still_returns_one_record() {
        let records: Vec<FetchedRecordMeta> = (0..10u32)
            .map(|n| fetched(0, u64::from(n), 0, 0, 100))
            .collect();
        let per_record = wire_size(&records[0]);
        let frame_bound = fit_to_reply(records.clone(), usize::MAX, per_record * 3).unwrap();
        assert_eq!(frame_bound.len(), 3);
        let tiny = fit_to_reply(records, 1, usize::MAX).unwrap();
        assert_eq!(tiny.len(), 1);
    }

    /// A record no reply could ever carry is an error, never an empty reply
    /// that would commit nothing and be asked for again for ever.
    #[test]
    fn a_first_record_that_cannot_fit_a_frame_is_refused() {
        let err = fit_to_reply(vec![fetched(0, 0, 0, 0, 5_000)], usize::MAX, 1_000).unwrap_err();
        assert!(
            matches!(err, BusServiceError::PayloadTooLarge { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn a_refusal_is_recognised_by_the_forwarding_layer_code() {
        let reply = refusal("forwarding_disabled");
        assert!(reply.records.is_none());
        let error = reply.error.expect("an error");
        assert_eq!(error.code, CODE_FORWARD_REFUSED);
        assert_eq!(error.texts, vec!["forwarding_disabled".to_string()]);
    }
}
