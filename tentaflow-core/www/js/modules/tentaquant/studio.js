// ===== File: modules/tentaquant/studio.js — Q07, the circuit Studio =====
//
// A full-screen `tf-quantum-circuit` with the live T0 state beside it: every
// edit reparses, resimulates and repaints the panel, because the whole point of
// the screen (plan §13.1 item 5) is that the state answers the gate you just
// dropped.
//
// The circuit is held as OpenQASM 3 — the canonical artefact — and the IR is
// derived by the wasm parser. The two directions are kept in step in ONE place
// (`setCircuit` / `setSource`), so the text tab and the grid can never drift.
//
// The evolution animation follows plan §13.6: the playhead crosses a gate in
// `MS_PER_GATE`, the amplitudes in between come from `stepFraction(t)`, and a
// measurement — which has no fractional form — is drawn as the collapse of one
// branch into the other. `prefers-reduced-motion` turns the interpolation off
// and leaves gate-by-gate stepping AT THE SAME PACE: the clock keeps running on
// real time, only the drawing stops gliding.
//
// Only the targets that exist are offered: T0 in this browser. T1–T4 arrive
// with their backends and are not rendered as disabled promises.

import { escapeHtml, escapeAttr, formatBytes, toast } from '/js/utils.js';
import {
  T, sprite, errMessage, canEditProject, circuitLabels, blochLabels, mimeLabels,
  editorLabels, viewportAllowsEditing, watchEditViewport,
} from '/js/modules/tentaquant/format.js';
import { DEFAULT_CIRCUIT_SOURCE } from '/js/modules/tentaquant/cells.js';
import { uploadFile } from '/js/modules/tentaquant/files.js';
import { saveCircuitToNotebook, openNotebookPicker } from '/js/modules/tentaquant/notebook.js';
import {
  MAX_LIVE_STATE_QUBITS, MS_PER_GATE, T0_MAX_QUBITS, advance, appliedColumns,
  blochFromAmplitudes, canSample, cellOfOp, collapseFrame, countsBundle, gateDetails, gridOf,
  isCollapsing, mergeCounts, opAtColumn, playheadAt, pyFileName, qasmFileName, renderFraction,
  resourceSummary, shotBatchSize, shotPlan, stateBundle, stepSummary, svgFileName,
  totalShots,
} from '/js/modules/tentaquant/quantum-view.js';
import '/js/components/tf-quantum-circuit.js';
import '/js/components/tf-bloch-sphere.js';
import '/js/components/tf-mime-output.js';
import '/js/components/tf-code-editor.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-input.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-select.js';
import '/js/components/tf-slider.js';

const MAX_SHOTS = 100000;

function alertHtml(tone, title, message) {
  return `<tf-alert tone="${tone}" title="${escapeAttr(title)}" message="${escapeAttr(message)}"></tf-alert>`;
}

export function studioState(patch = {}) {
  return {
    name: '',
    source: DEFAULT_CIRCUIT_SOURCE,
    step: 0,
    mode: 'edit',
    shots: 1024,
    notebookId: null,
    cellId: null,
    ...patch,
  };
}

export function drawStudio(screen, host) {
  const view = new StudioView(screen, host);
  screen.projectViewDispose = () => view.dispose();
  view.mount();
  return view;
}

class StudioView {
  constructor(screen, host) {
    this.screen = screen;
    this.host = host;
    this.state = screen.studio;
    // Two different questions: may this user write the project at all, and is
    // this viewport one the grid can be edited on (plan §13.5).
    this.writable = canEditProject(screen.project) && !screen.project.archivedAt;
    this.editable = this.writable && viewportAllowsEditing();
    this.unwatchViewport = null;
    this.selection = [];
    this.circuit = null;
    this.grid = gridOf(null);
    this.sim = null;
    this.simError = '';
    this.engineMissing = false;
    this.failure = '';
    this.statusHtml = '';
    this.parseErrors = [];
    this.isClifford = false;
    this.frame = { index: 0, t: 0 };
    this.collapse = null;
    this.raf = 0;
    this.lastTs = null;
    this.playing = false;
    this.counts = null;
    this.shotsPending = 0;
    // Bumped whenever the circuit on the grid changes, so the batches of a run
    // that is still in flight cannot land under the circuit that replaced it.
    this.runGeneration = 0;
    this.disposed = false;
  }

  // -------------------------------------------------------------------------
  // Lifecycle
  // -------------------------------------------------------------------------

