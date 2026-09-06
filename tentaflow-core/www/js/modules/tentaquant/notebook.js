// ===== File: modules/tentaquant/notebook.js — Q06, the project notebook =====
//
// A column of cells and the live state panel beside it. Two cell kinds exist
// because two backends exist: markdown (rendered by the dashboard's own
// renderer) and circuit (OpenQASM 3, run on the tier the toolbar's "Uruchom
// na…" names). Python cells are NOT offered — the kernel is a service that is
// not built yet, and a disabled cell type would promise one.
//
// A circuit cell runs on T0 (this browser, nothing stored) or on T1 (Core on a
// node, a stored run with a live stream). The two are not dressed up as the
// same thing: a T1 output names its run and links to it, and the state panel
// says which tier the frames it draws came from.
//
// The notebook is saved whole, with the version it was loaded at
// (`NotebookSaveRequest.expected_version`): a second editor that saved first
// makes this save a Conflict, which is reported and reloaded rather than
// overwritten.
//
// This view object is the ONLY copy of an edit until that save lands — the
// screen drops it on every tab switch and every way out of the project — so
// `confirmLeave` is what the screen asks before dropping it, and the same
// question guards the notebook picker and the trip to the Studio.

import { escapeHtml, escapeAttr, fmtMs, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { TfWindow } from '/js/components/tf-window.js';
import {
  T, sprite, fmtDate, errMessage, shortId, canEditProject, circuitLabels, blochLabels, mimeLabels,
  editorLabels, viewportAllowsEditing, watchEditViewport,
} from '/js/modules/tentaquant/format.js';
import {
  addCell, createCell, isDirty, isRenderableKind, isVersionConflict, lastCircuitCell,
  moveCell, notebookState, parseCells, removeCell, serializeCells, updateCell,
} from '/js/modules/tentaquant/cells.js';
import {
  MAX_LIVE_STATE_QUBITS, T0_MAX_QUBITS, blochFromAmplitudes, canSample, countsBundle,
  runSeed, stateBundle, totalShots,
} from '/js/modules/tentaquant/quantum-view.js';
import { openTqModal } from '/js/modules/tentaquant/dialogs.js';
import {
  AUTO_TARGET, autoHint, chooseTarget, effectiveTarget, isBrowserTarget, startRefusal, targetOptions,
} from '/js/modules/tentaquant/targets.js';
import {
  RunStream, countsBundleOf, keyframeBloch, keyframeGateLabel, keyframeProbsBundle,
  keyframeStateBundle, stateBundleOf,
} from '/js/modules/tentaquant/run-stream.js';
import { runStatusLabel, runStatusTone } from '/js/modules/tentaquant/run-model.js';
import '/js/components/tf-quantum-circuit.js';
import '/js/components/tf-bloch-sphere.js';
import '/js/components/tf-mime-output.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-code-editor.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-input.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-select.js';
import '/js/components/tf-slider.js';

const DEFAULT_SHOTS = 1024;

/// Lets go of one cell's T1 run: the session stops talking and whoever is
/// waiting for that run to end stops waiting. Both halves belong together —
/// a stream stopped without settling the wait keeps the caller (and the view
/// it closed over) alive forever, and settling twice is a no-op by design, so
/// every exit of a run goes through here.
function endRunEntry(entry) {
  if (!entry) return;
  if (entry.stream) entry.stream.stop();
  const settle = entry.settle;
  entry.settle = null;
  if (settle) settle();
}

export function drawNotebook(screen, host) {
  const view = new NotebookView(screen, host);
  screen.projectViewDispose = () => view.dispose();
  screen.projectViewGuard = () => view.confirmLeave();
  view.mount();
  return view;
}

class NotebookView {
  constructor(screen, host) {
    this.screen = screen;
    this.host = host;
    // Write access is the role; editing also needs a viewport the cell column
    // can be worked on (plan §13.5), which a phone is not.
    this.writable = canEditProject(screen.project) && !screen.project.archivedAt;
    this.editable = this.writable && viewportAllowsEditing();
    this.unwatchViewport = null;
    this.notebookId = screen.notebookId || screen.notebooks[0]?.notebookId || null;
    this.cells = [];
    this.version = 0;
    this.savedJson = '[]';
    this.readingVersion = null;
    this.editing = new Set();
    this.outputs = new Map();
    this.parsed = new Map();
    // Targets the laboratory offers, the `auto` resolution for the circuit the
    // panel follows, and the T1 run of each cell that has one.
    this.targets = [];
    this.target = AUTO_TARGET;
    this.resolution = null;
    this.resolvedQubits = -1;
    this.runs = new Map();
    this.busy = false;
    this.disposed = false;
  }

  dispose() {
    this.disposed = true;
    for (const entry of this.runs.values()) endRunEntry(entry);
    this.runs.clear();
    // The tab bar outlives the view, so the dot it was showing goes with it.
    this.paintTabDirty(false);
    if (this.unwatchViewport) this.unwatchViewport();
    this.unwatchViewport = null;
  }

  /// A phone turned sideways crosses §13.5's line: the column is redrawn with
  /// (or without) its editing bars rather than left in the previous mode.
  setEditable(next) {
    if (this.editable === next) return;
    this.editable = next;
    if (this.notebookId) this.render();
    else this.renderEmpty();
  }

  // -------------------------------------------------------------------------
  // Loading
  // -------------------------------------------------------------------------

  async mount() {
    if (!this.unwatchViewport) {
      this.unwatchViewport = watchEditViewport((wide) => this.setEditable(this.writable && wide));
    }
    this.loadTargets();
    if (!this.notebookId) { this.renderEmpty(); return; }
    this.host.innerHTML = `<div class="tq-loading">${escapeHtml(I18n.t('common.loading'))}</div>`;
    try {
      const res = await this.screen.tq('tentaQuantNotebookGetRequest', {
        projectId: this.screen.projectId,
        notebookId: this.notebookId,
      });
      if (this.disposed) return;
      Object.assign(this, notebookState(res));
      this.readingVersion = null;
      this.screen.notebookId = this.notebookId;
      this.render();
    } catch (e) {
      this.host.innerHTML = `<tf-alert tone="danger" title="${escapeAttr(T('notebook.load_failed'))}" message="${escapeAttr(errMessage(e))}"></tf-alert>`;
    }
  }

  renderEmpty() {
    this.host.innerHTML = `
      <tf-empty-state icon="file-text" title="${escapeAttr(T('notebook.empty'))}" message="${escapeAttr(T('notebook.empty_sub'))}">
      </tf-empty-state>
      <div class="tq-empty-actions">
        <tf-button variant="primary" icon="plus" data-act="create" ${this.editable ? '' : 'disabled'}>${escapeHtml(T('notebook.create'))}</tf-button>
        ${this.writable && !this.editable
          ? `<tf-chip status="info" icon="eye" label="${escapeAttr(T('studio.preview_only'))}"></tf-chip>`
          : ''}
      </div>`;
    this.host.querySelector('[data-act="create"]').addEventListener('click', () => this.create());
  }

  async create() {
    // Creating one switches this view to the new notebook, which drops these
    // cells exactly as a tab switch does.
    if (!await this.confirmLeave()) return;
    const name = await promptName(T('notebook.create_title'), T('notebook.name_label'), T('notebook.default_name', { n: this.screen.notebooks.length + 1 }));
    if (!name) return;
    try {
      const res = await this.screen.tq('tentaQuantNotebookCreateRequest', {
        projectId: this.screen.projectId,
        name,
        cellsJson: serializeCells([createCell('markdown', { source: `# ${name}\n` })]),
      });
      await this.screen.reloadNotebooks();
      if (this.disposed) return;
      this.notebookId = res.notebook.notebookId;
      this.screen.notebookId = this.notebookId;
      await this.mount();
    } catch (e) {
      toast(`${T('notebook.save_failed')}: ${errMessage(e)}`, 'error');
    }
  }

  // -------------------------------------------------------------------------
  // Rendering
  // -------------------------------------------------------------------------

  render() {
    const dirty = isDirty(this.cells, this.savedJson);
    this.host.innerHTML = `
      <div class="tf-toolbar nb-toolbar">
        <tf-select id="tq-nb-select" value="${escapeAttr(this.notebookId)}">
          ${this.screen.notebooks.map((n) => `<option value="${escapeAttr(n.notebookId)}">${escapeHtml(n.name)}</option>`).join('')}
        </tf-select>
        <tf-button variant="ghost" size="sm" icon="plus" data-act="create" ${this.editable ? '' : 'disabled'}>${escapeHtml(T('notebook.new'))}</tf-button>
        <tf-button variant="secondary" size="sm" icon="play" data-act="run-all">${escapeHtml(T('notebook.run_all'))}</tf-button>
        <tf-select id="tq-nb-target" value="${escapeAttr(this.target)}">
          <option value="${AUTO_TARGET}">${escapeHtml(T('targets.auto'))}</option>
          <option value="browser">${escapeHtml(T('targets.browser', { q: T0_MAX_QUBITS }))}</option>
        </tf-select>
        <span class="tq-target-hint" id="tq-nb-target-hint"></span>
        ${this.writable && !this.editable
          ? `<tf-chip status="info" icon="eye" label="${escapeAttr(T('studio.preview_only'))}"></tf-chip>`
          : ''}
        <span class="tf-toolbar-spacer"></span>
        ${this.readingVersion !== null
          ? `<tf-chip status="warn" label="${escapeAttr(T('notebook.reading_version', { v: this.readingVersion }))}"></tf-chip>
             <tf-button variant="secondary" size="sm" icon="rotate" data-act="head">${escapeHtml(T('notebook.back_to_head'))}</tf-button>`
          : `<span class="tq-save-state">${escapeHtml(dirty ? T('notebook.unsaved') : this.savedAtLabel())}</span>`}
        <tf-button variant="ghost" size="sm" icon="clock" data-act="versions">${escapeHtml(T('notebook.versions'))}</tf-button>
        <tf-button variant="primary" size="sm" icon="save" data-act="save"
          ${this.editable && dirty && this.readingVersion === null ? '' : 'disabled'}>${escapeHtml(T('notebook.save'))}</tf-button>
      </div>
      <div class="nb-layout">
        <div class="cells" id="tq-nb-cells"></div>
        <div class="state-panel" id="tq-nb-panel"></div>
      </div>`;

    this.host.querySelector('#tq-nb-select').addEventListener('change', async (e) => {
      const id = e.detail?.value;
      if (!id || id === this.notebookId) return;
      // Loading another notebook drops these cells exactly as a tab switch
      // does; the select goes back to the notebook on screen if the user
      // decides to stay with it.
      if (!await this.confirmLeave()) {
        // The property setter, not the attribute: tf-select ignores an
        // attribute write that matches what it already reflects.
        e.target.value = this.notebookId;
        return;
      }
      this.notebookId = id;
      this.mount();
    });
    this.host.querySelector('[data-act="create"]').addEventListener('click', () => this.create());
    this.host.querySelector('[data-act="run-all"]').addEventListener('click', () => this.runAll());
    const target = this.host.querySelector('#tq-nb-target');
    // Before the wire answers, the markup's own two options stand: this page
    // IS the browser tier and needs no confirmation. Replacing them with an
    // empty answer would take that away.
    if (this.targets.length) target.setOptions(targetOptions(this.targets), this.target);
    target.addEventListener('change', (e) => {
      this.target = e.detail?.value || AUTO_TARGET;
      this.paintTargetHint();
      this.refreshResolution();
    });
    this.paintTargetHint();
    this.host.querySelector('[data-act="save"]').addEventListener('click', () => this.save());
    this.host.querySelector('[data-act="versions"]').addEventListener('click', () => this.openVersions());
    this.host.querySelector('[data-act="head"]')?.addEventListener('click', () => this.mount());
    this.paintTabDirty(dirty);
    this.renderCells();
    // The panel follows the cells, so every redraw of the column refreshes it
    // — a circuit cell that was just added has a state to show too.
    this.refreshPanel();
  }

  renderCells() {
    const list = this.host.querySelector('#tq-nb-cells');
    if (!list) return;
    list.innerHTML = this.cells.map((cell, index) => this.cellHtml(cell, index)).join('')
      + this.addBarHtml(this.cells.length);
    for (const cell of this.cells) this.hydrate(cell);
    this.wireCells(list);
  }

  addBarHtml(index) {
    if (!this.editable || this.readingVersion !== null) return '';
    return `
      <div class="add-cell" data-add-at="${index}">
        <span class="ln"></span>
        <tf-button variant="ghost" size="sm" icon="plus" data-add="markdown">${escapeHtml(T('notebook.add_markdown'))}</tf-button>
        <tf-button variant="ghost" size="sm" icon="plus" data-add="circuit">${escapeHtml(T('notebook.add_circuit'))}</tf-button>
        <span class="ln"></span>
      </div>`;
  }

  cellHtml(cell, index) {
    const editing = this.editing.has(cell.id);
    const body = isRenderableKind(cell.kind)
      ? (cell.kind === 'circuit' ? this.circuitBodyHtml() : this.markdownBodyHtml(editing))
      : `<div class="tq-cell-unknown">${escapeHtml(T('notebook.unknown_kind', { kind: cell.kind }))}</div>`;
    const canEdit = this.editable && this.readingVersion === null;
    return `
      ${this.addBarHtml(index)}
      <div class="cell" data-cell="${escapeAttr(cell.id)}">
        <div class="cg">
          ${cell.kind === 'circuit'
            ? `<tf-button variant="ghost" size="sm" icon="play" data-act="run" title="${escapeAttr(T('notebook.run'))}"></tf-button>`
            : `<tf-button variant="ghost" size="sm" icon="${editing ? 'check' : 'edit'}" data-act="toggle-edit" title="${escapeAttr(T(editing ? 'notebook.preview' : 'notebook.edit'))}" ${canEdit ? '' : 'disabled'}></tf-button>`}
          <span class="kind">${escapeHtml(T('notebook.kind_' + (isRenderableKind(cell.kind) ? cell.kind : 'other')))}</span>
        </div>
        <div class="cb">
          <div class="ch">
            <span class="lang">${sprite(cell.kind === 'circuit' ? 'atom' : 'file-text')}${escapeHtml(T('notebook.kind_' + (isRenderableKind(cell.kind) ? cell.kind : 'other')))}</span>
            <span class="spacer"></span>
            ${cell.kind === 'circuit' ? `<tf-button variant="ghost" size="sm" icon="chip" data-act="studio">${escapeHtml(T('notebook.open_studio'))}</tf-button>` : ''}
            <tf-button variant="ghost" size="sm" icon="chevron-up" data-act="up" title="${escapeAttr(T('notebook.move_up'))}" ${canEdit ? '' : 'disabled'}></tf-button>
            <tf-button variant="ghost" size="sm" icon="chevron-down" data-act="down" title="${escapeAttr(T('notebook.move_down'))}" ${canEdit ? '' : 'disabled'}></tf-button>
            <tf-button variant="ghost" size="sm" icon="trash" data-act="delete" title="${escapeAttr(T('notebook.delete'))}" ${canEdit ? '' : 'disabled'}></tf-button>
          </div>
          ${body}
          <div class="out" data-out hidden></div>
        </div>
      </div>`;
  }

  markdownBodyHtml(editing) {
    if (editing) {
      return `<tf-code-editor data-editor language="markdown" aria-label="${escapeAttr(T('notebook.kind_markdown'))}"></tf-code-editor>`;
    }
    return `<div class="md" data-markdown></div>`;
  }

  circuitBodyHtml() {
    return `
      <div class="tq-cell-circuit">
        <tf-segmented data-circuit-view value="grid">
          <option value="grid" icon="chip">${escapeHtml(T('notebook.view_circuit'))}</option>
          <option value="text" icon="code">${escapeHtml(T('notebook.view_text'))}</option>
        </tf-segmented>
        <div class="tq-circuit-wrap" data-view="grid">
          <tf-quantum-circuit data-circuit palette="none" ${this.editable && this.readingVersion === null ? '' : 'readonly'}
            aria-label="${escapeAttr(T('notebook.kind_circuit'))}"></tf-quantum-circuit>
        </div>
        <div data-view="text" hidden>
          <tf-code-editor data-editor language="plain" aria-label="${escapeAttr(T('notebook.view_text'))}"></tf-code-editor>
          <div class="hint">${escapeHtml(T('notebook.text_hint'))}</div>
        </div>
        <div class="tq-parse-errors" data-errors hidden></div>
      </div>`;
  }

  /// Fills the components of one cell after its markup landed: editors get
  /// their text, a circuit gets its parsed IR, and a cell that already ran gets
  /// its output back.
  hydrate(cell) {
    const root = this.cellEl(cell.id);
    if (!root) return;
    const editor = root.querySelector('[data-editor]');
    if (editor) {
      editor.labels = editorLabels();
      editor.value = cell.source;
      if (!this.editable || this.readingVersion !== null) editor.setAttribute('readonly', '');
    }
    const markdown = root.querySelector('[data-markdown]');
    if (markdown) {
      markdown.textContent = cell.source;
      import('/js/lib/md-lite.js').then(({ renderMarkdown }) => {
        if (markdown.isConnected) markdown.innerHTML = renderMarkdown(cell.source);
      }).catch(() => {
        // The source is already in the holder as text — a renderer that fails
        // to load must not blank the cell.
      });
    }
    const circuit = root.querySelector('[data-circuit]');
    if (circuit) {
      circuit.labels = circuitLabels();
      this.parse(cell);
    }
    this.renderOutput(cell.id);
  }

  cellEl(id) {
    // Cell ids are minted locally but still go through a lookup rather than a
    // built selector: a selector is not the place to learn what an id may hold.
    return Array.from(this.host.querySelectorAll('.cell')).find((el) => el.dataset.cell === id) || null;
  }

  wireCells(list) {
    list.addEventListener('click', (event) => {
      const bar = event.target.closest('[data-add]');
      if (bar) {
        const at = Number(bar.closest('[data-add-at]').dataset.addAt);
        const { cells, id } = addCell(this.cells, bar.dataset.add, at);
        this.cells = cells;
        // A new markdown cell is empty: it opens in its editor, because the
        // alternative is a blank preview the user has to click again.
        if (bar.dataset.add === 'markdown') this.editing.add(id);
        this.render();
        this.focusCell(id);
        return;
      }
      const open = event.target.closest('[data-open-run]');
      if (open) { this.screen.openRun(open.dataset.openRun); return; }
      // The cell output shows what fits under a cell; everything §13.6 promises
      // about a result lives in the full-screen run view.
      const full = event.target.closest('[data-full-result]');
      if (full) { this.screen.openRunResult(full.dataset.fullResult); return; }
      const button = event.target.closest('[data-act]');
      const cellEl = event.target.closest('.cell');
      if (!button || !cellEl) return;
      const id = cellEl.dataset.cell;
      switch (button.dataset.act) {
        case 'run': this.run(id); break;
        case 'studio': this.openInStudio(id); break;
        case 'toggle-edit':
          if (this.editing.has(id)) this.editing.delete(id);
          else this.editing.add(id);
          this.render();
          break;
        case 'up': this.cells = moveCell(this.cells, id, -1); this.render(); break;
        case 'down': this.cells = moveCell(this.cells, id, 1); this.render(); break;
        case 'delete': this.confirmDelete(id); break;
        default: break;
      }
    });
    list.addEventListener('change', (event) => {
      const grid = event.target.closest('[data-circuit]');
      if (grid && event.detail && event.detail.circuit) {
        // The grid edits the IR, so the cell's OpenQASM is regenerated from it
        // — the text tab and the grid are one artefact, as in the Studio.
        this.adoptCircuit(event.target.closest('.cell').dataset.cell, event.detail.circuit);
        return;
      }
      const view = event.target.closest('[data-circuit-view]');
      if (view) {
        const holder = view.closest('.tq-cell-circuit');
        holder.querySelector('[data-view="grid"]').hidden = event.detail.value !== 'grid';
        holder.querySelector('[data-view="text"]').hidden = event.detail.value !== 'text';
        return;
      }
      const editor = event.target.closest('[data-editor]');
      const cellEl = event.target.closest('.cell');
      if (!editor || !cellEl) return;
      const id = cellEl.dataset.cell;
      this.cells = updateCell(this.cells, id, { source: editor.value });
      this.markDirty();
      const cell = this.cells.find((c) => c.id === id);
      if (cell && cell.kind === 'circuit') this.editedCircuitSource(id);
    });
    list.addEventListener('save', () => this.save());
  }

  /// When the notebook was last written. Undoing an edit has to put this back,
  /// so the label lives in one place rather than being rebuilt by `render` and
  /// preserved by everything else.
  savedAtLabel() {
    const head = this.screen.notebooks.find((n) => n.notebookId === this.notebookId);
    return T('notebook.saved_at', { when: fmtDate(head?.updatedAt) });
  }

  markDirty() {
    const save = this.host.querySelector('[data-act="save"]');
    const state = this.host.querySelector('.tq-save-state');
    const dirty = isDirty(this.cells, this.savedJson);
    if (save) save.toggleAttribute('disabled', !(this.editable && dirty && this.readingVersion === null));
    if (state) state.textContent = dirty ? T('notebook.unsaved') : this.savedAtLabel();
    this.paintTabDirty(dirty);
  }

  /// The unsaved dot on the project's own tab (`tf-tab[dirty]`): the toolbar
  /// label is inside the panel the next tab replaces, so without this the only
  /// warning disappears with the click that needs it.
  paintTabDirty(dirty) {
    const tab = this.screen.root?.querySelector('#tq-project-tabs tf-tab#notebook');
    if (tab) tab.toggleAttribute('dirty', Boolean(dirty));
  }

  /// Puts the caret in a cell that was just created or moved. A cell with no
  /// editor open (a circuit, a markdown preview) is scrolled to instead — the
  /// point is that the user sees what the click produced.
  focusCell(id) {
    const root = this.cellEl(id);
    if (!root) return;
    if (typeof root.scrollIntoView === 'function') root.scrollIntoView({ block: 'nearest' });
    const editor = root.querySelector('[data-editor]');
    if (editor && typeof editor.focus === 'function') editor.focus();
  }

  // -------------------------------------------------------------------------
  // Circuits (T0)
  // -------------------------------------------------------------------------

  async parse(cell) {
    try {
      const { available, parse } = await import('/js/quantum/index.js');
      if (!await available()) {
        this.showParseErrors(cell.id, [{ message: T('studio.no_wasm_sub') }]);
        return null;
      }
      const result = await parse(cell.source);
      if (this.disposed) return null;
      if (result.status !== 'parsed') {
        this.parsed.delete(cell.id);
        this.showParseErrors(cell.id, result.errors || []);
        return null;
      }
      this.parsed.set(cell.id, result.circuit);
      this.showParseErrors(cell.id, []);
      const element = this.cellEl(cell.id)?.querySelector('[data-circuit]');
      if (element) element.circuit = result.circuit;
      return result.circuit;
    } catch (e) {
      this.showParseErrors(cell.id, [{ message: errMessage(e) }]);
      return null;
    }
  }

  /// A text (OpenQASM 3) edit of a circuit cell. The panel follows the LAST
  /// circuit cell, so editing that one has to move the panel too — otherwise the
  /// spheres keep answering the program the user has just replaced. It runs on
  /// the parse's own beat: `tf-code-editor` debounces its `change` by 250 ms.
  async editedCircuitSource(id) {
    const cell = this.cells.find((c) => c.id === id);
    if (!cell) return;
    this.dropRun(id);
    await this.parse(cell);
    if (this.disposed || lastCircuitCell(this.cells)?.id !== id) return;
    await this.refreshPanel();
  }

  async adoptCircuit(id, circuit) {
    try {
      const { toQasm3 } = await import('/js/quantum/index.js');
      const source = await toQasm3(circuit);
      if (this.disposed) return;
      this.parsed.set(id, circuit);
      this.dropRun(id);
      this.cells = updateCell(this.cells, id, { source });
      const editor = this.cellEl(id)?.querySelector('[data-editor]');
      if (editor) editor.value = source;
      this.markDirty();
      this.refreshPanel();
    } catch (e) {
      this.showParseErrors(id, [{ message: errMessage(e) }]);
    }
  }

  /// Lets go of a cell's run. Its frames describe the program that WAS in the
  /// cell, so an edit is what ends them — the same rule the Studio applies when
  /// the grid changes under a recorded evolution.
  dropRun(id) {
    const entry = this.runs.get(id);
    if (!entry) return;
    endRunEntry(entry);
    this.runs.delete(id);
    if (this.outputs.get(id)?.tier === 'T1') this.outputs.delete(id);
  }

  showParseErrors(id, errors) {
    const box = this.cellEl(id)?.querySelector('[data-errors]');
    if (!box) return;
    box.hidden = errors.length === 0;
    box.innerHTML = errors.map((e) => `<div class="tq-parse-error">${
      Number.isFinite(Number(e.line))
        ? `<span class="mono">${escapeHtml(T('studio.error_at', { line: Number(e.line) || 0, column: Number(e.column) || 0 }))}</span>`
        : ''
    }<span>${escapeHtml(e.message || '')}</span></div>`).join('');
  }

  // -------------------------------------------------------------------------
  // Targets
  // -------------------------------------------------------------------------

  /// The tiers this laboratory offers, for the toolbar's "Uruchom na…". A
  /// refused target stays in the list with the server's reason on it.
  async loadTargets() {
    try {
      const res = await this.screen.tq('tentaQuantTargetListRequest');
      if (this.disposed) return;
      this.targets = res.targets || [];
    } catch (e) {
      if (this.disposed) return;
      toast(`${T('targets.load_failed')}: ${errMessage(e)}`, 'error');
      return;
    }
    if (!this.targets.length) return;
    this.target = chooseTarget(this.targets, this.target);
    const select = this.host.querySelector('#tq-nb-target');
    if (select) select.setOptions(targetOptions(this.targets), this.target);
    this.refreshResolution();
  }

  /// Evaluates the `auto` rule for the circuit the panel follows — the widest
  /// question the notebook can ask before a run, since every cell is run with
  /// the same selection.
  async refreshResolution() {
    if (this.target !== AUTO_TARGET) { this.paintTargetHint(); return; }
    const cell = lastCircuitCell(this.cells);
    const numQubits = Number(cell && this.parsed.get(cell.id)?.numQubits) || 0;
    if (this.resolution && this.resolvedQubits === numQubits) { this.paintTargetHint(); return; }
    this.resolvedQubits = numQubits;
    this.resolution = null;
    this.paintTargetHint();
    try {
      const res = await this.screen.tq('tentaQuantTargetResolveRequest', {
        numQubits,
        fromBrowser: true,
        needsKernel: false,
      });
      if (this.disposed || this.resolvedQubits !== numQubits) return;
      this.resolution = res;
    } catch (e) {
      if (this.disposed) return;
      this.resolvedQubits = -1;
      toast(`${T('targets.load_failed')}: ${errMessage(e)}`, 'error');
    }
    this.paintTargetHint();
  }

  paintTargetHint() {
    const hint = this.host.querySelector('#tq-nb-target-hint');
    if (!hint) return;
    hint.textContent = this.target === AUTO_TARGET ? autoHint(this.resolution, this.targets) : '';
    hint.title = this.resolution ? String(this.resolution.reason || '') : '';
  }

  // -------------------------------------------------------------------------
  // Running a cell
  // -------------------------------------------------------------------------

  /// Runs one circuit cell on the selected tier. T0 computes here and stores
  /// nothing; T1 starts a laboratory run and follows its stream.
  async run(id) {
    const cell = this.cells.find((c) => c.id === id);
    if (!cell || cell.kind !== 'circuit') return;
    const refusal = startRefusal(this.targets, this.target, this.resolution);
    if (refusal) {
      // A cell that cannot be placed says WHERE it would have gone and why,
      // instead of running somewhere the user did not choose.
      this.outputs.set(id, { error: refusal });
      this.renderOutput(id);
      return;
    }
    if (!isBrowserTarget(effectiveTarget(this.target, this.resolution))) {
      await this.runOnCore(id, cell);
      return;
    }
    await this.runInBrowser(id, cell);
  }

  /// Starts one cell as a laboratory run and follows it to its end, so
  /// "Uruchom wszystko" stays sequential across both tiers.
  async runOnCore(id, cell) {
    endRunEntry(this.runs.get(id));
    this.outputs.set(id, { running: true, tier: 'T1' });
    this.renderOutput(id);
    let run = null;
    try {
      const res = await this.screen.tq('tentaQuantCircuitSimulateRequest', {
        qasm3: cell.source,
        options: { shots: DEFAULT_SHOTS, seed: runSeed(), wantState: true, wantProbabilities: false },
        projectId: this.screen.projectId,
        notebookId: this.notebookId,
        cellId: id,
      });
      run = res.run;
    } catch (e) {
      this.outputs.set(id, { error: errMessage(e), tier: 'T1' });
      this.renderOutput(id);
      return;
    }
    if (this.disposed) return;
    const entry = { run, state: null, step: -1, stream: null, settle: null };
    this.runs.set(id, entry);
    // The wait belongs to the ENTRY, not to the stream: `dispose` and `dropRun`
    // stop a session without it ever reporting an end, so leaving the notebook
    // (or editing the cell) mid-run would otherwise hold this promise for good
    // — stalling "Uruchom wszystko" halfway and keeping the closed view, its
    // DOM and the screen alive behind it.
    await new Promise((resolve) => {
      entry.settle = resolve;
      entry.stream = new RunStream(this.screen, run.runId, {
        onUpdate: (state) => this.absorbRun(id, state),
        // The session is done talking; releasing it here drops its transport
        // listener instead of leaving one behind per cell that ran.
        onEnd: () => endRunEntry(entry),
      });
      entry.stream.start().catch(() => endRunEntry(entry));
    });
  }

  /// One frame of a cell's run: the output under the cell is rebuilt, and the
  /// panel follows when the run belongs to the cell the panel is watching.
  absorbRun(id, state) {
    if (this.disposed) return;
    const entry = this.runs.get(id);
    if (!entry) return;
    entry.state = state;
    if (state.run) entry.run = { ...entry.run, ...state.run };
    // The slider stays where the user put it unless it was sitting on the end,
    // which is where a live run keeps it.
    if (entry.step < 0 || entry.step >= state.keyframes.length - 1) entry.step = state.keyframes.length - 1;
    // The row itself lives in `this.runs`; the outputs entry only says which
    // tier drew what is under the cell.
    this.outputs.set(id, { tier: 'T1' });
    this.renderOutput(id);
    if (lastCircuitCell(this.cells)?.id === id) this.refreshPanel();
  }

  /// Runs one circuit cell in this browser (T0) and puts the counts and the
  /// state under it. Nothing is stored: a run of the browser tier is not a run
  /// of the laboratory (plan §4.1).
  async runInBrowser(id, cell) {
    const circuit = this.parsed.get(id) || await this.parse(cell);
    if (!circuit || this.disposed) return;
    // A T1 run of this cell is over as far as the screen is concerned: the
    // browser is about to answer the same cell, so the session goes and so does
    // anybody waiting on it.
    this.dropRun(id);
    this.outputs.set(id, { running: true, tier: 'T0' });
    this.renderOutput(id);
    // The state vector is 2^n complex numbers copied out of wasm and one table
    // row per amplitude; above the ceiling it is not asked for at all.
    const numQubits = Number(circuit.numQubits) || 0;
    const wide = numQubits > MAX_LIVE_STATE_QUBITS;
    // Shots need a classical register to land in: without one the engine
    // refuses the run outright, so the cell is run for its STATE instead — the
    // one answer such a circuit has.
    const shots = canSample(circuit) ? DEFAULT_SHOTS : 0;
    try {
      const { simulate } = await import('/js/quantum/index.js');
      const started = performance.now();
      const result = await simulate(circuit, {
        shots,
        // Without one the engine draws off its default seed 0 and a re-run
        // replays the previous histogram; a cell run twice is two samples.
        seed: runSeed(),
        state: !wide,
        maxQubits: T0_MAX_QUBITS,
      });
      if (this.disposed) return;
      this.outputs.set(id, {
        counts: shots ? result.counts || {} : null,
        shots: result.shots || shots,
        state: result.state || null,
        numQubits: result.numQubits || numQubits,
        wide,
        method: result.method || '',
        elapsedMs: Math.round(performance.now() - started),
      });
    } catch (e) {
      this.outputs.set(id, { error: errMessage(e), tier: 'T0' });
    }
    this.renderOutput(id);
    this.refreshPanel();
  }

  /// Runs every circuit cell, top to bottom. Sequentially: a T0 run holds the
  /// wasm module and two of them would only queue behind each other anyway,
  /// and a T1 run queues for a laboratory slot the same way.
  async runAll() {
    for (const cell of this.cells.filter((c) => c.kind === 'circuit')) {
      if (this.disposed) return;
      await this.run(cell.id);
    }
  }

  /// The tier pill of one output. It names the tier that PRODUCED what is
  /// under it, which is not always the one selected in the toolbar — the
  /// selection may have changed since the run.
  tierPill(output) {
    return output && output.tier === 'T1'
      ? `<span class="tier t1">${escapeHtml(T('studio.tier_core'))}</span>`
      : `<span class="tier t0">${escapeHtml(T('studio.tier_browser'))}</span>`;
  }

  renderOutput(id) {
    const box = this.cellEl(id)?.querySelector('[data-out]');
    if (!box) return;
    const output = this.outputs.get(id);
    if (!output) { box.hidden = true; box.innerHTML = ''; return; }
    box.hidden = false;
    if (output.running) {
      box.innerHTML = `<div class="oh">${this.tierPill(output)}<span>${escapeHtml(T('notebook.running'))}</span></div>`;
      return;
    }
    if (output.error) {
      box.innerHTML = `<div class="oh">${this.tierPill(output)}</div>
        <div class="out-err">${escapeHtml(output.error)}</div>`;
      return;
    }
    if (output.tier === 'T1') { this.renderRunOutput(id, box); return; }
    // A run without a classical register drew no shots, so its head names the
    // backend and the time only — a "0 shots" line would read as a failed run.
    const head = output.counts
      ? T('notebook.output_head', { shots: totalShots(output.counts), method: output.method, ms: output.elapsedMs })
      : T('notebook.output_head_state', { method: output.method, ms: output.elapsedMs });
    box.innerHTML = `
      <div class="oh">
        ${this.tierPill(output)}
        <span>${escapeHtml(head)}</span>
      </div>
      ${output.counts
        ? '<tf-mime-output data-counts></tf-mime-output>'
        : `<div class="hint">${escapeHtml(T('studio.no_counts'))}</div>`}
      ${output.state
        ? '<tf-mime-output data-state max-rows="8"></tf-mime-output>'
        : `<div class="hint">${escapeHtml(output.wide
          ? T('notebook.state_wide', { q: output.numQubits, max: MAX_LIVE_STATE_QUBITS })
          : T('notebook.no_state'))}</div>`}`;
    const counts = box.querySelector('[data-counts]');
    if (counts) {
      counts.labels = mimeLabels();
      counts.bundle = countsBundle(output.counts, output.shots);
    }
    const state = box.querySelector('[data-state]');
    if (state) {
      state.labels = mimeLabels();
      state.bundle = stateBundle({ amplitudes: output.state, numQubits: output.numQubits });
    }
  }

  /// The output of a laboratory run under its cell: what the stream delivered
  /// so far, the run's own row to open, and — while it is going — the fact that
  /// it is still going. Outputs too large to travel inline are not drawn here;
  /// the run detail is where they are downloaded.
  renderRunOutput(id, box) {
    const entry = this.runs.get(id);
    if (!entry) { box.hidden = true; box.innerHTML = ''; return; }
    const state = entry.state;
    const counts = state ? countsBundleOf(state) : null;
    const stateOut = state ? stateBundleOf(state) : null;
    const metrics = (state && state.metrics) || entry.run.metrics || {};
    const parts = [];
    if (Number(metrics.durationMs)) parts.push(fmtMs(Number(metrics.durationMs)));
    if (metrics.backend) parts.push(String(metrics.backend));
    if (state && state.keyframes.length) parts.push(T('studio.run_keyframes', { n: state.keyframes.length }));
    box.innerHTML = `
      <div class="oh">
        ${this.tierPill({ tier: 'T1' })}
        <span class="mono">${escapeHtml(shortId(entry.run.runId))}</span>
        <tf-chip status="${runStatusTone(entry.run.status)}" label="${escapeAttr(runStatusLabel(entry.run))}"></tf-chip>
        <span>${escapeHtml(parts.join(' · '))}</span>
        <span class="spacer"></span>
        <tf-button variant="ghost" size="sm" icon="bar-chart" data-full-result="${escapeAttr(entry.run.runId)}">${escapeHtml(T('notebook.full_result'))}</tf-button>
        <tf-button variant="ghost" size="sm" icon="chevron-right" data-open-run="${escapeAttr(entry.run.runId)}">${escapeHtml(T('studio.open_run'))}</tf-button>
      </div>
      ${counts ? '<tf-mime-output data-run-counts></tf-mime-output>' : ''}
      ${stateOut ? '<tf-mime-output data-run-state max-rows="8"></tf-mime-output>' : ''}
      ${!counts && !stateOut ? `<div class="hint">${escapeHtml(T('notebook.run_waiting'))}</div>` : ''}`;
    const countsEl = box.querySelector('[data-run-counts]');
    if (countsEl) { countsEl.labels = mimeLabels(); countsEl.bundle = counts; }
    const stateEl = box.querySelector('[data-run-state]');
    if (stateEl) { stateEl.labels = mimeLabels(); stateEl.bundle = stateOut; }
  }

  // -------------------------------------------------------------------------
  // The state panel (last circuit cell)
  // -------------------------------------------------------------------------

  renderPanel() {
    const panel = this.host.querySelector('#tq-nb-panel');
    if (!panel) return;
    const cell = lastCircuitCell(this.cells);
    if (!cell) {
      panel.innerHTML = `<div class="section-card"><div class="section-card-head"><div class="title">${sprite('atom')}${escapeHtml(T('notebook.panel_title'))}</div></div>
        <div class="hint">${escapeHtml(T('notebook.panel_empty'))}</div></div>`;
      return;
    }
    const keyframes = this.panelKeyframes();
    panel.innerHTML = `
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('atom')}${escapeHtml(T('notebook.panel_title'))}</div>
          <div class="actions"><span class="tier ${keyframes ? 't1' : 't0'}">${escapeHtml(T(keyframes ? 'studio.tier_core' : 'studio.tier_browser'))}</span></div>
        </div>
        <div class="bloch-row" id="tq-nb-bloch"></div>
        <div class="step-row" id="tq-nb-steps" ${keyframes ? '' : 'hidden'}>
          <span class="tq-step-label">${escapeHtml(T('studio.step'))}</span>
          <tf-slider id="tq-nb-step" min="0" max="0" value="0" step="1" aria-label="${escapeAttr(T('studio.step'))}"></tf-slider>
          <span class="sl-val" id="tq-nb-step-value"></span>
        </div>
        <div class="hint" id="tq-nb-panel-hint">${escapeHtml(T('notebook.panel_loading'))}</div>
        <tf-mime-output id="tq-nb-amps" max-rows="8"></tf-mime-output>
        ${keyframes ? `<div class="tq-kf-probs" id="tq-nb-kf-probs" hidden>
          <div class="hint">${escapeHtml(T('studio.keyframe_probs'))}</div>
          <tf-mime-output id="tq-nb-kf-hist"></tf-mime-output>
        </div>` : ''}
      </div>`;
    panel.querySelector('#tq-nb-amps').labels = mimeLabels();
    const hist = panel.querySelector('#tq-nb-kf-hist');
    if (hist) hist.labels = mimeLabels();
    panel.querySelector('#tq-nb-step').addEventListener('input', (e) => {
      const entry = this.panelRun();
      if (!entry) return;
      entry.step = Number(e.detail?.value) || 0;
      this.paintKeyframePanel(entry);
    });
  }

  /// The cell run whose recorded evolution the panel is showing, when the cell
  /// the panel follows has one.
  panelRun() {
    const cell = lastCircuitCell(this.cells);
    const entry = cell ? this.runs.get(cell.id) : null;
    return entry && entry.state && entry.state.keyframes.length ? entry : null;
  }

  panelKeyframes() {
    const entry = this.panelRun();
    return entry ? entry.state.keyframes : null;
  }

  /// One received frame, drawn where the browser's own state usually is. The
  /// frames are discrete, so the slider steps between them and the label says
  /// they are a recording — never a continuous playhead over a state this
  /// browser holds.
  paintKeyframePanel(entry) {
    const frames = entry.state.keyframes;
    const index = Math.max(0, Math.min(entry.step, frames.length - 1));
    const frame = frames[index];
    const slider = this.host.querySelector('#tq-nb-step');
    if (slider) {
      slider.setAttribute('max', String(frames.length - 1));
      slider.setAttribute('value', String(index));
    }
    const label = this.host.querySelector('#tq-nb-step-value');
    if (label) {
      const gate = keyframeGateLabel(frame);
      label.textContent = gate
        ? T('studio.keyframe_of_gate', { step: index + 1, total: frames.length, gate })
        : T('studio.keyframe_of', { step: index + 1, total: frames.length });
    }
    const hint = this.host.querySelector('#tq-nb-panel-hint');
    if (hint) hint.hidden = true;
    const bloch = keyframeBloch(frame);
    const row = this.host.querySelector('#tq-nb-bloch');
    row.innerHTML = '';
    for (let q = 0; q * 3 < bloch.length; q += 1) {
      const sphere = document.createElement('tf-bloch-sphere');
      sphere.setAttribute('size', '78');
      sphere.setAttribute('label', `q${q}`);
      row.appendChild(sphere);
      sphere.labels = blochLabels();
      sphere.vector = [bloch[q * 3], bloch[q * 3 + 1], bloch[q * 3 + 2]];
    }
    this.host.querySelector('#tq-nb-amps').bundle = keyframeStateBundle(frame, bloch.length / 3);
    // The distribution the node recorded AT this step, next to the state it
    // belongs to — the cell's own output below carries the measured counts.
    const probs = keyframeProbsBundle(frame);
    const box = this.host.querySelector('#tq-nb-kf-probs');
    if (box) {
      box.hidden = !probs;
      if (probs) this.host.querySelector('#tq-nb-kf-hist').bundle = probs;
    }
  }

  /// The panel follows the LAST circuit cell of the notebook (Q06). A cell that
  /// ran on a node shows the frames that node recorded; otherwise the circuit
  /// is run here without shots, so a program that ends in a measurement has no
  /// single state — the panel says so and points at the Studio.
  async refreshPanel() {
    this.renderPanel();
    // The `auto` rule follows the circuit the panel follows, so the widest
    // question the notebook can ask is asked here rather than per keystroke.
    this.refreshResolution();
    const running = this.panelRun();
    if (running) { this.paintKeyframePanel(running); return; }
    const cell = lastCircuitCell(this.cells);
    const hint = this.host.querySelector('#tq-nb-panel-hint');
    if (!cell || !hint) return;
    const circuit = this.parsed.get(cell.id) || await this.parse(cell);
    if (!circuit || this.disposed) {
      if (hint.isConnected) hint.textContent = T('notebook.panel_invalid');
      return;
    }
    const numQubits = Number(circuit.numQubits) || 0;
    if (numQubits > MAX_LIVE_STATE_QUBITS) {
      // Neither the spheres nor the amplitudes are worth a 2^n copy per redraw
      // of the column; the Studio steps a circuit this wide without one.
      if (hint.isConnected) hint.textContent = T('notebook.state_wide', { q: numQubits, max: MAX_LIVE_STATE_QUBITS });
      return;
    }
    try {
      const { simulate } = await import('/js/quantum/index.js');
      const result = await simulate(circuit, { state: true, maxQubits: T0_MAX_QUBITS });
      if (this.disposed || !hint.isConnected) return;
      if (!result.state) {
        // The simulator explains itself in English; the screen says the same
        // thing in the user's language and points at the Studio.
        hint.textContent = T('notebook.no_state');
        return;
      }
      hint.hidden = true;
      const bloch = blochFromAmplitudes(result.state, result.numQubits);
      const row = this.host.querySelector('#tq-nb-bloch');
      row.innerHTML = '';
      for (let q = 0; q < result.numQubits; q += 1) {
        const sphere = document.createElement('tf-bloch-sphere');
        sphere.setAttribute('size', '78');
        sphere.setAttribute('label', `q${q}`);
        row.appendChild(sphere);
        sphere.labels = blochLabels();
        sphere.vector = [bloch[q * 3], bloch[q * 3 + 1], bloch[q * 3 + 2]];
      }
      this.host.querySelector('#tq-nb-amps').bundle = stateBundle({
        amplitudes: result.state,
        numQubits: result.numQubits,
      });
    } catch (e) {
      if (hint.isConnected) hint.textContent = errMessage(e);
    }
  }

  // -------------------------------------------------------------------------
  // Saving
  // -------------------------------------------------------------------------

  async confirmDelete(id) {
    const ok = await TfWindow.confirm({
      title: T('notebook.delete_title'),
      message: T('notebook.delete_message'),
      confirmLabel: T('notebook.delete'),
      cancelLabel: I18n.t('common.cancel'),
      danger: true,
    });
    if (!ok) return;
    this.cells = removeCell(this.cells, id);
    this.dropRun(id);
    this.outputs.delete(id);
    this.parsed.delete(id);
    this.render();
  }

  /// Writes the whole notebook. Answers whether the edits LANDED: a conflict
  /// reloads what the other editor saved and a transport error changes nothing,
  /// and in both cases the leave guard has to keep the user on the screen.
  async save() {
    if (this.busy || !this.editable || this.readingVersion !== null) return false;
    this.busy = true;
    const cellsJson = serializeCells(this.cells);
    try {
      const res = await this.screen.tq('tentaQuantNotebookSaveRequest', {
        projectId: this.screen.projectId,
        notebookId: this.notebookId,
        cellsJson,
        expectedVersion: this.version,
      });
      this.version = Number(res.notebook.currentVersion) || this.version + 1;
      this.savedJson = cellsJson;
      await this.screen.reloadNotebooks();
      if (this.disposed) return true;
      toast(T('notebook.saved_ok', { v: this.version }), 'success');
      this.render();
      return true;
    } catch (e) {
      if (isVersionConflict(e)) {
        toast(T('notebook.conflict'), 'warning');
        await this.mount();
        return false;
      }
      toast(`${T('notebook.save_failed')}: ${errMessage(e)}`, 'error');
      return false;
    } finally {
      this.busy = false;
    }
  }

  /// Whether the screen may drop this view. Every way out of the notebook — a
  /// project tab, the breadcrumb, the notebook picker, the trip to the Studio —
  /// ends in `dispose()`, and the cells live nowhere but in this object until a
  /// save lands, so a silent drop is a silent data loss.
  async confirmLeave() {
    if (this.disposed || !isDirty(this.cells, this.savedJson)) return true;
    const answer = await askLeave();
    if (answer === 'save') return this.save();
    if (answer !== 'discard') return false;
    // Discarding REVERTS the model instead of just walking away from it: a
    // caller may keep using this view afterwards (the Studio reads a cell out
    // of it), and nothing may carry on edits the user has just thrown away.
    this.cells = parseCells(this.savedJson);
    this.parsed.clear();
    this.render();
    return true;
  }

  async openVersions() {
    let versions = [];
    try {
      const res = await this.screen.tq('tentaQuantNotebookVersionsRequest', {
        projectId: this.screen.projectId,
        notebookId: this.notebookId,
      });
      versions = res.versions || [];
    } catch (e) {
      toast(`${T('notebook.versions_failed')}: ${errMessage(e)}`, 'error');
      return;
    }
    const win = document.createElement('tf-window');
    win.className = 'tq-modal';
    win.setAttribute('sheet', '');
    win.setAttribute('title', T('notebook.versions_title'));
    win.setAttribute('icon', 'clock');
    win.setAttribute('buttons', 'close');
    win.setAttribute('width', '640');
    win.innerHTML = `
      <div slot="body">
        <div class="tq-table-scroll">
          <table class="tf-table tq-share-table">
            <thead><tr>
              <th>${escapeHtml(T('notebook.col_version'))}</th>
              <th>${escapeHtml(T('notebook.col_author'))}</th>
              <th>${escapeHtml(T('notebook.col_created'))}</th>
              <th class="tq-cell-right"></th>
            </tr></thead>
            <tbody>
              ${versions.map((v) => `<tr>
                <td class="mono">${escapeHtml(String(v.version))}${v.version === this.version ? ` <tf-chip status="ok" label="${escapeAttr(T('notebook.head'))}"></tf-chip>` : ''}</td>
                <td>${escapeHtml(v.author || '')}</td>
                <td>${escapeHtml(fmtDate(v.createdAt))}</td>
                <td class="tq-cell-right"><tf-button variant="ghost" size="sm" icon="eye" data-open="${escapeAttr(String(v.version))}">${escapeHtml(T('notebook.open_version'))}</tf-button></td>
              </tr>`).join('')}
            </tbody>
          </table>
        </div>
        <div class="tq-field-hint">${escapeHtml(T('notebook.versions_hint'))}</div>
      </div>`;
    win.addEventListener('click', async (event) => {
      const button = event.target.closest('[data-open]');
      if (!button) return;
      win.close(true);
      await this.openVersion(Number(button.dataset.open));
    });
    document.body.appendChild(win);
  }

  async openVersion(version) {
    // Reading an older version replaces the column with it.
    if (!await this.confirmLeave()) return;
    try {
      const res = await this.screen.tq('tentaQuantNotebookGetRequest', {
        projectId: this.screen.projectId,
        notebookId: this.notebookId,
        version,
      });
      if (this.disposed) return;
      const state = notebookState(res);
      this.cells = state.cells;
      this.savedJson = state.savedJson;
      this.readingVersion = version === Number(res.notebook.currentVersion) ? null : version;
      this.outputs.clear();
      this.parsed.clear();
      this.render();
    } catch (e) {
      toast(`${T('notebook.versions_failed')}: ${errMessage(e)}`, 'error');
    }
  }

  /// Hands one circuit cell to the Studio (Q07). The Studio writes it back by
  /// RE-READING the notebook from the server, so an unsaved notebook has to be
  /// settled first: writing this cell over the stored notebook would publish
  /// one edit and drop every other unsaved cell in the same move.
  async openInStudio(id) {
    if (!await this.confirmLeave()) return;
    const cell = this.cells.find((c) => c.id === id);
    if (!cell) {
      // The cell existed only in the changes that were just discarded.
      toast(T('notebook.cell_discarded'), 'warning');
      return;
    }
    this.screen.openStudioWithCell({
      notebookId: this.notebookId,
      cellId: cell.id,
      source: cell.source,
      name: this.screen.notebooks.find((n) => n.notebookId === this.notebookId)?.name || '',
    });
  }
}

// ---------------------------------------------------------------------------
// Used by the Studio
// ---------------------------------------------------------------------------

/// Writes a circuit into a notebook: into the cell it came from, or appended as
/// a new circuit cell. The save carries the version the notebook was READ at in
/// this call, so a notebook somebody else changed meanwhile answers Conflict
/// instead of losing their edit.
export async function saveCircuitToNotebook(screen, { notebookId, cellId, source }) {
  const res = await screen.tq('tentaQuantNotebookGetRequest', {
    projectId: screen.projectId,
    notebookId,
  });
  const state = notebookState(res);
  const cells = cellId && state.cells.some((c) => c.id === cellId)
    ? updateCell(state.cells, cellId, { source })
    : state.cells.concat(createCell('circuit', { source }));
  return screen.tq('tentaQuantNotebookSaveRequest', {
    projectId: screen.projectId,
    notebookId,
    cellsJson: serializeCells(cells),
    expectedVersion: state.version,
  });
}

/// Asks which notebook a circuit should land in. Answers `{notebookId}` or
/// null when the user backed out; a project without a notebook is offered one.
export async function openNotebookPicker(screen) {
  const notebooks = screen.notebooks;
  if (!notebooks.length) {
    toast(T('studio.no_notebook'), 'warning');
    return null;
  }
  const { win, answered } = openTqModal({
    title: T('studio.pick_notebook'),
    body: `
      <tf-select id="tq-pick-notebook" label="${escapeAttr(T('studio.pick_notebook_label'))}" value="${escapeAttr(notebooks[0].notebookId)}">
        ${notebooks.map((n) => `<option value="${escapeAttr(n.notebookId)}">${escapeHtml(n.name)}</option>`).join('')}
      </tf-select>
      <div class="tq-field-hint">${escapeHtml(T('studio.pick_notebook_hint'))}</div>`,
    footer: `
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <span class="tf-toolbar-spacer"></span>
      <tf-button variant="primary" icon="check" data-action="confirm">${escapeHtml(T('studio.save_cell'))}</tf-button>`,
  });
  const action = await answered;
  // The window is detached by now; the select still answers for its own value.
  return action === 'confirm' ? { notebookId: win.querySelector('#tq-pick-notebook').value } : null;
}

/// The three ways out of a notebook with unsaved cells. Not `TfWindow.confirm`:
/// that asks a two-way question, and throwing the work away must not share a
/// button with saving it. Answers 'save', 'discard' or 'stay' — and a dialog
/// dismissed with Escape means 'stay', which is why the answer comes from the
/// window closing rather than from a button being pressed.
async function askLeave() {
  const { answered } = openTqModal({
    title: T('notebook.leave_title'),
    icon: 'save',
    body: `<div class="tq-field-hint">${escapeHtml(T('notebook.leave_message'))}</div>`,
    footer: `
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <span class="tf-toolbar-spacer"></span>
      <tf-button variant="danger" icon="trash" data-action="discard">${escapeHtml(T('notebook.leave_discard'))}</tf-button>
      <tf-button variant="primary" icon="save" data-action="save">${escapeHtml(T('notebook.leave_save'))}</tf-button>`,
  });
  const action = await answered;
  return action === 'save' || action === 'discard' ? action : 'stay';
}

async function promptName(title, label, initial) {
  const { win, answered } = openTqModal({
    title,
    body: `<tf-input id="tq-name-input" label="${escapeAttr(label)}" value="${escapeAttr(initial)}" maxlength="120"></tf-input>`,
    footer: `
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <span class="tf-toolbar-spacer"></span>
      <tf-button variant="primary" icon="check" data-action="confirm">${escapeHtml(I18n.t('common.save'))}</tf-button>`,
  });
  const action = await answered;
  return action === 'confirm' ? win.querySelector('#tq-name-input').value.trim() : null;
}
