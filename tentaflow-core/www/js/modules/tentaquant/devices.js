// ===== File: modules/tentaquant/devices.js — Q09, the Urządzenia tab of one laboratory =====
//
// The tab says where a circuit can be computed TODAY, and it says only what
// `Target::List` answers: the browser (T0) and Core on each node of the fleet
// (T1), with the qubit ceiling the laboratory's settings give each of them, the
// precision and — when a target is refused — the server's own sentence for why.
// The memory a state vector needs at the ceiling is derived from those two
// numbers (`stateMemoryBytes`), never typed in.
//
// The second half is the `device="auto"` rule of plan §5.3 asked live: the
// rule is evaluated by Core (`Target::Resolve`), so this view only puts the
// question and prints the answer — the same one a run started from the Studio
// or an SDK call would get.
//
// What the mockup shows and this does NOT build: GPU, Python and QPU tiers, the
// per-device "runs today" counters, VRAM and the calibration panel. The wire has
// no such data, and a tier the server lists as `unavailable` appears as one
// plain note with its reason, never as a device row that cannot take a run.

import { I18n } from '/js/i18n.js';
import { escapeHtml, escapeAttr, formatBytes } from '/js/utils.js';
import { T, sprite, errMessage } from '/js/modules/tentaquant/format.js';
import { stateMemoryBytes } from '/js/modules/tentaquant/quantum-view.js';
import { autoHint } from '/js/modules/tentaquant/targets.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-input.js';
import '/js/components/tf-stat-card.js';
import '/js/components/tf-toggle.js';

/// The widest register the probe control accepts. The laboratory's own ceilings
/// stop at 40 (the settings validation), so a larger question has no answer
/// worth asking for.
export const PROBE_MAX_QUBITS = 40;
const PROBE_DEBOUNCE_MS = 250;