  mount() {
    this.host.innerHTML = `
      <div class="tf-toolbar tq-studio-toolbar">
        <span class="tq-toolbar-title">${sprite('layers')}${escapeHtml(T('studio.title'))}</span>
        <tf-input id="tq-studio-name" placeholder="${escapeAttr(T('studio.name_placeholder'))}"
          value="${escapeAttr(this.state.name)}" ${this.editable ? '' : 'disabled'}></tf-input>
        <span id="tq-studio-clifford"></span>
        <span id="tq-studio-preview"></span>
        <span class="tf-toolbar-spacer"></span>
        <tf-button variant="secondary" size="sm" icon="save" data-act="save-qasm" ${this.editable ? '' : 'disabled'}>${escapeHtml(T('studio.save_file'))}</tf-button>
        <tf-button variant="secondary" size="sm" icon="file-text" data-act="save-cell" ${this.editable ? '' : 'disabled'}>${escapeHtml(T('studio.save_cell'))}</tf-button>
        <tf-button variant="ghost" size="sm" icon="code" data-act="export-qasm">${escapeHtml(T('studio.export_qasm'))}</tf-button>
        <tf-button variant="ghost" size="sm" icon="download" data-act="export-qiskit">${escapeHtml(T('studio.export_qiskit'))}</tf-button>
        <tf-button variant="ghost" size="sm" icon="download" data-act="export-svg">${escapeHtml(T('studio.export_svg'))}</tf-button>
      </div>
      <div id="tq-studio-status"></div>
      <div class="studio-layout">
        <div class="studio-main">
          <div class="section-card">
            <div class="section-card-head">
              <div class="title">${sprite('atom')}${escapeHtml(T('studio.circuit_title'))}</div>
              <div class="actions">
                <tf-segmented id="tq-studio-mode" value="${escapeAttr(this.state.mode)}">
                  <option value="edit" icon="edit">${escapeHtml(T('studio.mode_edit'))}</option>
                  <option value="step" icon="play">${escapeHtml(T('studio.mode_step'))}</option>
                  <option value="text" icon="code">${escapeHtml(T('studio.mode_text'))}</option>
                </tf-segmented>
                <tf-button variant="ghost" size="sm" icon="rotate" data-act="undo" title="${escapeAttr(T('studio.undo'))}"></tf-button>
                <tf-button variant="ghost" size="sm" icon="refresh" data-act="redo" title="${escapeAttr(T('studio.redo'))}"></tf-button>
                <tf-button variant="ghost" size="sm" icon="trash" data-act="clear" title="${escapeAttr(T('studio.clear'))}"></tf-button>
              </div>
            </div>
            <div class="tq-circuit-wrap">
              <tf-quantum-circuit id="tq-studio-circuit" ${this.editable ? '' : 'readonly'}
                aria-label="${escapeAttr(T('studio.circuit_title'))}"></tf-quantum-circuit>
            </div>
            <div class="step-row" id="tq-studio-steps" hidden>
              <span class="tq-step-label">${escapeHtml(T('studio.step'))}</span>
              <tf-slider id="tq-studio-step" min="0" max="0" value="0" step="1" aria-label="${escapeAttr(T('studio.step'))}"></tf-slider>
              <span class="sl-val" id="tq-studio-step-value"></span>
            </div>
            <div class="tq-transport" id="tq-studio-transport" hidden>
              <tf-button variant="ghost" size="sm" icon="chevron-left" data-act="first" title="${escapeAttr(T('studio.to_start'))}"></tf-button>
              <tf-button variant="ghost" size="sm" icon="chevron-left" data-act="prev" title="${escapeAttr(T('studio.prev_gate'))}"></tf-button>
              <tf-button variant="primary" size="sm" icon="play" data-act="play" label="${escapeAttr(T('studio.play'))}"></tf-button>
              <tf-button variant="ghost" size="sm" icon="chevron-right" data-act="next" title="${escapeAttr(T('studio.next_gate'))}"></tf-button>
              <span class="hint">${escapeHtml(T('studio.step_hint'))}</span>
            </div>
          </div>
          <div class="section-card" id="tq-studio-text" hidden>
            <div class="section-card-head">
              <div class="title">${sprite('code')}${escapeHtml(T('studio.text_title'))}</div>
              <span class="hint">${escapeHtml(T('studio.text_hint'))}</span>
              <div class="actions">
                <tf-button variant="ghost" size="sm" icon="copy" data-act="copy">${escapeHtml(T('studio.copy'))}</tf-button>
                <tf-button variant="secondary" size="sm" icon="check" data-act="apply" ${this.editable ? '' : 'disabled'}>${escapeHtml(T('studio.apply'))}</tf-button>
              </div>
            </div>
            <tf-code-editor id="tq-studio-source" language="plain" aria-label="${escapeAttr(T('studio.text_title'))}"
              ${this.editable ? '' : 'readonly'}></tf-code-editor>
            <div class="tq-parse-errors" id="tq-studio-errors" hidden></div>
          </div>
        </div>
        <div class="state-panel">
          <div class="section-card">
            <div class="section-card-head">
              <div class="title">${sprite('atom')}<span id="tq-studio-state-title">${escapeHtml(T('studio.state_title'))}</span></div>
              <div class="actions"><span class="tier t0">${escapeHtml(T('studio.tier_browser'))}</span></div>
            </div>
            <div class="bloch-row" id="tq-studio-bloch"></div>
            <tf-mime-output id="tq-studio-amps" max-rows="8"></tf-mime-output>
          </div>
          <div class="section-card">
            <div class="section-card-head"><div class="title">${sprite('play')}${escapeHtml(T('studio.run_title'))}</div></div>
            <tf-select id="tq-studio-target" label="${escapeAttr(T('studio.target_label'))}" value="browser">
              <option value="browser">${escapeHtml(T('studio.target_browser', { q: T0_MAX_QUBITS }))}</option>
            </tf-select>
            <tf-input id="tq-studio-shots" type="number" min="1" max="${MAX_SHOTS}" label="${escapeAttr(T('studio.shots_label'))}" value="${escapeAttr(String(this.state.shots))}"></tf-input>
            <tf-button variant="primary" icon="play" data-act="run" full-width id="tq-studio-run"></tf-button>
            <div class="hint">${escapeHtml(T('studio.run_hint'))}</div>
            <tf-mime-output id="tq-studio-counts"></tf-mime-output>
          </div>
          <div class="section-card">
            <div class="section-card-head">
              <div class="title">${sprite('chip')}${escapeHtml(T('studio.gate_title'))}</div>
              <div class="actions"><span id="tq-studio-gate-chip"></span></div>
            </div>
            <div id="tq-studio-gate"></div>
          </div>
          <div class="section-card">
            <div class="section-card-head"><div class="title">${sprite('grid-rows')}${escapeHtml(T('studio.resources_title'))}</div></div>
            <div class="kv" id="tq-studio-resources"></div>
          </div>
        </div>
      </div>`;

    this.el = {
      status: this.host.querySelector('#tq-studio-status'),
      circuit: this.host.querySelector('#tq-studio-circuit'),
      steps: this.host.querySelector('#tq-studio-steps'),
      slider: this.host.querySelector('#tq-studio-step'),
      stepValue: this.host.querySelector('#tq-studio-step-value'),
      transport: this.host.querySelector('#tq-studio-transport'),
      text: this.host.querySelector('#tq-studio-text'),
      source: this.host.querySelector('#tq-studio-source'),
      errors: this.host.querySelector('#tq-studio-errors'),
      bloch: this.host.querySelector('#tq-studio-bloch'),
      amps: this.host.querySelector('#tq-studio-amps'),
      counts: this.host.querySelector('#tq-studio-counts'),
      resources: this.host.querySelector('#tq-studio-resources'),
      clifford: this.host.querySelector('#tq-studio-clifford'),
      preview: this.host.querySelector('#tq-studio-preview'),
      gate: this.host.querySelector('#tq-studio-gate'),
      gateChip: this.host.querySelector('#tq-studio-gate-chip'),
      stateTitle: this.host.querySelector('#tq-studio-state-title'),
      play: this.host.querySelector('[data-act="play"]'),
      run: this.host.querySelector('#tq-studio-run'),
    };
    this.el.circuit.labels = circuitLabels();
    this.el.amps.labels = mimeLabels();
    this.el.counts.labels = mimeLabels();
    this.el.source.labels = editorLabels();
    this.el.source.value = this.state.source;
    this.paintRunButton();
    this.paintPreview();
    this.paintGate();
    this.wire();
    this.applyMode();
    this.unwatchViewport = watchEditViewport((wide) => this.setEditable(this.writable && wide));
    this.init();
  }

