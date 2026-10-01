// =============================================================================
// File: modules/org-structure/handover-tab.js
// Description: The handover screen "Do przekazania" (mockup F07): everything a
//   person holds — tasks and test items, project memberships, positions and
//   deputies — grouped, each with a proposed taker and the reason for the
//   proposal, "hand everything to ..." for the selected rows, a required note
//   for the takers and one button that moves what is ticked.
//   Three reasons share the screen: a departure (permanent), an absence
//   (temporary: the work comes back on the return day unless the taker closed
//   or changed it) and the removal from one project (only that project).
//
//   The server decides what the person holds, who may take each item and who
//   is proposed; the screen keeps the operator's choices (handover-model.js).
//   The answer says what became of EVERY item: what failed stays on screen
//   with the rule that stopped it, and "Ponów nieudane" tries only those again
//   from the record the server kept. Nothing here is drawn from a guess.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { addDays, formatDay } from '/js/lib/date-format.js';
import { TfToast } from '/js/components/tf-toast.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-avatar.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-select.js';
import '/js/components/tf-checkbox.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-input.js';
import '/js/components/tf-empty-state.js';
import { writeErrorText } from '/js/modules/org-structure/list-actions.js';
import { scopeLabel } from '/js/modules/org-structure/cover-actions.js';
import {
  allowedReasons, applyPayload, applyToSelected, categoryIcon, grouped,
  initialReason, initials, problems, reasonKey, rowsOf, selectedRows, sortedTakers, summarize, takerOptions,
} from '/js/modules/org-structure/handover-model.js';

const ht = (key, params) => I18n.t(`org_structure.handover.${key}`, params);

const DEFAULT_ABSENCE_DAYS = 7;

let state = null;

/** The sentence for a code the server answered with: this screen's own, else the structure's rule text. */
export function reasonText(code) {
  const key = reasonKey(code);
  if (key) {
    const text = I18n.t(`org_structure.handover.${key}`);
    if (text && text !== `org_structure.handover.${key}`) return text;
  }
  return writeErrorText({ code });
}


// =============================================================================
// Loading
// =============================================================================

async function load() {
  const seq = ++state.loadSeq;
  const listing = await ApiBinary.one('orgHandoverListRequest', {
    userId: state.userId,
    reason: state.reason,
    projectId: state.reason === 'project_removal' ? state.projectId : null,
    date: state.reason === 'departure' ? state.date : null,
  });
  // A slower answer for an earlier choice must not overwrite the one on screen.
  if (!state || seq !== state.loadSeq) return;
  state.listing = listing;
  state.takers = sortedTakers(listing.takers);
  state.rows = rowsOf(listing.groups);
  state.date = listing.date;
  if (state.reason === 'absence' && !state.returnDate) state.returnDate = addDays(listing.date, DEFAULT_ABSENCE_DAYS);
}

async function reload() {
  try {
    await load();
    renderBody();
  } catch (err) {
    showLoadError(err);
  }
}

function showLoadError(err) {
  const box = state?.host.querySelector('[data-role="rows"]');
  if (box) box.innerHTML = `<div class="org-ho-error">${escapeHtml(ht('load_failed', { message: err.message || '' }))}</div>`;
}

// =============================================================================
// Drawing
// =============================================================================

function frame() {
  return `
    <div class="org-ho">
      <div class="org-ho-top">
        <tf-button variant="secondary" icon="arrow-left" data-act="back">${escapeHtml(ht('back'))}</tf-button>
      </div>
      <div class="org-ho-titlebar">
        <div class="org-ho-title" data-role="title"></div>
        <tf-segmented size="md" data-role="reason" aria-label="${escapeAttr(ht('reason_label'))}"></tf-segmented>
      </div>
      <div class="org-ho-ctx">
        <div class="org-ho-dates" data-role="dates"></div>
        <tf-alert tone="info" data-role="ctxnote"></tf-alert>
      </div>
      <div data-role="result"></div>
      <section class="org-ho-card" data-role="list-card">
        <div class="org-ho-bulk" data-role="bulk"></div>
        <div data-role="rows"><div class="org-loading">${escapeHtml(ht('loading'))}</div></div>
      </section>
      <section class="org-ho-card org-ho-foot" data-role="foot">
        <div class="org-ho-note">
          <tf-textarea data-role="note" rows="3" label="${escapeAttr(ht('note_label'))} *"
            placeholder="${escapeAttr(ht('note_placeholder'))}" maxlength="4000"></tf-textarea>
          <div class="org-ho-sumnote">${escapeHtml(ht('note_hint'))}</div>
        </div>
        <tf-button variant="primary" icon="send" data-act="submit" data-role="submit"></tf-button>
      </section>
    </div>`;
}

