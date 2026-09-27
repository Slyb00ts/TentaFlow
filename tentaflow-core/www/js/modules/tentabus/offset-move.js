// ===== File: modules/tentabus/offset-move.js — "Przesuń miejsce czytania": the window that moves where a consumer reads one partition =====
//
// Every message of a partition has the next number. A consumer remembers the
// number it reads next (`committed`): everything below it is read and
// confirmed, and `highWatermark - committed` messages wait. Moving that place
// (`OffsetResetRequest`) either makes the consumer read messages again
// (a number below `committed`) or skip waiting ones (a number above it).
//
// Server facts the window states (tentaflow-core/src/bus):
//   - the four ways resolve on the server: the oldest message kept, the next
//     number to be written, a number given here (refused outside the kept
//     range), or the first message written at the chosen time or later —
//     which the window looks up first (`OffsetForTimestampRequest`) so it can
//     count exactly what the move does before anyone confirms;
//   - a program of the consumer that is reading right now takes the new place
//     at its next fetch; what it fetched before the move and confirms later
//     does not undo the move (`reset_offset` + the consumer's own reload);
//   - copies of the partition on other nodes follow a move forward but not a
//     move back: after its leadership changes node, the consumer resumes from
//     the number it had before moving back;
//   - the move is written to the audit log (`bus.offset.reset`).

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, fmtCount } from '/js/modules/tentabus/format.js';
import { patchHtml } from '/js/lib/dom-patch.js';
import '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-select.js';
import '/js/components/tf-input.js';
import '/js/components/tf-choice-card.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

export const MOVE_MODES = ['earliest', 'latest', 'explicit', 'timestamp'];
/** How far back "Od wybranego numeru" starts out. */
const SUGGESTED_REREAD = 1000;
const MODE_ICONS = { earliest: 'history', latest: 'arrow-up', explicit: 'list', timestamp: 'clock' };

/**
 * One partition as the window reads it: the number read next, the next
 * number to be written and the oldest number still kept.
 */
export function partitionPlace({ partition, committedOffset, lag, earliestOffset }) {
  const committed = Number(committedOffset) || 0;
  const highWatermark = committed + (Number(lag) || 0);
  return {
    partition: Number(partition),
    committed,
    highWatermark,
    earliest: Math.min(Number(earliestOffset) || 0, highWatermark),
    waiting: highWatermark - committed,
  };
}

/** The number of the last message the consumer read, or `null` before it read any. */
export const lastRead = (place) => (place.committed > 0 ? place.committed - 1 : null);
/** The number of the newest message of the partition, or `null` for an empty one. */
export const lastMessage = (place) => (place.highWatermark > 0 ? place.highWatermark - 1 : null);

/**
 * The kept message numbers "Od wybranego numeru" accepts, `{ from, to }`, or
 * `null` for a partition that keeps none. Starting after the newest message
 * is "Od najnowszej wiadomości", not a number of a message.
 */
export function keptRange(place) {
  return place.highWatermark > place.earliest ? { from: place.earliest, to: place.highWatermark - 1 } : null;
}

/** The number typed for "Od wybranego numeru", or `null` when it is not a kept message number. */
export function parseOffsetInput(raw, place) {
  const range = keptRange(place);
  const text = String(raw ?? '').replace(/[\s  ]/g, '');
  if (!range || !/^\d+$/.test(text)) return null;
  const n = Number(text);
  if (!Number.isSafeInteger(n) || n < range.from || n > range.to) return null;
  return n;
}

/** `<tf-input type="datetime-local">` holds local time without a zone; `new Date` reads it the same way. */
export function datetimeLocalToMs(value) {
  if (!value) return null;
  const ms = new Date(value).getTime();
  return Number.isFinite(ms) ? ms : null;
}

/**
 * The number the consumer would read next after the move, or `null` while it
 * is not known yet (no valid number typed, time not looked up yet).
 */
export function moveTarget(mode, place, { explicit = null, resolved = null } = {}) {
  if (mode === 'earliest') return place.earliest;
  if (mode === 'latest') return place.highWatermark;
  if (mode === 'explicit') return explicit;
  return resolved;
}

/** What a move from `before` to `target` does: read again, skip, or nothing. */
export function moveEffect(place, target) {
  if (target == null) return null;
  if (target < place.committed) return { kind: 'reread', count: place.committed - target, waitingAfter: place.highWatermark - target };
  if (target > place.committed) return { kind: 'skip', count: target - place.committed, waitingAfter: place.highWatermark - target };
  return { kind: 'same', count: 0, waitingAfter: place.waiting };
}

