// ===== File: modules/tentaquant/run-view.js — the full-screen run result (Q15) =====
//
// Plan §13.6 gives a run a second, bigger home than the Q08 table: five views
// over ONE recording — Ewolucja, Stan, Histogram, Porównanie, Dane i eksport.
// They are real tabs and not stacked sections, because each of them owns work
// (a request, an animation, an export) that must not run while it is off
// screen.
//
// Where the frames come from is stated on screen, never guessed. A run records
// one `StateKeyframe` per program step; this view replays them and computes
// every frame BETWEEN two of them EXACTLY (`keyframe-math`), which is the whole
// argument of §13.6: a gate commutes with the partial trace over the qubits it
// does not touch, so the reduced quantities of its own qubits follow from the
// 4×4 matrix the frame carries. A run whose evolution was not recorded says so
// with the run's own `evolution_note` instead of animating a fabrication.
//
// The measured histogram is REPLAYED, not resampled: the bars fill with the
// run's own counts as the playhead crosses the measurement step. Drawing a
// fresh browser sample next to a run id would put numbers on screen that are
// in no artifact and in no export — the recorded decision under "Napełnianie
// histogramu" in §13.6.

import { escapeHtml, escapeAttr, formatBytes, toast } from '/js/utils.js';
import {
  T, sprite, blochLabels, errMessage, fmtDate, shortId,
} from '/js/modules/tentaquant/format.js';
import { downloadUrl } from '/js/modules/tentaquant/files.js';
import {
  canControlRun, runIsLive, runNodeName, runStatusLabel, runStatusTone, runTier, setRunPinned,
} from '/js/modules/tentaquant/run-model.js';
import { detailRows, timelineHtml } from '/js/modules/tentaquant/run-detail.js';
import { RunStream, outputOfMime, outputBundle } from '/js/modules/tentaquant/run-stream.js';
import { COUNTS_MIME, STATE_MIME, MS_PER_GATE } from '/js/modules/tentaquant/quantum-view.js';
import { PROBS_MIME, amplitudeRows } from '/js/components/tf-mime-output.js';
import {
  densityFromAmplitudes, frameAt, initialFrame, isCollapsingGate, measurementSteps, readFrame,
} from '/js/modules/tentaquant/keyframe-math.js';
import { explainGate, explainHistogram, explainState, explainText } from '/js/modules/tentaquant/explain.js';
import { COMPARE_MAX, resultTitle } from '/js/modules/tentaquant/results.js';
import {
  hellingerFidelity, seriesProbabilities, totalVariationDistance,
} from '/js/components/tf-shot-histogram.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-bloch-sphere.js';
import '/js/components/tf-breadcrumb.js';
import '/js/components/tf-button.js';
import '/js/components/tf-checkbox.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-density-plot.js';
import '/js/components/tf-entanglement-graph.js';
import '/js/components/tf-line-chart.js';
import '/js/components/tf-qsphere.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-shot-histogram.js';
import '/js/components/tf-state-bars.js';
import '/js/components/tf-state-timeline.js';
import '/js/components/tf-table.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-toggle.js';

export const RESULT_TABS = ['evolution', 'state', 'histogram', 'compare', 'data'];

/// `RUN_EXPORT_PARTS` on the wire, in archive order. The checklist offers all
/// of them; the node writes the ones the run actually has data for and answers
/// with the entries it wrote.
export const EXPORT_PARTS = [
  'counts_json', 'counts_csv', 'statevector_npz', 'circuit_qasm', 'method_md', 'citation_bib',
];

/// Above this width the full density matrix is 4^n complex numbers and the
/// picture is a grey square: §13.6 draws pair matrices instead.
export const MAX_DENSITY_QUBITS = 6;

/// Heaviest bitstring probabilities a state query asks for — enough to fill the
/// Q-sphere and the amplitude bars of a wide register.
const STATE_QUERY_TOP_K = 64;

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// The circuit strip of the timeline, straight out of the recorded frames: one
/// column per step, named by the gate the frame was taken after.
export function stepsFromFrames(frames) {
  return (Array.isArray(frames) ? frames : []).map((frame, index) => {
    const gate = frame.gate || null;
    return {
      step: Number(frame.step) || index + 1,
      name: String((gate && gate.name) || T('result.step_unknown')),
      qubits: Array.from((gate && gate.qubits) || [], Number),
      collapsing: isCollapsingGate(gate),
    };
  });
}

/// How much of the shot budget has been delivered at `position`. The histogram
/// fills AT the measurement (§13.6) and not before it, so the ramp spans the
/// measurement steps of the recording and nothing else.
export function shotProgress(position, steps, total) {
  const measures = Array.isArray(steps) ? steps : [];
  const p = Math.max(0, Number(position) || 0);
  if (!measures.length) return p >= (Number(total) || 0) - 1e-9 ? 1 : 0;
  const start = measures[0] - 1;
  const end = measures[measures.length - 1];
  if (p <= start) return 0;
  if (p >= end) return 1;
  return (p - start) / Math.max(1e-9, end - start);
}

/// The run's own counts, delivered up to `progress`. Rounding is per bar and
/// the total is left to follow: a replay that forced the total to an exact
/// figure would have to move shots between bars that never moved.
export function partialCounts(counts, progress) {
  const fraction = Math.max(0, Math.min(1, Number(progress) || 0));
  const out = {};
  for (const [key, value] of Object.entries(counts || {})) {
    out[key] = Math.round((Number(value) || 0) * fraction);
  }
  return out;
}

/// The `-counts+json` output as `tentaquant/runs.rs::stored_counts` reads it:
/// the histogram plus the shot total, which falls back to the sum of the bars
/// when the artifact did not record one.
export function storedCounts(bundle) {
  const value = bundle && bundle[COUNTS_MIME];
  const counts = value && value.counts;
  if (!counts) return null;
  const stored = Number(value.shots);
  const shots = Number.isFinite(stored) && stored > 0
    ? stored
    : Object.values(counts).reduce((sum, v) => sum + (Number(v) || 0), 0);
  return { counts, shots };
}

/// The exact distribution of a `-probs+json` output as a bitstring map — the
/// "ideał" series the measured bars are compared against. The payload is an
/// array indexed by basis state, which is what has to be labelled here.
export function probabilityMap(bundle) {
  const value = bundle && bundle[PROBS_MIME];
  const list = value && value.probabilities;
  if (!Array.isArray(list) || !list.length) return null;
  const numQubits = Math.max(1, Number(value.numQubits) || Math.round(Math.log2(list.length)));
  const out = {};
  for (let index = 0; index < list.length; index += 1) {
    const p = Number(list[index]);
    if (Number.isFinite(p) && p > 0) out[index.toString(2).padStart(numQubits, '0')] = p;
  }
  return out;
}

/// The BibTeX entry of the scientific package, byte for byte as
/// `tentaquant/export.rs::citation_bib` writes it — the button copies what the
/// archive contains, not something that merely resembles it.
export function citationBib(run) {
  const id = String(run.runId || '');
  const author = String(run.userName || '').replace(/[{}\\]/g, '');
  const started = String(run.startedAt || '');
  return `@misc{tentaquant-${id},\n  title  = {TentaQuant run ${id}},\n  author = {${author}},\n`
    + `  year   = {${started.slice(0, 4)}},\n  note   = {Run ${id}, started ${started}, target ${run.target}},\n}\n`;
}

