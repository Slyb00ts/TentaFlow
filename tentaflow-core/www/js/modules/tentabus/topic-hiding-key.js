// ===== File: modules/tentabus/topic-hiding-key.js — "Zmień klucz haszowania": rotating the organization's hash key, confirmed by retyping words =====
//
// A "Zahaszuj" rule replaces a value with a keyed hash. The key belongs to the
// ORGANIZATION, not to the instance or the topic, so every instance of the
// organization hashes with the same one — and rotating it changes every hash
// in all of them at once: the old hashes stop matching the new ones, and
// whatever joined data by a hash (a report, a program) stops finding the older
// messages. The window says exactly that before anyone confirms, and wants the
// words retyped (the danger window every screen shares).
//
// How far the change reaches is counted by the server from the rules THIS
// node knows (`hashingRules`, `hashingTopics`, `hashingInstances`); rules of
// instances hosted on other nodes are not among them. The figures are
// therefore always "at least", and the window says whose data they are. Only
// the instance administrator may see or rotate the key; the request names the
// version the window showed (`expectedKeyVersion`), so a second administrator,
// or a double click, rotates nothing.

import { escapeHtml } from '/js/utils.js';
import { T, fmtCount, fmtDate } from '/js/modules/tentabus/format.js';
import { openRetypeDialog } from '/js/lib/retype-dialog.js';

/** The first lines of `keyImpact` are what the change does; the rest say how far it was counted and which version follows. */
const CONSEQUENCES = 2;

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

/**
 * How many rules the change touches, as the server counted them on this node
 * (`info` = `BusFieldHashKeyWire`): "co najmniej 3 zasady „zahaszuj” w 2
 * topikach (1 instancja)", or the plain statement that this node sees none.
 */
export function reachText(info) {
  const rules = Number(info?.hashingRules) || 0;
  const topics = Number(info?.hashingTopics) || 0;
  const instances = Number(info?.hashingInstances) || 0;
  if (rules === 0) return T('hiding.key.reach_none');
  return T('hiding.key.reach', {
    rules: fmtCount(rules), r: rules, topics: fmtCount(topics), t: topics, instances: fmtCount(instances), i: instances,
  });
}

/**
 * The lines the window lists under "Co się stanie": how far the key reaches,
 * what changes for the hashes, where the figures come from and the version
 * step. `info` = `BusFieldHashKeyWire`, `orgName` = the organization's name
 * or `null`.
 */
export function keyImpact({ info, orgName }) {
  const version = Number(info?.keyVersion) || 0;
  return [
    T(orgName ? 'hiding.key.scope' : 'hiding.key.scope_org', { org: orgName ?? '' }),
    T('hiding.key.stale'),
    reachText(info),
    T('hiding.key.reach_note'),
    T('hiding.key.version_step', { date: fmtDate(info?.rotatedAtMs || info?.createdAtMs), from: version, to: version + 1 }),
  ];
}

/** The note after the key changed: the new version and what it did to the old hashes. `key` = the answer's `BusFieldHashKeyWire` (its figures are from before the change). */
export function rotatedNotice({ key, orgName }) {
  return {
    title: T('hiding.key.rotated_title'),
    text: T(orgName ? 'hiding.key.rotated_text' : 'hiding.key.rotated_text_org', { org: orgName ?? '', version: key.keyVersion, reach: reachText(key) }),
  };
}

/** Whether `err` is the server's refusal because the key is no longer the version the window named (`bus.field_hash_key_changed`). */
const keyChanged = (err) => /\bbus\.field_hash_key_changed\b/.test(String(err?.message ?? err ?? ''));

const impactHtml = (impact) => impact.map((line, i) => `<li${i < CONSEQUENCES ? '' : ' class="tb-impact-note"'}>${sprite(i < CONSEQUENCES ? 'alert' : 'info')}<span>${escapeHtml(line)}</span></li>`).join('');

/**
 * Opens the window. `info` = the key as the server reports it now (it must
 * exist), `orgName`, `rotate(expectedKeyVersion)` rotates it (may throw: the
 * window stays open with `describeError(err)`) and answers `{ key }`;
 * `onRotated(key)` runs after the window closed. `reload()` (optional) reads
 * the key again: when somebody changed it first (`bus.field_hash_key_changed`)
 * the window takes the current version, repaints what it lists, says so, and
 * stays open — a second confirmation then names the version that is stored,
 * where the frozen one would be refused for ever.
 */
export function openHashKeyRotate({ info: opened, orgName, rotate, describeError, onRotated, reload = null }) {
  let info = opened;
  const word = T('hiding.key.retype_word');
  return openRetypeDialog({
    title: T('hiding.key.window_title'),
    icon: 'key',
    name: word,
    modal: true,
    className: 'tb-window tb-delete-window tb-hash-key-window',
    width: 600,
    bodyHtml: `
      <div class="tb-explain-box" data-role="explain">${escapeHtml(T(orgName ? 'hiding.key.explain' : 'hiding.key.explain_org', { org: orgName ?? '' }))}</div>
      <div class="tb-danger-box">${sprite('alert')}<div><b>${escapeHtml(T('hiding.key.irreversible'))}</b> ${escapeHtml(T('hiding.key.explain_change'))}</div></div>
      <div><b>${escapeHtml(T('hiding.key.impact_title'))}</b></div>
      <ul class="tb-impact-list" data-role="impact">${impactHtml(keyImpact({ info, orgName }))}</ul>
      <div class="tb-kept-box">${sprite('file-text')} ${escapeHtml(T('hiding.key.audit'))}</div>`,
    retypeLabel: `${escapeHtml(T('hiding.key.retype'))} <code class="tb-retype-name">${escapeHtml(word)}</code>`,
    confirmLabel: T('hiding.key.confirm'),
    confirmIcon: 'key',
    describeError,
    onConfirm: async (win) => {
      let answer;
      try {
        answer = await rotate(info.keyVersion);
      } catch (err) {
        const fresh = keyChanged(err) && reload ? await reload().catch(() => null) : null;
        if (!fresh?.exists) throw err;
        info = fresh;
        win.querySelector('[data-role="impact"]').innerHTML = impactHtml(keyImpact({ info, orgName }));
        const note = win.querySelector('#retype-error');
        note.textContent = T('hiding.key.changed_reloaded', { version: info.keyVersion });
        note.hidden = false;
        return false;
      }
      queueMicrotask(() => onRotated?.(answer?.key ?? info));
      return true;
    },
  });
}
