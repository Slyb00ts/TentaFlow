// =============================================================================
// File: modules/org-structure/history-tab.js
// Description: The Historia tab of the organization structure screen (mockup
//   F04): the date jumper ("stan na dzień": a day, quick jumps and a slider whose
//   marks are the days something changed or is planned), the timeline of changes
//   with what each one changed (before → after), the planned reorganizations
//   (history-changeset.js) and "Przed i po" for a unit (history-compare.js), plus
//   "Eksport stanu na dzień". Everyone in the organization reads the structure's
//   changes; the position history of other people is the server's to leave out
//   (`personal_visible` says so, and the tab says it too), and the reorganizations
//   are drawn for `org.admin` only. Nothing here decides what a person may see.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';
import { TfToast } from '/js/components/tf-toast.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-slider.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-spinner.js';
import { openActionMenu } from '/js/lib/actions/index.js';
import { downloadBytes } from '/js/lib/download.js';
import { dateFormatHint, formatDay, isIsoDay } from '/js/lib/date-format.js';
import { summarize } from '/js/modules/org-structure/model.js';
import { userIdHex } from '/js/modules/org-structure/tree.js';
import {
  diffCounts, entryModel, markers, quickJumps, railPercent, railTicks, relativeDay, sliderDayOf, sliderRange, sliderValueOf,
} from '/js/modules/org-structure/history-model.js';
import { diffDays, listHistory } from '/js/modules/org-structure/history-api.js';
import { onChangeSetsChanged } from '/js/modules/org-structure/history-bridge.js';
import { requestTreeAsOf } from '/js/modules/org-structure/history-asof.js';
import { createComparePanel } from '/js/modules/org-structure/history-compare.js';
import { createReorgPanel } from '/js/modules/org-structure/history-changeset.js';

const t = (key, params) => I18n.t(`org_structure.history.${key}`, params);

const PAGE = 30;
const MARKER_SCAN = 200;

let state = null;

const isAdmin = () => Boolean(state?.myPermissions.includes('org.admin'));
const q = (role) => state.host.querySelector(`[data-role="${role}"]`);

// ---------------------------------------------------------------------------
// Markup
// ---------------------------------------------------------------------------

function shell() {
  const admin = isAdmin();
  return `
    <div class="org-hist">
      <div class="org-hist-head">
        <div class="org-section-title">${escapeHtml(t('title'))}</div>
        <span class="org-hist-grow"></span>
        <tf-button variant="secondary" icon="download" data-act="export" data-role="export">${escapeHtml(t('export_day'))}</tf-button>
        ${admin ? `<tf-button variant="primary" icon="plus" data-act="new-reorg">${escapeHtml(t('new_reorg'))}</tf-button>` : ''}
      </div>
      <div data-role="privacy"></div>
      <section class="org-hist-card org-hist-date" aria-label="${escapeAttr(t('date_title'))}">
        <div class="org-hist-date-top">
          <div>
            <div class="org-hist-sm">${escapeHtml(t('date_title'))}</div>
            <div class="org-hist-big" data-role="big"></div>
          </div>
          <span data-role="relative"></span>
          <span class="org-hist-grow"></span>
          <div class="org-hist-quick"><span class="org-hist-hint">${escapeHtml(t('jump_label'))}</span><span data-role="quick"></span></div>
        </div>
        <div class="org-hist-slider">
          <tf-slider data-role="slider" min="0" max="1" value="0" aria-label="${escapeAttr(t('slider_label'))}"></tf-slider>
          <div class="org-hist-rail" data-role="rail" aria-hidden="true"></div>
          <div class="org-hist-ticks" data-role="ticks" aria-hidden="true"></div>
        </div>
        <div class="org-hist-date-tools">
          <tf-date-field class="org-hist-date-input" data-role="day" label="${escapeAttr(t('date_input'))}"></tf-date-field>
          <div class="org-hist-stats" data-role="stats"></div>
        </div>
      </section>
      <div class="org-hist-cols${admin ? '' : ' is-single'}">
        <section class="org-hist-card" data-role="timeline"></section>
        ${admin ? '<section class="org-hist-card org-hist-reorg" data-role="reorg"></section>' : ''}
      </div>
      <section class="org-hist-card" data-role="compare"></section>
    </div>`;
}