  dispose() {
    this.disposed = true;
    this.stop();
    this.freeSimulator();
    if (this.unwatchViewport) this.unwatchViewport();
    this.unwatchViewport = null;
  }

  freeSimulator() {
    if (this.sim) {
      this.sim.free();
      this.sim = null;
    }
  }

  // -------------------------------------------------------------------------
  // Events
  // -------------------------------------------------------------------------

  wire() {
    const host = this.host;
    this.el.circuit.addEventListener('change', (e) => this.setCircuit(e.detail.circuit));
    this.el.circuit.addEventListener('select', (e) => {
      this.selection = (e.detail.indices || []).map(Number);
      this.paintGate();
    });
    this.el.circuit.addEventListener('column-click', (e) => {
      const op = opAtColumn(this.grid, Number(e.detail.column));
      if (op !== null) this.seek(op + 1);
    });
    this.el.slider.addEventListener('input', (e) => this.seek(Number(e.detail.value)));
    host.querySelector('#tq-studio-mode').addEventListener('change', (e) => {
      this.state.mode = e.detail.value;
      this.applyMode();
    });
    // tf-input owns its value; the event only says that it changed.
    const name = host.querySelector('#tq-studio-name');
    name.addEventListener('change', () => { this.state.name = name.value ?? ''; });
    const shots = host.querySelector('#tq-studio-shots');
    shots.addEventListener('change', () => {
      const wanted = Math.max(1, Math.min(MAX_SHOTS, Number(shots.value) || 1));
      this.state.shots = wanted;
      shots.value = String(wanted);
      this.paintRunButton();
    });
    host.addEventListener('click', (event) => {
      const button = event.target.closest('[data-act]');
      if (!button || !host.contains(button)) return;
      const action = button.dataset.act;
      if (action === 'undo') this.el.circuit.undo();
      else if (action === 'redo') this.el.circuit.redo();
      else if (action === 'clear') this.clear();
      else if (action === 'first') this.seek(0);
      else if (action === 'prev') this.seek(this.state.step - 1);
      else if (action === 'next') this.seek(this.state.step + 1);
      else if (action === 'play') this.togglePlay();
      else if (action === 'apply') this.setSource(this.el.source.value);
      else if (action === 'copy') this.copySource();
      else if (action === 'run') this.run();
      else if (action === 'export-qasm') this.download(qasmFileName(this.circuitName()), this.state.source, 'text/plain');
      else if (action === 'export-qiskit') this.exportQiskit();
      else if (action === 'export-svg') this.download(svgFileName(this.circuitName()), this.el.circuit.toSvg(), 'image/svg+xml');
      else if (action === 'gate-duplicate') this.el.circuit.duplicateOp(this.selection[0]);
      else if (action === 'gate-delete') this.el.circuit.deleteOps(this.selection);
      else if (action === 'save-qasm') this.saveAsFile();
      else if (action === 'save-cell') this.saveToNotebook();
    });
  }