/** The sentences under "Co się stanie po przesunięciu". */
export function moveImpact({ group, place, target, paused, replicated, timeAfterAll = false }) {
  const effect = moveEffect(place, target);
  if (!effect) return [];
  const p = fmtCount(place.partition);
  if (effect.kind === 'same') return [T('move.impact_same', { partition: p })];
  const key = effect.kind === 'skip' && effect.waitingAfter === 0 ? 'skip_all' : effect.kind;
  const lines = [T(`move.impact_${key}`, {
    group,
    partition: p,
    count: fmtCount(effect.count),
    n: effect.count,
    from: fmtCount(target),
    waiting: fmtCount(effect.waitingAfter),
  })];
  // A time after every kept message resolves to the next number written:
  // the consumer then reads all new messages, also those written before
  // that time — the card's "o tej godzinie lub później" does not hold.
  if (timeAfterAll) lines.push(T('move.impact_time_after_all'));
  lines.push(T(paused ? 'move.impact_paused' : 'move.impact_live'));
  if (paused && replicated) lines.push(T('consumer.paused_copies'));
  if (effect.kind === 'reread' && replicated) lines.push(T('move.impact_copies', { from: fmtCount(place.committed) }));
  return lines;
}

/**
 * The "Zapisano" line: the number the server set and what waits in the
 * partition by the page's answer after the move. Not a count against the
 * place the window opened with — the consumer may have read on meanwhile.
 */
export function movedText({ partition, after, waiting }) {
  const p = fmtCount(partition);
  if (waiting == null) return T('move.done_from', { partition: p, from: fmtCount(after) });
  return T('move.done', { partition: p, from: fmtCount(after), count: fmtCount(waiting), n: waiting });
}

/** The descriptions of the four choices, with the numbers of this partition. */
export function modeDescription(mode, place) {
  if (mode === 'earliest') return T('move.mode_earliest_sub', { from: fmtCount(place.earliest) });
  if (mode === 'latest') {
    return place.waiting > 0
      ? T('move.mode_latest_sub', { count: fmtCount(place.waiting), n: place.waiting })
      : T('move.mode_latest_sub_none');
  }
  return T(`move.mode_${mode}_sub`);
}

/**
 * Opens the window for one consumer, on `partition`. `places` = one
 * `partitionPlace` per partition; `resolveTimestamp(partition, tsMs)`
 * answers the number of the first message at that time or later; `move({
 * partition, mode, offset?, tsMs? })` sends the move and answers the number
 * the server set (may throw: the window stays with `describeError(err)`);
 * `onMoved({ partition, after })` runs after the window closed.
 */