function statsHtml() {
  const s = summarize(state.atView);
  const chips = [
    `<tf-chip variant="outline" icon="users">${escapeHtml(t('stat_people', { count: s.people }))}</tf-chip>`,
    `<tf-chip variant="outline" icon="sitemap">${escapeHtml(t('stat_units', { count: s.units }))}</tf-chip>`,
  ];
  if (s.vacancies) chips.push(`<tf-chip variant="outline" status="warn" icon="user">${escapeHtml(t('stat_vacancies', { count: s.vacancies }))}</tf-chip>`);
  if (state.at !== state.today) {
    const c = diffCounts(state.atDiff);
    const same = state.atDiff.length === 0;
    chips.push(same
      ? `<tf-chip variant="outline" status="ok">${escapeHtml(t('diff_none'))}</tf-chip>`
      : `<tf-chip variant="outline" status="info" icon="history">${escapeHtml(t('diff_vs_today', { added: c.added, removed: c.removed, changed: c.changed }))}</tf-chip>`);
    chips.push(`<tf-button variant="ghost" size="sm" icon="sitemap" data-act="show-tree">${escapeHtml(t('show_in_tree'))}</tf-button>`);
  }
  return chips.join('');
}

function relativeHtml() {
  const { kind, days } = relativeDay(state.at, state.today);
  if (kind === 'today') return `<tf-chip variant="outline" status="ok">${escapeHtml(t('chip_today'))}</tf-chip>`;
  if (kind === 'past') return `<tf-chip variant="outline" status="neutral">${escapeHtml(t('chip_past', { days }))}</tf-chip>`;
  const after = state.marks.some((m) => m.planned && m.day <= state.at);
  return `<tf-chip variant="outline" status="warn" icon="calendar">${escapeHtml(t('chip_future', { days }))}${after ? ` · ${escapeHtml(t('chip_after_changes'))}` : ''}</tf-chip>`;
}

function jumpLabel(jump) {
  if (jump.kind === 'today') return t('jump_today', { date: formatDay(jump.day) });
  if (jump.kind === 'planned') return t('jump_planned', { date: formatDay(jump.day) });
  return formatDay(jump.day);
}

function drawJumps() {
  const jumps = quickJumps(state.marks, state.today);
  q('quick').innerHTML = `<tf-segmented size="sm" data-role="jumps" aria-label="${escapeAttr(t('jump_label'))}" value="${escapeAttr(state.at)}">
    ${jumps.map((j) => `<option value="${escapeAttr(j.day)}">${escapeHtml(jumpLabel(j))}</option>`).join('')}</tf-segmented>`;
}

function drawRail() {
  const marks = [...state.marks];
  if (!marks.some((m) => m.day === state.today)) marks.push({ day: state.today, planned: false, today: true });
  q('rail').innerHTML = marks.map((m) => {
    const kind = m.day === state.today ? 'is-today' : m.planned ? 'is-plan' : '';
    return `<span class="org-hist-mk ${kind}" style="--pos:${railPercent(m.day, state.range).toFixed(2)}%" title="${escapeAttr(formatDay(m.day))}"></span>`;
  }).join('');
  q('ticks').innerHTML = railTicks(state.marks, state.today, state.range).map((tick) => {
    const text = tick.kind === 'today' ? t('tick_today') : formatDay(tick.day, { short: tick.kind !== 'start' });
    return `<span class="org-hist-tick ${tick.kind === 'today' ? 'is-today' : ''}" style="--pos:${tick.pos.toFixed(2)}%">${escapeHtml(text)}</span>`;
  }).join('');
  const slider = q('slider');
  slider.setAttribute('max', String(sliderValueOf(state.range.to, state.range)));
}

/** Everything that follows the chosen day, updated in place so the slider and the date field keep the focus. */
function updateDate() {
  q('big').textContent = formatDay(state.at);
  q('relative').innerHTML = relativeHtml();
  q('stats').innerHTML = state.atView ? statsHtml() : '';
  const day = q('day');
  if (day.value !== state.at) day.setAttribute('value', state.at);
  const slider = q('slider');
  const value = String(Math.max(0, sliderValueOf(state.at, state.range)));
  if (slider.getAttribute('value') !== value) slider.setAttribute('value', value);
  q('jumps')?.setAttribute('value', state.at);
  const exportBtn = q('export');
  const denied = !isAdmin() && state.at < state.today;
  exportBtn.toggleAttribute('disabled', denied);
  if (denied) exportBtn.setAttribute('title', t('export_past_denied'));
  else exportBtn.removeAttribute('title');
}

