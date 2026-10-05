// ===== File: modules/tentaquant/examples.js — Q10, the Przykłady tab of one laboratory =====
//
// The gallery of circuits that ship inside Core (plan §12.1) and the page of
// one of them: its README, the circuit drawn by the same `tf-quantum-circuit`
// the Studio uses, and the outcome a correct run lands on — the very
// `expected.json` a Rust test holds each shipped circuit to.
//
// Two actions, both backed by `Example::Fork`: "Kopiuj do moich" makes a new
// private project with one notebook (README cell + circuit cell) and opens it,
// "Otwórz w studio" does the same and lands on the Studio with that circuit
// cell. Forking needs `quant.run`; reading the gallery needs `quant.read`.
//
// What the mockup shows and this does NOT build: CPU / GPU / QPU variants (no
// example ships a file for a tier that cannot run it), run times per tier and
// fork counters (no run or fork history is recorded), and the category tabs —
// the wire carries a level and tags, so those are the filters.

import { I18n } from '/js/i18n.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import {
  T, sprite, errMessage, has, circuitLabels, editorLabels, mimeLabels,
} from '/js/modules/tentaquant/format.js';
import { pickText } from '/js/modules/tentaquant/course.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-code-editor.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-input.js';
import '/js/components/tf-mime-output.js';
import '/js/components/tf-quantum-circuit.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-select.js';

const LEVEL_ORDER = ['intro', 'core', 'advanced'];
const LEVEL_TONE = { intro: 'ok', core: 'info', advanced: 'warn' };