/// The question the probe starts with: a width every tier takes, from a page
/// that can run T0 itself, with a plain circuit (not a kernel cell).
export function probeState(patch = {}) {
  return { qubits: 5, fromBrowser: true, needsKernel: false, ...patch };
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// A width typed into the control, held to what the rule can be asked: a whole
/// number from 1 to [`PROBE_MAX_QUBITS`]. Anything else is the previous value —
/// half-typed input must not send a question about 0 qubits.
export function clampProbeQubits(value, fallback) {
  const n = Math.trunc(Number(value));
  if (!Number.isFinite(n) || n < 1) return fallback;
  return Math.min(n, PROBE_MAX_QUBITS);
}

/// The wire payload of one probe.
export function probeRequest(state) {
  return {
    numQubits: state.qubits,
    fromBrowser: Boolean(state.fromBrowser),
    needsKernel: Boolean(state.needsKernel),
  };
}

const PRECISION_LABEL = { single: 'f32', double: 'f64' };

/// The cards of the target list: the browser first, then Core on each node with
/// the node answering this request ahead of the rest. Each card is plain data —
/// a title, who it is, whether it takes a run and why not — so the markup below
/// has nothing left to decide.
export function deviceCards(list) {
  const targets = (list?.targets || []).slice();
  const rank = (t) => (String(t.tier) === 'T0' ? 0 : t.isLocal ? 1 : 2);
  targets.sort((a, b) => rank(a) - rank(b)
    || String(a.nodeName || '').localeCompare(String(b.nodeName || '')));
  return targets.map((target) => {
    const browser = String(target.tier) === 'T0';
    const maxQubits = Number(target.maxQubits) || 0;
    const rows = [
      { key: 'where', value: browser ? T('devices.where_browser') : T(target.isLocal ? 'devices.where_this_node' : 'devices.where_other_node') },
      { key: 'max_qubits', value: String(maxQubits) },
      { key: 'precision', value: PRECISION_LABEL[target.precision] || String(target.precision || '') },
      { key: 'memory', value: formatBytes(stateMemoryBytes(maxQubits, target.precision)) },
    ];
    return {
      target: String(target.target),
      tier: String(target.tier),
      title: browser ? T('devices.browser_title') : (target.nodeName || target.nodeId || ''),
      subtitle: browser ? T('devices.browser_sub') : T('devices.core_sub'),
      available: Boolean(target.available),
      reason: target.available ? '' : (target.reason || T('targets.no_reason')),
      online: Boolean(target.online),
      rows,
    };
  });
}

/// What the KPI row counts, read off the same list: the targets that take a run
/// now, the widest register any of them accepts and the nodes running Core.
export function deviceTotals(list) {
  const targets = list?.targets || [];
  const open = targets.filter((t) => t.available);
  return {
    available: open.length,
    total: targets.length,
    widest: open.reduce((max, t) => Math.max(max, Number(t.maxQubits) || 0), 0),
    nodes: targets.filter((t) => String(t.tier) === 'T1').length,
  };
}

/// The answer to one probe as the view prints it: the headline `auto → T1 ·
/// node-a` (the very text the run selects show), the server's reason for the
/// choice and the tiers it considered and could not use.
export function resolutionView(resolution, targets) {
  if (!resolution) return null;
  const none = !resolution.target || String(resolution.tier) === 'none';
  return {
    tone: none ? 'warn' : 'ok',
    headline: autoHint(resolution, targets),
    reason: resolution.reason || '',
    skipped: (resolution.unavailable || []).map((u) => ({ tier: u.tier, reason: u.reason })),
  };
}

// ---------------------------------------------------------------------------
// Markup
// ---------------------------------------------------------------------------

function kpiHtml(totals) {
  return `<div class="tq-kpi">
    <tf-stat-card label="${escapeAttr(T('devices.kpi_available'))}" icon="chip"
      value="${escapeAttr(T('devices.kpi_available_value', { n: totals.available, total: totals.total }))}"></tf-stat-card>
    <tf-stat-card label="${escapeAttr(T('devices.kpi_widest'))}" icon="atom"
      value="${escapeAttr(totals.widest > 0 ? T('devices.qubits_value', { n: totals.widest }) : '—')}"></tf-stat-card>
    <tf-stat-card label="${escapeAttr(T('devices.kpi_nodes'))}" icon="cluster"
      value="${totals.nodes}"></tf-stat-card>
  </div>`;
}

function cardHtml(card) {
  const tone = card.tier.toLowerCase();
  return `
    <div class="q-card dev-card${card.available ? '' : ' is-disabled'}" data-target="${escapeAttr(card.target)}">
      <div class="qc-top">
        <div class="qc-ico${card.tier === 'T0' ? '' : ' local'}">${sprite(card.tier === 'T0' ? 'globe' : 'cpu')}</div>
        <div class="qc-head">
          <div class="qc-name">${escapeHtml(card.title)}</div>
          <div class="qc-type">${escapeHtml(card.subtitle)}</div>
        </div>
      </div>
      <div class="qc-tiers">
        <span class="tier ${tone}">${escapeHtml(card.tier)}</span>
        <tf-chip status="${card.available ? 'ok' : 'warn'}" dot label="${escapeAttr(card.available ? T('devices.state_available') : T('devices.state_unavailable'))}"></tf-chip>
      </div>
      ${card.available ? '' : `<div class="dev-reason" role="note">${escapeHtml(card.reason)}</div>`}
      <div class="kv">
        ${card.rows.map((r) => `<span class="k">${escapeHtml(T(`devices.row_${r.key}`))}</span><span class="v">${escapeHtml(r.value)}</span>`).join('')}
      </div>
    </div>`;
}

function probeHtml(state) {
  return `
    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${sprite('target')} ${escapeHtml(T('devices.auto_title'))}</div>
      </div>
      <div class="section-sub">${escapeHtml(T('devices.auto_sub'))}</div>
      <div class="dev-probe">
        <tf-input id="tq-probe-qubits" type="number" stepper min="1" max="${PROBE_MAX_QUBITS}" step="1"
          label="${escapeAttr(T('devices.probe_qubits'))}" value="${state.qubits}"
          stepper-dec-label="${escapeAttr(T('devices.probe_dec'))}" stepper-inc-label="${escapeAttr(T('devices.probe_inc'))}"></tf-input>
        <label class="toggle-row"><tf-toggle id="tq-probe-browser" ${state.fromBrowser ? 'checked' : ''}></tf-toggle>
          <span><span class="tr-name">${escapeHtml(T('devices.probe_browser'))}</span><span class="tr-sub">${escapeHtml(T('devices.probe_browser_sub'))}</span></span></label>
        <label class="toggle-row"><tf-toggle id="tq-probe-kernel" ${state.needsKernel ? 'checked' : ''}></tf-toggle>
          <span><span class="tr-name">${escapeHtml(T('devices.probe_kernel'))}</span><span class="tr-sub">${escapeHtml(T('devices.probe_kernel_sub'))}</span></span></label>
      </div>
      <div id="tq-probe-answer" class="dev-answer" aria-live="polite"></div>
    </div>`;
}

function answerHtml(view, error) {
  if (error) return `<tf-alert tone="danger" title="${escapeAttr(T('devices.probe_failed'))}" message="${escapeAttr(error)}"></tf-alert>`;
  if (!view) return `<div class="hint">${escapeHtml(T('targets.auto_checking'))}</div>`;
  return `
    <div class="check-result ${view.tone}" role="status">
      <div class="cr-ico">${sprite(view.tone === 'ok' ? 'check-circle' : 'alert')}</div>
      <div class="cr-body">
        <div class="cr-title mono">${escapeHtml(view.headline)}</div>
        ${view.reason ? `<div class="cr-sub">${escapeHtml(view.reason)}</div>` : ''}
        ${view.skipped.map((s) => `<div class="cr-sub">${escapeHtml(T('devices.skipped', { tier: s.tier, reason: s.reason }))}</div>`).join('')}
      </div>
    </div>`;
}

function missingHtml(unavailable) {
  if (!unavailable.length) return '';
  return `
    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${sprite('info')} ${escapeHtml(T('devices.missing_title'))}</div>
      </div>
      <div class="section-sub">${escapeHtml(T('devices.missing_sub'))}</div>
      <div class="dev-missing">
        ${unavailable.map((u) => `<div class="dev-missing-row"><span class="tier off">${escapeHtml(u.tier)}</span><span>${escapeHtml(u.reason)}</span></div>`).join('')}
      </div>
    </div>`;
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// Loads the target list and draws the tab into `host`. A response that lands
/// after the tab, the laboratory or a newer draw replaced this one is dropped.
export async function drawDevices(screen, host) {
  const draw = (screen.devicesDraw = (screen.devicesDraw || 0) + 1);
  const instanceId = screen.instanceId;
  const stale = () => screen.disposed || screen.devicesDraw !== draw
    || screen.instanceId !== instanceId || screen.tab !== 'devices' || !host.isConnected;

  host.innerHTML = `<div class="tq-loading">${escapeHtml(I18n.t('common.loading'))}</div>`;
  let list;
  try {
    list = await screen.tq('tentaQuantTargetListRequest');
  } catch (e) {
    if (stale()) return;
    host.innerHTML = `<tf-alert tone="danger" title="${escapeAttr(T('targets.load_failed'))}" message="${escapeAttr(errMessage(e))}"></tf-alert>`;
    return;
  }
  if (stale()) return;

  const cards = deviceCards(list);
  if (!cards.length) {
    host.innerHTML = `<tf-empty-state icon="chip" title="${escapeAttr(T('devices.empty'))}"></tf-empty-state>`;
    return;
  }
  const state = (screen.probe = screen.probe || probeState());
  host.innerHTML = `
    ${kpiHtml(deviceTotals(list))}
    <div class="card-grid dev-grid">${cards.map(cardHtml).join('')}</div>
    ${probeHtml(state)}
    ${missingHtml(list.unavailable || [])}`;

  const answer = host.querySelector('#tq-probe-answer');
  const qubits = host.querySelector('#tq-probe-qubits');
  let timer = null;
  let probe = 0;
  const ask = async () => {
    const mine = ++probe;
    answer.innerHTML = answerHtml(null);
    let view = null;
    let error = '';
    try {
      view = resolutionView(await screen.tq('tentaQuantTargetResolveRequest', probeRequest(state)), list.targets);
    } catch (e) {
      error = errMessage(e);
    }
    if (stale() || mine !== probe) return;
    answer.innerHTML = answerHtml(view, error);
  };
  const askSoon = () => {
    clearTimeout(timer);
    timer = setTimeout(ask, PROBE_DEBOUNCE_MS);
  };

  qubits.addEventListener('input', () => {
    const next = clampProbeQubits(qubits.value, state.qubits);
    if (next === state.qubits) return;
    state.qubits = next;
    askSoon();
  });
  host.querySelector('#tq-probe-browser').addEventListener('change', (e) => {
    state.fromBrowser = Boolean(e.detail?.checked);
    askSoon();
  });
  host.querySelector('#tq-probe-kernel').addEventListener('change', (e) => {
    state.needsKernel = Boolean(e.detail?.checked);
    askSoon();
  });
  ask();
}
