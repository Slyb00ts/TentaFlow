// =============================================================================
// Plik: modules/flows-builder.js
// Opis: Ekran Flow Buildera - orkiestracja paleta / canvas / config,
//       topbar (nazwa, status, zoom, testuj, zapisz, history), bottombar
//       (breadcrumb, undo/redo, mobile tabs), autosave co 10s, historia wersji.
// =============================================================================

import { escapeHtml, escapeAttr, formatRelative, toast, byId } from '/js/utils.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { Router } from '/js/router.js';
import { FlowCanvas } from '/js/modules/flows-builder/canvas.js';
import { FlowPalette } from '/js/modules/flows-builder/palette.js';
import { FlowConfig } from '/js/modules/flows-builder/config.js';
import { TfWindow } from '/js/components/tf-window.js';
import { I18n } from '/js/i18n.js';

// Stan aktualnie otwartego buildera (przechowywany poza klasa dla param route'a).
let pendingFlowId = null;
let pendingMe = null;

export function openFlowBuilder(flowId, me) {
  pendingFlowId = flowId;
  pendingMe = me || null;
  Router.navigate('flow-builder');
}

const FlowBuilderScreen = {
  title: 'Flow Builder',
  _state: null,

  render() {
    return `
      <div class="fb-shell" data-role="shell">
        <header class="fb-topbar">
          <tf-button variant="ghost" size="sm" data-role="back" title="${escapeAttr(I18n.t('flows_builder.back_title'))}"><svg width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" style="transform:rotate(180deg)"><use href="#i-chevron-right"/></svg>${escapeHtml(I18n.t('flows_builder.back'))}</tf-button>
          <div class="fb-topbar-separator"></div>
          <input class="fb-flow-name" data-role="name" aria-label="${escapeAttr(I18n.t('flows_builder.name_label'))}" placeholder="${escapeAttr(I18n.t('flows_builder.name_placeholder'))}">
          <select class="fb-status-select" data-role="status" aria-label="${escapeAttr(I18n.t('flows_builder.status_label'))}">
            <option value="draft">${escapeHtml(I18n.t('flows_builder.status_draft'))}</option>
            <option value="active">${escapeHtml(I18n.t('flows_builder.status_active'))}</option>
            <option value="archived">${escapeHtml(I18n.t('flows_builder.status_archived'))}</option>
          </select>
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
          <tf-button variant="ghost" size="sm" icon="play" data-role="test" title="${escapeAttr(I18n.t('flows_builder.test_title'))}">${escapeHtml(I18n.t('flows_builder.test'))}</tf-button>
          <tf-button variant="primary" size="sm" icon="check" data-role="save">${escapeHtml(I18n.t('flows_builder.save'))}</tf-button>
          <tf-button variant="ghost" size="sm" icon="clock" data-role="history" title="${escapeAttr(I18n.t('flows_builder.history_title'))}"></tf-button>
        </header>

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
          <div class="fb-mobile-tabs">
            <button class="fb-mobile-tab" data-mobile-tab="palette"><svg><use href="#i-plus"/></svg>${escapeHtml(I18n.t('flows_builder.tab_palette'))}</button>
            <button class="fb-mobile-tab active" data-mobile-tab="canvas"><svg><use href="#i-flow"/></svg>${escapeHtml(I18n.t('flows_builder.tab_canvas'))}</button>
            <button class="fb-mobile-tab" data-mobile-tab="config"><svg><use href="#i-settings"/></svg>${escapeHtml(I18n.t('flows_builder.tab_config'))}</button>
          </div>
        </footer>
      </div>
    `;
  },

  async mount() {
    const flowId = pendingFlowId;
    if (!flowId) {
      toast(I18n.t('flows_builder.missing_id'), 'warning');
      Router.navigate('flows');
      return;
    }

    const root = document.querySelector('[data-role="shell"]');
    const state = {
      flowId,
      me: pendingMe,
      flow: null,
      root,
      canvas: null,
      palette: null,
      config: null,
      dirty: false,
      autosaveTimer: null,
      saving: false,
      templatesMap: new Map(),
      cleanupFns: [],
    };
    this._state = state;

    // Załaduj flow
    try {
      const detail = await ApiBinary.one('flowDetailRequest', { flowId: String(flowId) });
      state.flow = {
        id: detail.id,
        name: detail.name,
        description: detail.description ?? null,
        flow_json: detail.graphJson ?? '{"nodes":[],"edges":[]}',
        status: detail.status ?? (detail.enabled ? 'active' : 'draft'),
      };
    } catch (err) {
      toast(I18n.t('flows_builder.load_error', { error: err.message }), 'error');
      Router.navigate('flows');
      return;
    }

    let parsed = { nodes: [], edges: [] };
    try {
      parsed = JSON.parse(state.flow.flow_json || state.flow.flowJson || '{"nodes":[],"edges":[]}');
    } catch (_) { parsed = { nodes: [], edges: [] }; }

    // Paleta
    state.palette = new FlowPalette(root.querySelector('[data-role="palette"]'), {
      onTemplatesLoaded: (list) => {
        for (const t of list) state.templatesMap.set(t.node_type, t);
        state.canvas?.setTemplates(list);
      },
      onDrop: (tpl, clientX, clientY) => {
        state.canvas.addNodeFromTemplate(tpl, clientX, clientY);
        this._markDirty();
      },
    });
    await state.palette.init();

    // Canvas
    const canvasRoot = root.querySelector('[data-role="canvas"]');
    state.canvas = new FlowCanvas(canvasRoot, {
      onChange: () => {
        this._markDirty();
        this._updateStats();
        this._renderMinimap();
      },
      onSelect: (node) => {
        const tpl = node ? state.templatesMap.get(node.type) : null;
        state.config.show(node, tpl);
        const crumb = root.querySelector('[data-role="crumb-name"]');
        if (crumb) crumb.textContent = node ? (node.label || node.type) : (state.flow?.name || I18n.t('flows_builder.crumb_empty'));
      },
      onViewChange: (v) => {
        const zl = root.querySelector('[data-role="zoom-level"]');
        if (zl) zl.textContent = `${Math.round(v.zoom * 100)}%`;
        this._renderMinimap();
      },
    });
    state.canvas.setTemplates(state.palette.getTemplates());
    state.canvas.setData(parsed.nodes || [], parsed.edges || []);

    // Config
    state.config = new FlowConfig(root.querySelector('[data-role="config"]'), {
      onConfigChange: (id, patch) => { state.canvas.updateNodeConfig(id, patch); },
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

    nameEl.addEventListener('input', () => { this._markDirty(); crumbName.textContent = nameEl.value || I18n.t('flows_builder.crumb_empty'); });
    statusEl.addEventListener('change', () => this._markDirty());

    root.querySelector('[data-role="back"]').addEventListener('click', async () => {
      if (state.dirty) {
        const ok = await TfWindow.confirm({
          title: I18n.t('flows_builder.unsaved_title'),
          message: I18n.t('flows_builder.unsaved_message'),
          confirmLabel: I18n.t('flows_builder.unsaved_confirm'),
          cancelLabel: I18n.t('flows_builder.unsaved_cancel'),
        });
        if (ok) await this._save();
      }
      Router.navigate('flows');
    });

    root.querySelector('[data-role="zoom-in"]').addEventListener('click', () => state.canvas.zoomBy(1.2));
    root.querySelector('[data-role="zoom-out"]').addEventListener('click', () => state.canvas.zoomBy(1 / 1.2));
    root.querySelector('[data-role="zoom-fit"]').addEventListener('click', () => state.canvas.fitToContent());
    root.querySelector('[data-role="save"]').addEventListener('click', () => this._save());
    root.querySelector('[data-role="test"]').addEventListener('click', () => {
      toast(I18n.t('flows_builder.test_soon'), 'info');
    });
    root.querySelector('[data-role="history"]').addEventListener('click', () => this._openHistory());

    root.querySelector('[data-role="undo"]').addEventListener('click', () => state.canvas.undo());
    root.querySelector('[data-role="redo"]').addEventListener('click', () => state.canvas.redo());

    // Mobile tabs
    root.querySelectorAll('[data-mobile-tab]').forEach((btn) => {
      btn.addEventListener('click', () => {
        const tab = btn.dataset.mobileTab;
        root.querySelectorAll('[data-mobile-tab]').forEach((x) => x.classList.toggle('active', x === btn));
        const body = root.querySelector('[data-role="body"]');
        body.querySelector('[data-role="palette"]').classList.toggle('open', tab === 'palette');
        body.querySelector('[data-role="config"]').classList.toggle('open', tab === 'config');
        body.classList.toggle('overlay-backdrop', tab !== 'canvas');
      });
    });

    // Swipe z lewej krawędzi → paleta; z prawej → config (tablet)
    this._setupEdgeSwipes(root);

    // Klawiatura
    const onKey = (ev) => this._onKey(ev);
    document.addEventListener('keydown', onKey);
    state.cleanupFns.push(() => document.removeEventListener('keydown', onKey));

    // Autosave co 10s gdy dirty
    state.autosaveTimer = setInterval(() => {
      if (state.dirty && !state.saving) this._save({ silent: true });
    }, 10000);

    this._updateStats();
    this._renderMinimap();
    this._setAutosave('ok', I18n.t('flows_builder.autosave_saved'));
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
    pendingFlowId = null;
  },

  _markDirty() {
    const s = this._state;
    if (!s) return;
    s.dirty = true;
    this._setAutosave('pending', I18n.t('flows_builder.autosave_pending'));
    this._updateStats();
  },

  _setAutosave(kind, text) {
    const s = this._state;
    if (!s) return;
    const el = s.root.querySelector('[data-role="autosave"]');
    const t = s.root.querySelector('[data-role="autosave-text"]');
    if (!el || !t) return;
    el.classList.remove('pending', 'error');
    if (kind === 'pending') el.classList.add('pending');
    if (kind === 'error') el.classList.add('error');
    t.textContent = text;
  },

  _updateStats() {
    const s = this._state;
    if (!s || !s.canvas) return;
    const stats = s.root.querySelector('[data-role="stats"]');
    if (stats) stats.textContent = I18n.t('flows_builder.stats', { nodes: s.canvas.nodes.length, edges: s.canvas.edges.length });
  },

  async _save({ silent = false } = {}) {
    const s = this._state;
    if (!s || s.saving) return;
    s.saving = true;
    try {
      const nameEl = s.root.querySelector('[data-role="name"]');
      const statusEl = s.root.querySelector('[data-role="status"]');
      const name = (nameEl.value || '').trim() || I18n.t('flows_builder.default_name');
      const status = statusEl.value || 'draft';
      const data = s.canvas.getData();
      await ApiBinary.action('flowUpdateRequest', {
        flowId: String(s.flowId),
        name,
        description: s.flow.description ?? null,
        flowJson: JSON.stringify({ nodes: data.nodes, edges: data.edges }),
        status,
      });
      s.flow = {
        ...s.flow,
        name,
        status,
        flow_json: JSON.stringify({ nodes: data.nodes, edges: data.edges }),
      };
      s.dirty = false;
      this._setAutosave('ok', I18n.t('flows_builder.autosave_saved'));
      if (!silent) toast(I18n.t('flows_builder.save_success'), 'success');
    } catch (err) {
      this._setAutosave('error', I18n.t('flows_builder.autosave_error'));
      toast(I18n.t('flows_builder.save_error', { error: err.message }), 'error');
    } finally {
      s.saving = false;
    }
  },

  _onKey(ev) {
    const s = this._state;
    if (!s) return;
    const tag = (ev.target.tagName || '').toUpperCase();
    if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
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
      maxX = Math.max(maxX, n.x + 220); maxY = Math.max(maxY, n.y + 96);
    }
    const w = Math.max(1, maxX - minX);
    const h = Math.max(1, maxY - minY);
    const miniW = 180, miniH = 120;
    const scale = Math.min(miniW / w, miniH / h) * 0.85;
    const offX = (miniW - w * scale) / 2;
    const offY = (miniH - h * scale) / 2;
    const TYPE_VAR = {
      trigger: '--node-trigger', llm: '--node-llm', stt: '--node-stt', tts: '--node-tts',
      rag: '--node-rag', memory: '--node-memory', embeddings: '--node-embeddings',
      reranker: '--node-reranker', condition: '--node-condition', switch: '--node-switch',
      template: '--node-template', pii_filter: '--node-pii_filter', tts_clean: '--node-tts_clean',
      router: '--node-router', output: '--node-output',
    };
    for (const n of nodes) {
      const dot = document.createElement('div');
      dot.className = 'fb-minimap-node';
      dot.style.setProperty('--node-color', `var(${TYPE_VAR[n.type] || '--node-llm'})`);
      dot.style.left = `${offX + (n.x - minX) * scale}px`;
      dot.style.top = `${offY + (n.y - minY) * scale}px`;
      dot.style.width = `${Math.max(8, 220 * scale)}px`;
      dot.style.height = `${Math.max(4, 40 * scale)}px`;
      mini.appendChild(dot);
    }
    // Viewport
    const canvas = s.canvas;
    const rect = canvas.root.getBoundingClientRect();
    const vWorldW = rect.width / canvas.view.zoom;
    const vWorldH = rect.height / canvas.view.zoom;
    const vWorldX = -canvas.view.x / canvas.view.zoom;
    const vWorldY = -canvas.view.y / canvas.view.zoom;
    vp.style.left = `${offX + (vWorldX - minX) * scale}px`;
    vp.style.top = `${offY + (vWorldY - minY) * scale}px`;
    vp.style.width = `${Math.max(8, vWorldW * scale)}px`;
    vp.style.height = `${Math.max(8, vWorldH * scale)}px`;
  },

  async _openHistory() {
    const s = this._state;
    if (!s) return;
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
        const id = s.flowId;
        pendingFlowId = id;
        Router.navigate('flow-builder');
      } catch (err) {
        toast(I18n.t('flows_builder.restore_error', { error: err.message }), 'error');
      }
    });
  },
};

export default FlowBuilderScreen;
