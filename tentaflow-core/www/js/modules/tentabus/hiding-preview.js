// ===== File: modules/tentabus/hiding-preview.js — "Podgląd, jak widzi…": a real message as one subject would read it =====
//
// The administrator picks who (a person, a group, an addon or "Wszyscy") and
// which message (partition and number; it opens on the newest message of the
// busiest partition), and the server answers with the record after that
// subject's reading rule (`FieldPolicyPreviewRequest`) and what the rule did
// to each field. Every preview is written to the audit log, and the window
// says so before anyone asks.
//
// The server applies the subject's rule on top of the administrator's own, so
// it never shows more than the administrator can read. When the
// administrator's own rule hides fields the subject would see, the answer says
// so (`limitedByCaller`) and the window explains it instead of presenting a
// narrower message as the subject's view.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { T, fmtCount, fmtWhen } from '/js/modules/tentabus/format.js';
import { windowEl } from '/js/modules/tentabus/windows.js';
import { bytesToPreviewText } from '/js/modules/tentabus/payload.js';
import { pickPartition, newestOffset, rangeText } from '/js/modules/tentabus/message-preview.js';
import { directoryLoader, pickLabel } from '/js/modules/tentabus/topic-access.js';
import { SUBJECT_KINDS, ANY_ID, fieldLabel } from '/js/modules/tentabus/topic-hiding.js';
import '/js/components/tf-button.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-spinner.js';
import '/js/components/tf-alert.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

const PAYLOAD_BYTES = 4096;

/** "grupa Księgowość", "addon Asystent lekarza", "wszyscy" — the subject as the sentences name it. */
export function whoPhrase(subject) {
  if (subject.subjectType === 'any') return T('hiding.preview.who_any');
  return `${T(`access.kind.${subject.subjectType}`).toLocaleLowerCase()} ${subject.label}`;
}

/**
 * The record as text for the window: JSON indented, anything else with its
 * line breaks made plain (an HL7 message separates segments by CR).
 */
export function displayPayload(bytes) {
  const text = bytesToPreviewText(bytes, PAYLOAD_BYTES);
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text.replace(/\r\n?|\n/g, '\n');
  }
}

/**
 * The answer of one preview as markup. `resp` = `{ record, applied,
 * limitedByCaller }`, `subject` = `{ subjectType, label }`, `source` as in
 * `fieldSource` (names the fields). The hidden fields are named and counted;
 * a limit set by the administrator's own rule is explained.
 */
export function previewResultHtml({ resp, subject, source, nowMs = Date.now() }) {
  const { record, applied } = resp;
  const hidden = (applied || []).filter((a) => a.action === 'hide').map((a) => a.field);
  const title = T('hiding.preview.record_title', { number: fmtCount(record.offset), partition: fmtCount(record.partition), when: fmtWhen(record.timestampMs, nowMs) });
  const rows = (applied || []).map((a) => {
    const label = source ? fieldLabel(source, a.field) : '';
    return `<div class="tb-right-row" data-field-action="${escapeAttr(a.action)}">
      <div class="tb-right-name"><b class="mono">${escapeHtml(a.field)}</b>${label ? `<span>${escapeHtml(label)}</span>` : ''}</div>
      <span class="tf-chip tf-chip--outline ${a.action === 'hide' ? 'warn' : 'ok'}">${escapeHtml(T(a.action === 'hide' ? 'hiding.preview.hidden' : 'hiding.preview.shown'))}</span>
    </div>`;
  }).join('');
  const summary = hidden.length
    ? T('hiding.preview.summary_hidden', { count: fmtCount(hidden.length), n: hidden.length, fields: hidden.join(', ') })
    : T('hiding.preview.summary_none');
  return `
    ${resp.limitedByCaller ? `<tf-alert tone="warning" data-role="limited" title="${escapeAttr(T('hiding.preview.limited_title'))}" message="${escapeAttr(T('hiding.preview.limited_text', { who: whoPhrase(subject) }))}"></tf-alert>` : ''}
    <div class="tb-preview-record">
      <div class="tb-preview-record-head"><span>${escapeHtml(T('hiding.preview.sees', { who: whoPhrase(subject) }))}</span> <span class="muted">${escapeHtml(title)}</span></div>
      <pre class="tb-payload" data-role="payload">${escapeHtml(displayPayload(record.payloadPreview)) || escapeHtml(T('topics.preview.payload_empty'))}</pre>
    </div>
    <div class="field">
      <label>${escapeHtml(T('hiding.preview.rules_title'))}</label>
      <div class="muted" data-role="summary">${escapeHtml(summary)}</div>
      ${rows ? `<div class="tb-rights" data-role="applied">${rows}</div>` : ''}
    </div>`;
}

