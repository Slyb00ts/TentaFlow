// =============================================================================
// Plik: modules/flows-builder.js
// Opis: Ekran Flow Buildera - orkiestracja paleta / canvas / config,
//       topbar (nazwa, status, zoom, testuj, zapisz, history), bottombar
//       (breadcrumb, undo/redo, mobile tabs), autosave co 10s, historia wersji.
// =============================================================================

import { escapeHtml, escapeAttr, formatRelative, toast } from '/js/utils.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { Router } from '/js/router.js';
import { FlowCanvas } from '/js/modules/flows-builder/canvas.js';
import { FlowPalette } from '/js/modules/flows-builder/palette.js';
import { FlowConfig } from '/js/modules/flows-builder/config.js';
import { openVariablesEditor } from '/js/modules/flows-builder/variables.js';
import { TfWindow } from '/js/components/tf-window.js';
import { I18n } from '/js/i18n.js';
import { getNodeDisplayTitle } from '/js/modules/flows-builder/node-i18n.js';
import { nodeColorVar } from '/js/modules/flows-builder/node-visuals.js';
import { checkProcessDocument, processCommand, processEditorLabels, processHasTimerStart } from '/js/modules/flows-builder/bpmn.js';
import { openProcessInstances, openProcessRun, openProcessSchedule, openProcessVariables, processTimerReasonText, processTimerText } from '/js/modules/flows-builder/process-monitor.js';
import { openFormWindow } from '/js/lib/actions/form-window.js';
import '/js/components/tf-input.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-file-input.js';
import '/js/components/tf-table.js';

export function openFlowBuilder(flowId, { mode = 'flow' } = {}) {
  return Router.navigate('flow-builder', { flowId, mode });
}

