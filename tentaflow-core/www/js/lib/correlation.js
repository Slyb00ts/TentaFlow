// ===== File: lib/correlation.js — correlation id helpers shared by Events, TentaBus and Flows (TB-02.4) =====
//
// Pure functions: how a correlation id is shortened for a table, which
// address each screen is reached by, and where the record that triggered a
// bus run sits. The id is an untrusted label, so every address is built with
// `encodeURIComponent` and every id is only ever put into text or attributes
// through the caller's escaping.

const SHORT_MAX = 16;

/** "0193c7e2-5b1a-7c3e-9a40-2f6d8e1b7a55" → "0193c7e2…7a55"; short ids stay whole. */
export function shortCorrelation(id) {
  const text = String(id ?? '');
  return text.length > SHORT_MAX ? `${text.slice(0, 8)}…${text.slice(-4)}` : text;
}

/** The text a filter input holds, or `null` when it names no correlation. */
export function normalizeCorrelation(text) {
  const value = String(text ?? '').trim();
  return value === '' ? null : value;
}

/** The Events screen narrowed to one correlation. */
export function eventsCorrelationHref(correlationId) {
  return `#/events?correlation=${encodeURIComponent(correlationId)}`;
}

/**
 * The TentaBus preview opened on the record that triggered a run. The
 * correlation id narrows the preview to that chain; `partition` and `offset`
 * are where its search starts.
 */
export function busRecordHref({ instanceId = null, topic, partition, offset }, correlationId = null) {
  const params = [];
  if (instanceId) params.push(`instance=${encodeURIComponent(instanceId)}`);
  params.push(`topic=${encodeURIComponent(topic)}`);
  if (correlationId) params.push(`corr=${encodeURIComponent(correlationId)}`);
  params.push(`partition=${encodeURIComponent(partition)}`, `offset=${encodeURIComponent(offset)}`);
  return `#/tentabus?${params.join('&')}`;
}

/**
 * Coordinates of the bus record that triggered a run, read from its
 * `request_started` row (with the instance it lives on, when recorded); `null` for a run of any other origin, a batch, or a
 * row written before the coordinates were recorded.
 */
export function busSourceOf(startRow) {
  const source = startRow?.payload?.bus_source;
  if (!source || typeof source.topic !== 'string' || source.topic === '') return null;
  const partition = Number(source.partition);
  const offset = Number(source.offset);
  if (!Number.isInteger(partition) || partition < 0 || !Number.isInteger(offset) || offset < 0) return null;
  const instanceId = typeof source.instance_id === 'string' && source.instance_id !== '' ? source.instance_id : null;
  return { instanceId, topic: source.topic, partition, offset };
}
