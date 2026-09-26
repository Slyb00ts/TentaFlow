// ===== File: modules/tentabus/topic-delete.js — "Usuń topik" (T02): what goes with the topic, confirmed by retyping its name =====
//
// The danger window every screen shares (lib/retype-dialog.js): it lists
// what disappears with the topic — its messages, its unprocessed messages,
// the access entries and data-hiding rules set on it, the consumers that
// stop receiving — and what stays (the message pattern and the API keys,
// which belong to the instance). Counts that could not be read are left out
// rather than guessed; the delete button unlocks only on the exact name.

import { escapeHtml } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, fmtCount, fmtBytes } from '/js/modules/tentabus/format.js';
import { openRetypeDialog } from '/js/lib/retype-dialog.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

/**
 * The lines of the window, as `{ lost: string[], kept: string }` (plain
 * text; the caller escapes). `topic` = the list row (`partitions`,
 * `schemaId`), `stats` = its stats row or null, `consumers` = names of the
 * consumers reading it, `aclCount` / `policyCount` = the entries on it, or
 * `null` when they could not be read.
 */
export function deleteImpact({ topic, stats, consumers = [], aclCount = null, policyCount = null }) {
  const lost = [];
  const partitions = Number(topic?.partitions) || 0;
  const bytes = Number(stats?.totalBytesOnDisk) || 0;
  lost.push(bytes > 0
    ? T('topics.delete.lost_messages', { size: fmtBytes(bytes), count: fmtCount(partitions), n: partitions })
    : T('topics.delete.lost_empty_partitions', { count: fmtCount(partitions), n: partitions }));
  const dlq = Number(stats?.dlqDepth) || 0;
  if (dlq > 0) {
    lost.push(T('topics.delete.lost_dlq', { count: fmtCount(dlq), n: dlq }));
    // They go before the topic, so a delete that fails half-way has already taken them.
    lost.push(T('topics.delete.lost_dlq_first'));
  }
  const acl = Number(aclCount) || 0;
  const policies = Number(policyCount) || 0;
  if (acl > 0 && policies > 0) lost.push(T('topics.delete.lost_rules_and_access', { rules: fmtCount(policies), r: policies, entries: fmtCount(acl), n: acl }));
  else if (policies > 0) lost.push(T('topics.delete.lost_rules', { count: fmtCount(policies), n: policies }));
  else if (acl > 0) lost.push(T('topics.delete.lost_access', { count: fmtCount(acl), n: acl }));
  const names = [...new Set(consumers)].sort();
  if (names.length) {
    const list = new Intl.ListFormat(I18n.getLanguage(), { type: 'conjunction' }).format(names);
    lost.push(T('topics.delete.lost_consumers', { names: list, n: names.length }));
  }
  const kept = topic?.schemaId
    ? T('topics.delete.kept_schema', { name: topic.schemaId })
    : T('topics.delete.kept_keys');
  return { lost, kept };
}

/**
 * Opens the window. `loadCounts()` resolves `{ aclCount, policyCount }`
 * (either may be `null`); the lines appear as soon as the window opens and
 * the counts join them when they answer. `remove()` deletes the topic (it
 * may throw: the window stays open with `describeError(err)`), `onDeleted()`
 * runs after the window closed.
 */
export function openTopicDelete({ topic, stats, consumers, loadCounts, remove, describeError, onDeleted }) {
  const name = topic.name;
  const impactHtml = (impact) => `
    <ul class="tb-impact-list">${impact.lost.map((line) => `<li>${sprite('trash')}<span>${escapeHtml(line)}</span></li>`).join('')}</ul>
    <div class="tb-kept-box">${escapeHtml(impact.kept)}</div>`;
  const first = deleteImpact({ topic, stats, consumers });
  const win = openRetypeDialog({
    title: T('topics.delete.title', { name }),
    icon: 'alert',
    name,
    modal: true,
    className: 'tb-window tb-delete-window',
    width: 600,
    bodyHtml: `
      <div class="tb-danger-box">${sprite('alert')}<div><b>${escapeHtml(T('topics.delete.irreversible'))}</b> ${escapeHtml(T('topics.delete.everything', { name }))}</div></div>
      <div data-role="impact">${impactHtml(first)}</div>`,
    retypeLabel: `${escapeHtml(T('topics.delete.retype'))} <code class="tb-retype-name">${escapeHtml(name)}</code>`,
    confirmLabel: T('topics.delete.confirm', { name }),
    describeError,
    onConfirm: async () => {
      await remove();
      queueMicrotask(() => onDeleted?.());
      return true;
    },
  });
  Promise.resolve(loadCounts?.()).then((counts) => {
    const host = win.querySelector('[data-role="impact"]');
    if (!host || !counts) return;
    host.innerHTML = impactHtml(deleteImpact({ topic, stats, consumers, ...counts }));
  }, () => {});
  return win;
}