/// The screen's gallery state: the open example (the route's `example`), its
/// chosen width and the filters, kept so a tab switch does not reset them.
export function examplesState(patch = {}) {
  return { exampleId: null, qubits: null, query: '', level: 'all', ...patch };
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

export const exampleTitle = (example) => pickText(example?.titles);
export const exampleDescription = (example) => pickText(example?.descriptions);

/// "2 kubity" for a fixed circuit, "3–28 kubitów" for one with a range.
export function widthLabel(example) {
  const min = Number(example.qubitsMin) || 0;
  const max = Number(example.qubitsMax) || 0;
  if (min === max) return T('examples.width_fixed', { n: min });
  return T('examples.width_range', { min, max });
}

export const isParametric = (example) => Number(example?.qubitsMin) !== Number(example?.qubitsMax);

/// A width typed into the control: a whole number inside the example's range,
/// otherwise the previous one — never a request the server would refuse.
export function clampWidth(example, raw, fallback) {
  const n = Math.trunc(Number(raw));
  if (!Number.isFinite(n)) return fallback;
  return Math.min(Math.max(n, Number(example.qubitsMin)), Number(example.qubitsMax));
}

/// The levels present in the list, in teaching order, preceded by "all".
export function levelOptions(examples) {
  const present = new Set((examples || []).map((e) => e.level));
  return ['all', ...LEVEL_ORDER.filter((l) => present.has(l))];
}

/// The examples a query and a level leave. The query matches the title and the
/// description in the dashboard's language and the tags verbatim.
export function filterExamples(examples, { query = '', level = 'all' } = {}) {
  const q = String(query || '').trim().toLowerCase();
  return (examples || []).filter((example) => {
    if (level !== 'all' && example.level !== level) return false;
    if (!q) return true;
    const haystack = [exampleTitle(example), exampleDescription(example), ...(example.tags || [])]
      .join(' ').toLowerCase();
    return haystack.includes(q);
  });
}

/// The reference outcome as rows, the likeliest first: a bitstring and the
/// share of shots a correct run puts on it.
export function outcomeRows(expected) {
  return Object.entries(expected?.outcomes || {})
    .map(([bits, probability]) => ({ bits, probability: Number(probability) }))
    .sort((a, b) => b.probability - a.probability || a.bits.localeCompare(b.bits));
}

const percent = (value) => `${(value * 100).toLocaleString(I18n.getLanguage(), { maximumFractionDigits: 2 })} %`;

// ---------------------------------------------------------------------------
// Markup
// ---------------------------------------------------------------------------

function cardHtml(example, canFork) {
  const tags = (example.tags || []).map((tag) => `<tf-chip label="${escapeAttr(tag)}"></tf-chip>`).join('');
  return `
    <div class="q-card ex-card" data-example="${escapeAttr(example.exampleId)}" role="button" tabindex="0"
      aria-label="${escapeAttr(exampleTitle(example))}">
      <div class="qc-top">
        <div class="qc-ico">${sprite('atom')}</div>
        <div class="qc-head">
          <div class="qc-name">${escapeHtml(exampleTitle(example))}</div>
          <div class="qc-id mono">${escapeHtml(example.exampleId)}</div>
        </div>
      </div>
      <div class="qc-desc">${escapeHtml(exampleDescription(example))}</div>
      <div class="qc-tiers">
        <tf-chip status="${LEVEL_TONE[example.level] || 'neutral'}" label="${escapeAttr(T(`examples.level_${example.level}`))}"></tf-chip>
        ${tags}
      </div>
      <div class="qc-stats">
        <div class="qc-stat"><div class="v">${escapeHtml(widthLabel(example))}</div><div class="l">${escapeHtml(T('examples.stat_width'))}</div></div>
        <div class="qc-stat"><div class="v">${Number(example.depth) || 0}</div><div class="l">${escapeHtml(T('examples.stat_depth'))}</div></div>
      </div>
      <div class="qc-foot">
        <span class="qc-foot-right">
          <tf-button variant="ghost" size="sm" icon="eye" data-act="preview">${escapeHtml(T('examples.action_preview'))}</tf-button>
          <tf-button variant="secondary" size="sm" icon="copy" data-act="fork" ${canFork ? '' : 'disabled'}>${escapeHtml(T('examples.action_fork'))}</tf-button>
        </span>
      </div>
    </div>`;
}

function galleryHtml(state, all, visible, canFork) {
  const levels = levelOptions(all);
  return `
    <div class="tf-toolbar">
      <tf-searchbox id="tq-ex-search" placeholder="${escapeAttr(T('examples.search_placeholder'))}" debounce="200" value="${escapeAttr(state.query)}"></tf-searchbox>
      <tf-select id="tq-ex-level" value="${escapeAttr(levels.includes(state.level) ? state.level : 'all')}">
        ${levels.map((l) => `<option value="${escapeAttr(l)}">${escapeHtml(l === 'all' ? T('examples.level_all') : T(`examples.level_${l}`))}</option>`).join('')}
      </tf-select>
    </div>
    ${visible.length
      ? `<div class="card-grid" id="tq-ex-grid">${visible.map((e) => cardHtml(e, canFork)).join('')}</div>`
      : `<tf-empty-state icon="atom" title="${escapeAttr(T('examples.empty_title'))}" message="${escapeAttr(T('examples.empty_sub'))}"></tf-empty-state>`}
    <div class="tq-table-footer"><span>${escapeHtml(T('examples.footer', { n: visible.length, total: all.length }))}</span></div>
    ${canFork ? '' : `<tf-alert tone="info" message="${escapeAttr(T('examples.run_required'))}"></tf-alert>`}`;
}

function detailHtml(detail, canFork) {
  const example = detail.example;
  const rows = outcomeRows(detail.expected);
  const expected = detail.expected || {};
  return `
    <div class="tq-section-head">
      <tf-button variant="ghost" size="sm" icon="arrow-left" data-act="back">${escapeHtml(T('examples.back'))}</tf-button>
    </div>
    <div class="section-card">
      <div class="kata-head">
        <div>
          <h3>${sprite('atom')}${escapeHtml(exampleTitle(example))}</h3>
          <div class="section-sub">${escapeHtml(exampleDescription(example))}</div>
        </div>
        <div class="kh-meta">
          <tf-chip status="${LEVEL_TONE[example.level] || 'neutral'}" label="${escapeAttr(T(`examples.level_${example.level}`))}"></tf-chip>
          <tf-chip label="${escapeAttr(T('examples.depth_value', { n: Number(example.depth) || 0 }))}"></tf-chip>
        </div>
      </div>
      ${isParametric(example)
        ? `<div class="ex-width"><tf-input id="tq-ex-width" type="number" stepper min="${example.qubitsMin}" max="${example.qubitsMax}" step="1"
            label="${escapeAttr(T('examples.width_label'))}" hint="${escapeAttr(T('examples.width_hint', { min: example.qubitsMin, max: example.qubitsMax }))}"
            value="${example.qubits}" stepper-dec-label="${escapeAttr(T('examples.width_dec'))}" stepper-inc-label="${escapeAttr(T('examples.width_inc'))}"></tf-input></div>`
        : ''}
      <div class="kata-actions">
        <tf-button variant="primary" icon="copy" data-act="fork" ${canFork ? '' : 'disabled'}>${escapeHtml(T('examples.action_fork'))}</tf-button>
        <tf-button variant="secondary" icon="chip" data-act="studio" ${canFork ? '' : 'disabled'}>${escapeHtml(T('examples.action_studio'))}</tf-button>
        ${canFork ? '' : `<span class="hint">${escapeHtml(T('examples.run_required'))}</span>`}
      </div>
    </div>
    <div class="section-card">
      <div class="readme"><tf-mime-output id="tq-ex-readme"></tf-mime-output></div>
    </div>
    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${sprite('chip')} ${escapeHtml(T('examples.circuit_title'))}</div>
        <div class="actions">
          <tf-segmented id="tq-ex-view" value="grid">
            <option value="grid" icon="chip">${escapeHtml(T('examples.view_circuit'))}</option>
            <option value="text" icon="code">${escapeHtml(T('examples.view_text'))}</option>
          </tf-segmented>
        </div>
      </div>
      <div class="tq-circuit-wrap" data-view="grid">
        <tf-quantum-circuit id="tq-ex-circuit" palette="none" readonly aria-label="${escapeAttr(T('examples.circuit_title'))}"></tf-quantum-circuit>
      </div>
      <div data-view="text" hidden>
        <tf-code-editor id="tq-ex-source" language="plain" readonly aria-label="${escapeAttr(T('examples.view_text'))}"></tf-code-editor>
      </div>
      <div class="tq-parse-errors" id="tq-ex-errors" hidden></div>
    </div>
    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${sprite('check-circle')} ${escapeHtml(T('examples.expected_title'))}</div>
      </div>
      <div class="section-sub">${escapeHtml(T('examples.expected_sub', {
        shots: Number(expected.shots) || 0,
        tolerance: Number(expected.tolerance).toLocaleString(I18n.getLanguage()),
      }))}</div>
      <div class="ex-outcomes">
        ${rows.map((r) => `<div class="ex-outcome"><span class="mono">${escapeHtml(r.bits)}</span><span class="pts">${escapeHtml(percent(r.probability))}</span></div>`).join('')}
      </div>
    </div>`;
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// Forks one example into a new private project and opens it. With `studio` the
/// project opens on the Studio with the circuit cell that was just made.
async function fork(screen, { exampleId, qubits, qasm3 = '' }, { studio }, button) {
  if (button) button.setAttribute('disabled', '');
  let made;
  try {
    made = await screen.tq('tentaQuantExampleForkRequest', {
      exampleId, qubits, language: I18n.getLanguage(),
    });
  } catch (e) {
    if (button && button.isConnected) button.removeAttribute('disabled');
    toast(`${T('examples.fork_failed')}: ${errMessage(e)}`, 'error');
    return;
  }
  if (screen.disposed) return;
  toast(T('examples.fork_ok', { name: made.project.name }), 'success');
  await screen.openProject(made.project.projectId);
  if (studio && !screen.disposed) {
    await screen.openStudioWithCell({
      notebookId: made.notebook.notebookId,
      cellId: made.circuitCellId,
      source: qasm3,
      name: made.notebook.name,
    });
  }
}

/// Parses the program with the browser's own front end for the grid; the text
/// view always has the source, so a missing wasm module costs the drawing only.
async function drawCircuit(host, qasm3, stale) {
  const circuit = host.querySelector('#tq-ex-circuit');
  const errors = host.querySelector('#tq-ex-errors');
  circuit.labels = circuitLabels();
  const showError = (message) => {
    errors.hidden = false;
    errors.innerHTML = `<div class="tq-parse-error">${escapeHtml(message)}</div>`;
  };
  try {
    const { available, parse } = await import('/js/quantum/index.js');
    if (!await available()) { showError(T('studio.no_wasm_sub')); return; }
    const result = await parse(qasm3);
    if (stale()) return;
    if (result.status !== 'parsed') {
      showError((result.errors || []).map((e) => e.message).join(' · '));
      return;
    }
    circuit.circuit = result.circuit;
  } catch (e) {
    if (!stale()) showError(errMessage(e));
  }
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// Draws the tab into `host`: the page of the example the route names, or the
/// gallery.
export async function drawExamples(screen, host) {
  const state = screen.examples;
  const draw = (screen.examplesDraw = (screen.examplesDraw || 0) + 1);
  const instanceId = screen.instanceId;
  const stale = () => screen.disposed || screen.examplesDraw !== draw
    || screen.instanceId !== instanceId || screen.tab !== 'examples' || !host.isConnected;
  const canFork = has(screen.lab?.myPermissions, 'quant.run');

  host.innerHTML = `<div class="tq-loading">${escapeHtml(I18n.t('common.loading'))}</div>`;
  if (state.exampleId) {
    let detail;
    try {
      detail = await screen.tq('tentaQuantExampleGetRequest', { exampleId: state.exampleId, qubits: state.qubits });
    } catch (e) {
      if (stale()) return;
      // A route that names an example this build does not ship, or a width it
      // does not take, falls back to the gallery instead of a dead page.
      toast(`${T('examples.load_failed')}: ${errMessage(e)}`, 'error');
      state.exampleId = null;
      state.qubits = null;
      screen.setLocation();
      await drawExamples(screen, host);
      return;
    }
    if (stale()) return;
    paintDetail(screen, host, detail, canFork, stale);
    return;
  }

  let list;
  try {
    list = (await screen.tq('tentaQuantExampleListRequest')).examples || [];
  } catch (e) {
    if (stale()) return;
    host.innerHTML = `<tf-alert tone="danger" title="${escapeAttr(T('examples.load_failed'))}" message="${escapeAttr(errMessage(e))}"></tf-alert>`;
    return;
  }
  if (stale()) return;
  paintGallery(screen, host, list, canFork);
}

function open(screen, host, exampleId) {
  const state = screen.examples;
  state.exampleId = exampleId;
  state.qubits = null;
  screen.setLocation();
  drawExamples(screen, host);
}

function paintGallery(screen, host, list, canFork) {
  const state = screen.examples;
  const visible = filterExamples(list, state);
  host.innerHTML = galleryHtml(state, list, visible, canFork);

  host.querySelector('#tq-ex-search').addEventListener('search', (e) => {
    state.query = String(e.detail?.value ?? '');
    paintGallery(screen, host, list, canFork);
  });
  host.querySelector('#tq-ex-level').addEventListener('change', (e) => {
    state.level = e.detail?.value || 'all';
    paintGallery(screen, host, list, canFork);
  });
  const grid = host.querySelector('#tq-ex-grid');
  if (!grid) return;
  grid.addEventListener('click', (e) => {
    const card = e.target.closest('[data-example]');
    if (!card) return;
    const button = e.target.closest('[data-act="fork"]');
    if (button) {
      if (!button.hasAttribute('disabled')) fork(screen, { exampleId: card.dataset.example }, { studio: false }, button);
      return;
    }
    open(screen, host, card.dataset.example);
  });
  grid.addEventListener('keydown', (e) => {
    const card = e.target.closest('[data-example]');
    if (card && e.target === card && (e.key === 'Enter' || e.key === ' ')) {
      e.preventDefault();
      open(screen, host, card.dataset.example);
    }
  });
}

function paintDetail(screen, host, detail, canFork, stale) {
  const state = screen.examples;
  const example = detail.example;
  host.innerHTML = detailHtml(detail, canFork);

  const readme = host.querySelector('#tq-ex-readme');
  readme.labels = mimeLabels();
  readme.bundle = { 'text/markdown': pickText(detail.readme) };

  const source = host.querySelector('#tq-ex-source');
  source.labels = editorLabels();
  source.value = detail.qasm3;
  host.querySelector('#tq-ex-view').addEventListener('change', (e) => {
    const view = e.detail?.value || 'grid';
    host.querySelectorAll('[data-view]').forEach((el) => { el.hidden = el.dataset.view !== view; });
  });
  drawCircuit(host, detail.qasm3, stale);

  host.querySelector('[data-act="back"]').addEventListener('click', () => {
    state.exampleId = null;
    state.qubits = null;
    screen.setLocation();
    drawExamples(screen, host);
  });
  const width = host.querySelector('#tq-ex-width');
  if (width) {
    width.addEventListener('change', () => {
      const next = clampWidth(example, width.value, example.qubits);
      if (next === example.qubits) { width.value = String(next); return; }
      state.qubits = next;
      drawExamples(screen, host);
    });
  }
  const request = { exampleId: example.exampleId, qubits: state.qubits, qasm3: detail.qasm3 };
  host.querySelector('[data-act="fork"]').addEventListener('click', (e) => fork(screen, request, { studio: false }, e.currentTarget));
  host.querySelector('[data-act="studio"]').addEventListener('click', (e) => fork(screen, request, { studio: true }, e.currentTarget));
}
