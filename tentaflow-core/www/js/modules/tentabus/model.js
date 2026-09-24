// ===== File: modules/tentabus/model.js — what the header, the main tabs and Przegląd show, derived from the wire answers =====
//
// Pure functions: every number the shell and the overview print comes from
// here, so the counters of the header card, the tab strip and the KPI tiles
// can never disagree with each other. Names starting with `__` are the
// broker's own: `__dlq.*` holds the unprocessed messages of its source topic
// and `__bus.*` the broker's internals. Neither is counted or listed as a
// topic of the reader's.

export const isInternalTopic = (name) => String(name || '').startsWith('__');

/** Topics the reader thinks of as topics (the list without `__*`). */
export function userTopics(topicList) {
  return (topicList || []).filter((t) => !t.isDlq && !isInternalTopic(t.name));
}

/** Number of the reader's topics in a stats snapshot. */
export function snapshotTopicCount(stats) {
  return (stats?.topics || []).filter((t) => !isInternalTopic(t.topic)).length;
}

/**
 * Messages per second written to the reader's topics. The snapshot's own
 * total also counts the broker's internal topics, which the reader never
 * writes to and which would make an idle instance look busy.
 */
export function userRate(stats) {
  return (stats?.topics || [])
    .filter((t) => !isInternalTopic(t.topic))
    .reduce((sum, t) => sum + (Number(t.msgsInPerSec) || 0), 0);
}

/**
 * The counters shared by the header badges and the tab strip. A figure whose
 * source has not answered yet is `null` — the caller leaves it out rather
 * than printing a zero it does not know.
 */
export function shellCounts({ stats, topicList, subjects, nodes }) {
  const topics = stats ? snapshotTopicCount(stats) : (topicList ? userTopics(topicList).length : null);
  return {
    topics,
    groups: stats ? (stats.groups || []).length : null,
    dlq: stats ? Number(stats.totalDlqDepth) || 0 : null,
    schemas: subjects ? subjects.length : null,
    nodes: nodes ? nodes.length : null,
  };
}

/**
 * The four KPI tiles of Przegląd, over the reader's topics only: the
 * snapshot's own totals (partitions, bytes on disk) include the broker's
 * `__*` topics.
 */
export function overviewKpis({ stats, topicList }) {
  const statTopics = (stats?.topics || []).filter((t) => !isInternalTopic(t.topic));
  const topics = userTopics(topicList);
  const groups = stats?.groups || [];
  return {
    rate: userRate(stats),
    writingTopics: statTopics.filter((t) => Number(t.msgsInPerSec) > 0).length,
    topics: topics.length,
    partitions: topics.reduce((s, t) => s + (Number(t.partitions) || 0), 0),
    bytesOnDisk: statTopics.reduce((sum, t) => sum + (Number(t.totalBytesOnDisk) || 0), 0),
    groups: groups.length,
    dlq: Number(stats?.totalDlqDepth) || 0,
  };
}

/**
 * The "Najbardziej obciążone topiki" rows: the busiest topics by incoming
 * rate (then by size, then by name, so an idle instance still lists its
 * topics in a stable order), each with its share of the busiest one.
 */
export function busiestTopics({ stats, topicList, limit = 5 }) {
  const byName = new Map(userTopics(topicList).map((t) => [t.name, t]));
  const rows = (stats?.topics || [])
    .filter((t) => byName.has(t.topic))
    .map((t) => {
      const cfg = byName.get(t.topic);
      return {
        name: t.topic,
        rate: Number(t.msgsInPerSec) || 0,
        waiting: Number(t.totalLag) || 0,
        bytes: Number(t.totalBytesOnDisk) || 0,
        partitions: Number(cfg.partitions) || 0,
        contentType: cfg.contentType || '',
      };
    })
    .sort((a, b) => b.rate - a.rate || b.bytes - a.bytes || a.name.localeCompare(b.name))
    .slice(0, limit);
  const max = rows.reduce((m, r) => Math.max(m, r.rate), 0);
  return rows.map((r) => ({ ...r, share: max > 0 ? Math.round((r.rate / max) * 100) : 0 }));
}

/**
 * One row per node of "Stan kopii na nodach", counted over the partitions of
 * the reader's topics (`perTopic` = `[{ topic, partitions }]`, one replica
 * snapshot per topic): how many it leads, how many copies of other leaders
 * it holds, and how many of all those are in sync. The node summary on the
 * wire counts the broker's own `__*` topics too, so it is only the source of
 * the node list, name and reachability. `null` until the per-topic snapshots
 * have answered.
 */
export function nodeRows(nodes, perTopic) {
  if (!perTopic) return null;
  const partitions = perTopic
    .filter(({ topic }) => !isInternalTopic(topic))
    .flatMap(({ partitions: parts }) => parts || []);
  return (nodes || []).map((n) => {
    const id = n.nodeId;
    let leads = 0;
    let holds = 0;
    let inSync = 0;
    for (const p of partitions) {
      const replicas = p.replicas || [];
      if (p.leaderNodeId === id) leads += 1;
      else if (replicas.includes(id)) holds += 1;
      else continue;
      if ((p.isr || []).includes(id)) inSync += 1;
    }
    return {
      nodeId: id,
      label: n.label || id,
      isLocal: Boolean(n.isLocal),
      reachable: n.reachable !== false,
      leads,
      holds,
      total: leads + holds,
      inSync,
    };
  });
}