const FlowBuilderScreen = {
  title: 'Flow Builder',
  _state: null,

  render({ mode = 'flow' } = {}) {
    const process = mode === 'bpmn';
    return `
      <div class="fb-shell ${process ? 'fb-process-shell' : ''}" data-role="shell">
        <header class="fb-topbar">
          <tf-button variant="ghost" size="sm" data-role="back" title="${escapeAttr(I18n.t('flows_builder.back_title'))}"><svg width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" style="transform:rotate(180deg)"><use href="#i-chevron-right"/></svg>${escapeHtml(I18n.t('flows_builder.back'))}</tf-button>
          <div class="fb-topbar-separator"></div>
          <${process ? 'tf-textarea autogrow rows="1"' : 'tf-input'} class="fb-flow-name" data-role="name" aria-label="${escapeAttr(I18n.t('flows_builder.name_label'))}" placeholder="${escapeAttr(I18n.t('flows_builder.name_placeholder'))}"></${process ? 'tf-textarea' : 'tf-input'}>
          <tf-select class="fb-status-select" data-role="status" ${process ? 'hidden' : ''} aria-label="${escapeAttr(I18n.t('flows_builder.status_label'))}">
            <option value="draft">${escapeHtml(I18n.t('flows_builder.status_draft'))}</option>
            <option value="active">${escapeHtml(I18n.t('flows_builder.status_active'))}</option>
            <option value="archived">${escapeHtml(I18n.t('flows_builder.status_archived'))}</option>
          </tf-select>
          <div class="fb-topbar-separator"></div>
          <span class="fb-autosave" data-role="autosave">
            <svg><use href="#i-check"/></svg>
            <span data-role="autosave-text">${escapeHtml(I18n.t('flows_builder.autosave_saved'))}</span>
          </span>
          <div class="fb-topbar-spacer"></div>
          <div class="fb-zoom-controls" role="group" aria-label="${escapeAttr(I18n.t('flows_builder.zoom_label'))}">
            <tf-button variant="ghost" size="sm" icon="min" data-role="zoom-out" title="${escapeAttr(I18n.t('flows_builder.zoom_out'))}"></tf-button>
            <span class="fb-zoom-level" data-role="zoom-level">100%</span>
            <tf-button variant="ghost" size="sm" icon="plus" data-role="zoom-in" title="${escapeAttr(I18n.t('flows_builder.zoom_in'))}"></tf-button>
            <tf-button variant="ghost" size="sm" icon="max" data-role="zoom-fit" title="${escapeAttr(I18n.t('flows_builder.zoom_fit'))}"></tf-button>
          </div>
          <div class="fb-topbar-separator"></div>
          <tf-button variant="ghost" size="sm" icon="code" data-role="variables" title="${escapeAttr(I18n.t('flows_vars.button_title'))}">${escapeHtml(I18n.t('flows_vars.button'))}</tf-button>
          ${process ? `<tf-button variant="ghost" size="sm" icon="arrow-up" data-role="import">${escapeHtml(I18n.t('bpmn.import_xml'))}</tf-button>
          <tf-button variant="ghost" size="sm" icon="download" data-role="export">${escapeHtml(I18n.t('bpmn.export_xml'))}</tf-button>
          <tf-button variant="secondary" size="sm" icon="check" data-role="publish">${escapeHtml(I18n.t('bpmn.publish'))}</tf-button>
          <tf-button variant="ghost" size="sm" icon="play" data-role="run" disabled>${escapeHtml(I18n.t('bpmn.run'))}</tf-button>
          <tf-button variant="ghost" size="sm" icon="clock" data-role="schedule" hidden>${escapeHtml(I18n.t('bpmn.timer_schedule'))}</tf-button>
          <tf-button variant="ghost" size="sm" icon="clock" data-role="instances">${escapeHtml(I18n.t('bpmn.instances'))}</tf-button>` : ''}
          <tf-button variant="primary" size="sm" icon="check" data-role="save">${escapeHtml(I18n.t('flows_builder.save'))}</tf-button>
          <tf-button variant="ghost" size="sm" icon="clock" data-role="history" title="${escapeAttr(I18n.t('flows_builder.history_title'))}"></tf-button>
        </header>
        ${process ? `<div class="fb-process-notice"><tf-chip status="info" data-role="publication">${escapeHtml(I18n.t('bpmn.draft'))}</tf-chip><span data-role="process-hint">${escapeHtml(I18n.t('bpmn.supported_hint'))}</span><tf-input class="fb-timer-timezone" data-role="timer-timezone" label="${escapeAttr(I18n.t('bpmn.timer_timezone'))}" hint="${escapeAttr(I18n.t('bpmn.timer_timezone_hint'))}" placeholder="Europe/Warsaw" hidden></tf-input><tf-button variant="ghost" size="sm" data-role="draft" hidden>${escapeHtml(I18n.t('bpmn.return_draft'))}</tf-button><tf-button variant="ghost" size="sm" data-role="archive">${escapeHtml(I18n.t('bpmn.archive'))}</tf-button><span data-role="timer-summary" hidden></span></div>` : ''}
        <tf-alert data-role="error" tone="danger" hidden></tf-alert>

        <div class="fb-body" data-role="body">
          <aside class="fb-palette" data-role="palette"></aside>
          <main class="fb-canvas-wrap" data-role="canvas-wrap">
            <div data-role="canvas"></div>
            <div class="fb-minimap" data-role="minimap" aria-label="${escapeAttr(I18n.t('flows_builder.minimap_label'))}">
              <span class="fb-minimap-label">${escapeHtml(I18n.t('flows_builder.minimap_name'))}</span>
              <div class="fb-minimap-viewport" data-role="minimap-viewport"></div>
            </div>
          </main>
          <aside class="fb-config" data-role="config"></aside>
        </div>

        <footer class="fb-bottombar">
          <div class="fb-breadcrumb">
            <span class="fb-crumb">${escapeHtml(I18n.t('flows_builder.crumb_root'))}</span>
            <span class="fb-sep">›</span>
            <span class="fb-crumb active" data-role="crumb-name">${escapeHtml(I18n.t('flows_builder.crumb_empty'))}</span>
          </div>
          <div class="fb-bottombar-separator"></div>
          <div class="fb-tool-group" role="group" aria-label="${escapeAttr(I18n.t('flows_builder.history_group'))}">
            <tf-button variant="ghost" size="sm" icon="rotate" data-role="undo" title="${escapeAttr(I18n.t('flows_builder.undo_title'))}"></tf-button>
            <tf-button variant="ghost" size="sm" icon="refresh" data-role="redo" title="${escapeAttr(I18n.t('flows_builder.redo_title'))}"></tf-button>
          </div>
          <div class="fb-bottombar-spacer"></div>
          <span class="fb-stats" data-role="stats">${escapeHtml(I18n.t('flows_builder.stats', { nodes: 0, edges: 0 }))}</span>
          <tf-tabs class="fb-mobile-tabs" value="canvas" variant="pill">
            <tf-tab id="palette" icon="plus" label="${escapeAttr(I18n.t('flows_builder.tab_palette'))}"></tf-tab>
            <tf-tab id="canvas" icon="flow" label="${escapeAttr(I18n.t('flows_builder.tab_canvas'))}"></tf-tab>
            <tf-tab id="config" icon="settings" label="${escapeAttr(I18n.t('flows_builder.tab_config'))}"></tf-tab>
          </tf-tabs>
        </footer>
      </div>
    `;
  },

  async mount({ flowId, mode = 'flow' } = {}) {
    if (!flowId) {
      // No flow selected (e.g. the builder route was hit directly) — the flow
      // list is the real entry point, so just go there. Not a warning.
      Router.navigate('flows');
      return;
    }

    const root = document.querySelector('[data-role="shell"]');
    const state = {
      flowId,
      mode,
      flow: null,
      root,
      canvas: null,
      palette: null,
      config: null,
      dirty: false,
      // System flows (`flows.is_system`) are rejected by the server on update,
      // so the builder opens them as a pure preview.
      readOnly: false,
      autosaveTimer: null,
      saving: false,
      editRevision: 0,
      previewVersion: null,
      processOptions: null,
      saveCommand: processCommand(),
      publishCommand: processCommand(),
      archiveCommand: processCommand(),
      templatesMap: new Map(),
      // Deklaracje zmiennych flow (§3.12 / R10). Wczytane z flow_json i
      // dopisywane z powrotem przy zapisie — bez tego save gubilby sekcje.
      flowVariables: [],
      cleanupFns: [],
    };
    this._state = state;

    // Załaduj flow
    try {
      if (state.mode === 'bpmn') {
        const [response, options] = await Promise.all([
          ApiBinary.one('processDefinitionGetRequest', { definitionId: flowId }),
          ApiBinary.one('processOptionsRequest', {}),
        ]);
        if (!this._current(state)) return;
        state.definition = response.definition;
        state.timerStart = response.timerStart ?? null;
        state.publishedTimerStart = !!response.timerStart;
        state.processOptions = options;
        state.flow = { id: flowId, name: response.definition.name, description: response.definition.description };
        state.readOnly = response.definition.archived;
      } else {
        const detail = await ApiBinary.one('flowDetailRequest', { flowId: String(flowId) });
        if (!this._current(state)) return;
        state.flow = { id: detail.id, name: detail.name, description: detail.description ?? null,
          flow_json: detail.graphJson ?? '{"nodes":[],"edges":[]}', status: detail.status ?? (detail.enabled ? 'active' : 'draft') };
        state.readOnly = !!detail.isSystem;
      }
    } catch (err) {
      if (!this._current(state)) return;
      toast(I18n.t('flows_builder.load_error', { error: err.message }), 'error');
      if (this._current(state)) Router.navigate('flows');
      return;
    }

    let parsed = { nodes: [], edges: [] };
    try {
      if (state.mode === 'bpmn') parsed = state.definition.model;
      else
      parsed = JSON.parse(state.flow.flow_json || state.flow.flowJson || '{"nodes":[],"edges":[]}');
    } catch (_) { parsed = { nodes: [], edges: [] }; }
    state.flowVariables = Array.isArray(parsed.variables) ? parsed.variables : [];

    // Paleta
    state.palette = new FlowPalette(root.querySelector('[data-role="palette"]'), {
      readOnly: state.readOnly,
      mode: state.mode,
      onTemplatesLoaded: (list) => {
        for (const t of list) state.templatesMap.set(t.node_type, t);
        state.canvas?.setTemplates(list);
      },
      onDrop: (tpl, clientX, clientY) => {
        state.canvas.addNodeFromTemplate(tpl, clientX, clientY);
        this._markDirty();
      },
      onAdd: (tpl) => {
        const rect = root.querySelector('[data-role="canvas"]').getBoundingClientRect();
        state.canvas.addNodeFromTemplate(tpl, rect.left + rect.width / 2, rect.top + rect.height / 2);
      },
    });
    await state.palette.init();
    if (!this._current(state)) return;

    // Canvas
    const canvasRoot = root.querySelector('[data-role="canvas"]');
    state.canvas = new FlowCanvas(canvasRoot, {
      readOnly: state.readOnly,
      mode: state.mode,
      onChange: () => {
        this._markDirty();
        this._updateStats();
        this._renderMinimap();
        if (state.mode === 'bpmn' && state.config) this._syncProcessControls();
      },
      onSelect: (node, edge) => {
        const tpl = node ? state.templatesMap.get(node.type) : null;
        if (edge) state.config?.showEdge(edge);
        else state.config?.show(node, tpl);
        const crumb = root.querySelector('[data-role="crumb-name"]');
        if (crumb) crumb.textContent = node ? getNodeDisplayTitle(node, tpl) : (state.flow?.name || I18n.t('flows_builder.crumb_empty'));
      },
      onViewChange: (v) => {
        const zl = root.querySelector('[data-role="zoom-level"]');
        if (zl) zl.textContent = `${Math.round(v.zoom * 100)}%`;
        this._renderMinimap();
      },
      onInvalidConnection: (msg) => {
        toast(msg, 'warning');
      },
    });
    state.canvas.setTemplates(state.palette.getTemplates());
    if (state.mode === 'bpmn') state.canvas.setData(parsed);
    else state.canvas.setData(parsed.nodes || [], parsed.edges || []);

    // Config
    state.config = new FlowConfig(root.querySelector('[data-role="config"]'), {
      readOnly: state.readOnly,
      mode: state.mode,
      processOptions: state.processOptions,
      // Declared flow variables (flow_json.variables) feed the per-node
      // io-mapping editor (§3.12): output_mapping targets must be declared (R10).
      getFlowVariables: () => state.flowVariables || [],
      // Lets the config panel ask the live graph for a node's loop-region role
      // (entry/exit/member) so region-level loop config renders on the entry node.
      getCanvas: () => state.canvas,
      onConfigChange: (id, patch) => { state.canvas.updateNodeConfig(id, patch); },
      onEdgeChange: (id, patch) => state.canvas.updateEdge(id, patch),
      onLabelChange: (id, label) => { state.canvas.updateNodeLabel(id, label); },
      onPositionChange: (id, patch) => {
        const n = state.canvas.nodes.find((x) => x.id === id);
        if (!n) return;
        if (patch.x !== undefined) n.x = patch.x;
        if (patch.y !== undefined) n.y = patch.y;
        state.canvas.render();
        this._markDirty();
      },
      onRawConfigChange: (id, cfg) => {
        const n = state.canvas.nodes.find((x) => x.id === id);
        if (!n) return;
        n.config = cfg;
        state.canvas._renderSingleNode(n);
        this._markDirty();
      },
      onDelete: (id) => { state.canvas.removeNodes([id]); state.config.renderEmpty(); },
      onDuplicate: (id) => { state.canvas.duplicateNodes([id]); },
    });

    // Topbar bindings
    const nameEl = root.querySelector('[data-role="name"]');
    const statusEl = root.querySelector('[data-role="status"]');
    const crumbName = root.querySelector('[data-role="crumb-name"]');
    nameEl.value = state.flow.name || '';
    statusEl.value = state.flow.status || 'draft';
    crumbName.textContent = state.flow.name || I18n.t('flows_builder.crumb_empty');

    if (state.readOnly && state.mode !== 'bpmn') {
      root.classList.add('fb-readonly');
      nameEl.setAttribute('disabled', '');
      statusEl.setAttribute('disabled', '');
      for (const role of ['save', 'variables', 'test', 'undo', 'redo', 'autosave']) {
        root.querySelector(`[data-role="${role}"]`)?.setAttribute('hidden', '');
      }
      const banner = document.createElement('tf-alert');
      banner.setAttribute('tone', 'info');
      banner.setAttribute('data-role', 'system-readonly');
      banner.setAttribute('message', I18n.t(state.mode === 'bpmn' ? 'bpmn.archived_hint' : 'flows_builder.system_readonly'));
      root.insertBefore(banner, root.querySelector('[data-role="body"]'));
    }

    nameEl.addEventListener('input', (event) => { if (event.target !== nameEl) return; this._markDirty(); crumbName.textContent = nameEl.value || I18n.t('flows_builder.crumb_empty'); });
    statusEl.addEventListener('change', () => this._markDirty());

    root.querySelector('[data-role="back"]').addEventListener('click', async () => {
      if (state.dirty) {
        const ok = await TfWindow.confirm({
          title: I18n.t('flows_builder.unsaved_title'),
          message: I18n.t('flows_builder.unsaved_message'),
          confirmLabel: I18n.t('flows_builder.unsaved_confirm'),
          cancelLabel: I18n.t('flows_builder.unsaved_cancel'),
        });
        if (ok && !await this._save()) return;
      }
      Router.navigate('flows');
    });

    root.querySelector('[data-role="zoom-in"]').addEventListener('click', () => state.canvas.zoomBy(1.2));
    root.querySelector('[data-role="zoom-out"]').addEventListener('click', () => state.canvas.zoomBy(1 / 1.2));
    root.querySelector('[data-role="zoom-fit"]').addEventListener('click', () => state.canvas.fitToContent());
    root.querySelector('[data-role="save"]').addEventListener('click', () => this._save());
    root.querySelector('[data-role="variables"]').addEventListener('click', () => this._openVariables());
    root.querySelector('[data-role="history"]').addEventListener('click', () => this._openHistory());

    root.querySelector('[data-role="undo"]').addEventListener('click', () => state.canvas.undo());
    root.querySelector('[data-role="redo"]').addEventListener('click', () => state.canvas.redo());

    // Mobile tabs
    root.querySelector('.fb-mobile-tabs').addEventListener('change', (event) => {
        if (event.target.tagName !== 'TF-TABS') return;
        const tab = event.detail.value;
        const body = root.querySelector('[data-role="body"]');
        body.querySelector('[data-role="palette"]').classList.toggle('open', tab === 'palette');
        body.querySelector('[data-role="config"]').classList.toggle('open', tab === 'config');
        body.classList.toggle('overlay-backdrop', tab !== 'canvas');
    });

    if (state.mode === 'bpmn') {
      root.querySelector('[data-role="publish"]').addEventListener('click', () => this._publish());
      root.querySelector('[data-role="run"]').addEventListener('click', () => this._runProcess());
      root.querySelector('[data-role="schedule"]').addEventListener('click', () => openProcessSchedule(state.flowId));
      const timezone = root.querySelector('[data-role="timer-timezone"]');
      timezone.addEventListener('change', (event) => {
        if (event.target !== timezone || !this._current(state) || state.readOnly) return;
        state.canvas.updateProcessTimezone(timezone.value);
      });
      root.querySelector('[data-role="instances"]').addEventListener('click', () => openProcessInstances(state.flowId));
      root.querySelector('[data-role="import"]').addEventListener('click', () => this._importProcess());
      root.querySelector('[data-role="export"]').addEventListener('click', () => this._exportProcess());
      root.querySelector('[data-role="archive"]').addEventListener('click', () => this._archiveProcess());
      root.querySelector('[data-role="draft"]').addEventListener('click', () => this._loadDraft());
      this._syncProcessControls();
    }

    // Swipe z lewej krawędzi → paleta; z prawej → config (tablet)
    this._setupEdgeSwipes(root);

    // Klawiatura
    const onKey = (ev) => this._onKey(ev);
    document.addEventListener('keydown', onKey);
    state.cleanupFns.push(() => document.removeEventListener('keydown', onKey));

    // Autosave co 10s gdy dirty
    if (!state.readOnly || state.mode === 'bpmn') {
      state.autosaveTimer = setInterval(() => {
        if (this._current(state) && state.dirty && !state.readOnly && !state.saving) this._save({ silent: true });
      }, 10000);
    }

    this._updateStats();
    this._renderMinimap();
    if (!state.readOnly) this._setAutosave('ok');
  },

  async unmount() {
    const s = this._state;
    if (!s) return;
    if (s.autosaveTimer) clearInterval(s.autosaveTimer);
    for (const fn of s.cleanupFns) { try { fn(); } catch (_) {} }
    s.palette?.destroy();
    s.canvas?.destroy();
    s.config?.destroy();
    this._state = null;
  },

  async canUnmount() {
    const state = this._state;
    if (!state?.dirty) return true;
    if (state.saving || state.operationBusy) return false;
    const save = await TfWindow.confirm({ title: I18n.t('flows_builder.unsaved_title'), message: I18n.t('flows_builder.unsaved_message'),
      confirmLabel: I18n.t('flows_builder.unsaved_confirm'), cancelLabel: I18n.t('flows_builder.unsaved_cancel') });
    return save ? this._save() : true;
  },

  _current(state) { return this._state === state && state.root.isConnected; },

  _syncProcessControls() {
    const state = this._state;
    if (!state || state.mode !== 'bpmn' || !this._current(state)) return;
    const readOnly = state.definition.archived || state.previewVersion !== null || !!state.operationBusy;
    state.readOnly = readOnly;
    state.canvas.readOnly = readOnly;
    if (state.config.readOnly !== readOnly) {
      state.config.readOnly = readOnly;
      const selected = state.canvas.nodes.find((node) => state.canvas.selectedIds.has(node.id));
      if (selected) state.config.show(selected, state.templatesMap.get(selected.type));
      else if (state.canvas.selectedEdgeId) state.config.showEdge(state.canvas.edges.find((edge) => edge.id === state.canvas.selectedEdgeId));
      else state.config.renderEmpty();
    }
    if (state.palette.readOnly !== readOnly) { state.palette.readOnly = readOnly; state.palette._render(); }
    state.root.querySelector('[data-role="name"]').toggleAttribute('disabled', readOnly);
    for (const role of ['save', 'variables', 'publish', 'import', 'undo', 'redo']) state.root.querySelector(`[data-role="${role}"]`).toggleAttribute('disabled', readOnly);
    const timedVersion = state.previewVersion === null ? state.publishedTimerStart : processHasTimerStart(state.canvas.getData());
    const run = state.root.querySelector('[data-role="run"]');
    run.toggleAttribute('disabled', !state.definition.publishedVersion || state.definition.archived || !!state.operationBusy || timedVersion);
    run.setAttribute('title', I18n.t(timedVersion ? 'bpmn.timer_start_manual_hint' : 'bpmn.run_hint'));
    const schedule = state.root.querySelector('[data-role="schedule"]');
    schedule.hidden = !state.publishedTimerStart;
    schedule.toggleAttribute('disabled', !!state.operationBusy);
    const timezone = state.root.querySelector('[data-role="timer-timezone"]');
    timezone.hidden = !state.canvas.nodes.some((node) => ['bpmn_timer_start', 'bpmn_timer_catch', 'bpmn_boundary_timer'].includes(node.type));
    timezone.value = state.canvas.processModel.timerTimezone || '';
    timezone.toggleAttribute('disabled', readOnly);
    const timerSummary = state.root.querySelector('[data-role="timer-summary"]');
    timerSummary.hidden = !state.timerStart;
    timerSummary.textContent = state.timerStart ? I18n.t('bpmn.timer_latest_summary', { version: state.definition.publishedVersion, summary: processTimerText(state.timerStart) }) + (state.timerStart.lastReason ? ` · ${I18n.t('bpmn.timer_reason')}: ${processTimerReasonText(state.timerStart.lastReason)}` : '') : '';
    state.root.querySelector('[data-role="draft"]').hidden = state.previewVersion === null;
    state.root.querySelector('[data-role="archive"]').textContent = I18n.t(state.definition.archived ? 'bpmn.unarchive' : 'bpmn.archive');
    state.root.querySelector('[data-role="archive"]').toggleAttribute('disabled', !!state.operationBusy || state.previewVersion !== null);
    state.root.querySelector('[data-role="process-hint"]').textContent = I18n.t(state.definition.archived ? 'bpmn.archived_hint' : 'bpmn.supported_hint');
    state.root.querySelector('[data-role="publication"]').textContent = state.previewVersion !== null
      ? I18n.t('bpmn.preview_version', { version: state.previewVersion })
      : state.definition.archived ? I18n.t('bpmn.archived')
      : state.definition.publishedVersion ? I18n.t('bpmn.draft_published', { version: state.definition.publishedVersion }) : I18n.t('bpmn.draft');
  },

  async _processOperation(action) {
    const state = this._state;
    if (!state || !this._current(state) || state.operationBusy) return;
    state.operationBusy = true;
    state.root.querySelector('[data-role="error"]').hidden = true;
    this._syncProcessControls();
    try { await action(state); }
    catch (error) {
      if (!this._current(state)) return;
      const alert = state.root.querySelector('[data-role="error"]');
      alert.hidden = false;
      alert.setAttribute('message', I18n.t('bpmn.request_error', { error: error.message }));
    } finally {
      state.operationBusy = false;
      if (this._current(state)) this._syncProcessControls();
    }
  },

  async _publish() {
    const state = this._state;
    if (!state || state.readOnly || state.saving) return;
    if (state.dirty && !await this._save()) return;
    if (!this._current(state) || state.dirty) return;
    await this._processOperation(async (current) => {
      if (processHasTimerStart(current.canvas.getData())) {
        const approved = await TfWindow.confirm({ title: I18n.t('bpmn.timer_publish_title'), message: I18n.t('bpmn.timer_publish_hint', { timezone: current.canvas.processModel.timerTimezone || I18n.t('bpmn.timer_timezone_required') }), confirmLabel: I18n.t('bpmn.publish') });
        if (!approved || !this._current(current)) return;
      }
      const response = await ApiBinary.one('processDefinitionPublishRequest', current.publishCommand({ definitionId: current.flowId, expectedRevision: current.definition.draftRevision }));
      if (!this._current(current)) return;
      const previouslyTimed = current.publishedTimerStart;
      current.definition = { ...response.definition, model: response.version.model };
      current.publishedTimerStart = processHasTimerStart(response.version.model);
      current.timerStart = null;
      toast(I18n.t('bpmn.published', { version: response.version.version }), 'success');
      if (current.publishedTimerStart || previouslyTimed) {
        const actual = await ApiBinary.one('processDefinitionGetRequest', { definitionId: current.flowId });
        if (!this._current(current)) return;
        current.timerStart = actual.timerStart ?? null;
      }
    });
  },

  async _runProcess() {
    const state = this._state;
    if (!state || state.definition.archived || !state.definition.publishedVersion) return;
    const version = state.previewVersion ?? state.definition.publishedVersion;
    await this._processOperation(async (current) => {
      const response = await ApiBinary.one('processVersionGetRequest', { definitionId: current.flowId, version });
      if (!this._current(current)) return;
      if (processHasTimerStart(response.version.model)) {
        if (version === current.definition.publishedVersion) current.publishedTimerStart = true;
        await openProcessSchedule(current.flowId);
        return;
      }
      openProcessRun({ ...current.definition, model: response.version.model }, [response.version]);
    });
  },

  async _archiveProcess() {
    const state = this._state;
    if (!state || state.operationBusy || state.previewVersion !== null) return;
    if (state.dirty && !await this._save()) return;
    const archived = !state.definition.archived;
    const approved = await TfWindow.confirm({ title: I18n.t(archived ? 'bpmn.archive' : 'bpmn.unarchive'), message: I18n.t(archived ? 'bpmn.archive_hint' : 'bpmn.unarchive_hint') });
    if (!approved || !this._current(state)) return;
    await this._processOperation(async (current) => {
      const response = await ApiBinary.one('processDefinitionArchiveRequest', current.archiveCommand({ definitionId: current.flowId, expectedRevision: current.definition.draftRevision, archived }));
      if (!this._current(current)) return;
      current.definition = response.definition;
      if (current.publishedTimerStart) {
        const actual = await ApiBinary.one('processDefinitionGetRequest', { definitionId: current.flowId });
        if (!this._current(current)) return;
        current.timerStart = actual.timerStart ?? null;
      }
      current.root.querySelector('[data-role="system-readonly"]')?.remove();
      current.root.classList.remove('fb-readonly');
    });
  },

  async _loadDraft() {
    await this._processOperation(async (state) => {
      const response = await ApiBinary.one('processDefinitionGetRequest', { definitionId: state.flowId });
      if (!this._current(state)) return;
      state.definition = response.definition;
      state.timerStart = response.timerStart ?? null;
      state.publishedTimerStart = !!response.timerStart;
      state.previewVersion = null;
      state.canvas.setData(response.definition.model);
      state.root.querySelector('[data-role="name"]').value = response.definition.name;
      state.dirty = false;
      this._setAutosave('ok');
    });
  },

  _importProcess() {
    const state = this._state;
    if (!state || state.readOnly) return;
    const section = document.createElement('div');
    section.className = 'fb-process-import';
    section.innerHTML = `<tf-file-input accept=".bpmn,.xml" label="${escapeAttr(I18n.t('bpmn.xml_file'))}"></tf-file-input>
      <tf-code-editor language="html" aria-label="${escapeAttr(I18n.t('bpmn.xml_document'))}"></tf-code-editor>
      <tf-alert data-file-error tone="danger" hidden></tf-alert>`;
    section.querySelector('tf-code-editor').labels = processEditorLabels();
    let fileGeneration = 0;
    let fileBusy = false;
    const win = openFormWindow({ title: I18n.t('bpmn.import_xml'), icon: 'arrow-up', subject: state.definition.name,
      note: { text: I18n.t('bpmn.import_hint') }, sections: [section], submitLabel: I18n.t('bpmn.import_apply'),
      canSubmit: () => !fileBusy && !!section.querySelector('tf-code-editor').value.trim() && this._current(state) && !state.readOnly,
      collect: () => section.querySelector('tf-code-editor').value,
      onSubmit: async (xml) => {
        checkProcessDocument(xml);
        const response = await ApiBinary.one('processXmlImportRequest', { xml });
        if (!this._current(state) || !win.isConnected || state.readOnly) throw new Error(I18n.t('bpmn.editor_changed'));
        const fatal = response.diagnostics.filter((diagnostic) => diagnostic.fatal);
        if (fatal.length) throw new Error(I18n.t('bpmn.import_rejected', { error: fatal.map((diagnostic) => diagnostic.message).join('\n') }));
        state.canvas.setData(response.model);
        this._markDirty();
        this._updateStats();
        this._renderMinimap();
        this._syncProcessControls();
        return { message: I18n.t('bpmn.imported_draft') };
      },
    });
    section.querySelector('tf-file-input').addEventListener('change', async (event) => {
      if (event.target.tagName !== 'TF-FILE-INPUT') return;
      const generation = ++fileGeneration;
      const file = event.detail.files[0];
      if (!file) return;
      const error = section.querySelector('[data-file-error]');
      error.hidden = true;
      fileBusy = true;
      try {
        if (file.size > 512 * 1024) throw new Error(I18n.t('bpmn.document_too_large'));
        const xml = await file.text();
        if (generation !== fileGeneration || !win.isConnected) return;
        checkProcessDocument(xml);
        section.querySelector('tf-code-editor').value = xml;
      } catch (failure) {
        if (generation !== fileGeneration || !win.isConnected) return;
        error.hidden = false;
        error.setAttribute('message', failure.message);
      } finally {
        if (generation === fileGeneration) { fileBusy = false; section.dispatchEvent(new CustomEvent('change', { bubbles: true })); }
      }
    });
  },

  async _exportProcess() {
    const state = this._state;
    if (!state || state.operationBusy) return;
    if (state.previewVersion === null && state.dirty && !await this._save()) return;
    await this._processOperation(async (current) => {
      const response = await ApiBinary.one('processXmlExportRequest', { definitionId: current.flowId, version: current.previewVersion });
      if (!this._current(current)) return;
      const url = URL.createObjectURL(new Blob([response.xml], { type: 'application/xml;charset=utf-8' }));
      const link = document.createElement('a');
      link.href = url;
      link.download = `${current.definition.name.replace(/[^\p{L}\p{N}_-]/gu, '_')}.bpmn`;
      link.click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    });
  },

  async _openProcessVersions() {
    const state = this._state;
    if (!state || !this._current(state)) return;
    const win = document.createElement('tf-window');
    win.setAttribute('title', I18n.t('bpmn.versions'));
    win.setAttribute('modal', '');
    win.setAttribute('buttons', 'close');
    win.setAttribute('width', '800');
    win.setAttribute('initial-x', 'center');
    win.setAttribute('initial-y', 'center');
    win.innerHTML = `<div slot="body" class="fb-process-content"><tf-alert tone="danger" data-error hidden></tf-alert><tf-table page-size="25" page="1" empty-message="${escapeAttr(I18n.t('bpmn.versions_empty'))}"><tf-column key="label" label="${escapeAttr(I18n.t('bpmn.version'))}"></tf-column><tf-column key="at" label="${escapeAttr(I18n.t('bpmn.published_at'))}"></tf-column></tf-table></div>`;
    document.body.appendChild(win);
    const table = win.querySelector('tf-table');
    let offset = 0;
    let generation = 0;
    let previewGeneration = 0;
    const current = () => this._current(state) && win.isConnected;
    const error = (failure) => {
      if (!current()) return;
      const alert = win.querySelector('[data-error]');
      alert.hidden = false;
      alert.setAttribute('message', I18n.t('bpmn.request_error', { error: failure.message }));
    };
    table.rowKey = 'version';
    table.rowActionsKey = (row) => row.version;
    table.rowActions = (_row, _index, selected) => {
      const button = document.createElement('tf-button');
      button.setAttribute('variant', 'secondary');
      button.textContent = I18n.t('bpmn.preview');
      button.addEventListener('click', async () => {
        const request = ++previewGeneration;
        const version = selected().version;
        if (state.dirty && !await this._save()) return;
        if (!current() || request !== previewGeneration || state.dirty) return;
        try {
          const response = await ApiBinary.one('processVersionGetRequest', { definitionId: state.flowId, version });
          if (!current() || request !== previewGeneration) return;
          state.previewVersion = response.version.version;
          state.canvas.setData(response.version.model);
          this._syncProcessControls();
          this._updateStats();
          this._renderMinimap();
          win.close();
        } catch (failure) { if (request === previewGeneration) error(failure); }
      });
      return button;
    };
    const load = async () => {
      const request = ++generation;
      try {
        const response = await ApiBinary.one('processVersionListRequest', { definitionId: state.flowId, offset, limit: 25 });
        if (!current() || request !== generation) return;
        table.setAttribute('page', String(offset / 25 + 1));
        table.setAttribute('total', String(response.total));
        table.rows = response.versions.map((version) => ({ ...version, label: I18n.t('bpmn.version_number', { version: version.version }), at: new Date(version.publishedAtMs).toLocaleString(I18n.getLanguage()) }));
      } catch (failure) { if (request === generation) error(failure); }
    };
    table.addEventListener('page-change', (event) => { offset = (event.detail.page - 1) * 25; load(); });
    await load();
  },

  _markDirty() {
    const s = this._state;
    if (!s || s.readOnly) return;
    s.dirty = true;
    s.editRevision += 1;
    this._setAutosave('pending');
    this._updateStats();
  },

  // Jeden punkt, w którym powstaje napis o stanie zapisu — „Zapisano" obok
  // trzech bloków z czerwoną obwódką i wierszem „wymagane" przeczy płótnu, więc
  // stan zapisany raportuje też, ile bloków czeka na konfigurację.
  _setAutosave(kind) {
    const s = this._state;
    if (!s) return;
    const el = s.root.querySelector('[data-role="autosave"]');
    const t = s.root.querySelector('[data-role="autosave-text"]');
    if (!el || !t) return;
    const incomplete = s.canvas.incompleteNodeCount();
    const warn = kind === 'ok' && incomplete > 0;
    el.classList.toggle('pending', kind === 'pending');
    el.classList.toggle('error', kind === 'error');
    el.classList.toggle('warn', warn);
    const glyph = (kind === 'error' || warn) ? 'alert' : 'check';
    el.querySelector('svg use')?.setAttribute('href', `#i-${glyph}`);
    if (kind === 'pending') t.textContent = I18n.t('flows_builder.autosave_pending');
    else if (kind === 'error') t.textContent = I18n.t('flows_builder.autosave_error');
    else if (warn) t.textContent = I18n.t('flows_builder.autosave_incomplete', { count: incomplete });
    else t.textContent = I18n.t('flows_builder.autosave_saved');
  },

  _updateStats() {
    const s = this._state;
    if (!s || !s.canvas) return;
    const stats = s.root.querySelector('[data-role="stats"]');
    if (stats) stats.textContent = I18n.t('flows_builder.stats', { nodes: s.canvas.nodes.length, edges: s.canvas.edges.length });
  },

  async _save({ silent = false } = {}) {
    const s = this._state;
    if (!s || s.saving || s.readOnly) return false;
    // Walidacja klient-side przed wyslaniem do backendu — porty, wiszace
    // krawedzie, cykle. Backend ma swoj validate_flow_json_str, wiec to jest
    // ochrona przed zbednym round-tripem i czytelnym komunikatem inline.
    const errors = s.canvas.validate ? s.canvas.validate() : [];
    if (errors.length > 0) {
      this._setAutosave('error');
      if (!silent) toast(errors[0], 'error');
      return false;
    }
    s.saving = true;
    const editRevision = s.editRevision;
    try {
      const nameEl = s.root.querySelector('[data-role="name"]');
      const statusEl = s.root.querySelector('[data-role="status"]');
      const name = (nameEl.value || '').trim() || I18n.t('flows_builder.default_name');
      const status = statusEl.value || 'draft';
      const data = s.canvas.getData();
      if (s.mode === 'bpmn') {
        checkProcessDocument(JSON.stringify(data));
        const response = await ApiBinary.one('processDefinitionSaveRequest', s.saveCommand({
          definitionId: s.flowId, expectedRevision: s.definition.draftRevision,
          name, description: s.definition.description, model: data,
        }));
        if (!this._current(s)) return false;
        s.definition = response.definition;
        s.flow.name = response.definition.name;
        s.dirty = s.editRevision !== editRevision;
        this._syncProcessControls();
        this._setAutosave(s.dirty ? 'pending' : 'ok');
        if (!silent) toast(I18n.t('flows_builder.save_success'), 'success');
        return true;
      }
      // `variables` dopisujemy tylko gdy niepuste — pusty flow round-trippuje
      // byte-identycznie z legacy flow_json (serde skip_serializing_if).
      const graph = { nodes: data.nodes, edges: data.edges };
      if (Array.isArray(s.flowVariables) && s.flowVariables.length > 0) {
        graph.variables = s.flowVariables;
      }
      const graphJson = JSON.stringify(graph);
      await ApiBinary.action('flowUpdateRequest', {
        flowId: String(s.flowId),
        name,
        description: s.flow.description ?? null,
        flowJson: graphJson,
        status,
      });
      s.flow = {
        ...s.flow,
        name,
        status,
        flow_json: graphJson,
      };
      if (!this._current(s)) return false;
      s.dirty = s.editRevision !== editRevision;
      this._setAutosave('ok');
      if (!silent) toast(I18n.t('flows_builder.save_success'), 'success');
      return true;
    } catch (err) {
      if (!this._current(s)) return false;
      this._setAutosave('error');
      toast(I18n.t('flows_builder.save_error', { error: err.message }), 'error');
      return false;
    } finally {
      s.saving = false;
    }
  },

  async _openVariables() {
    const s = this._state;
    if (!s) return;
    if (s.readOnly) return;
    if (s.mode === 'bpmn') {
      openProcessVariables(s.canvas.processModel.variables, (values) => {
        if (!this._current(s) || s.readOnly) return;
        s.canvas.processModel.variables = values;
        this._markDirty();
      });
      return;
    }
    const result = await openVariablesEditor(s.flowVariables || []);
    if (result === null || !this._current(s)) return;
    s.flowVariables = result;
    this._markDirty();
    toast(I18n.t('flows_vars.saved', { count: result.length }), 'success');
  },

  _onKey(ev) {
    const s = this._state;
    if (!s) return;
    if (ev.composedPath().some((element) => element.tagName?.startsWith('TF-') || element.isContentEditable)) return;
    const tag = (ev.target.tagName || '').toUpperCase();
    if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
    if (s.readOnly) return;
    if (ev.key === 'Delete' || ev.key === 'Backspace') {
      s.canvas.deleteSelected();
      ev.preventDefault();
      return;
    }
    if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === 's') {
      ev.preventDefault();
      this._save();
      return;
    }
    if ((ev.ctrlKey || ev.metaKey) && !ev.shiftKey && ev.key.toLowerCase() === 'z') {
      ev.preventDefault();
      s.canvas.undo();
      return;
    }
    if ((ev.ctrlKey || ev.metaKey) && (ev.key.toLowerCase() === 'y' || (ev.shiftKey && ev.key.toLowerCase() === 'z'))) {
      ev.preventDefault();
      s.canvas.redo();
      return;
    }
    if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === 'd') {
      ev.preventDefault();
      if (s.canvas.selectedIds.size) s.canvas.duplicateNodes([...s.canvas.selectedIds]);
      return;
    }
  },

  _setupEdgeSwipes(root) {
    const body = root.querySelector('[data-role="body"]');
    const paletteEl = body.querySelector('[data-role="palette"]');
    const configEl = body.querySelector('[data-role="config"]');
    let startX = null;
    let target = null;
    body.addEventListener('touchstart', (ev) => {
      if (window.innerWidth >= 1024) return;
      const x = ev.touches[0].clientX;
      const rect = body.getBoundingClientRect();
      if (x - rect.left < 24) { startX = x; target = 'palette'; }
      else if (rect.right - x < 24) { startX = x; target = 'config'; }
      else { startX = null; target = null; }
    }, { passive: true });
    body.addEventListener('touchmove', (ev) => {
      if (startX == null) return;
      const dx = ev.touches[0].clientX - startX;
      if (target === 'palette' && dx > 60) { paletteEl.classList.add('open'); body.classList.add('overlay-backdrop'); startX = null; }
      if (target === 'config' && dx < -60) { configEl.classList.add('open'); body.classList.add('overlay-backdrop'); startX = null; }
    }, { passive: true });
    body.addEventListener('click', (ev) => {
      if (window.innerWidth >= 1024) return;
      if (ev.target === body) {
        paletteEl.classList.remove('open');
        configEl.classList.remove('open');
        body.classList.remove('overlay-backdrop');
      }
    });
  },

  _renderMinimap() {
    const s = this._state;
    if (!s) return;
    const mini = s.root.querySelector('[data-role="minimap"]');
    const vp = s.root.querySelector('[data-role="minimap-viewport"]');
    if (!mini || !vp) return;
    // Usuń poprzednie kropki
    mini.querySelectorAll('.fb-minimap-node').forEach((el) => el.remove());
    const nodes = s.canvas.nodes;
    if (nodes.length === 0) return;
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    for (const n of nodes) {
      minX = Math.min(minX, n.x); minY = Math.min(minY, n.y);
      maxX = Math.max(maxX, n.x + (s.mode === 'bpmn' ? n.width : 220)); maxY = Math.max(maxY, n.y + (s.mode === 'bpmn' ? n.height : 96));
    }
    const w = Math.max(1, maxX - minX);
    const h = Math.max(1, maxY - minY);
    const captionHeight = s.mode === 'bpmn' ? 18 : 0;
    const miniW = s.mode === 'bpmn' ? mini.clientWidth : 180;
    const miniH = s.mode === 'bpmn' ? mini.clientHeight - captionHeight : 120;
    if (miniW <= 0 || miniH <= 0) return;
    const scale = Math.min(miniW / w, miniH / h) * 0.85;
    const offX = (miniW - w * scale) / 2;
    const offY = captionHeight + (miniH - h * scale) / 2;
    for (const n of nodes) {
      const dot = document.createElement('div');
      dot.className = 'fb-minimap-node';
      dot.style.setProperty('--node-color', `var(${nodeColorVar(n.type, s.templatesMap.get(n.type)?.category)})`);
      dot.style.left = `${offX + (n.x - minX) * scale}px`;
      dot.style.top = `${offY + (n.y - minY) * scale}px`;
      dot.style.width = `${Math.max(8, (s.mode === 'bpmn' ? n.width : 220) * scale)}px`;
      dot.style.height = `${Math.max(4, (s.mode === 'bpmn' ? n.height : 40) * scale)}px`;
      mini.appendChild(dot);
    }
    // Viewport
    const canvas = s.canvas;
    const rect = canvas.root.getBoundingClientRect();
    const vWorldW = rect.width / canvas.view.zoom;
    const vWorldH = rect.height / canvas.view.zoom;
    const vWorldX = -canvas.view.x / canvas.view.zoom;
    const vWorldY = -canvas.view.y / canvas.view.zoom;
    const left = offX + (vWorldX - minX) * scale;
    const top = offY + (vWorldY - minY) * scale;
    const width = Math.max(8, vWorldW * scale);
    const height = Math.max(8, vWorldH * scale);
    if (s.mode === 'bpmn') {
      const x = Math.max(0, Math.min(miniW, left));
      const y = Math.max(captionHeight, Math.min(mini.clientHeight, top));
      const right = Math.max(x, Math.min(miniW, left + width));
      const bottom = Math.max(y, Math.min(mini.clientHeight, top + height));
      vp.hidden = right === x || bottom === y;
      vp.style.left = `${x}px`;
      vp.style.top = `${y}px`;
      vp.style.width = `${right - x}px`;
      vp.style.height = `${bottom - y}px`;
    } else {
      vp.style.left = `${left}px`;
      vp.style.top = `${top}px`;
      vp.style.width = `${width}px`;
      vp.style.height = `${height}px`;
    }
  },

  async _openHistory() {
    const s = this._state;
    if (!s) return;
    if (s.mode === 'bpmn') return this._openProcessVersions();
    let versions = [];
    try {
      const resp = await ApiBinary.one('flowVersionListRequest', { flowId: String(s.flowId) });
      versions = resp?.versions ?? [];
    } catch (err) {
      toast(I18n.t('flows_builder.history_load_error', { error: err.message }), 'error');
      return;
    }

    const body = document.createElement('div');
    body.style.display = 'flex';
    body.style.flexDirection = 'column';
    body.style.gap = '8px';
    body.style.minWidth = '420px';
    if (!versions.length) {
      body.innerHTML = `<div style="padding:24px;text-align:center;color:var(--tf-text-3);">${escapeHtml(I18n.t('flows_builder.history_empty'))}</div>`;
    } else {
      body.innerHTML = versions.map((v) => {
        const id = v.id ?? v.versionId ?? v.version_id;
        const author = v.author || v.created_by || v.createdBy || '—';
        const ts = v.created_at_epoch || v.createdAtEpoch || v.created_at || 0;
        const rel = typeof ts === 'number' ? formatRelative(ts) : '—';
        const name = v.name || s.flow.name || '—';
        const status = v.status || '—';
        return `
          <div class="fb-history-item" data-version-id="${escapeAttr(id)}" style="display:flex;align-items:center;gap:10px;padding:10px;border:1px solid var(--tf-border);border-radius:10px;">
            <div style="flex:1;min-width:0;">
              <div style="font-weight:600;font-size:13px;">${escapeHtml(name)}</div>
              <div style="font-size:11px;color:var(--tf-text-3);">${escapeHtml(rel)} · ${escapeHtml(author)} · ${escapeHtml(status)}</div>
            </div>
            <tf-button variant="secondary" size="sm" icon="rotate" data-action="restore">${escapeHtml(I18n.t('flows_builder.restore'))}</tf-button>
          </div>`;
      }).join('');
    }

    const foot = document.createElement('div');
    foot.innerHTML = `<tf-button variant="ghost" data-action="close">${escapeHtml(I18n.t('flows_builder.close'))}</tf-button>`;

    const win = document.createElement('tf-window');
    win.setAttribute('title', I18n.t('flows_builder.history_title'));
    win.setAttribute('icon', 'clock');
    win.setAttribute('buttons', 'close');
    win.setAttribute('width', '520');
    win.setAttribute('initial-x', 'center');
    win.setAttribute('initial-y', 'center');
    const bWrap = document.createElement('div'); bWrap.slot = 'body'; bWrap.appendChild(body);
    const fWrap = document.createElement('div'); fWrap.slot = 'footer'; fWrap.appendChild(foot);
    win.appendChild(bWrap); win.appendChild(fWrap);
    const backdrop = document.createElement('div');
    backdrop.className = 'tf-window-backdrop';
    document.body.appendChild(backdrop);
    document.body.appendChild(win);
    const cleanup = () => { if (win.isConnected) win.remove(); if (backdrop.isConnected) backdrop.remove(); };

    win.addEventListener('action', (ev) => {
      if (ev.detail?.action === 'close') cleanup();
    });
    foot.addEventListener('click', (ev) => {
      if (ev.target.closest('[data-action="close"]')) cleanup();
    });
    body.addEventListener('click', async (ev) => {
      const btn = ev.target.closest('[data-action="restore"]');
      if (!btn) return;
      const item = btn.closest('.fb-history-item');
      const versionId = item?.dataset.versionId;
      if (!versionId) return;
      const ok = await TfWindow.confirm({
        title: I18n.t('flows_builder.restore_confirm_title'),
        message: I18n.t('flows_builder.restore_confirm_message'),
        description: I18n.t('flows_builder.restore_confirm_description'),
        confirmLabel: I18n.t('flows_builder.restore_confirm_label'),
        cancelLabel: I18n.t('flows_builder.restore_cancel_label'),
      });
      if (!ok) return;
      try {
        await ApiBinary.action('flowVersionRestoreRequest', {
          flowId: String(s.flowId),
          versionId: String(versionId),
        });
        toast(I18n.t('flows_builder.restore_success'), 'success');
        cleanup();
        // Reload builder
        openFlowBuilder(s.flowId);
      } catch (err) {
        toast(I18n.t('flows_builder.restore_error', { error: err.message }), 'error');
      }
    });
  },
};

export default FlowBuilderScreen;
