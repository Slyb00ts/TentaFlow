// ===== File: modules/tentabus/leader-balance.js — who leads how much (TB-04): the numbers of "Rozkład prowadzenia" and the switch of the automatic return =====
//
// Pure functions over the replica lists the shell already polls. A topic is
// led as a whole by one node (K5), so the unit here is a topic: the node that
// leads most of its partitions "has" it, and the node its partitions name as
// preferred (`preferredLeaderNodeId`, position 0 of the replicas) is where it
// returns to. A topic that failover split between nodes still counts each
// partition where it is led, but is listed under the node leading most of it.
// The broker's own `__*` topics are not listed, but a topic's `__dlq.<topic>`
// partitions are counted into it, as the planner weighs its units: the card
// then shows the same numbers the preview and the server work with.

import { T } from '/js/modules/tentabus/format.js';
import { userTopics } from '/js/modules/tentabus/model.js';
import { nodeLabel, nodeNamer, nodeTitle } from '/js/modules/tentabus/node-label.js';

/** The `settings` key of the per-instance switch of the automatic return (K6); absent means on. */
export const leaderReturnKey = (instanceId) => `bus.leader_return:${instanceId}`;

/**
 * Reads the switch from the settings list (`[{ key, value }]`): `true` when it
 * is on or was never set, `false` when it is off or unreadable (the nodes read
 * an unreadable value as off, so the screen does too), `null` when the list
 * has not been read.
 */
export function leaderReturnEnabled(entries, instanceId) {
  if (!Array.isArray(entries)) return null;
  const entry = entries.find((e) => e.key === leaderReturnKey(instanceId));
  if (!entry) return true;
  const value = String(entry.value ?? '').trim().toLowerCase();
  if (['1', 'true', 'on'].includes(value)) return true;
  return false;
}

/** The value to store for the switch. */
export const leaderReturnValue = (enabled) => (enabled ? 'on' : 'off');

/**
 * Whether a node is of an older build that cannot take part in moving leaders.
 * The server names three states (`forwarding`: capable, lacking, unknown): only
 * "lacking" is an older build; a node that is down or not asked yet is
 * "unknown". A server that sends only `forwardCapable` cannot tell them apart,
 * so there a node that answers and is not capable is taken for an older one.
 */
export function isOlderNode(node) {
  if (node?.forwarding != null) return node.forwarding === 'lacking';
  return node?.forwardCapable === false && node?.reachable !== false;
}

/**
 * One row per topic of the reader: how many partitions it has (with those of
 * its `__dlq.` topic, when `dlqPartitions` was read), which node leads most of
 * them and which node it prefers (`null` when its partitions disagree or the
 * server does not say).
 */
export function leadershipUnits({ topics, perTopic }) {
  const byTopic = new Map((perTopic || []).map((t) => [t.topic, [...(t.partitions || []), ...(t.dlqPartitions || [])]]));
  const units = [];
  for (const t of userTopics(topics)) {
    const parts = byTopic.get(t.name) || [];
    if (!parts.length) continue;
    const led = new Map();
    for (const p of parts) if (p.leaderNodeId) led.set(p.leaderNodeId, (led.get(p.leaderNodeId) || 0) + 1);
    const leaderId = [...led.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))[0]?.[0] || null;
    const named = new Set(parts.map((p) => p.preferredLeaderNodeId || null));
    units.push({
      name: t.name,
      partitions: parts.length,
      led,
      leaderId,
      preferredId: named.size === 1 ? [...named][0] : null,
      // Partitions and dead-letter partitions led by different nodes: no single node can be named the leader.
      split: led.size > 1,
    });
  }
  return units.sort((a, b) => a.name.localeCompare(b.name));
}

/**
 * The table of "Rozkład prowadzenia": per node the partitions it leads, its
 * topics, its distance from an even split and the topics that prefer it but
 * are led elsewhere; plus the sentence's numbers: the gap between the most and
 * the least loaded node against the smallest topic (a gap that small cannot
 * be evened out by moving a topic).
 */
