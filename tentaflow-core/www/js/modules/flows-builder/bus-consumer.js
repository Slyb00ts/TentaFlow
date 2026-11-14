// =============================================================================
// File: modules/flows-builder/bus-consumer.js
// Description: Text of a `bus_consume` flow's consumer status (`busConsumer` on
//       the flow list and detail responses), shared by the flow list chip, the
//       canvas badge and the inspector banner so all three say the same thing.
//       The status is per node: the answering node is named.
// =============================================================================

import { I18n } from '/js/i18n.js';

const STATES = new Set(['running', 'waiting', 'refused', 'halted']);

// The node id the status carries is a 64-character mesh id; eight characters
// tell nodes apart and fit a phone.
const NODE_ID_SHORT = 8;
const HALT_DETAIL_MAX = 200;

/**
 * `{ state, text, reason, detail }` for a status, or `null` when there is none.
 * `text` is the short label ("Odbiór wstrzymany"), `reason` says why in words
 * (so a phone, which has no hover, can show it), and `detail` is both joined
 * with the node ("brak konta usługowego, na węźle fb2b1369").
 */
export function busConsumerText(status) {
  if (!status || !STATES.has(status.state)) return null;
  const text = I18n.t(`flows_config.consumer_state_${status.state}`);
  const why = [];
  if (status.reason) {
    const key = `flows_config.consumer_reason_${status.reason}`;
    const reason = I18n.t(key);
    why.push(reason && reason !== key ? reason : status.reason);
  }
  if (status.state === 'halted') {
    if (status.detail) why.push(String(status.detail).slice(0, HALT_DETAIL_MAX));
    why.push(I18n.t('flows_config.consumer_halted_hint'));
  }
  const reason = why.join(' — ');
  const parts = reason ? [reason] : [];
  if (status.nodeId) {
    const node = String(status.nodeId);
    parts.push(I18n.t('flows_config.consumer_on_node', { node: node.length > NODE_ID_SHORT ? node.slice(0, NODE_ID_SHORT) : node }));
  }
  return { state: status.state, text, reason, detail: parts.length ? `${text}: ${parts.join(', ')}` : text };
}

/** Chip status for `<tf-chip status>`: the names the chip knows (ok, warn, err, info). */
export function busConsumerChipStatus(state) {
  return { running: 'ok', waiting: 'info', refused: 'warn', halted: 'err' }[state] || 'info';
}

/**
 * The text of a refused flow save. The server's English message ends with
 * `[sa:<kind> <json>]` when it is a service-account refusal; this swaps it for
 * the localised `flows_config.sa_error.<kind>` string (node by its label, key by
 * its name) and drops the transport prefix. Anything else comes back as it was,
 * minus the prefix.
 */
export function explainServiceAccountError(message) {
  const text = String(message ?? '').replace(/^protocol error \w+: /, '');
  const m = text.match(/^([\s\S]*?) \[sa:(\w+) (\{[\s\S]*\})\]\s*$/);
  if (!m) return text;
  const [, english, kind, json] = m;
  let params = {};
  try {
    params = JSON.parse(json);
  } catch {
    return english;
  }
  const key = `flows_config.sa_error.${kind}`;
  const localised = I18n.t(key, params);
  return localised && localised !== key ? localised : english;
}

const GROUP_PATTERN = /^[a-z0-9][a-z0-9.-]{1,120}$/;

/** The reason a consumer group name is refused, or `''` when it is fine (or still empty). */
export function busGroupProblem(group) {
  const value = String(group ?? '').trim();
  if (!value) return '';
  return GROUP_PATTERN.test(value) ? '' : 'flows_config.group_invalid';
}