function q(role) {
  return state.host.querySelector(`[data-role="${role}"]`);
}

function reasonLabel(reason) {
  return reason === 'project_removal'
    ? ht('reason_project_removal', { project: state.listing?.project_name ?? '' })
    : ht(`reason_${reason}`);
}

function renderTitle() {
  const name = state.listing?.user?.display_name ?? '';
  const count = state.rows.length;
  q('title').innerHTML = `
    <tf-avatar initials="${escapeAttr(initials(name))}" size="sm"></tf-avatar>
    <h2>${escapeHtml(ht('title', { name }))}</h2>
    <tf-chip variant="outline" status="${count ? 'warn' : 'ok'}">${escapeHtml(ht('items_count', { count }))}</tf-chip>`;
}

function renderContext() {
  const segmented = q('reason');
  segmented.setOptions(state.allowed.map((value) => ({ value, label: reasonLabel(value) })), state.reason);

  const dates = q('dates');
  const listing = state.listing;
  if (state.reason === 'departure') {
    const ended = listing?.assignment_ended_on;
    dates.innerHTML = `
      <tf-date-field data-role="date" label="${escapeAttr(ht('f_departure_date'))}" value="${escapeAttr(state.date ?? '')}"
        hint="${escapeAttr(ended ? ht('f_departure_locked') : ht('f_departure_hint'))}"${ended ? ' disabled' : ''}></tf-date-field>`;
  } else if (state.reason === 'absence') {
    dates.innerHTML = `
      <tf-date-field data-role="return" label="${escapeAttr(ht('f_return'))} *" value="${escapeAttr(state.returnDate ?? '')}"
        hint="${escapeAttr(ht('f_return_hint'))}"></tf-date-field>`;
  } else {
    dates.replaceChildren();
  }

  renderNote();
}

function renderNote() {
  const listing = state.listing;
  const note = q('ctxnote');
  const date = formatDay(state.date);
  if (state.reason === 'departure') {
    note.setAttribute('message', listing?.assignment_ended_on ? ht('note_departure_ended', { date }) : ht('note_departure', { date }));
  } else if (state.reason === 'absence') {
    note.setAttribute('message', ht('note_absence', { date: formatDay(state.returnDate) }));
  } else {
    note.setAttribute('message', ht('note_project', { project: listing?.project_name ?? '' }));
  }
}

/** This screen's sentence for `key`, or `fallback` when the server sent a code this build has none for. */
function known(key, fallback) {
  const full = `org_structure.handover.${key}`;
  const text = I18n.t(full);
  return text && text !== full ? text : fallback;
}

const roleText = (row) => known(`role_${row.role}`, row.role);

function stateText(row) {
  if (!row.state) return '';
  if (row.category === 'deputy') return scopeLabel(row.state);
  return known(`state_${row.state}`, row.state);
}

function subline(row) {
  const parts = [roleText(row)];
  const status = stateText(row);
  if (status) parts.push(status);
  if (row.unitName) parts.push(row.unitName);
  if (row.projectName && row.category !== 'membership') parts.push(row.projectName);
  if (row.validTo) parts.push(ht('until', { date: formatDay(row.validTo) }));
  return parts.join(' · ');
}

function takerName(userId) {
  return state.takers.find((p) => p.user_id === userId)?.display_name ?? '';
}

function whyText(row) {
  if (row.blocked) return `<b>${escapeHtml(ht(`why_blocked_${row.blocked}`))}</b>`;
  if (row.action === 'end') {
    return `<span>${escapeHtml(row.category === 'deputy' ? ht('why_deputy_end') : ht('why_membership'))}</span>`;
  }
  if (row.manual && row.taker) return `<b>${escapeHtml(ht('why_manual'))}</b>`;
  if (row.taker && row.suggestion && row.taker === row.suggestion.user_id) {
    return `<b>${escapeHtml(ht(`why_${row.suggestion.reason}`))}</b>`;
  }
  if (!row.taker) {
    const text = row.action === 'transfer_or_end'
      ? ht(row.category === 'position' ? 'why_vacancy' : 'why_deputy_end')
      : ht('why_none');
    return `<span>${escapeHtml(text)}</span>`;
  }
  return `<b>${escapeHtml(ht('why_manual'))}</b>`;
}