  applyMode() {
    const mode = this.state.mode;
    this.el.text.hidden = mode !== 'text';
    this.el.steps.hidden = mode !== 'step';
    this.el.transport.hidden = mode !== 'step';
    if (mode !== 'step') {
      this.stop();
      // Outside the step mode the panel shows the WHOLE circuit: that is what
      // "the state answers the gate you just dropped" means while editing.
      this.seek((this.circuit?.ops || []).length);
    }
    this.el.circuit.toggleAttribute('readonly', !this.editable || mode !== 'edit');
    if (mode === 'text') this.el.source.value = this.state.source;
    this.paintStep();
  }

  circuitName() {
    return this.state.name || this.screen.project?.name || T('studio.default_name');
  }

  // -------------------------------------------------------------------------
  // Circuit ⇄ source
  // -------------------------------------------------------------------------

  async init() {
    const { available } = await import('/js/quantum/index.js');
    if (this.disposed) return;
    if (!await available()) {
      this.engineMissing = true;
      this.paintStatus();
      return;
    }
    await this.setSource(this.state.source);
  }

  /// Parses OpenQASM 3 into the IR. A rejected program is the normal state
  /// while somebody is typing, so the errors are listed and the last good
  /// circuit stays on the grid.
  async setSource(source) {
    this.state.source = source;
    this.failure = '';
    try {
      const { parse } = await import('/js/quantum/index.js');
      const result = await parse(source);
      if (this.disposed) return;
      if (result.status !== 'parsed') {
        this.parseErrors = result.errors || [];
        // A program that never parsed has no IR behind it: the two panel cards
        // say so instead of standing there as empty boxes.
        this.isClifford = false;
        this.paintErrors();
        this.paintResources();
        this.paintClifford();
        return;
      }
      this.parseErrors = [];
      this.isClifford = Boolean(result.isClifford);
      this.paintErrors();
      this.adopt(result.circuit);
    } catch (e) {
      this.fail(e);
    }
  }

  /// Takes a circuit the GRID produced: the text has to be regenerated from it,
  /// which is what keeps the two tabs one artefact.
  async setCircuit(circuit) {
    this.failure = '';
    try {
      const { toQasm3, isClifford } = await import('/js/quantum/index.js');
      const source = await toQasm3(circuit);
      if (this.disposed) return;
      this.state.source = source;
      this.el.source.value = source;
      this.isClifford = await isClifford(circuit);
      if (this.disposed) return;
      this.parseErrors = [];
      this.paintErrors();
      this.adopt(circuit, { fromGrid: true });
    } catch (e) {
      this.fail(e);
    }
  }