export function balanceModel({ units, nodes }) {
  const list = nodes || [];
  const rows = list.map((n) => ({
    nodeId: n.nodeId,
    label: nodeLabel(n),
    title: nodeTitle(n),
    reachable: n.reachable !== false,
    partitions: units.reduce((sum, u) => sum + (u.led.get(n.nodeId) || 0), 0),
    topics: units.filter((u) => u.leaderId === n.nodeId).map((u) => u.name),
    away: units.filter((u) => u.preferredId === n.nodeId && u.leaderId && (u.leaderId !== n.nodeId || u.split)),
  }));
  const total = rows.reduce((s, r) => s + r.partitions, 0);
  const mean = rows.length ? total / rows.length : 0;
  for (const r of rows) r.deviation = r.partitions - mean;
  const counts = rows.map((r) => r.partitions);
  const gap = counts.length ? Math.max(...counts) - Math.min(...counts) : 0;
  const smallest = units.length ? Math.min(...units.map((u) => u.partitions)) : 0;
  let verdict = 'uneven';
  if (gap < smallest || units.length === 0) verdict = 'less';
  else if (gap === smallest) verdict = 'equal';
  return { rows, total, mean, gap, smallest, verdict, away: rows.reduce((s, r) => s + r.away.length, 0), split: units.filter((u) => u.split) };
}

/** `17.5` → "17,5", whole numbers without a fraction, in the reader's language. */
export function fmtNumber(value, language) {
  return new Intl.NumberFormat(language, { maximumFractionDigits: 1 }).format(value);
}

/** A signed distance: "+0,5" / "−0,5" (a real minus), "0" for none. */
export function fmtSigned(value, language) {
  const abs = Math.abs(value);
  const text = fmtNumber(abs, language);
  if (Number(text.replace(',', '.')) === 0) return text;
  return `${value < 0 ? '−' : '+'}${text}`;
}

/**
 * The number that picks a plural form for a count that may have a fraction:
 * "17,5 partycji" takes the form of five, not of seventeen.
 */
export const pluralNumber = (value) => (Number.isInteger(value) ? value : 5);

/**
 * Whether "Rozłóż prowadzenie" can run now: `{ reason }` says why not (`null`
 * when it can), `older` = the nodes that cannot lead yet (`forwardCapable ===
 * false`: an older build that takes no forwarded writes).
 */
export function rebalanceAvailability(nodes) {
  const list = nodes || [];
  const older = list.filter(isOlderNode).map(nodeLabel);
  if (older.length) {
    return { reason: T(older.length === 1 ? 'replication.balance_blocked_old_one' : 'replication.balance_blocked_old_many', { nodes: older.join(', ') }), older };
  }
  if (list.length < 2) return { reason: T('replication.balance_blocked_single'), older };
  if (!list.some((n) => !n.isLocal && n.reachable !== false)) return { reason: T('replication.balance_blocked_down'), older };
  return { reason: null, older };
}

/**
 * The node that leads `topic` when it is not this one: `{ nodeId, node }`
 * (the node that leads most of its partitions, by name), or `null` when this
 * node leads it, no node is known to, or the replica lists are not read yet.
 * What only the leading node can do (pause a consumer, move its reading
 * place) is offered by it alone.
 */
export function leaderElsewhere(topic, perTopic, nodes) {
  const local = (nodes || []).find((n) => n.isLocal)?.nodeId;
  const parts = (perTopic || []).find((t) => t.topic === topic)?.partitions || [];
  if (!local || !parts.length) return null;
  const led = new Map();
  for (const p of parts) if (p.leaderNodeId) led.set(p.leaderNodeId, (led.get(p.leaderNodeId) || 0) + 1);
  const leaderId = [...led.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))[0]?.[0];
  if (!leaderId || led.has(local) && led.get(local) >= led.get(leaderId)) return null;
  return { nodeId: leaderId, node: nodeNamer(nodes)(leaderId) };
}

/**
 * The node a new topic would go to: the reachable node with the fewest
 * partitions preferring it. It counts the preferred node (`replicas[0]`), not
 * the current leader, over every topic with its `__dlq.` partitions, as the
 * server's choice does, so a node that took partitions over in a failover is
 * not also given the new topics.
 */
export function leastLoadedNode({ perTopic, nodes }) {
  const counts = new Map();
  for (const t of perTopic || []) {
    for (const p of [...(t.partitions || []), ...(t.dlqPartitions || [])]) {
      const id = p.preferredLeaderNodeId || p.replicas?.[0];
      if (id) counts.set(id, (counts.get(id) || 0) + 1);
    }
  }
  const rows = (nodes || []).filter((n) => n.reachable !== false).map((n) => ({ nodeId: n.nodeId, label: nodeLabel(n), partitions: counts.get(n.nodeId) || 0 }));
  if (!rows.length) return null;
  rows.sort((a, b) => a.partitions - b.partitions || a.nodeId.localeCompare(b.nodeId));
  // The server breaks a tie by hashing the topic name onto a ring of the tied nodes (K4). The name is not hashed here, so a tie is
  // reported as the set of tied nodes instead of a guess at one of them.
  const tied = rows.filter((r) => r.partitions === rows[0].partitions);
  return tied.length > 1 ? { ...rows[0], tied: tied.map((r) => r.label) } : rows[0];
}
