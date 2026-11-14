// =============================================================================
// File: modules/flows-builder/bus-consumer.js
// Description: Text of a `bus_consume` flow's consumer status (`busConsumer` on
//       the flow list and detail responses), shared by the flow list chip, the
//       canvas badge and the inspector banner so all three say the same thing.
//       The status is per node: the answering node is named.
// =============================================================================

import { I18n } from '/js/i18n.js';

const STATES = new Set(['running', 'waiting', 'refused', 'halted']);

/**
 * `{ state, text, detail }` for a status, or `null` when there is none.
 * `text` is the short label ("Odbiór wstrzymany"), `detail` adds the reason and
 * the node ("brak konta usługowego, na węźle n1").
 */
export function busConsumerText(status) {
  if (!status || !STATES.has(status.state)) return null;
  const text = I18n.t(`flows_config.consumer_state_${status.state}`);
  const parts = [];
  if (status.reason) {
    const key = `flows_config.consumer_reason_${status.reason}`;
    const reason = I18n.t(key);
    parts.push(reason && reason !== key ? reason : status.reason);
  }
  if (status.nodeId) parts.push(I18n.t('flows_config.consumer_on_node', { node: status.nodeId }));
  return { state: status.state, text, detail: parts.length ? `${text}: ${parts.join(', ')}` : text };
}

/** Chip status for `<tf-chip status>`. */
export function busConsumerChipStatus(state) {
  return { running: 'success', waiting: 'info', refused: 'warning', halted: 'danger' }[state] || 'neutral';
}