  async adopt(circuit, { fromGrid = false } = {}) {
    this.circuit = circuit;
    this.grid = gridOf(circuit);
    // A circuit that came FROM the grid is already drawn there; assigning it
    // back would reset the component's undo stack on every edit.
    if (!fromGrid) this.el.circuit.circuit = circuit;
    // The grid owns the selection — an edit made from the panel changes it
    // without a `select` event, so the card reads it back rather than guessing.
    this.selection = this.el.circuit.selection;
    this.paintGate();
    const total = (circuit.ops || []).length;
    // Stepping keeps the position the user is looking at; every other mode
    // shows the state of the whole circuit.
    this.state.step = this.state.mode === 'step' ? Math.min(this.state.step, total) : total;
    await this.rebuildSimulator();
  }

  async rebuildSimulator() {
    this.stop();
    this.freeSimulator();
    this.collapse = null;
    // The shots on screen belong to the circuit that was run, not to the one
    // that just replaced it — a stale histogram beside a live state would read
    // as this circuit's result. A run still drawing batches is disowned by the
    // same move, or its next batch would repaint the bars it just lost.
    this.runGeneration += 1;
    this.counts = null;
    this.el.counts.bundle = null;
    try {
      const { createSimulator } = await import('/js/quantum/index.js');
      const sim = await createSimulator(this.circuit, { maxQubits: T0_MAX_QUBITS });
      if (this.disposed) { sim.free(); return; }
      this.sim = sim;
      this.simError = '';
    } catch (e) {
      this.sim = null;
      this.simError = errMessage(e);
    }
    this.seek(this.state.step);
    this.paintResources();
    this.paintClifford();
  }

  clear() {
    if (!this.circuit) return;
    this.setCircuit({ ...this.circuit, ops: [] });
  }

  fail(error) {
    this.failure = errMessage(error);
    this.paintStatus();
  }

  // -------------------------------------------------------------------------
  // Stepping and the evolution animation
  // -------------------------------------------------------------------------

  seek(step) {
    const total = (this.circuit?.ops || []).length;
    const target = Math.max(0, Math.min(Number(step) || 0, total));
    this.state.step = target;
    this.frame = { index: target, t: 0 };
    this.collapse = null;
    if (this.sim) {
      try {
        this.sim.rewind();
        for (let i = 0; i < target; i += 1) this.sim.step();
      } catch (e) {
        this.simError = errMessage(e);
      }
    }
    this.paintStep();
    this.paintState();
  }

  togglePlay() {
    if (this.playing) { this.stop(); return; }
    const total = (this.circuit?.ops || []).length;
    if (!this.sim || !total) return;
    if (this.state.step >= total) this.seek(0);
    this.playing = true;
    this.lastTs = null;
    this.el.play.setAttribute('icon', 'pause');
    this.el.play.setAttribute('label', T('studio.pause'));
    this.raf = requestAnimationFrame((ts) => this.tick(ts));
  }

  stop() {
    if (this.raf) cancelAnimationFrame(this.raf);
    this.raf = 0;
    if (!this.playing) return;
    this.playing = false;
    if (this.el?.play) {
      this.el.play.setAttribute('icon', 'play');
      this.el.play.setAttribute('label', T('studio.play'));
    }
  }

  reducedMotion() {
    return Boolean(window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches);
  }

  /// The fraction to draw the pending gate at — 0 under reduced motion, where
  /// the clock still runs but the playhead only ever sits on a boundary.
  renderT() {
    return renderFraction(this.frame, this.reducedMotion());
  }

  tick(timestamp) {
    if (!this.playing || this.disposed) return;
    // The first frame of a playback has no previous timestamp, and a
    // timestamp of 0 is a legal one — hence the explicit null.
    const delta = this.lastTs === null ? 0 : timestamp - this.lastTs;
    this.lastTs = timestamp;
    const total = (this.circuit?.ops || []).length;
    const next = advance(this.frame, delta, { stepCount: total, msPerGate: MS_PER_GATE });
    for (let i = 0; i < next.apply; i += 1) this.applyPending();
    const landed = next.apply > 0 || next.done;
    this.frame = { index: next.index, t: next.t };
    this.state.step = next.index;
    // Under reduced motion nothing is drawn between two gates, so the loop only
    // keeps time there — repainting would recompute the same state 60x a second.
    if (landed || !this.reducedMotion()) {
      this.paintStep();
      this.paintState();
    }
    if (next.done) { this.stop(); return; }
    this.raf = requestAnimationFrame((ts) => this.tick(ts));
  }

  /// Consumes the pending operation. A collapsing one was already applied by
  /// the frame that started drawing it, so the simulator is only stepped when
  /// nothing has been pre-applied.
  applyPending() {
    if (this.collapse) { this.collapse = null; return; }
    try {
      this.sim.step();
    } catch (e) {
      this.simError = errMessage(e);
      this.stop();
    }
  }