function takerCell(row) {
  if (row.action === 'end') {
    const text = row.category === 'deputy'
      ? ht('end_covered', { date: formatDay(state.date) })
      : ht('end_membership', { date: formatDay(state.date) });
    return `<div class="org-ho-end">${escapeHtml(text)}</div>`;
  }
  const options = [];
  if (row.action === 'transfer_or_end') {
    options.push(`<option value="">${escapeHtml(ht(row.category === 'position' ? 'taker_vacant' : 'taker_end'))}</option>`);
  } else if (!row.taker) {
    options.push(`<option value="">${escapeHtml(ht('taker_choose'))}</option>`);
  }
  for (const person of takerOptions(row, state.takers)) {
    options.push(`<option value="${escapeAttr(person.user_id)}"${person.user_id === row.taker ? ' selected' : ''}>${escapeHtml(person.display_name || '—')}</option>`);
  }
  return `<tf-select data-taker="${escapeAttr(row.key)}" value="${escapeAttr(row.taker)}"
    aria-label="${escapeAttr(ht('taker_aria', { title: row.title }))}"${row.blocked ? ' disabled' : ''}>${options.join('')}</tf-select>`;
}

function rowHtml(row) {
  return `
    <div class="org-ho-row${row.error ? ' org-ho-row-error' : ''}" data-row="${escapeAttr(row.key)}">
      <tf-checkbox data-pick="${escapeAttr(row.key)}" aria-label="${escapeAttr(ht('pick_aria', { title: row.title }))}"${row.selected ? ' checked' : ''}${row.blocked ? ' disabled' : ''}></tf-checkbox>
      <div class="org-ho-item"><b>${escapeHtml(row.title)}</b><small>${escapeHtml(subline(row))}</small></div>
      ${takerCell(row)}
      <div class="org-ho-why">${whyText(row)}</div>
    </div>`;
}

function groupHtml({ category, rows }) {
  return `
    <div class="org-ho-group" data-group="${escapeAttr(category)}">
      <div class="org-ho-ghead">
        <tf-chip variant="outline" icon="${escapeAttr(categoryIcon(category))}">${escapeHtml(ht(`group_${category}`))}</tf-chip>
        <span class="org-ho-count">${rows.length}</span>
        <span class="org-ho-gsub">${escapeHtml(ht(`group_${category}_sub`))}</span>
      </div>
      ${rows.map(rowHtml).join('')}
    </div>`;
}

function bulkHtml() {
  const options = state.takers
    .map((p) => `<option value="${escapeAttr(p.user_id)}"${p.user_id === state.allTo ? ' selected' : ''}>${escapeHtml(p.display_name || '—')}</option>`)
    .join('');
  return `
    <b class="org-ho-selected" data-role="selected"></b>
    <span class="org-ho-bulklabel">${escapeHtml(ht('all_to'))}</span>
    <tf-select data-role="all-to" aria-label="${escapeAttr(ht('all_to_aria'))}" value="${escapeAttr(state.allTo ?? '')}">${options}</tf-select>
    <tf-button variant="secondary" size="sm" icon="check" data-act="apply-all">${escapeHtml(ht('apply_selected'))}</tf-button>`;
}

function renderRows() {
  const box = q('rows');
  if (!state.rows.length) {
    box.innerHTML = `<tf-empty-state icon="check" title="${escapeAttr(ht('empty_title'))}" message="${escapeAttr(ht('empty_message'))}"></tf-empty-state>`;
    return;
  }
  const head = `
    <div class="org-ho-row org-ho-colhead">
      <tf-checkbox data-role="pick-all" aria-label="${escapeAttr(ht('select_all'))}"></tf-checkbox>
      <div class="org-ho-colname">${escapeHtml(ht('select_all'))}</div>
      <div class="org-ho-colname">${escapeHtml(ht('col_taker'))}</div>
      <div class="org-ho-colname">${escapeHtml(ht('col_why'))}</div>
    </div>`;
  box.innerHTML = head + grouped(state.rows).map(groupHtml).join('');
  syncSelection();
}