/// The methodological note of the package. It MIRRORS
/// `tentaquant/export.rs::method_md` — the same rows, the same order, the same
/// rule that a value the run did not store gets no row — so the preview here
/// is the file the archive will contain and not a second, prettier document.
export function methodNote(run, { projectName = '', counts = null } = {}) {
  const lines = [`# Method note — run \`${run.runId}\``, '',
    'Generated from the stored record of this run. Every value below is read from the '
    + "laboratory's database; nothing is recomputed or inferred.", '',
    '## Execution', '', '| | |', '|---|---|'];
  const field = (name, value) => lines.push(`| ${name} | ${value} |`);
  field('Run id', `\`${run.runId}\``);
  field('Kind', run.kind);
  if (projectName) field('Project', projectName);
  field('Started', run.startedAt);
  if (run.endedAt) field('Ended', run.endedAt);
  field('Status', run.status);
  field('Target', run.target);
  if (run.nodeId) field('Node', run.nodeId);
  const metrics = run.metrics;
  if (metrics) {
    if (metrics.coreVersion) field('Engine', `TentaFlow Core ${metrics.coreVersion}`);
    lines.push('', '## Simulation', '', '| | |', '|---|---|');
    field('Simulator backend', metrics.backend || '');
    field('Method', metrics.method || '');
    field('Amplitude precision', metrics.precision || '');
    field('Qubits', String(Number(metrics.qubits) || 0));
    field('Classical bits', String(Number(metrics.clbits) || 0));
    field('Gates', String(Number(metrics.gates) || 0));
    field('Shots', String(Number(metrics.shots) || 0));
    field('Seed', String(Number(metrics.seed) || 0));
    field('Duration', `${Number(metrics.durationMs) || 0} ms`);
    field('Peak state memory', `${Number(metrics.memoryBytes) || 0} B`);
    field('Recorded evolution', Number(metrics.keyframes) > 0 ? `yes, ${metrics.keyframes} keyframes` : 'no');
    field('Stored state vector', (run.artifacts || []).some((a) => a.mime === STATE_MIME) ? 'yes' : 'no');
    if (metrics.evolutionNote) field('Evolution note', metrics.evolutionNote);
    if (metrics.stateNote) field('State note', metrics.stateNote);
  }
  if (counts) {
    lines.push('', '## Measurement', '', '| | |', '|---|---|');
    field('Shots', String(Number(counts.shots) || 0));
    field('Distinct outcomes', String(Object.keys(counts.counts || {}).length));
    lines.push('', 'The full histogram is in `counts.json` and `counts.csv`.');
  }
  if (run.error) lines.push('', '## Outcome', '', `The run ended with: ${run.error}`);
  return `${lines.join('\n')}\n`;
}