export function openOffsetMove({ group, places, partition, paused, replicated, resolveTimestamp, move, describeError, onMoved }) {
  let place = places.find((p) => p.partition === Number(partition)) || places[0];
  let mode = 'explicit';
  let resolved = null;
  // `lookups` only ever grows: it names the newest lookup, so an answer to an
  // older one (another time, another partition, a mode left since) is dropped
  // even after a lookup that ended without asking the server.
  let lookups = 0;
  let resolving = false;
  let resolveError = null;
  let busy = false;

  const win = document.createElement('tf-window');
  win.className = 'tb-window tb-move-window';
  win.setAttribute('icon', 'history');
  win.setAttribute('buttons', 'close');
  win.setAttribute('modal', '');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '640');
  win.setAttribute('min-width', '360');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      <div class="form-grid-2 tb-move-head">
        <tf-select id="tb-move-partition" label="${escapeAttr(T('move.partition'))}"></tf-select>
        <div class="field">
          <label>${escapeHtml(T('move.now'))}</label>
          <div class="tb-move-now" data-role="now"></div>
        </div>
      </div>
      <tf-choice-group id="tb-move-mode" value="${mode}" columns="1" aria-label="${escapeAttr(T('move.mode_label'))}">
        ${MOVE_MODES.map((m) => `<tf-choice-card value="${m}" icon="${MODE_ICONS[m]}" heading="${escapeAttr(T(`move.mode_${m}`))}"></tf-choice-card>`).join('')}
      </tf-choice-group>
      <tf-input id="tb-move-offset" class="tb-mono-input" inputmode="numeric" label="${escapeAttr(T('move.offset_label'))}"></tf-input>
      <tf-input id="tb-move-time" type="datetime-local" step="1" label="${escapeAttr(T('move.time_label'))}"></tf-input>
      <div class="tb-will-happen" data-role="impact" aria-live="polite"></div>
      <div class="tb-window-error" role="alert" data-role="error" hidden>${sprite('alert')}<span></span></div>
    </div>
    <div slot="footer">
      <span class="tb-foot-note">${sprite('shield')}${escapeHtml(T('move.audit'))}</span>
      <tf-button variant="ghost" data-act="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="history" data-act="move" disabled>${escapeHtml(T('move.button'))}</tf-button>
    </div>`;
  document.body.appendChild(win);

  const pick = win.querySelector('#tb-move-partition');
  const group$ = win.querySelector('#tb-move-mode');
  const offsetInput = win.querySelector('#tb-move-offset');
  const timeInput = win.querySelector('#tb-move-time');
  const moveBtn = win.querySelector('[data-act="move"]');
  const cancelBtn = win.querySelector('[data-act="cancel"]');
  pick.setOptions(places.map((p) => ({ value: String(p.partition), label: T('move.partition_name', { n: fmtCount(p.partition) }) })), String(place.partition));
  // Read the last thousand again: a first number that already changes something.
  const suggest = () => String(Math.max(place.earliest, place.committed - SUGGESTED_REREAD));
  offsetInput.value = suggest();

  const explicitValue = () => parseOffsetInput(offsetInput.value, place);
  const target = () => moveTarget(mode, place, { explicit: explicitValue(), resolved });

  const sync = () => {
    win.setAttribute('title', T('move.title', { group, partition: fmtCount(place.partition) }));
    const read = lastRead(place);
    const last = lastMessage(place);
    let now;
    if (last == null) now = escapeHtml(T('move.now_empty'));
    else if (read == null) now = T('move.now_none_read', { last: escapeHtml(fmtCount(last)) });
    else now = T('move.now_value', { read: `<b>${escapeHtml(fmtCount(read))}</b>`, last: escapeHtml(fmtCount(last)) });
    patchHtml(win.querySelector('[data-role="now"]'), now);
    for (const card of group$.querySelectorAll('tf-choice-card')) {
      card.setAttribute('description', modeDescription(card.getAttribute('value'), place));
    }
    offsetInput.hidden = mode !== 'explicit';
    timeInput.hidden = mode !== 'timestamp';
    const range = keptRange(place);
    const offsetBad = mode === 'explicit' && explicitValue() == null;
    if (!range) offsetInput.setAttribute('error', T('move.offset_none_kept'));
    else if (offsetBad) offsetInput.setAttribute('error', T('move.offset_invalid', { from: fmtCount(range.from), to: fmtCount(range.to) }));
    else offsetInput.removeAttribute('error');
    if (range) offsetInput.setAttribute('hint', T('move.offset_hint', { from: fmtCount(range.from), to: fmtCount(range.to) }));
    else offsetInput.removeAttribute('hint');

    const t = target();
    let impact;
    if (mode === 'timestamp' && resolving) impact = `${sprite('info')}<div>${escapeHtml(T('move.time_resolving'))}</div>`;
    else if (mode === 'timestamp' && resolveError) impact = `${sprite('info')}<div>${escapeHtml(resolveError)}</div>`;
    else if (t == null) impact = `${sprite('info')}<div>${escapeHtml(T(mode === 'timestamp' ? 'move.time_pick' : 'move.offset_fix'))}</div>`;
    else {
      const timeAfterAll = mode === 'timestamp' && t === place.highWatermark;
      const lines = moveImpact({ group, place, target: t, paused, replicated, timeAfterAll });
      impact = `${sprite('info')}<div><b>${escapeHtml(T('move.will_happen'))}</b> ${lines.map(escapeHtml).join(' ')}</div>`;
    }
    patchHtml(win.querySelector('[data-role="impact"]'), impact);
    const effect = moveEffect(place, t);
    moveBtn.toggleAttribute('disabled', busy || !effect || effect.kind === 'same' || resolving);
    cancelBtn.toggleAttribute('disabled', busy);
  };

  // Only the newest lookup counts: a slower answer for an earlier time or
  // another partition never lands under the current choice.
  const lookup = async () => {
    const turn = ++lookups;
    resolved = null;
    resolveError = null;
    const tsMs = datetimeLocalToMs(timeInput.value);
    if (mode !== 'timestamp' || tsMs == null) { resolving = false; sync(); return; }
    resolving = true;
    sync();
    const forPartition = place.partition;
    try {
      const offset = await resolveTimestamp(forPartition, tsMs);
      if (turn !== lookups) return;
      resolved = Math.min(Math.max(Number(offset), place.earliest), place.highWatermark);
    } catch (err) {
      if (turn !== lookups) return;
      resolveError = describeError(err);
    }
    resolving = false;
    sync();
  };

  pick.addEventListener('change', (e) => {
    place = places.find((p) => p.partition === Number(e.detail?.value)) || place;
    offsetInput.value = suggest();
    lookup();
  });
  group$.addEventListener('change', (e) => { mode = e.detail?.value || mode; lookup(); });
  offsetInput.addEventListener('input', sync);
  timeInput.addEventListener('change', lookup);
  timeInput.addEventListener('input', lookup);
  sync();

  win.addEventListener('close-request', (e) => { if (busy) e.preventDefault(); });
  win.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-act]');
    if (!btn || btn.hasAttribute('disabled')) return;
    if (btn.dataset.act === 'cancel') { win.close(true); return; }
    const t = target();
    const effect = moveEffect(place, t);
    if (!effect || effect.kind === 'same') return;
    busy = true;
    sync();
    const errEl = win.querySelector('[data-role="error"]');
    errEl.hidden = true;
    const request = { partition: place.partition, mode };
    if (mode === 'explicit') request.offset = t;
    if (mode === 'timestamp') request.tsMs = datetimeLocalToMs(timeInput.value);
    let after;
    try {
      after = Number(await move(request));
    } catch (err) {
      busy = false;
      sync();
      errEl.querySelector('span').textContent = describeError(err);
      errEl.hidden = false;
      return;
    }
    win.close(true);
    onMoved?.({ partition: place.partition, after });
  });
  return win;
}