  /// The amplitudes to draw for the current frame.
  frameAmplitudes() {
    if (!this.sim) return null;
    const t = this.renderT();
    const cell = cellOfOp(this.grid, this.frame.index);
    if (!cell || t <= 0) return this.sim.amplitudes();
    if (isCollapsing(cell)) {
      if (!this.collapse) {
        const before = this.sim.amplitudes();
        this.sim.step();
        this.collapse = { before, after: this.sim.amplitudes() };
      }
      return collapseFrame(this.collapse.before, this.collapse.after, t);
    }
    return this.sim.stepFraction(t);
  }

  // -------------------------------------------------------------------------
  // Painting
  // -------------------------------------------------------------------------

  paintStep() {
    const total = (this.circuit?.ops || []).length;
    this.el.slider.setAttribute('max', String(total));
    this.el.slider.setAttribute('value', String(this.state.step));
    const summary = stepSummary(this.grid, this.state.step, total, circuitLabels());
    this.el.stepValue.textContent = summary.applied
      ? T('studio.step_of_applied', { step: summary.step, total, gate: summary.applied })
      : T('studio.step_of', { step: summary.step, total });
    this.el.circuit.step = appliedColumns(this.grid, this.state.step);
    this.el.circuit.playhead = this.state.mode === 'step'
      ? playheadAt(this.grid, this.frame.index, this.renderT())
      : null;
    this.el.stateTitle.textContent = T('studio.state_after', { step: this.state.step });
  }

  paintState() {
    this.paintStatus();
    if (this.simError || !this.sim) return;
    const numQubits = this.grid.numQubits;
    // Above the width ceiling the vector never leaves the wasm heap: copying it
    // is itself the cost, and this runs once a frame while the evolution plays.
    const wide = numQubits > MAX_LIVE_STATE_QUBITS;
    let amplitudes = null;
    let bloch = null;
    try {
      amplitudes = wide ? null : this.frameAmplitudes();
      bloch = amplitudes && this.renderT() > 0
        ? blochFromAmplitudes(amplitudes, numQubits)
        : this.sim.blochVectors();
    } catch (e) {
      this.simError = errMessage(e);
      this.stop();
      this.paintStatus();
      return;
    }
    this.el.circuit.state = bloch;
    this.paintBloch(bloch);
    this.el.amps.bundle = wide
      ? { 'text/plain': T('studio.amps_wide', { q: numQubits, max: MAX_LIVE_STATE_QUBITS }) }
      : stateBundle({ amplitudes, numQubits });
  }

  paintBloch(flat) {
    const count = this.grid.numQubits;
    const row = this.el.bloch;
    while (row.children.length > count) row.lastElementChild.remove();
    const labels = blochLabels();
    for (let q = 0; q < count; q += 1) {
      let sphere = row.children[q];
      if (!sphere) {
        sphere = document.createElement('tf-bloch-sphere');
        sphere.setAttribute('size', '84');
        row.appendChild(sphere);
        sphere.labels = labels;
      }
      sphere.setAttribute('label', `q${q}`);
      sphere.vector = [flat[q * 3], flat[q * 3 + 1], flat[q * 3 + 2]];
    }
  }

  paintRunButton() {
    this.el.run.setAttribute('label', T('studio.run', { n: this.state.shots }));
  }

  paintClifford() {
    this.el.clifford.innerHTML = this.isClifford
      ? `<tf-chip status="ok" icon="check" label="${escapeAttr(T('studio.clifford'))}"></tf-chip>`
      : '';
  }

  /// Says why the grid is read-only on a narrow screen, so a preview is never
  /// mistaken for a broken editor (plan §13.5).
  paintPreview() {
    this.el.preview.innerHTML = this.writable && !this.editable
      ? `<tf-chip status="info" icon="eye" label="${escapeAttr(T('studio.preview_only'))}"></tf-chip>`
      : '';
  }