/** The person proposed for the most rows: the natural pick for "hand everything to". */
function mostProposed() {
  const counts = new Map();
  for (const row of state.rows) {
    const id = row.suggestion?.user_id;
    if (id) counts.set(id, (counts.get(id) ?? 0) + 1);
  }
  return [...counts.entries()].sort((a, b) => b[1] - a[1])[0]?.[0] ?? null;
}

function renderBulk() {
  const bulk = q('bulk');
  const empty = !state.rows.length;
  bulk.hidden = empty;
  if (!empty) {
    if (!state.allTo) state.allTo = mostProposed() ?? state.takers[0]?.user_id ?? '';
    bulk.innerHTML = bulkHtml();
  }
}

/** Counters, the select-all state and the submit button follow the ticked rows. */
function syncSelection() {
  if (!state) return;
  const chosen = selectedRows(state.rows);
  const selectable = state.rows.filter((r) => !r.blocked);
  const selected = q('selected');
  if (selected) selected.textContent = ht('selected', { count: chosen.length });
  const all = q('pick-all');
  if (all) {
    all.checked = selectable.length > 0 && chosen.length === selectable.length;
    all.indeterminate = chosen.length > 0 && chosen.length < selectable.length;
  }
  const submit = q('submit');
  submit.setAttribute('label', state.busy ? ht('submitting') : ht('submit', { count: chosen.length }));
  submit.toggleAttribute('disabled', state.busy || chosen.length === 0);
  q('bulk').querySelector('[data-act="apply-all"]')?.toggleAttribute('disabled', chosen.length === 0);
}

function renderBody() {
  renderTitle();
  renderContext();
  renderBulk();
  renderRows();
  syncSelection();
}

// ---- the result of an apply ---------------------------------------------------

const STATUS_TONE = {
  done: 'ok', scheduled: 'info', failed: 'err', not_started: 'warn', skipped: 'neutral', returned: 'ok', kept: 'neutral',
};

function resultItemHtml(item) {
  const reason = item.reason ? reasonText(item.reason) : '';
  const taker = item.taker_user_id ? ht('result_to', { name: takerName(item.taker_user_id) || '—' }) : '';
  const status = item.status === 'scheduled' ? ht('status_scheduled', { date: formatDay(state.date) }) : ht(`status_${item.status}`);
  return `
    <div class="org-ho-result-row">
      <tf-chip status="${escapeAttr(STATUS_TONE[item.status] ?? 'neutral')}">${escapeHtml(status)}</tf-chip>
      <div class="org-ho-result-main"><b>${escapeHtml(item.title || item.key)}</b>
        <small>${escapeHtml([item.project_name, taker, reason].filter(Boolean).join(' · '))}</small></div>
    </div>`;
}

function renderResult() {
  const box = q('result');
  const answer = state.result;
  if (!answer) {
    box.replaceChildren();
    return;
  }
  const sum = summarize(answer);
  const tone = sum.failed ? 'warning' : 'success';
  const error = answer.error && !answer.handover_id ? `<tf-alert tone="danger" message="${escapeAttr(reasonText(answer.error.code))}"></tf-alert>` : '';
  const chips = [
    sum.done ? `<tf-chip status="ok">${escapeHtml(ht('result_done', { count: sum.done }))}</tf-chip>` : '',
    sum.scheduled ? `<tf-chip status="info">${escapeHtml(ht('result_scheduled', { count: sum.scheduled }))}</tf-chip>` : '',
    sum.failed ? `<tf-chip status="err">${escapeHtml(ht('result_failed', { count: sum.failed }))}</tf-chip>` : '',
    sum.skipped ? `<tf-chip status="neutral">${escapeHtml(ht('result_skipped', { count: sum.skipped }))}</tf-chip>` : '',
  ].join('');
  const order = { failed: 0, not_started: 1, skipped: 2, scheduled: 3, done: 4 };
  const items = [...answer.items].sort((a, b) => (order[a.status] ?? 9) - (order[b.status] ?? 9));
  const orgNote = items.some((i) => i.status === 'not_started' && i.reason === 'org_failed')
    ? `<tf-alert tone="warning" message="${escapeAttr(ht('result_org_note'))}"></tf-alert>` : '';
  box.innerHTML = `
    <section class="org-ho-card org-ho-result tone-${tone}">
      <div class="org-ho-result-head"><h3>${escapeHtml(ht('result_title'))}</h3><div class="org-ho-result-chips">${chips}</div></div>
      ${error}${orgNote}
      <div class="org-ho-result-list">${items.map(resultItemHtml).join('')}</div>
      <div class="org-ho-result-actions">
        ${sum.retryable ? `<tf-button variant="primary" icon="refresh" data-act="retry">${escapeHtml(ht('result_retry'))}</tf-button>` : ''}
        <tf-button variant="secondary" data-act="dismiss">${escapeHtml(ht('result_close'))}</tf-button>
      </div>
    </section>`;
}

