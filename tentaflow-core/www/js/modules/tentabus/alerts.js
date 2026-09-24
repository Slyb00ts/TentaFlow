// ===== File: modules/tentabus/alerts.js — the Przegląd alerts, computed from the stats snapshot, the lag history trend and the replica state =====
//
// Pure functions (PLAN-UI-20260923 U0, thresholds P4 as decided 23.09):
//   - a consumer "nie nadąża" when its lag has been growing for at least
//     10 minutes AND more than 1 000 messages wait;
//   - "przybywa nieprzetworzonych" when at least one message of a topic
//     entered its unprocessed store in the last hour;
//   - a paused consumer is an alert only while something waits for it;
//   - every replica the leader reports as lagging is an alert, one per node.
// A lag the node could not measure (`lagTotal == null`) never raises or
// clears anything: an unknown is not a zero.

export const LAGGING_MIN_RISE_MS = 10 * 60_000;
export const LAGGING_MIN_WAITING = 1000;
export const DLQ_MIN_LAST_HOUR = 1;
/// How recent the last growth of a lag must be to say it "rośnie" (grows):
/// one sampling round plus slack, the same window the history treats as one
/// continuous run.
export const RISING_RECENT_MS = 3 * 60_000;

const measured = (v) => v != null && Number.isFinite(Number(v));

/** A consumer whose lag grows for ≥ 10 min with more than 1 000 waiting. */
export function isLagging(group, nowMs) {
  if (!group || group.paused || !measured(group.lagTotal) || !measured(group.lagRisingSinceMs)) return false;
  return Number(group.lagTotal) > LAGGING_MIN_WAITING && nowMs - Number(group.lagRisingSinceMs) >= LAGGING_MIN_RISE_MS;
}

export const lagSeriesKey = (group, topic) => `${group}\u0000${topic}`;

/**
 * Whether a lag is still growing, from its per-minute history (oldest
 * first, `{ atMs, lagTotal }`): the newest sample is recent and above the one
 * before it. A backlog that stopped growing has been "waiting" since the
 * growth began, not "rising" — the wording has to say which.
 */
export function lagWording(samples, nowMs) {
  const list = samples || [];
  if (list.length < 2) return 'waiting';
  const last = list[list.length - 1];
  const prev = list[list.length - 2];
  const recent = nowMs - Number(last.atMs) <= RISING_RECENT_MS;
  return recent && Number(last.lagTotal) > Number(prev.lagTotal) ? 'rising' : 'waiting';
}

/** A paused consumer that has messages waiting. */
export function isPausedWithBacklog(group) {
  return Boolean(group?.paused) && measured(group.lagTotal) && Number(group.lagTotal) > 0;
}

/** Consumers with anything waiting — the "Odbiorcy z opóźnieniem" tile. */
export function delayedGroups(groups) {
  return (groups || []).filter((g) => measured(g.lagTotal) && Number(g.lagTotal) > 0);
}

/**
 * Flattens per-topic replica snapshots into one lag entry per lagging
 * replica. `perTopic` is `[{ topic, partitions }]` (a `ReplicaListResponse`
 * asked for that topic: the wire partition carries no topic name of its own),
 * `nodes` names the node ids.
 */
export function laggingReplicas(perTopic, nodes) {
  const labels = new Map((nodes || []).map((n) => [n.nodeId, n.label || n.nodeId]));
  const out = [];
  for (const { topic, partitions } of perTopic || []) {
    for (const p of partitions || []) {
      for (const l of p.lagging || []) {
        out.push({
          nodeId: l.nodeId,
          nodeLabel: labels.get(l.nodeId) || l.nodeId,
          topic,
          partition: Number(p.partition),
          lagBytes: Number(l.lagBytes) || 0,
          lagMs: Number(l.lagMs) || 0,
        });
      }
    }
  }
  return out;
}

/**
 * Every alert the overview shows, in the order it shows them: consumers
 * that fall behind, topics gaining unprocessed messages, paused consumers
 * with a backlog, then replicas behind their leader. Each alert carries
 * the facts its card prints and the target its button opens.
 *
 * `groups` = `StatsSnapshot.groups`, `topics` = `StatsSnapshot.topics`
 * (unprocessed-message rows sit on the SOURCE topic), `replicaLags` =
 * `laggingReplicas(...)`, `lagSeries` = recent lag samples per
 * `group\u0000topic` (see `lagSeriesKey`), which decide "rośnie" vs "czeka".
 */
export function computeAlerts({ groups = [], topics = [], replicaLags = [], lagSeries = new Map(), nowMs }) {
  const byGroup = (a, b) => a.group.localeCompare(b.group) || a.topic.localeCompare(b.topic);
  const lagging = groups.filter((g) => isLagging(g, nowMs)).sort(byGroup).map((g) => ({
    key: `lagging:${g.group}:${g.topic}`,
    kind: 'lagging',
    tone: 'warning',
    group: g.group,
    topic: g.topic,
    waiting: Number(g.lagTotal),
    risingSinceMs: Number(g.lagRisingSinceMs),
    wording: lagWording(lagSeries.get(lagSeriesKey(g.group, g.topic)), nowMs),
  }));
  const dlq = topics
    .filter((t) => !String(t.topic || '').startsWith('__') && Number(t.dlqLastHour) >= DLQ_MIN_LAST_HOUR)
    .sort((a, b) => Number(b.dlqLastHour) - Number(a.dlqLastHour) || a.topic.localeCompare(b.topic))
    .map((t) => ({
      key: `dlq:${t.topic}`,
      kind: 'dlq',
      tone: 'warning',
      topic: t.topic,
      lastHour: Number(t.dlqLastHour),
      total: Number(t.dlqDepth) || 0,
    }));
  const paused = groups.filter(isPausedWithBacklog).sort(byGroup).map((g) => ({
    key: `paused:${g.group}:${g.topic}`,
    kind: 'paused',
    tone: 'warning',
    group: g.group,
    topic: g.topic,
    waiting: Number(g.lagTotal),
  }));
  const perNode = new Map();
  for (const r of replicaLags) {
    if (!perNode.has(r.nodeId)) perNode.set(r.nodeId, { nodeId: r.nodeId, nodeLabel: r.nodeLabel, entries: [] });
    perNode.get(r.nodeId).entries.push(r);
  }
  const replica = [...perNode.values()]
    .sort((a, b) => a.nodeLabel.localeCompare(b.nodeLabel))
    .map(({ nodeId, nodeLabel, entries }) => ({
      key: `replica:${nodeId}`,
      kind: 'replica',
      tone: 'info',
      nodeId,
      nodeLabel,
      partitions: entries.length,
      topics: [...new Set(entries.map((e) => e.topic))].sort(),
      lagBytes: entries.reduce((s, e) => s + e.lagBytes, 0),
      maxLagMs: entries.reduce((m, e) => Math.max(m, e.lagMs), 0),
    }));
  return [...lagging, ...dlq, ...paused, ...replica];
}