  /// The gate-properties card of Q07. Angles and operands are edited in the
  /// component's own cell popover — the card names what is selected and carries
  /// the two edits that have no place on the grid itself.
  paintGate() {
    const details = gateDetails(this.grid, this.selection, circuitLabels());
    const actions = this.editable ? `
      <div class="tq-gate-actions">
        ${details.count === 1 ? `<tf-button variant="secondary" size="sm" icon="copy" data-act="gate-duplicate">${escapeHtml(T('studio.gate_duplicate'))}</tf-button>` : ''}
        <tf-button variant="ghost" size="sm" icon="trash" data-act="gate-delete">${escapeHtml(details.count === 1
          ? T('studio.gate_delete')
          : T('studio.gate_delete_many', { n: details.count }))}</tf-button>
      </div>` : '';
    if (details.count !== 1) {
      this.el.gateChip.innerHTML = details.count
        ? `<tf-chip status="accent" label="${escapeAttr(T('studio.gate_many', { n: details.count }))}"></tf-chip>`
        : '';
      this.el.gate.innerHTML = details.count
        ? actions
        : `<div class="hint">${escapeHtml(T('studio.gate_empty'))}</div>`;
      return;
    }
    this.el.gateChip.innerHTML = `<tf-chip status="accent" label="${escapeAttr(details.label)}"></tf-chip>`;
    // A global phase sits on no wire, so its row is dropped rather than drawn
    // as a label with nothing after it.
    const rows = [
      [T('studio.gate_column'), String(details.column)],
      [T('studio.gate_qubits'), details.qubits],
      ...details.params.map((param) => [param.name, param.value]),
    ].filter(([, value]) => value);
    this.el.gate.innerHTML = `
      <div class="kv">${rows
        .map(([k, v]) => `<span class="k">${escapeHtml(k)}</span><span class="v">${escapeHtml(v)}</span>`)
        .join('')}</div>
      ${actions}
      <div class="hint">${escapeHtml(T('studio.gate_hint'))}</div>`;
  }

  /// The editing rights of a mounted view. The role cannot change under the
  /// user, but the viewport can — a phone turned sideways crosses §13.5's line.
  setEditable(next) {
    if (this.editable === next) return;
    this.editable = next;
    for (const action of ['save-qasm', 'save-cell', 'apply']) {
      this.host.querySelector(`[data-act="${action}"]`).toggleAttribute('disabled', !next);
    }
    this.host.querySelector('#tq-studio-name').toggleAttribute('disabled', !next);
    this.el.source.toggleAttribute('readonly', !next);
    this.applyMode();
    this.paintPreview();
    this.paintGate();
  }

  /// The ONE writer of the status banner. Its conditions outlive a repaint —
  /// a missing simulator and a rejected program stay true across a mode switch —
  /// so no other method may clear the element under them. The markup is cached
  /// because `paintState` runs this once a frame while the evolution plays.
  paintStatus() {
    let html = '';
    if (this.engineMissing) {
      html = alertHtml('warning', T('studio.no_wasm'), T('studio.no_wasm_sub'));
    } else if (this.failure) {
      html = alertHtml('danger', T('studio.failed'), this.failure);
    } else if (this.parseErrors.length) {
      html = alertHtml('warning', T('studio.parse_failed'), this.parseErrors[0].message || '');
    } else if (this.simError) {
      html = alertHtml('warning', T('studio.sim_failed'), this.simError);
    }
    if (html === this.statusHtml) return;
    this.statusHtml = html;
    this.el.status.innerHTML = html;
  }

  /// The error list belongs to the text tab, which is hidden in the other two
  /// modes — so the status banner carries the first message as well; a program
  /// the parser refuses must never fail silently.
  paintErrors() {
    this.paintStatus();
    const box = this.el.errors;
    box.hidden = this.parseErrors.length === 0;
    box.innerHTML = this.parseErrors.map((e) => `
      <div class="tq-parse-error">
        <span class="mono">${escapeHtml(T('studio.error_at', { line: Number(e.line) || 0, column: Number(e.column) || 0 }))}</span>
        <span>${escapeHtml(e.message || '')}</span>
      </div>`).join('');
  }

  paintResources() {
    if (!this.circuit) {
      this.el.resources.innerHTML = `<span class="v">${escapeHtml(T('studio.res_unknown'))}</span>`;
      return;
    }
    const summary = resourceSummary(this.circuit, this.sim ? this.sim.precision : 'single');
    const rows = [
      [T('studio.res_qubits'), T('studio.res_qubits_value', { q: summary.numQubits, c: summary.numClbits })],
      [T('studio.res_depth'), String(summary.depth)],
      [T('studio.res_gates'), T('studio.res_gates_value', { gates: summary.gates, ops: summary.ops })],
      [T('studio.res_memory'), formatBytes(summary.memoryBytes)],
      [T('studio.res_backend'), this.sim ? this.sim.backendName : '—'],
    ];
    this.el.resources.innerHTML = rows
      .map(([k, v]) => `<span class="k">${escapeHtml(k)}</span><span class="v">${escapeHtml(String(v))}</span>`)
      .join('');
  }

  // -------------------------------------------------------------------------
  // Running (T0)
  // -------------------------------------------------------------------------