// =============================================================================
// Actions
// =============================================================================

function markProblems(found) {
  const keys = new Set(found.filter((p) => p.key).map((p) => p.key));
  for (const row of state.rows) row.error = keys.has(row.key);
  renderRows();
  const note = q('note');
  if (found.some((p) => p.code === 'note_required')) {
    note.setAttribute('error', ht('note_missing'));
    note.focus?.();
  } else {
    note.removeAttribute('error');
  }
}

function problemText(found) {
  const takerless = found.filter((p) => p.code === 'taker_required' || p.code === 'taker_not_eligible').length;
  const sentences = [];
  if (found.some((p) => p.code === 'nothing_selected')) sentences.push(ht('problem_nothing_selected'));
  if (found.some((p) => p.code === 'return_required')) sentences.push(ht('problem_return_required'));
  if (found.some((p) => p.code === 'return_not_after_today')) sentences.push(ht('problem_return_past'));
  if (takerless) sentences.push(ht('problem_taker', { count: takerless }));
  return sentences.join(' ');
}

function afterAnswer(answer) {
  state.result = answer;
  const sum = summarize(answer);
  // Positions and deputies moved, and somebody may no longer be pending: the screen around this one reads the structure again.
  if (sum.done || sum.scheduled) state.onChanged?.();
  if (answer.error && !answer.handover_id) {
    TfToast.show({ tone: 'danger', message: reasonText(answer.error.code) });
  } else if (sum.failed) {
    TfToast.show({ tone: 'warning', message: ht('toast_partial', { done: sum.done + sum.scheduled, failed: sum.failed }) });
  } else {
    TfToast.show({ tone: 'success', message: ht('toast_done', { count: sum.done + sum.scheduled }) });
  }
}

async function submit() {
  if (state.busy) return;
  const badDate = [...q('dates').querySelectorAll('tf-date-field')].filter((field) => !field.validate());
  if (badDate.length) {
    badDate[0].focus();
    return;
  }
  const note = q('note').value ?? '';
  const found = problems({ rows: state.rows, note, reason: state.reason, returnDate: state.returnDate, today: state.listing?.date });
  markProblems(found);
  if (found.length) {
    const text = problemText(found);
    if (text) TfToast.show({ tone: 'warning', message: text });
    return;
  }
  state.busy = true;
  syncSelection();
  try {
    const answer = await ApiBinary.one('orgHandoverApplyRequest', applyPayload({
      userId: state.userId,
      reason: state.reason,
      projectId: state.projectId,
      date: state.date,
      returnDate: state.returnDate,
      note,
      rows: state.rows,
    }));
    afterAnswer(answer);
    if (answer.error && !answer.handover_id) {
      // Nothing moved: what the operator chose stays, with the rows the server refused marked.
      const refused = new Set(answer.items.filter((item) => item.status === 'failed').map((item) => item.key));
      for (const row of state.rows) row.error = refused.has(row.key);
    } else {
      q('note').value = answer.handover_id && summarize(answer).failed ? note : '';
      await load();
    }
  } catch (err) {
    TfToast.show({ tone: 'danger', message: ht('apply_failed', { message: err.message || '' }) });
  } finally {
    state.busy = false;
    if (state) {
      renderBody();
      renderResult();
    }
  }
}

async function retry() {
  const answer = state.result;
  if (!answer?.handover_id || state.busy) return;
  state.busy = true;
  syncSelection();
  try {
    const next = await ApiBinary.one('orgHandoverRetryRequest', {
      handoverId: answer.handover_id,
      keys: summarize(answer).failedKeys,
    });
    afterAnswer(next);
    await load();
  } catch (err) {
    TfToast.show({ tone: 'danger', message: ht('apply_failed', { message: err.message || '' }) });
  } finally {
    state.busy = false;
    if (state) {
      renderBody();
      renderResult();
    }
  }
}