function renderPrivacy() {
  q('privacy').innerHTML = state.personalVisible
    ? ''
    : `<tf-alert tone="info" message="${escapeAttr(t('privacy_note'))}"></tf-alert>`;
}

function timelineItemHtml(entry) {
  const m = entryModel(entry, { t, today: state.today });
  const changes = m.changes.map((c) => `
    <div class="org-hist-diff">
      <span class="org-hist-field">${escapeHtml(c.label)}</span>
      ${c.created ? '' : `<span class="o">${escapeHtml(c.before)}</span><span aria-hidden="true">→</span>`}
      <span class="n">${escapeHtml(c.after)}</span>
    </div>`).join('');
  const ops = m.ops.length ? `<ul class="org-hist-ops">${m.ops.map((line) => `<li>${escapeHtml(line)}</li>`).join('')}</ul>` : '';
  const hidden = m.hiddenOps ? `<div class="org-hist-hint">${escapeHtml(t('hidden_ops', { count: m.hiddenOps }))}</div>` : '';
  return `
    <li class="org-hist-item${m.dot ? ` is-${m.dot}` : ''}">
      <span class="org-hist-dot" aria-hidden="true"></span>
      <div class="org-hist-item-head">
        <tf-button variant="ghost" size="sm" data-act="jump" data-day="${escapeAttr(m.day)}" title="${escapeAttr(t('jump_to_day'))}">${escapeHtml(m.dayText)}</tf-button>
        ${m.planned ? `<tf-chip variant="outline" status="info" icon="calendar">${escapeHtml(t('planned_tag'))}</tf-chip>` : ''}
      </div>
      <div class="org-hist-title">${escapeHtml(m.title)}</div>
      ${changes}${ops}${hidden}
      ${m.who ? `<div class="org-hist-who">${escapeHtml(m.who)}</div>` : ''}
    </li>`;
}

function timelineFilterHtml() {
  return `
    <div class="org-hist-filters">
      <tf-select data-role="unit-filter" label="${escapeAttr(t('filter_unit'))}"></tf-select>
      <tf-date-field data-role="from" label="${escapeAttr(t('filter_from'))}" value="${escapeAttr(state.from)}"></tf-date-field>
      <tf-date-field data-role="to" label="${escapeAttr(t('filter_to'))}" value="${escapeAttr(state.to)}"></tf-date-field>
      <tf-button variant="ghost" size="sm" icon="x" data-act="clear-filters">${escapeHtml(t('filter_clear'))}</tf-button>
    </div>`;
}

function drawTimeline() {
  const host = q('timeline');
  const list = state.entries.length
    ? `<ul class="org-hist-timeline">${state.entries.map(timelineItemHtml).join('')}</ul>`
    : `<div class="org-hist-empty">${escapeHtml(t('timeline_empty'))}</div>`;
  const more = state.entries.length < state.total
    ? `<tf-button variant="secondary" size="sm" data-act="more">${escapeHtml(t('timeline_more', { shown: state.entries.length, total: state.total }))}</tf-button>`
    : '';
  host.querySelector('[data-role="list"]').innerHTML = `${list}${more}`;
  host.querySelector('[data-role="count"]').textContent = t('timeline_count', { count: state.total });
}

function fillUnitFilter() {
  const select = q('unit-filter');
  const units = [...state.view.units].sort((a, b) => a.name.localeCompare(b.name));
  select.setOptions([{ value: '', label: t('filter_unit_all') }, ...units.map((u) => ({ value: u.unit_id, label: u.name }))], state.unit);
}

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

async function fetchView(day) {
  if (day === state.today && state.view.at === day) return state.view;
  return (await ApiBinary.one('orgStructureRequest', { at: day })).view;
}

async function loadTimeline({ append = false } = {}) {
  const screen = state;
  const mine = ++state.timelineToken;
  const offset = append ? state.entries.length : 0;
  try {
    const page = await listHistory({
      from: state.from || null, to: state.to || null, unitId: state.unit || null, offset, limit: PAGE,
    });
    if (state !== screen || mine !== state.timelineToken) return;
    state.entries = append ? [...state.entries, ...page.entries] : page.entries;
    state.total = page.total;
    state.personalVisible = page.personalVisible;
    state.today = page.today || state.today;
  } catch (err) {
    if (state !== screen || mine !== state.timelineToken) return;
    q('timeline').querySelector('[data-role="list"]').innerHTML = `<div class="org-hist-empty is-error">${escapeHtml(t('timeline_failed', { message: err.message || '' }))}</div>`;
    return;
  }
  renderPrivacy();
  drawTimeline();
}