  /// Draws the shots in batches, so the histogram fills as the run proceeds
  /// (§13.6). Every batch is a real, INDEPENDENT sample of the circuit — see
  /// `shotPlan` for why each one has to carry its own seed, and `shotBatchSize`
  /// for why a wide run gets wider batches rather than more of them.
  async run() {
    if (!this.circuit || this.shotsPending) return;
    // Sampling needs somewhere to sample INTO. A circuit with no classical
    // register cannot be measured, and the engine answers such a run with an
    // English refusal — so the question is settled here, in the user's language,
    // before the call. The state panel needs no shots and stays as it is.
    if (!canSample(this.circuit)) {
      this.counts = null;
      this.el.counts.bundle = { 'text/plain': T('studio.no_counts') };
      return;
    }
    const wanted = Math.max(1, Math.min(MAX_SHOTS, Number(this.state.shots) || 1));
    const generation = this.runGeneration;
    const circuit = this.circuit;
    this.shotsPending = wanted;
    this.counts = {};
    try {
      const { simulate } = await import('/js/quantum/index.js');
      const base = Math.floor(Math.random() * 2 ** 32);
      for (const batch of this.runPlan(wanted, base)) {
        const result = await simulate(circuit, {
          shots: batch.shots,
          seed: batch.seed,
          maxQubits: T0_MAX_QUBITS,
        });
        if (!this.applyBatch(generation, result.counts || {})) return;
      }
    } catch (e) {
      toast(`${T('studio.run_failed')}: ${errMessage(e)}`, 'error');
    } finally {
      // Only one run is ever in flight (the guard above), so this always frees
      // the button — including for a run the grid disowned halfway through.
      this.shotsPending = 0;
    }
  }

  /// The batches this run will draw. A method rather than a call inside the
  /// loop, because the shape of a run — how many evolutions it costs and how
  /// many shots it ends up drawing — is checkable without a wasm module.
  runPlan(wanted, base) {
    return shotPlan(wanted, shotBatchSize(wanted), base);
  }

  /// Adds one finished batch to the histogram. A batch of a superseded run is
  /// dropped instead of merged: its draws belong to a circuit that is no longer
  /// on the grid. The return value tells the loop whether to keep drawing.
  applyBatch(generation, counts) {
    if (this.disposed || generation !== this.runGeneration) return false;
    this.counts = mergeCounts(this.counts, counts);
    // A run only starts with a classical register to sample into, so every
    // batch that reaches here carries draws.
    this.el.counts.bundle = countsBundle(this.counts, totalShots(this.counts));
    return true;
  }

  // -------------------------------------------------------------------------
  // Export and save
  // -------------------------------------------------------------------------

  download(filename, text, mime) {
    const url = URL.createObjectURL(new Blob([text], { type: `${mime};charset=utf-8` }));
    const link = document.createElement('a');
    link.href = url;
    link.download = filename;
    document.body.appendChild(link);
    link.click();
    link.remove();
    URL.revokeObjectURL(url);
  }

  /// The Qiskit export of §6.1. The program is rendered by the crate, not by the
  /// screen, so the browser and a node export the same file — which is why this
  /// one export is asynchronous while `.qasm` and `.svg` are not.
  async exportQiskit() {
    if (!this.circuit) return;
    try {
      const { exportQiskitPython } = await import('/js/quantum/index.js');
      const python = await exportQiskitPython(this.circuit);
      if (this.disposed) return;
      this.download(pyFileName(this.circuitName()), python, 'text/x-python');
    } catch (e) {
      toast(`${T('studio.export_failed')}: ${errMessage(e)}`, 'error');
    }
  }

  async copySource() {
    try {
      await navigator.clipboard.writeText(this.state.source);
      toast(T('studio.copied'), 'success');
    } catch (e) {
      toast(`${T('studio.copy_failed')}: ${errMessage(e)}`, 'error');
    }
  }

  async saveAsFile() {
    const path = qasmFileName(this.circuitName());
    try {
      await uploadFile(this.screen, path, new TextEncoder().encode(this.state.source));
      toast(T('studio.saved_file', { name: path }), 'success');
      await this.screen.reloadFiles();
    } catch (e) {
      toast(`${T('studio.save_failed')}: ${errMessage(e)}`, 'error');
    }
  }

  async saveToNotebook() {
    const target = this.state.notebookId
      ? { notebookId: this.state.notebookId, cellId: this.state.cellId }
      : await openNotebookPicker(this.screen);
    if (!target) return;
    try {
      await saveCircuitToNotebook(this.screen, { ...target, source: this.state.source });
      this.state.notebookId = target.notebookId;
      this.state.cellId = target.cellId ?? null;
      toast(T('studio.saved_cell'), 'success');
      await this.screen.reloadNotebooks();
    } catch (e) {
      toast(`${T('studio.save_failed')}: ${errMessage(e)}`, 'error');
    }
  }
}