function applyAll() {
  if (!state.allTo) return;
  const { set, skipped } = applyToSelected(state.rows, state.allTo);
  renderRows();
  if (set) TfToast.show({ tone: 'success', message: ht('toast_all_set', { name: takerName(state.allTo), count: set }) });
  if (skipped) TfToast.show({ tone: 'warning', message: ht('toast_all_skipped', { count: skipped }) });
}

async function switchReason(reason) {
  if (!state.allowed.includes(reason) || reason === state.reason) return;
  state.reason = reason;
  state.result = null;
  state.date = null;
  state.returnDate = null;
  q('rows').innerHTML = `<div class="org-loading">${escapeHtml(ht('loading'))}</div>`;
  renderResult();
  await reload();
}

function onChange(e) {
  const target = e.target;
  const detail = e.detail ?? {};
  if (target.matches('[data-role="reason"]')) {
    switchReason(String(detail.value ?? ''));
  } else if (target.matches('[data-pick]')) {
    const row = state.rows.find((r) => r.key === target.dataset.pick);
    if (row) row.selected = Boolean(detail.checked);
    syncSelection();
  } else if (target.matches('[data-role="pick-all"]')) {
    for (const row of state.rows) if (!row.blocked) row.selected = Boolean(detail.checked);
    for (const box of state.host.querySelectorAll('tf-checkbox[data-pick]')) {
      if (!box.hasAttribute('disabled')) box.checked = Boolean(detail.checked);
    }
    syncSelection();
  } else if (target.matches('[data-taker]')) {
    const row = state.rows.find((r) => r.key === target.dataset.taker);
    if (row) {
      row.taker = String(detail.value ?? '');
      row.manual = row.taker !== (row.suggestion?.user_id ?? '');
      row.error = false;
      const why = target.closest('.org-ho-row')?.querySelector('.org-ho-why');
      if (why) why.innerHTML = whyText(row);
      target.closest('.org-ho-row')?.classList.remove('org-ho-row-error');
    }
  } else if (target.matches('[data-role="all-to"]')) {
    state.allTo = String(detail.value ?? '');
  } else if (target.matches('[data-role="date"]')) {
    const value = String(detail.value ?? '');
    if (value && value !== state.date) {
      state.date = value;
      reload();
    }
  } else if (target.matches('[data-role="return"]')) {
    // Only the note: redrawing the field would wipe text that is not (yet) a day.
    state.returnDate = String(detail.value ?? '');
    renderNote();
  }
}

function onInput(e) {
  if (e.target.matches('[data-role="note"]') && String(e.target.value ?? '').trim()) {
    e.target.removeAttribute('error');
  }
}

function onClick(e) {
  const button = e.target.closest('[data-act]');
  if (!button || button.hasAttribute('disabled')) return;
  switch (button.dataset.act) {
    case 'back': state.onBack(); break;
    case 'apply-all': applyAll(); break;
    case 'submit': submit(); break;
    case 'retry': retry(); break;
    case 'dismiss': state.result = null; renderResult(); break;
    default: break;
  }
}

// =============================================================================
// Public
// =============================================================================

/**
 * Draws the screen into `host` for `target` = { userId, reason, projectId } and loads what the person holds.
 * `isAdmin` says whether the caller has org.admin; `onBack` returns to the list; `onChanged` is called after
 * anything was moved, so the structure and the counter of pending people can be read again.
 */
export async function mountHandoverTab(host, {
  target, isAdmin, onBack, onChanged = null,
}) {
  unmountHandoverTab();
  const allowed = allowedReasons({ isAdmin, projectId: target.projectId });
  state = {
    host,
    onBack,
    onChanged,
    userId: target.userId,
    projectId: target.projectId,
    allowed,
    reason: initialReason(target.reason, allowed),
    date: null,
    returnDate: null,
    listing: null,
    rows: [],
    takers: [],
    allTo: '',
    result: null,
    busy: false,
    loadSeq: 0,
  };
  host.innerHTML = frame();
  host.addEventListener('click', onClick);
  host.addEventListener('change', onChange);
  host.addEventListener('input', onInput);
  try {
    await load();
  } catch (err) {
    showLoadError(err);
    return;
  }
  renderBody();
}

export function unmountHandoverTab() {
  if (!state) return;
  state.host.removeEventListener('click', onClick);
  state.host.removeEventListener('change', onChange);
  state.host.removeEventListener('input', onInput);
  state.host.replaceChildren();
  state = null;
}