/**
 * Opens the window. `ctx` = `{ instanceId, topic, partitionCount, source,
 * directory({ kind, query }), loadPartitions(), preview(request),
 * describeError }`; `loadPartitions` may reject (the window then starts on
 * partition 0 without a number).
 */
export function openHidingPreview(ctx) {
  const { topic, source } = ctx;
  const state = { kind: 'group', query: '', found: { entries: null, truncated: false, error: null }, partitions: new Map(), partition: 0, offset: '', chosen: false, turn: 0 };
  const win = windowEl({ title: T('hiding.preview.window_title', { topic }), icon: 'eye', width: 760, cls: 'tb-hiding-preview tb-access-window' });
  win.innerHTML = `
    <div slot="body" class="stack">
      <div class="tb-audit-banner">${sprite('shield')}<span><b>${escapeHtml(T('hiding.preview.audit_title'))}</b> ${escapeHtml(T('hiding.preview.audit_text'))}</span></div>
      <div class="field">
        <label>${escapeHtml(T('hiding.preview.who'))}</label>
        <tf-segmented size="md" data-role="kind" aria-label="${escapeAttr(T('hiding.preview.who'))}"></tf-segmented>
      </div>
      <div data-role="pick-box">
        <tf-searchbox data-role="query" debounce="250" placeholder="${escapeAttr(T('access.grant.search'))}"></tf-searchbox>
        <tf-select data-role="subject" label="${escapeAttr(T('access.grant.pick.group'))}"></tf-select>
      </div>
      <div class="muted" data-role="pick-note" aria-live="polite"></div>
      <div class="tb-preview-controls">
        <tf-select data-role="partition" label="${escapeAttr(T('topics.preview.partition_label'))}"></tf-select>
        <tf-input data-role="offset" type="number" inputmode="numeric" min="0" label="${escapeAttr(T('hiding.preview.number_label'))}"></tf-input>
        <div class="tb-preview-range"><div class="k">${escapeHtml(T('topics.preview.range_label'))}</div><div class="v" data-role="range">—</div></div>
      </div>
      <div data-role="result" aria-live="polite"></div>
      <div class="tb-window-error" role="alert" data-role="error" hidden>${sprite('alert')}<div data-role="error-text"></div></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-act="close">${escapeHtml(T('topics.preview.close'))}</tf-button>
      <tf-button variant="primary" icon="eye" data-act="show" disabled>${escapeHtml(T('hiding.preview.show'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  const $ = (role) => win.querySelector(`[data-role="${role}"]`);
  const kindSeg = $('kind');
  const select = $('subject');
  const note = $('pick-note');
  const offsetInput = $('offset');
  const showBtn = win.querySelector('[data-act="show"]');
  let busy = false;

  const subjectOf = () => {
    if (state.kind === 'any') return { subjectType: 'any', subjectId: ANY_ID, label: T('hiding.everyone') };
    const value = select.value;
    const e = (state.found.entries || []).find((x) => `${x.subjectType}:${x.subjectId}` === value);
    return e ? { subjectType: e.subjectType, subjectId: e.subjectId, label: e.label } : null;
  };
  const offsetValue = () => {
    const raw = String(offsetInput.value ?? '').trim();
    return /^\d+$/.test(raw) ? Number(raw) : null;
  };
  const syncButton = () => showBtn.toggleAttribute('disabled', busy || !subjectOf() || offsetValue() == null);

  const paintPick = () => {
    $('pick-box').hidden = state.kind === 'any';
    if (state.kind === 'any') {
      note.textContent = T('hiding.preview.any_hint');
      syncButton();
      return;
    }
    const options = state.found.entries || [];
    select.setAttribute('label', T(`access.grant.pick.${state.kind}`));
    select.setOptions(options.map((e) => ({ value: `${e.subjectType}:${e.subjectId}`, label: pickLabel(e) })), options[0] ? `${options[0].subjectType}:${options[0].subjectId}` : '');
    select.toggleAttribute('disabled', options.length === 0);
    if (state.found.error) note.textContent = ctx.describeError(state.found.error);
    else if (!state.found.entries) note.textContent = T('shell.loading');
    else if (!options.length) note.textContent = T(state.query ? 'access.grant.none_found' : `hiding.preview.none.${state.kind}`);
    else note.textContent = state.found.truncated ? T('access.grant.truncated') : '';
    syncButton();
  };
  const load = directoryLoader({
    fetch: ctx.directory,
    alive: () => win.isConnected,
    apply: (outcome) => {
      state.found = { entries: outcome.entries || null, truncated: Boolean(outcome.truncated), error: outcome.error || null };
      paintPick();
    },
  });
  const ask = () => {
    state.found = { entries: null, truncated: false, error: null };
    paintPick();
    if (state.kind !== 'any') load(state.kind, state.query);
  };

  kindSeg.setOptions(SUBJECT_KINDS.map((k) => ({ value: k, label: T(k === 'any' ? 'hiding.everyone' : `access.kind.${k}`) })), state.kind);
  kindSeg.addEventListener('change', () => { state.kind = kindSeg.value; ask(); });
  $('query').addEventListener('search', (e) => { state.query = String(e.detail?.value ?? '').trim(); ask(); });
  select.addEventListener('change', syncButton);

  const count = Math.max(1, Number(ctx.partitionCount) || 1);
  const partitionSelect = $('partition');
  partitionSelect.setOptions(Array.from({ length: count }, (_, i) => ({ value: String(i), label: T('topics.preview.partition_n', { n: i }) })), '0');
  const paintRange = () => { $('range').textContent = rangeText(state.partitions.get(state.partition)); };
  partitionSelect.addEventListener('change', (e) => {
    state.chosen = true;
    state.partition = Number(e.detail.value) || 0;
    const newest = newestOffset(state.partitions.get(state.partition));
    offsetInput.value = newest == null ? '' : String(newest);
    paintRange();
    syncButton();
  });
  offsetInput.addEventListener('input', () => { state.chosen = true; syncButton(); });
  offsetInput.addEventListener('change', syncButton);

  const show = async () => {
    const subject = subjectOf();
    const offset = offsetValue();
    if (!subject || offset == null || busy) return;
    busy = true;
    syncButton();
    const turn = ++state.turn;
    $('error').hidden = true;
    $('result').innerHTML = `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('shell.loading'))}</div>`;
    try {
      const resp = await ctx.preview({ instanceId: ctx.instanceId, topic, partition: state.partition, offset, subjectType: subject.subjectType, subjectId: subject.subjectId });
      if (turn === state.turn && win.isConnected) $('result').innerHTML = previewResultHtml({ resp, subject, source });
    } catch (err) {
      if (turn === state.turn && win.isConnected) {
        $('result').innerHTML = '';
        $('error').querySelector('[data-role="error-text"]').textContent = ctx.describeError(err);
        $('error').hidden = false;
      }
    }
    busy = false;
    if (win.isConnected) syncButton();
  };
  win.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-act]');
    if (!btn || btn.hasAttribute('disabled')) return;
    if (btn.dataset.act === 'close') win.close(true);
    else if (btn.dataset.act === 'show') show();
  });

  paintRange();
  ask();
  Promise.resolve()
    .then(() => ctx.loadPartitions?.())
    .then((parts) => {
      for (const p of parts || []) state.partitions.set(Number(p.partition), p);
      if (!state.chosen) {
        state.partition = pickPartition(parts);
        partitionSelect.value = String(state.partition);
        const newest = newestOffset(state.partitions.get(state.partition));
        offsetInput.value = newest == null ? '' : String(newest);
      }
    }, () => {})
    .then(() => {
      if (!win.isConnected) return;
      paintRange();
      syncButton();
    });
  return win;
}