/** The days something changed and the days plans take effect: what the slider and the quick jumps mark. */
function recomputeMarks() {
  if (!state) return;
  state.marks = markers(state.markerEntries, state.sets, state.today);
  state.range = sliderRange([...state.marks, { day: state.at }], state.today);
  drawJumps();
  drawRail();
  updateDate();
}

async function loadMarkers() {
  const mine = state;
  let entries = [];
  try {
    entries = (await listHistory({ limit: MARKER_SCAN })).entries;
  } catch {
    entries = [];
  }
  if (state !== mine) return;
  state.markerEntries = entries;
  recomputeMarks();
}

function showComparison() {
  if (state.compareSource === 'set') return;
  if (state.at === state.today || !state.atView) {
    state.compare.show(null);
    return;
  }
  state.compare.show({
    before: { view: state.view, label: t('compare_before', { date: formatDay(state.today) }) },
    after: { view: state.atView, label: formatDay(state.at), planned: state.at > state.today },
    items: state.atDiff,
    unitTypes: state.unitTypes,
  });
}

/** Shows the structure on `day`. A comparison drawn for a reorganization stays when `keepComparison` says the day is only its own. */
async function setAt(day, { keepComparison = false } = {}) {
  if (!isIsoDay(day)) {
    q('day').setAttribute('error', t('date_invalid', { format: dateFormatHint() }));
    return;
  }
  q('day').removeAttribute('error');
  if (!keepComparison) state.compareSource = 'days';
  state.at = day;
  const screen = state;
  const mine = ++state.atToken;
  if (!state.range || day < state.range.from || day > state.range.to) {
    state.range = sliderRange([...state.marks, { day }], state.today);
    drawRail();
  }
  updateDate();
  try {
    const [view, diff] = await Promise.all([
      fetchView(day),
      day === state.today ? { items: [] } : diffDays({ from: state.today, to: day }),
    ]);
    if (state !== screen || mine !== state.atToken) return;
    state.atView = view;
    state.atDiff = diff.items;
  } catch (err) {
    if (state !== screen || mine !== state.atToken) return;
    TfToast.show({ tone: 'danger', message: t('load_failed', { message: err.message || '' }) });
    return;
  }
  updateDate();
  showComparison();
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

async function exportFile(format, anchor) {
  try {
    const body = await ApiBinary.one('orgExportRequest', { format, at: state.at });
    downloadBytes(body.file_name ?? body.fileName, body.bytes, body.mime);
  } catch (err) {
    TfToast.show({ tone: 'danger', message: t('export_failed', { message: err.message || '' }) });
  }
  anchor?.focus?.();
}

function openExportMenu(anchor) {
  openActionMenu(anchor, [
    { label: t('export_csv'), icon: 'file', run: () => exportFile('csv', anchor) },
    { label: t('export_xlsx'), icon: 'file', run: () => exportFile('xlsx', anchor) },
  ], formatDay(state.at));
}

function onClick(e) {
  const target = e.target.closest('[data-act]');
  if (!target || target.hasAttribute('disabled')) return;
  switch (target.dataset.act) {
    case 'export': openExportMenu(target); break;
    case 'new-reorg': state.reorg?.openNew(target); break;
    case 'jump': setAt(target.dataset.day); break;
    case 'show-tree': requestTreeAsOf(state.at); break;
    case 'more': loadTimeline({ append: true }); break;
    case 'clear-filters':
      state.unit = '';
      state.from = '';
      state.to = '';
      q('unit-filter').value = '';
      q('from').setAttribute('value', '');
      q('to').setAttribute('value', '');
      loadTimeline();
      break;
    default: break;
  }
}

function onInput(e) {
  // The slider's own custom event (the native range input under it also bubbles one, without a detail).
  if (e.target.tagName === 'TF-SLIDER' && e.detail) {
    const day = sliderDayOf(e.detail.value, state.range);
    q('big').textContent = formatDay(day);
  }
}

function onChange(e) {
  const role = e.target.dataset?.role;
  const value = String(e.detail?.value ?? '');
  if (e.target.tagName === 'TF-SLIDER' && e.detail) setAt(sliderDayOf(value, state.range));
  else if (role === 'jumps' && e.detail) setAt(value);
  else if (role === 'day' && e.detail) setAt(value);
  else if (role === 'unit-filter' && e.detail) {
    state.unit = value;
    loadTimeline();
  } else if ((role === 'from' || role === 'to') && e.detail) {
    if (value !== '' && !isIsoDay(value)) return;
    state[role] = value;
    loadTimeline();
  }
}

// ---------------------------------------------------------------------------
// Mounting
// ---------------------------------------------------------------------------

/**
 * Draws the Historia tab into `host`. `view`, `unitTypes` and `myPermissions` come from a structure answer of today;
 * `reload` reads the structure again after a reorganization was approved.
 */
export async function mountHistoryTab(host, { view, unitTypes, myPermissions = [] }, { reload = null } = {}) {
  unmountHistoryTab();
  const me = await ApiBinary.one('authMeRequest').catch(() => null);
  state = {
    host,
    view,
    unitTypes,
    myPermissions,
    reloadScreen: reload,
    meHex: userIdHex(me?.userId),
    today: view.at,
    at: view.at,
    atView: view,
    atDiff: [],
    atToken: 0,
    timelineToken: 0,
    compareSource: null,
    entries: [],
    total: 0,
    personalVisible: true,
    marks: [],
    markerEntries: [],
    sets: [],
    range: null,
    unit: '',
    from: '',
    to: '',
    reorg: null,
    compare: null,
    unsubscribe: null,
  };
  const root = document.createElement('div');
  root.innerHTML = shell();
  host.replaceChildren(root.firstElementChild);
  state.compare = createComparePanel(q('compare'));
  q('timeline').innerHTML = `
    <div class="org-hist-card-head">
      <div class="org-hist-card-title">${escapeHtml(t('timeline_title'))}</div>
      <span class="org-hist-hint" data-role="count"></span>
    </div>
    ${timelineFilterHtml()}
    <div data-role="list"></div>`;
  fillUnitFilter();
  if (isAdmin()) {
    state.reorg = createReorgPanel({
      host: q('reorg'),
      context: () => ({
        today: state.today, meHex: state.meHex, view: state.view, unitTypes: state.unitTypes,
      }),
      onShowDay: (day, options) => setAt(day, options),
      isComparing: () => state.compareSource !== null,
      onCompare: (comparison) => {
        state.compareSource = 'set';
        state.compare.show(comparison);
      },
      onLoaded: (sets) => {
        if (!state) return;
        state.sets = sets;
        recomputeMarks();
      },
      onChanged: () => {
        loadTimeline();
        loadMarkers();
        state.reloadScreen?.();
      },
    });
    state.unsubscribe = onChangeSetsChanged(() => {
      state.reorg?.reload();
    });
  }
  host.addEventListener('click', onClick);
  host.addEventListener('input', onInput);
  host.addEventListener('change', onChange);
  state.listeners = { onClick, onInput, onChange };
  initRange();
  drawJumps();
  updateDate();
  const screen = state;
  await Promise.all([loadTimeline(), loadMarkers()]);
  if (state === screen) showComparison();
}

function initRange() {
  state.range = sliderRange([{ day: state.at }], state.today);
  drawRail();
}

/** Today's structure was read again (the screen's tab was shown, or a write finished): keep it, and read the timeline again. */
export function refreshHistoryTab({ view, unitTypes, myPermissions = [] }) {
  if (!state) return;
  state.view = view;
  state.unitTypes = unitTypes;
  state.myPermissions = myPermissions;
  state.today = view.at;
  if (state.at === view.at) state.atView = view;
  fillUnitFilter();
  loadTimeline();
  loadMarkers();
  state.reorg?.reload();
}

export function unmountHistoryTab() {
  if (!state) return;
  state.unsubscribe?.();
  state.reorg?.dispose();
  state.compare?.dispose();
  const { host, listeners } = state;
  if (listeners) {
    host.removeEventListener('click', listeners.onClick);
    host.removeEventListener('input', listeners.onInput);
    host.removeEventListener('change', listeners.onChange);
  }
  host.replaceChildren();
  state = null;
}