/// The tone a comparison series is drawn in. Eight runs, eight distinguishable
/// colours defined in controls.css, in the order the chips are.
export function compareTone(index) {
  return 'abcdefgh'[Math.max(0, Math.min(COMPARE_MAX - 1, Number(index) || 0))];
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// Draws the run result screen into `host` and hands the screen the disposer
/// for the animation and the stream it owns.
export function drawRunView(screen, host, runId, { tab = 'evolution', compare = [] } = {}) {
  if (!host) return null;
  const view = new RunResultView(screen, host, runId, { tab, compare });
  screen.setRunViewDispose(() => view.dispose());
  view.ready = view.mount();
  return view;
}

class RunResultView {
  constructor(screen, host, runId, { tab, compare }) {
    this.screen = screen;
    this.host = host;
    this.runId = runId;
    this.tab = RESULT_TABS.includes(tab) ? tab : 'evolution';
    this.run = null;
    this.frames = [];
    /// Where the frames on screen came from — the honest label of §13.6.
    this.frameSource = '';
    this.counts = null;
    this.storedCounts = null;
    this.ideal = null;
    this.stateOutput = null;
    this.stateQuery = null;
    this.stateError = '';
    this.comparison = null;
    this.compareError = '';
    this.comparePending = false;
    this.compareIds = Array.from(new Set([runId, ...compare])).slice(0, COMPARE_MAX);
    this.exportParts = new Set(EXPORT_PARTS);
    this.exportResult = null;
    this.explain = new Set(['evolution', 'state', 'histogram']);
    this.position = 0;
    this.playing = false;
    this.speed = 1;
    this.stream = null;
    this.raf = 0;
    this.lastTick = 0;
    this.disposed = false;
    this.ready = null;
    // Two delegated listeners for the life of the view: the panel is replaced
    // on every tab switch and on every stream frame, so a listener attached in
    // a render would fire once per render that had happened.
    this.onHostClick = (event) => this.onClick(event);
    this.onHostChange = (event) => this.onChange(event);
    this.host.addEventListener('click', this.onHostClick);
    this.host.addEventListener('change', this.onHostChange);
  }

  dispose() {
    this.disposed = true;
    this.stopClock();
    this.host.removeEventListener('click', this.onHostClick);
    this.host.removeEventListener('change', this.onHostChange);
    if (this.stream) this.stream.stop();
    this.stream = null;
  }

  // -- loading ---------------------------------------------------------------

  async mount() {
    this.host.innerHTML = `<div class="tq-loading">${escapeHtml(T('run.loading'))}</div>`;
    try {
      const res = await this.screen.tq('tentaQuantRunGetRequest', { runId: this.runId });
      if (this.disposed) return;
      this.run = res.run;
    } catch (e) {
      this.host.innerHTML = `<tf-alert tone="danger" title="${escapeAttr(T('run.load_failed'))}" message="${escapeAttr(errMessage(e))}"></tf-alert>`;
      return;
    }
    this.readArtifacts();
    await this.loadFrames();
    if (this.disposed) return;
    this.render();
    if (runIsLive(this.run)) this.follow();
  }

  readArtifacts() {
    const artifacts = this.run.artifacts || [];
    const state = { outputs: artifacts };
    this.storedCounts = storedCounts(outputBundle(outputOfMime(state, COUNTS_MIME)));
    this.counts = this.storedCounts ? this.storedCounts.counts : null;
    this.ideal = probabilityMap(outputBundle(outputOfMime(state, PROBS_MIME)));
    this.stateOutput = (outputBundle(outputOfMime(state, STATE_MIME)) || {})[STATE_MIME] || null;
  }

  /// The recorded evolution. A run that recorded none is not an error: it says
  /// so with its own note, which is exactly what `RunMetrics.evolution_note`
  /// exists for.
  async loadFrames() {
    if (!Number(this.run.metrics?.keyframes)) return;
    try {
      const res = await this.screen.tq('tentaQuantRunKeyframesRequest', { runId: this.runId });
      if (this.disposed) return;
      this.frames = res.keyframes || [];
      this.frameSource = this.frames.length ? 'stored' : '';
    } catch {
      // The frames are an extra: a run whose recording cannot be read still
      // has a histogram, a state and an export, so the tab says what is
      // missing rather than the whole screen failing.
      this.frames = [];
      this.frameSource = '';
    }
  }

  /// A run still going streams its frames in; the animation follows the tail.
  async follow() {
    this.stream = new RunStream(this.screen, this.runId, {
      onUpdate: (state) => this.absorb(state),
      onEnd: () => { if (this.stream) { this.stream.stop(); this.stream = null; } },
    });
    await this.stream.start();
  }

  absorb(state) {
    if (this.disposed || !this.run) return;
    if (state.keyframes.length) {
      this.frames = state.keyframes;
      this.frameSource = 'live';
    }
    if (state.run) this.run = { ...this.run, ...state.run };
    if (state.metrics) this.run = { ...this.run, metrics: state.metrics };
    if (state.outputs.length) {
      this.run = { ...this.run, artifacts: state.outputs };
      this.readArtifacts();
    }
    this.render();
  }

  // -- shell -----------------------------------------------------------------

  render() {
    const run = this.run;
    const nodes = this.screen.lab?.nodes || [];
    const tier = runTier(run);
    const mine = canControlRun(run, this.screen.userId);
    this.host.innerHTML = `
      <tf-breadcrumb class="tq-crumbs">
        <tf-breadcrumb-item href="#/tentaquant">${escapeHtml(T('title'))}</tf-breadcrumb-item>
        <tf-breadcrumb-item href="#/tentaquant?instance=${escapeAttr(this.screen.instanceId)}">${escapeHtml(this.screen.lab?.displayName || this.screen.instanceId)}</tf-breadcrumb-item>
        ${this.screen.projectId ? `<tf-breadcrumb-item href="#/tentaquant?project=${escapeAttr(this.screen.projectId)}">${escapeHtml(this.screen.project?.name || this.screen.projectId)}</tf-breadcrumb-item>` : ''}
        <tf-breadcrumb-item current>${escapeHtml(T('result.crumb', { id: shortId(run.runId) }))}</tf-breadcrumb-item>
      </tf-breadcrumb>
      <div class="tf-detail-header tq-project-header">
        <div class="big-ico tq-ico">${sprite('bar-chart')}</div>
        <div class="d-meta">
          <div class="d-name">${escapeHtml(resultTitle(run))}
            <span class="tier ${tier ? tier.toLowerCase() : 'off'}">${escapeHtml(tier ? `${T('runs.tier_' + tier.toLowerCase())} · ${runNodeName(run, nodes)}` : run.target)}</span>
            <tf-chip status="${runStatusTone(run.status)}" label="${escapeAttr(runStatusLabel(run))}"></tf-chip>
            ${run.pinnedAt ? `<tf-chip status="accent" label="${escapeAttr(T('runs.pinned'))}"></tf-chip>` : ''}
          </div>
          <div class="d-sub mono">${escapeHtml(run.runId)} · ${escapeHtml(fmtDate(run.startedAt))} · ${escapeHtml(run.userName || run.userId || '')}</div>
        </div>
        <div class="d-actions">
          <tf-button variant="secondary" size="sm" icon="bar-chart" data-act="tab-compare">${escapeHtml(T('result.action_compare'))}</tf-button>
          <tf-button variant="secondary" size="sm" icon="download" data-act="tab-data">${escapeHtml(T('result.action_export'))}</tf-button>
          ${mine ? `<tf-button variant="secondary" size="sm" icon="star" data-act="pin">${escapeHtml(T(run.pinnedAt ? 'runs.unpin' : 'runs.pin'))}</tf-button>` : ''}
          ${run.notebookId ? `<tf-button variant="ghost" size="sm" icon="file-text" data-act="notebook">${escapeHtml(T('result.action_notebook'))}</tf-button>` : ''}
          <tf-button variant="ghost" size="sm" icon="x" data-act="close">${escapeHtml(T('run.close'))}</tf-button>
        </div>
      </div>
      <tf-tabs variant="underline" value="${escapeAttr(this.tab)}" id="tq-result-tabs">
        <tf-tab id="evolution" icon="play">${escapeHtml(T('result.tab_evolution'))}</tf-tab>
        <tf-tab id="state" icon="atom">${escapeHtml(T('result.tab_state'))}</tf-tab>
        <tf-tab id="histogram" icon="bar-chart">${escapeHtml(T('result.tab_histogram'))}</tf-tab>
        <tf-tab id="compare" icon="layers" count="${this.compareIds.length}">${escapeHtml(T('result.tab_compare'))}</tf-tab>
        <tf-tab id="data" icon="download">${escapeHtml(T('result.tab_data'))}</tf-tab>
      </tf-tabs>
      <div class="res-layout">
        <div class="res-main" id="tq-result-panel"></div>
        <div class="res-rail">
          <div class="section-card">
            <div class="section-card-head"><div class="title">${sprite('list')}${escapeHtml(T('result.rail_meta'))}</div></div>
            ${kvHtml(detailRows(run, { projectName: this.screen.project?.name || '', nodes }))}
          </div>
          <div class="section-card">
            <div class="section-card-head"><div class="title">${sprite('clock')}${escapeHtml(T('run.timeline_title'))}</div></div>
            ${timelineHtml(run)}
          </div>
        </div>
      </div>`;
    this.host.querySelector('#tq-result-tabs').addEventListener('change', (e) => {
      if (e.detail.value !== this.tab) this.selectTab(e.detail.value);
    });
    this.host.querySelector('tf-breadcrumb.tq-crumbs').addEventListener('click', (event) => {
      const link = event.target.closest('a.tf-breadcrumb-item');
      if (!link) return;
      event.preventDefault();
      const href = link.getAttribute('href');
      if (href.includes('project=')) this.screen.closeRunResult();
      else if (href.includes('instance=')) this.screen.closeProject();
      else this.screen.backToLabs();
    });
    this.renderTab();
  }

  selectTab(tab) {
    if (!RESULT_TABS.includes(tab)) return;
    this.stopClock();
    this.tab = tab;
    this.screen.setResultTab(tab);
    const tabs = this.host.querySelector('#tq-result-tabs');
    if (tabs) tabs.setAttribute('value', tab);
    this.renderTab();
  }

  renderTab() {
    const panel = this.host.querySelector('#tq-result-panel');
    if (!panel) return;
    this.stopClock();
    if (this.tab === 'state') { this.renderState(panel); return; }
    if (this.tab === 'histogram') { this.renderHistogram(panel); return; }
    if (this.tab === 'compare') { this.renderCompare(panel); return; }
    if (this.tab === 'data') { this.renderData(panel); return; }
    this.renderEvolution(panel);
  }

  // -- Ewolucja --------------------------------------------------------------

  renderEvolution(panel) {
    if (!this.frames.length) {
      const note = this.run.metrics?.evolutionNote || T('result.evolution_none_sub');
      panel.innerHTML = `
        <div class="section-card">
          ${sectionHead('play', T('result.tab_evolution'))}
          <tf-alert tone="info" title="${escapeAttr(T('result.evolution_none'))}" message="${escapeAttr(note)}"></tf-alert>
        </div>`;
      return;
    }
    const numQubits = this.numQubits();
    panel.innerHTML = `
      <div class="section-card" id="tq-evolution">
        ${sectionHead('play', T('result.tab_evolution'), `
          <tf-chip status="info" label="${escapeAttr(T('result.frame_source_' + (this.frameSource || 'stored')))}"></tf-chip>
          <tf-chip label="${escapeAttr(T('result.evolution_size', { q: numQubits, n: this.frames.length }))}"></tf-chip>
          <span class="tf-toolbar-spacer"></span>
          ${explainToggle('evolution', this.explain.has('evolution'))}`)}
        <tf-state-timeline id="tq-strip"></tf-state-timeline>
        <div class="evo-grid">
          <div class="res-panel">
            <div class="rp-title">${escapeHtml(T('result.panel_bloch'))}<span class="tf-toolbar-spacer"></span><tf-chip status="accent" data-ent hidden label="${escapeAttr(T('bloch.entangled'))}"></tf-chip></div>
            <div class="bloch-row" id="tq-evo-bloch"></div>
          </div>
          <div class="res-panel">
            <div class="rp-title">${escapeHtml(T('result.panel_amplitudes'))}<span class="tf-toolbar-spacer"></span><span class="tq-phase-wheel" title="${escapeAttr(T('result.phase_wheel'))}"></span></div>
            <tf-state-bars id="tq-evo-bars" size="sm" max-bars="16"></tf-state-bars>
          </div>
          <div class="res-panel">
            <div class="rp-title">${escapeHtml(T('result.panel_shots'))}<span class="tf-toolbar-spacer"></span><tf-chip id="tq-evo-shots" mono label=""></tf-chip></div>
            <tf-shot-histogram id="tq-evo-hist" height="120" whiskers="off" max-bars="8"></tf-shot-histogram>
          </div>
        </div>
        <div class="explain-box" id="tq-explain-evolution" ${this.explain.has('evolution') ? '' : 'hidden'}></div>
      </div>`;

    const strip = panel.querySelector('#tq-strip');
    strip.labels = timelineLabels();
    strip.numQubits = numQubits;
    strip.steps = stepsFromFrames(this.frames);
    strip.setAttribute('speed', String(this.speed));
    strip.addEventListener('seek', (event) => {
      this.playing = false;
      strip.playing = false;
      this.stopClock();
      this.position = Number(event.detail.position) || 0;
      this.paintFrame();
    });
    strip.addEventListener('speed-change', (event) => { this.speed = Number(event.detail.speed) || 1; });
    strip.addEventListener('transport', (event) => this.transport(event.detail.action));

    const row = panel.querySelector('#tq-evo-bloch');
    const labels = blochLabels();
    for (let qubit = 0; qubit < numQubits; qubit += 1) {
      const sphere = document.createElement('tf-bloch-sphere');
      sphere.setAttribute('size', '84');
      sphere.setAttribute('duration', '0');
      sphere.setAttribute('trail-length', '0');
      sphere.label = `q${qubit}`;
      sphere.labels = labels;
      row.appendChild(sphere);
    }
    panel.querySelector('#tq-evo-bars').labels = barsLabels();
    panel.querySelector('#tq-evo-hist').labels = histogramLabels();
    this.paintFrame();
  }

  transport(action) {
    if (action === 'prev') { this.seekStep(-1); return; }
    if (action === 'next') { this.seekStep(1); return; }
    this.playing = !this.playing;
    if (this.playing) this.startClock();
    else this.stopClock();
  }

  seekStep(delta) {
    this.playing = false;
    this.stopClock();
    const strip = this.host.querySelector('#tq-strip');
    if (strip) strip.playing = false;
    const current = Math.round(this.position);
    this.position = Math.max(0, Math.min(this.frames.length, current + delta));
    this.paintFrame();
  }

  startClock() {
    this.stopClock();
    if (typeof requestAnimationFrame !== 'function') return;
    if (this.position >= this.frames.length) this.position = 0;
    this.lastTick = 0;
    this.raf = requestAnimationFrame((now) => this.tick(now));
  }

  stopClock() {
    if (this.raf && typeof cancelAnimationFrame === 'function') cancelAnimationFrame(this.raf);
    this.raf = 0;
  }

  /// One animation frame. The clock always runs on real time — a gate takes
  /// `MS_PER_GATE / speed` whatever the motion preference is — but under
  /// `prefers-reduced-motion` the DRAWN position snaps to the gate boundary, so
  /// the evolution still advances and simply stops gliding (§13.4).
  tick(now) {
    if (this.disposed || !this.playing) return;
    const delta = this.lastTick ? now - this.lastTick : 0;
    this.lastTick = now;
    this.position = Math.min(this.frames.length, this.position + (delta * this.speed) / MS_PER_GATE);
    this.paintFrame();
    if (this.position >= this.frames.length) {
      this.playing = false;
      const strip = this.host.querySelector('#tq-strip');
      if (strip) strip.playing = false;
      this.stopClock();
      return;
    }
    this.raf = requestAnimationFrame((next) => this.tick(next));
  }

  drawnPosition() {
    return reducedMotion() ? Math.round(this.position) : this.position;
  }

  paintFrame() {
    const panel = this.host.querySelector('#tq-evolution');
    if (!panel) return;
    const position = this.drawnPosition();
    const frame = frameAt(this.frames, position) || initialFrame(this.numQubits());
    const strip = panel.querySelector('#tq-strip');
    if (strip) strip.position = position;
    const spheres = panel.querySelectorAll('#tq-evo-bloch tf-bloch-sphere');
    spheres.forEach((sphere, qubit) => {
      sphere.vector = frame.bloch[qubit] || [0, 0, 1];
      const purity = Number(frame.purity[qubit]);
      if (Number.isFinite(purity)) sphere.purity = purity;
    });
    const entangled = frame.purity.some((p) => Number(p) < 0.99);
    const chip = panel.querySelector('[data-ent]');
    if (chip) chip.hidden = !entangled;
    const bars = panel.querySelector('#tq-evo-bars');
    if (bars) bars.state = { top: amplitudeGroups(frame), numQubits: this.numQubits() };
    this.paintLiveShots(panel, position);
    if (this.explain.has('evolution')) {
      const box = panel.querySelector('#tq-explain-evolution');
      // The state the current step started from. For the FIRST step that is
      // the register before the circuit ran — |0…0⟩ — not "nothing": without it
      // the opening gate would be reported as having changed nothing.
      const previous = position > 1
        ? readFrame(this.frames[Math.ceil(position) - 2])
        : initialFrame(this.numQubits());
      box.textContent = explainText([...explainGate(frame, previous), ...explainState(frame)]);
    }
  }

  paintLiveShots(panel, position) {
    const histogram = panel.querySelector('#tq-evo-hist');
    const chip = panel.querySelector('#tq-evo-shots');
    if (!histogram) return;
    const total = Number(this.run.metrics?.shots) || 0;
    if (!this.counts || !total) {
      histogram.series = [];
      if (chip) chip.setAttribute('label', T('result.shots_none'));
      return;
    }
    const progress = shotProgress(position, measurementSteps(this.frames), this.frames.length);
    const counts = partialCounts(this.counts, progress);
    const drawn = Object.values(counts).reduce((sum, v) => sum + v, 0);
    histogram.series = [{ id: 'measured', label: T('result.series_measured'), tone: 'measured', counts, shots: total }];
    if (chip) chip.setAttribute('label', T('result.shots_progress', { n: drawn, total }));
  }

  // -- Stan ------------------------------------------------------------------

  async renderState(panel) {
    if (!this.stateQuery && !this.stateError) {
      panel.innerHTML = `<div class="tq-loading">${escapeHtml(T('result.state_loading'))}</div>`;
      await this.loadStateQuery();
      if (this.disposed || this.tab !== 'state') return;
    }
    if (this.stateError) {
      panel.innerHTML = `<div class="section-card">${sectionHead('atom', T('result.tab_state'))}
        <tf-alert tone="warning" title="${escapeAttr(T('result.state_failed'))}" message="${escapeAttr(this.stateError)}"></tf-alert></div>`;
      return;
    }
    const query = this.stateQuery;
    const numQubits = this.numQubits();
    const amplitudes = this.stateAmplitudes();
    const frame = this.stateFrame(amplitudes, numQubits);
    const density = this.densityMatrix(numQubits, amplitudes, query.pairs);
    panel.innerHTML = `
      <div class="section-card" id="tq-state">
        ${sectionHead('atom', T('result.tab_state'), `
          <tf-chip status="info" label="${escapeAttr(T('result.state_source_' + (query.source === 'keyframe' ? 'keyframe' : 'state')))}"></tf-chip>
          <span class="tf-toolbar-spacer"></span>
          ${explainToggle('state', this.explain.has('state'))}`)}
        <div class="state-grid">
          <div class="res-panel">
            <div class="rp-title">${escapeHtml(T('result.panel_bloch'))}</div>
            <div class="bloch-row" id="tq-state-bloch"></div>
          </div>
          <div class="res-panel">
            <div class="rp-title">${escapeHtml(T('result.panel_qsphere'))}</div>
            <tf-qsphere id="tq-qsphere" size="200"></tf-qsphere>
            <div class="hint">${escapeHtml(T('result.qsphere_hint'))}</div>
          </div>
          <div class="res-panel">
            <div class="rp-title">${escapeHtml(T('result.panel_amplitudes'))}<span class="tf-toolbar-spacer"></span><span class="tq-phase-wheel" title="${escapeAttr(T('result.phase_wheel'))}"></span></div>
            <tf-state-bars id="tq-state-bars" max-bars="16"></tf-state-bars>
          </div>
          <div class="res-panel">
            <div class="rp-title">${escapeHtml(T('result.panel_density'))}<span class="tf-toolbar-spacer"></span>
              <tf-segmented size="sm" id="tq-density-part" value="re">
                <option value="re">Re</option><option value="im">Im</option>
              </tf-segmented>
              <tf-segmented size="sm" id="tq-density-mode" value="heat">
                <option value="heat">${escapeHtml(T('result.density_heat'))}</option>
                <option value="city">${escapeHtml(T('result.density_city'))}</option>
              </tf-segmented>
            </div>
            ${density ? '<tf-density-plot id="tq-density"></tf-density-plot>' : `<div class="hint">${escapeHtml(T('result.density_none'))}</div>`}
            ${density ? `<div class="hint">${escapeHtml(T(density.scope === 'full' ? 'result.density_full' : 'result.density_pair', { max: MAX_DENSITY_QUBITS, a: density.a, b: density.b }))}</div>` : ''}
          </div>
          <div class="res-panel res-panel--wide">
            <div class="rp-title">${escapeHtml(T('result.panel_entanglement'))}<span class="tf-toolbar-spacer"></span><span class="hint">${escapeHtml(T('result.entanglement_hint'))}</span></div>
            <div class="tq-chart-scroll"><tf-entanglement-graph id="tq-entgraph"></tf-entanglement-graph></div>
          </div>
        </div>
        <div class="explain-box" id="tq-explain-state" ${this.explain.has('state') ? '' : 'hidden'}></div>
      </div>`;

    const row = panel.querySelector('#tq-state-bloch');
    const labels = blochLabels();
    for (let qubit = 0; qubit < numQubits; qubit += 1) {
      const sphere = document.createElement('tf-bloch-sphere');
      sphere.setAttribute('size', '92');
      sphere.label = `q${qubit}`;
      sphere.labels = labels;
      sphere.vector = frame.bloch[qubit] || [0, 0, 1];
      const purity = Number(frame.purity[qubit]);
      if (Number.isFinite(purity)) sphere.purity = purity;
      row.appendChild(sphere);
    }
    const sphere = panel.querySelector('#tq-qsphere');
    sphere.labels = qsphereLabels();
    sphere.state = amplitudes;
    const bars = panel.querySelector('#tq-state-bars');
    bars.labels = barsLabels();
    bars.state = amplitudes;
    const graph = panel.querySelector('#tq-entgraph');
    graph.labels = entanglementLabels();
    graph.numQubits = numQubits;
    graph.pairs = query.pairs || [];
    const plot = panel.querySelector('#tq-density');
    if (plot) {
      plot.labels = densityLabels();
      plot.matrix = density.matrix;
      panel.querySelector('#tq-density-part').addEventListener('change', (e) => plot.setAttribute('part', e.detail.value));
      panel.querySelector('#tq-density-mode').addEventListener('change', (e) => plot.setAttribute('mode', e.detail.value));
    }
    if (this.explain.has('state')) {
      panel.querySelector('#tq-explain-state').textContent = explainText(explainState(frame));
    }
  }

  async loadStateQuery() {
    try {
      const res = await this.screen.tq('tentaQuantRunStateQueryRequest', {
        // Empty asks for EVERY pair. The node refuses that above
        // `RUN_STATE_QUERY_MAX_PAIRS` (496), and a run cannot get there: the
        // simulator ceiling is `DEFAULT_MAX_QUBITS` = 30, which is 435 pairs.
        pairs: [],
        runId: this.runId,
        topK: STATE_QUERY_TOP_K,
      });
      if (this.disposed) return;
      this.stateQuery = res || {};
    } catch (e) {
      this.stateError = errMessage(e);
    }
  }

  /// The state the Stan tab draws: the query's reduced quantities plus whatever
  /// amplitudes the run kept, in the shape every view of this module reads —
  /// the explanation and the pictures then describe one and the same state.
  stateFrame(amplitudes, numQubits) {
    const query = this.stateQuery || {};
    return {
      bloch: (query.bloch || []).map((v) => Array.from(v || [], Number)),
      purity: (query.purity || []).map(Number),
      pairs: query.pairs || [],
      amplitudes: new Map(amplitudeRows(amplitudes || {}, numQubits)
        .map((row) => [row.index, [row.re, row.im]])),
      probs: (query.probsTop ?? query.probs_top ?? []),
      gate: null,
      collapsing: false,
    };
  }

  /// Amplitudes for the Q-sphere and the bars: the stored state vector when the
  /// run kept one, otherwise the sparse `top` list of the last recorded frame.
  /// A run with neither draws neither, and says so through the empty state of
  /// the components themselves.
  stateAmplitudes() {
    const numQubits = this.numQubits();
    if (this.stateOutput && this.stateOutput.amplitudes) {
      return { amplitudes: this.stateOutput.amplitudes, numQubits };
    }
    const last = this.frames[this.frames.length - 1];
    return { top: (last && last.top) || [], numQubits };
  }

  /// Which density matrix this run can show: the full ρ from a stored state
  /// vector up to `MAX_DENSITY_QUBITS`, otherwise the 4×4 of the strongest
  /// recorded pair (§13.6).
  densityMatrix(numQubits, amplitudes, pairs) {
    if (numQubits <= MAX_DENSITY_QUBITS && amplitudes.amplitudes) {
      const { dim, rho } = densityFromAmplitudes(amplitudes.amplitudes, numQubits);
      if (dim) return { scope: 'full', matrix: { dim, rho } };
    }
    const strongest = (pairs || []).slice()
      .sort((x, y) => (Number(y.mutualInformation ?? y.mutual_information) || 0)
        - (Number(x.mutualInformation ?? x.mutual_information) || 0))[0];
    if (!strongest || !strongest.rho) return null;
    const qubits = Array.from(strongest.qubits || [], Number);
    return {
      scope: 'pair',
      a: `q${qubits[0]}`,
      b: `q${qubits[1]}`,
      matrix: { dim: 4, rho: strongest.rho, labels: ['00', '01', '10', '11'] },
    };
  }

  // -- Histogram -------------------------------------------------------------

  renderHistogram(panel) {
    const shots = Number(this.run.metrics?.shots) || 0;
    if (!this.counts) {
      panel.innerHTML = `<div class="section-card">${sectionHead('bar-chart', T('result.tab_histogram'))}
        <tf-alert tone="info" message="${escapeAttr(T('result.histogram_none'))}"></tf-alert></div>`;
      return;
    }
    const measured = { id: 'measured', label: T('result.series_measured'), tone: 'measured', counts: this.counts, shots };
    const series = [measured];
    if (this.ideal) series.push({ id: 'ideal', label: T('result.series_ideal'), tone: 'ideal', probabilities: this.ideal });
    const measuredMap = seriesProbabilities(measured);
    const idealMap = this.ideal ? seriesProbabilities({ probabilities: this.ideal }) : null;
    const tvd = idealMap ? totalVariationDistance(measuredMap, idealMap) : null;
    const fidelity = idealMap ? hellingerFidelity(measuredMap, idealMap) : null;
    const top = Object.entries(this.counts).sort((a, b) => b[1] - a[1])[0] || ['', 0];
    const convergence = this.convergenceSeries();

    panel.innerHTML = `
      <div class="section-card" id="tq-histogram">
        ${sectionHead('bar-chart', T('result.tab_histogram'), `
          <tf-chip label="${escapeAttr(T('result.histogram_shots', { n: shots }))}"></tf-chip>
          <label class="tq-inline-toggle"><tf-toggle id="tq-hist-log"></tf-toggle>${escapeHtml(T('result.histogram_log'))}</label>
          <span class="tf-toolbar-spacer"></span>
          ${explainToggle('histogram', this.explain.has('histogram'))}`)}
        <tf-shot-histogram id="tq-hist" height="220"></tf-shot-histogram>
        <div class="cmp-metrics">
          <div class="cmp-metric"><div class="l">${escapeHtml(T('result.metric_tvd'))}</div><div class="v">${escapeHtml(tvd === null ? '—' : tvd.toFixed(3))}</div></div>
          <div class="cmp-metric"><div class="l">${escapeHtml(T('result.metric_fidelity'))}</div><div class="v">${escapeHtml(fidelity === null ? '—' : fidelity.toFixed(3))}</div></div>
          <div class="cmp-metric"><div class="l">${escapeHtml(T('result.metric_shots'))}</div><div class="v">${shots}</div></div>
          <div class="cmp-metric"><div class="l">${escapeHtml(T('result.metric_outcomes'))}</div><div class="v">${Object.keys(this.counts).length}</div></div>
        </div>
        ${tvd === null ? `<div class="hint">${escapeHtml(T('result.histogram_no_ideal'))}</div>` : ''}
        <div class="explain-box" id="tq-explain-histogram" ${this.explain.has('histogram') ? '' : 'hidden'}></div>
      </div>
      ${convergence ? `
        <div class="section-card">
          ${sectionHead('chart-line', T('result.convergence_title'), `<tf-chip label="${escapeAttr(T('result.convergence_iterations', { n: convergence.length }))}"></tf-chip>`)}
          <tf-line-chart id="tq-convergence" height="220"></tf-line-chart>
        </div>` : ''}`;

    const histogram = panel.querySelector('#tq-hist');
    histogram.labels = histogramLabels();
    histogram.series = series;
    panel.querySelector('#tq-hist-log').addEventListener('change', (event) => {
      if (event.detail.checked) histogram.setAttribute('log', '');
      else histogram.removeAttribute('log');
    });
    if (this.explain.has('histogram')) {
      panel.querySelector('#tq-explain-histogram').textContent = explainText(explainHistogram({
        shots, topState: top[0], topCount: Number(top[1]) || 0, tvd, fidelity,
      }));
    }
    const chart = panel.querySelector('#tq-convergence');
    if (chart) {
      chart.series = [{
        id: 'value',
        name: T('result.convergence_series'),
        tone: 'accent',
        style: 'solid',
        showInLegend: true,
        points: convergence.map((y, x) => ({ x: x + 1, y })),
      }];
    }
  }

  /// The convergence series of a variational run, as its tile recorded it. A
  /// run with no such tile simply has no chart — the series exists nowhere else.
  convergenceSeries() {
    const raw = this.run.tileJson ?? this.run.tile_json;
    if (!raw) return null;
    try {
      const tile = typeof raw === 'string' ? JSON.parse(raw) : raw;
      const series = (tile.series || []).map(Number).filter(Number.isFinite);
      return String(tile.kind) === 'convergence' && series.length ? series : null;
    } catch {
      return null;
    }
  }

  // -- Porównanie ------------------------------------------------------------

  renderCompare(panel) {
    const candidates = (this.screen.runs || [])
      .filter((run) => run.status === 'succeeded' && !this.compareIds.includes(run.runId));
    panel.innerHTML = `
      <div class="section-card" id="tq-compare">
        ${sectionHead('layers', T('result.tab_compare'), `
          <tf-chip label="${escapeAttr(T('result.compare_count', { n: this.compareIds.length, max: COMPARE_MAX }))}"></tf-chip>
          <span class="tf-toolbar-spacer"></span>
          <tf-button variant="primary" size="sm" icon="refresh" data-act="compare-run">${escapeHtml(T('result.compare_run'))}</tf-button>`)}
        <div class="cmp-chips">
          ${this.compareIds.map((id, index) => `
            <span class="run-chip tone-${compareTone(index)}">
              <i></i><span class="mono">${escapeHtml(shortId(id))}</span>
              ${id === this.runId ? `<small>${escapeHtml(T('result.compare_this'))}</small>`
                : `<tf-button variant="ghost" size="sm" icon="x" data-drop="${escapeAttr(id)}"></tf-button>`}
            </span>`).join('')}
          ${candidates.length && this.compareIds.length < COMPARE_MAX ? `
            <tf-select id="tq-compare-add" value="">
              <option value="">${escapeHtml(T('result.compare_add'))}</option>
              ${candidates.slice(0, 40).map((run) => `<option value="${escapeAttr(run.runId)}">${escapeHtml(`${shortId(run.runId)} · ${resultTitle(run)}`)}</option>`).join('')}
            </tf-select>` : ''}
        </div>
        <div id="tq-compare-body">
          ${this.compareError ? `<tf-alert tone="danger" message="${escapeAttr(this.compareError)}"></tf-alert>`
            : (this.comparison ? '' : `<div class="hint">${escapeHtml(T('result.compare_hint'))}</div>`)}
        </div>
      </div>`;
    // Arriving here with a selection already made ("Porównaj zaznaczone" in the
    // gallery) is a request, not a suggestion: the comparison runs by itself.
    if (this.compareIds.length >= 2 && !this.comparison && !this.compareError && !this.comparePending) {
      this.runComparison();
    }
    panel.querySelector('#tq-compare-add')?.addEventListener('change', (event) => {
      const runId = event.detail?.value;
      if (!runId || this.compareIds.includes(runId)) return;
      this.compareIds = [...this.compareIds, runId].slice(0, COMPARE_MAX);
      this.comparison = null;
      this.compareError = '';
      this.renderCompare(panel);
    });
    if (this.comparison) this.paintComparison(panel.querySelector('#tq-compare-body'));
  }

  async runComparison() {
    const panel = this.host.querySelector('#tq-compare');
    if (!panel || this.comparePending) return;
    this.comparePending = true;
    const body = panel.querySelector('#tq-compare-body');
    body.innerHTML = `<div class="tq-loading">${escapeHtml(T('result.compare_loading'))}</div>`;
    try {
      this.comparison = await this.screen.tq('tentaQuantRunCompareRequest', { runIds: this.compareIds });
      this.compareError = '';
    } catch (e) {
      this.comparison = null;
      this.compareError = errMessage(e);
    }
    this.comparePending = false;
    if (this.disposed || this.tab !== 'compare') return;
    this.renderCompare(this.host.querySelector('#tq-result-panel'));
  }

  paintComparison(host) {
    const data = this.comparison;
    const bitstrings = data.bitstrings || [];
    const runs = data.runs || [];
    host.innerHTML = `
      <tf-shot-histogram id="tq-compare-hist" height="220" whiskers="off" max-bars="16"></tf-shot-histogram>
      <div class="table-scroll"><tf-table id="tq-compare-table">
        <tf-column key="run" label="${escapeAttr(T('result.col_run'))}" renderer="html" fill></tf-column>
        <tf-column key="target" label="${escapeAttr(T('runs.col_target'))}" renderer="text" nowrap></tf-column>
        <tf-column key="backend" label="${escapeAttr(T('run.field_backend'))}" renderer="text" nowrap></tf-column>
        <tf-column key="shots" label="${escapeAttr(T('runs.col_shots'))}" renderer="text" nowrap></tf-column>
        <tf-column key="duration" label="${escapeAttr(T('runs.col_time'))}" renderer="text" nowrap></tf-column>
        <tf-column key="tvd" label="${escapeAttr(T('result.metric_tvd'))}" renderer="text" nowrap></tf-column>
        <tf-column key="fidelity" label="${escapeAttr(T('result.metric_fidelity'))}" renderer="text" nowrap></tf-column>
      </tf-table></div>
      <div class="cmp-diff">
        <span class="l">${escapeHtml(T('result.compare_diff'))}</span>
        ${bitstrings.map((bitstring, index) => `
          <span class="d"><b class="mono">${escapeHtml(bitstring)}</b>${escapeHtml((Number((data.diff || [])[index]) || 0).toFixed(3))}</span>`).join('')}
      </div>`;
    const histogram = host.querySelector('#tq-compare-hist');
    histogram.labels = histogramLabels();
    histogram.series = runs.map((run, index) => ({
      id: run.runId,
      label: `${shortId(run.runId)} · ${run.label}`,
      tone: compareTone(index),
      probabilities: Object.fromEntries(bitstrings.map((key, i) => [key, Number(run.probabilities[i]) || 0])),
    }));
    host.querySelector('#tq-compare-table').rows = runs.map((run, index) => ({
      run: `<div class="tf-table__cell-title"><i class="cmp-dot tone-${compareTone(index)}"></i>${escapeHtml(run.label)}</div>`
        + `<div class="tf-table__cell-sub tf-table__cell-sub--mono">${escapeHtml(shortId(run.runId))}</div>`,
      target: run.target,
      backend: run.backend || '—',
      shots: String(Number(run.shots) || 0),
      duration: `${Number(run.durationMs ?? run.duration_ms) || 0} ms`,
      tvd: formatMetric(run.totalVariationDistance ?? run.total_variation_distance),
      fidelity: formatMetric(run.hellingerFidelity ?? run.hellinger_fidelity),
    }));
  }

  // -- Dane i eksport --------------------------------------------------------

  renderData(panel) {
    const note = methodNote(this.run, {
      projectName: this.screen.project?.name || '',
      counts: this.storedCounts,
    });
    const bib = citationBib(this.run);
    panel.innerHTML = `
      <div class="section-card" id="tq-data">
        ${sectionHead('download', T('result.tab_data'), `
          <span class="tf-toolbar-spacer"></span>
          <tf-button variant="primary" size="sm" icon="download" data-act="export">${escapeHtml(T('result.export_download'))}</tf-button>`)}
        <div class="export-grid">
          <div class="res-panel">
            <div class="rp-title">${escapeHtml(T('result.export_package'))}</div>
            <div class="tq-part-list">
              ${EXPORT_PARTS.map((part) => `
                <label class="tq-part">
                  <tf-checkbox data-part="${part}" ${this.exportParts.has(part) ? 'checked' : ''}></tf-checkbox>
                  <span class="pn">${escapeHtml(T('result.part_' + part))}<small>${escapeHtml(T('result.part_' + part + '_sub'))}</small></span>
                </label>`).join('')}
            </div>
            <div class="hint">${escapeHtml(T('result.export_hint'))}</div>
            <div id="tq-export-result">${this.exportResult ? exportResultHtml(this.exportResult) : ''}</div>
          </div>
          <div>
            <div class="res-panel">
              <div class="rp-title">${escapeHtml(T('result.method_title'))}<span class="tf-toolbar-spacer"></span>
                <tf-button variant="ghost" size="sm" icon="copy" data-copy="method">${escapeHtml(T('result.copy'))}</tf-button></div>
              <pre class="tq-file-preview" data-method>${escapeHtml(note)}</pre>
            </div>
            <div class="res-panel">
              <div class="rp-title">${escapeHtml(T('result.citation_title'))}<span class="tf-toolbar-spacer"></span>
                <tf-button variant="ghost" size="sm" icon="copy" data-copy="bib">${escapeHtml(T('result.copy_bibtex'))}</tf-button></div>
              <pre class="tq-file-preview" data-bib>${escapeHtml(bib)}</pre>
            </div>
          </div>
        </div>
      </div>`;
    for (const box of panel.querySelectorAll('tf-checkbox[data-part]')) {
      box.addEventListener('change', (event) => {
        if (event.detail.checked) this.exportParts.add(box.dataset.part);
        else this.exportParts.delete(box.dataset.part);
      });
    }
  }

  async exportPackage() {
    const box = this.host.querySelector('#tq-export-result');
    if (box) box.innerHTML = `<div class="tq-loading">${escapeHtml(T('result.export_running'))}</div>`;
    try {
      // An empty list means "every part the run has"; sending all six is the
      // same request with the checklist intact, so the two paths never differ.
      const parts = this.exportParts.size === EXPORT_PARTS.length ? [] : Array.from(this.exportParts);
      this.exportResult = await this.screen.tq('tentaQuantRunExportRequest', { runId: this.runId, parts });
    } catch (e) {
      this.exportResult = null;
      if (box) box.innerHTML = `<tf-alert tone="danger" message="${escapeAttr(errMessage(e))}"></tf-alert>`;
      return;
    }
    if (this.disposed || this.tab !== 'data') return;
    if (box) box.innerHTML = exportResultHtml(this.exportResult);
    downloadUrl(this.exportResult.url, `run-${shortId(this.runId)}-package.zip`);
  }

  // -- shared ----------------------------------------------------------------

  numQubits() {
    const stated = Number(this.run.metrics?.qubits) || 0;
    if (stated) return stated;
    const frame = this.frames[this.frames.length - 1];
    return (frame && (frame.bloch || []).length) || 0;
  }

  /// The "Wyjaśnij" switches are `tf-toggle`s, which answer the pointer AND the
  /// keyboard with one `change` — so the toggle is followed there and not on a
  /// raw click, which a key press never produces.
  onChange(event) {
    if (this.disposed || !this.run) return;
    const toggle = event.target.closest('[data-explain]');
    if (!toggle) return;
    const id = toggle.dataset.explain;
    if (event.detail && event.detail.checked === false) this.explain.delete(id);
    else this.explain.add(id);
    this.renderTab();
  }

  onClick(event) {
    if (this.disposed || !this.run) return;
    const drop = event.target.closest('[data-drop]');
    if (drop) {
      this.compareIds = this.compareIds.filter((id) => id !== drop.dataset.drop);
      this.comparison = null;
      this.compareError = '';
      this.renderCompare(this.host.querySelector('#tq-result-panel'));
      return;
    }
    const copy = event.target.closest('[data-copy]');
    if (copy) { this.copy(copy.dataset.copy); return; }
    const button = event.target.closest('[data-act]');
    if (!button) return;
    const act = button.dataset.act;
    if (act === 'close') { this.screen.closeRunResult(); return; }
    if (act === 'pin') { setRunPinned(this.screen, this.run, !this.run.pinnedAt, { projectId: this.screen.projectId }); return; }
    if (act === 'notebook') { this.screen.openNotebookForRun(this.run); return; }
    if (act === 'tab-compare') { this.selectTab('compare'); return; }
    if (act === 'tab-data') { this.selectTab('data'); return; }
    if (act === 'compare-run') { this.runComparison(); return; }
    if (act === 'export') this.exportPackage();
  }

  async copy(what) {
    const source = this.host.querySelector(what === 'bib' ? '[data-bib]' : '[data-method]');
    if (!source || !navigator.clipboard) return;
    try {
      await navigator.clipboard.writeText(source.textContent);
      toast(T('result.copied'), 'success');
    } catch (e) {
      toast(`${T('result.copy_failed')}: ${errMessage(e)}`, 'error');
    }
  }
}

// ---------------------------------------------------------------------------
// Markup helpers
// ---------------------------------------------------------------------------

function sectionHead(icon, title, extra = '') {
  return `<div class="section-card-head">
    <div class="title">${sprite(icon)}${escapeHtml(title)}</div>
    ${extra}
  </div>`;
}

function explainToggle(id, on) {
  return `<label class="tq-inline-toggle"><tf-toggle data-explain="${id}" ${on ? 'checked' : ''}></tf-toggle>${escapeHtml(T('result.explain'))}</label>`;
}

function kvHtml(rows) {
  return `<div class="kv">${rows
    .map(([k, v]) => `<span class="k">${escapeHtml(k)}</span><span class="v">${escapeHtml(String(v))}</span>`)
    .join('')}</div>`;
}

function exportResultHtml(result) {
  return `<div class="tq-export-result">
    <div class="hint">${escapeHtml(T('result.export_ready', { size: formatBytes(Number(result.sizeBytes) || 0) }))}</div>
    <ul class="tq-export-entries">${(result.entries || []).map((entry) => `<li class="mono">${escapeHtml(entry)}</li>`).join('')}</ul>
  </div>`;
}

const formatMetric = (value) => (value === null || value === undefined ? '—' : Number(value).toFixed(3));

/// The amplitude groups of a frame, in the shape `tf-state-bars` and
/// `tf-qsphere` read (`amplitudeRows`' sparse `top` list).
function amplitudeGroups(frame) {
  return Array.from(frame.amplitudes || [], ([index, amplitude]) => ({ index, amplitude, partners: [] }));
}

function reducedMotion() {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return false;
  return window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

// ---------------------------------------------------------------------------
// Component label sets
// ---------------------------------------------------------------------------
//
// The six result components ship English fallbacks only, exactly like
// `tf-bloch-sphere`; the screen hands them its own translations. The key lists
// mirror each component's DEFAULT_LABELS, so a key added there without one here
// falls back to English — which is why they are enumerated and not derived.

const labelsFrom = (namespace, keys) => Object.fromEntries(keys.map((k) => [k, T(`${namespace}.${k}`)]));

export const barsLabels = () => labelsFrom('bars', ['amplitudes', 'phase', 'empty', 'more']);
export const histogramLabels = () => labelsFrom('hist', ['histogram', 'empty', 'shots', 'probability', 'interval', 'more']);
export const qsphereLabels = () => labelsFrom('qsphere', ['qsphere', 'empty', 'north', 'south', 'probability', 'phase', 'more']);
export const densityLabels = () => labelsFrom('density', ['density', 'empty', 'real', 'imaginary']);
export const entanglementLabels = () => labelsFrom('entgraph', ['entanglement', 'empty', 'mutual', 'concurrence', 'bits']);
export const timelineLabels = () => labelsFrom('timeline', ['timeline', 'play', 'pause', 'previous', 'next', 'time', 'before', 'empty', 'step']);
