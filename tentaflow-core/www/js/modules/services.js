// =============================================================================
// File: modules/services.js — Services screen with 3 tabs (tf-tabs underline)
//   1) List     — deployed services table (binary serviceListRequest), 5s refresh
//   2) Aliases  — model alias CRUD (binary modelAlias*Request), tf-window editor
//   3) Models   — mesh-wide model aggregate (binary catalogListRequest)
//   "New service" opens Catalog (target picker → wizard). Aliases edit + delete
//   confirmations use <tf-window>. Auto-refresh uses morphdom via /js/lib/patch.js
//   so the table does not flicker. Multi-node aggregation lands in Krok N5;
//   today the list is local-only.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { Router } from '/js/router.js';
import { patchInner } from '/js/lib/patch.js';
import { createRefresher } from '/js/lib/refresh.js';
import { TfWindow } from '/js/components/tf-window.js';
import * as ManifestStore from '/js/modules/catalog/manifest-store.js';

let services = [];
let aliases = [];
let meshNodes = [];
let unifiedModels = [];
let modelsCache = [];
let refresher = null;
let currentTab = 'list';
let lastQuery = '';
// M16 alias view state. Filter is one of: all | addon | manual | active | inactive
// | fallback | empty_target. Search is a case-insensitive substring on alias name.
// editingId !== null means an inline edit panel is open under the row.
let aliasFilter = 'all';
let aliasSearch = '';
let aliasEditingId = null;
// Per-alias staged edit state: { targetModel, strategy, fallbackTargets[] }.
// Keyed by alias id so opening another row resets cleanly.
let aliasEditDraft = null;

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

const ServicesScreen = {
  get title() { return I18n.t('nav.services'); },
  render() {
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('services')} ${escapeHtml(I18n.t('services.title'))}</h1>
          <div class="sub" id="services-sub">${escapeHtml(I18n.t('common.loading'))}</div>
        </div>
        <div class="actions">
          <tf-button variant="primary" icon="plus" id="svc-new">${escapeHtml(I18n.t('services.add_service'))}</tf-button>
        </div>
      </div>

      <tf-tabs variant="underline" value="${currentTab}" id="svc-tabs">
        <tf-tab id="list" icon="services" count="0">${escapeHtml(I18n.t('services.tab_list'))}</tf-tab>
        <tf-tab id="aliases" icon="share" count="0">${escapeHtml(I18n.t('services.tab_aliases'))}</tf-tab>
        <tf-tab id="models" icon="model" count="0">${escapeHtml(I18n.t('services.tab_models'))}</tf-tab>
      </tf-tabs>

      <div id="svc-tab-body"></div>
    `;
  },
  async mount() {
    byId('svc-new')?.addEventListener('click', () => Router.navigate('catalog'));
    byId('svc-tabs')?.addEventListener('change', handleTabChange);

    await loadAll();
    refresher = createRefresher({
      run: () => loadForCurrentTab(),
      intervalMs: 5000,
      hiddenIntervalMs: 20000,
    });
    refresher.start();
  },
  unmount() {
    if (refresher) refresher.dispose();
    refresher = null;
    services = [];
    aliases = [];
    meshNodes = [];
    unifiedModels = [];
    modelsCache = [];
  },
};

// ---- Data loading ---------------------------------------------------------

async function loadAll() {
  try {
    const [svc, al, nodes, unified, models] = await Promise.all([
      ApiBinary.list('serviceListRequest', { arrayKey: 'services' }).catch(() => []),
      ApiBinary.list('modelAliasListRequest', { arrayKey: 'aliases' }).catch(() => []),
      ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' }).catch(() => []),
      ApiBinary.list('catalogListRequest', { arrayKey: 'entries' }).catch(() => []),
      ApiBinary.list('modelListRequest', { arrayKey: 'models' }).catch(() => []),
      ManifestStore.init().catch(() => false),
    ]);
    services = Array.isArray(svc) ? svc : [];
    aliases = al || [];
    meshNodes = nodes || [];
    unifiedModels = Array.isArray(unified) ? unified : [];
    modelsCache = Array.isArray(models) ? models : [];
    // Legacy unified merge feeds the per-node models[] still consumed by the
    // Mesh detail page; the Models tab itself reads from modelsCache.
    mergeUnifiedModelsIntoNodes();
    renderTab();
    updateSubtitle();
    updateTabCounts();
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

async function loadForCurrentTab() {
  try {
    if (currentTab === 'list') {
      // meshNodes is needed for hostname resolution in the Node column. Both
      // requests run in parallel — peer_store updates land lazily so the local
      // node is always present even when remotes are offline.
      const [svc, nodes] = await Promise.all([
        ApiBinary.list('serviceListRequest', { arrayKey: 'services' }),
        ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' }).catch(() => meshNodes),
      ]);
      services = Array.isArray(svc) ? svc : [];
      meshNodes = Array.isArray(nodes) ? nodes : meshNodes;
      patchListTab();
    } else if (currentTab === 'aliases') {
      aliases = await ApiBinary.list('modelAliasListRequest', { arrayKey: 'aliases' });
      patchAliasesTab();
    } else if (currentTab === 'models') {
      const [nodes, models] = await Promise.all([
        ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' }).catch(() => []),
        ApiBinary.list('modelListRequest', { arrayKey: 'models' }).catch(() => []),
      ]);
      meshNodes = nodes || [];
      modelsCache = Array.isArray(models) ? models : [];
      patchModelsTab();
    }
    updateSubtitle();
    updateTabCounts();
  } catch (err) {
    console.warn('[services] refresh failed:', err.message);
  }
}

function updateSubtitle() {
  const running = services.filter((s) => ['running'].includes((s.status || '').toLowerCase())).length;
  const total = services.length;
  const sub = byId('services-sub');
  if (!sub) return;
  sub.textContent = total === 0
    ? I18n.t('services.subtitle_empty')
    : I18n.t('services.subtitle_format', { total, running, nodes: meshNodes.length });
}

function updateTabCounts() {
  const tabs = byId('svc-tabs');
  if (!tabs) return;
  const listTab = tabs.querySelector('tf-tab#list');
  const aliasTab = tabs.querySelector('tf-tab#aliases');
  const modelsTab = tabs.querySelector('tf-tab#models');
  if (listTab) listTab.setAttribute('count', String(services.length));
  if (aliasTab) aliasTab.setAttribute('count', String(aliases.length));
  if (modelsTab) modelsTab.setAttribute('count', String(collectUniqueModels().length));
}

// ---- Tabs -----------------------------------------------------------------

function handleTabChange(e) {
  const id = e.detail?.value;
  if (!id || id === currentTab) return;
  currentTab = id;
  renderTab();
}

function renderTab() {
  const body = byId('svc-tab-body');
  if (!body) return;
  if (currentTab === 'list') body.innerHTML = renderListTab();
  else if (currentTab === 'aliases') body.innerHTML = renderAliasesTab();
  else if (currentTab === 'models') body.innerHTML = renderModelsTab();
  bindTabEvents();
}

function patchListTab() {
  if (currentTab !== 'list') return;
  const body = byId('svc-tab-body');
  if (!body) return;
  patchInner(body, renderListTab());
  bindTabEvents();
}

function patchAliasesTab() {
  if (currentTab !== 'aliases') return;
  const body = byId('svc-tab-body');
  if (!body) return;
  patchInner(body, renderAliasesTab());
  bindTabEvents();
}

function patchModelsTab() {
  if (currentTab !== 'models') return;
  const body = byId('svc-tab-body');
  if (!body) return;
  patchInner(body, renderModelsTab());
  bindTabEvents();
}

function bindTabEvents() {
  const body = byId('svc-tab-body');
  if (!body) return;

  // List tab — N5 row actions: Pause/Play toggle, Pin toggle, Delete.
  body.querySelectorAll('[data-svc-delete]').forEach((b) => {
    b.onclick = (e) => {
      e.stopPropagation();
      stopService(
        b.dataset.svcDelete,
        b.dataset.svcName,
        b.dataset.svcNode,
        b.dataset.svcNodeLabel,
      );
    };
  });
  body.querySelectorAll('[data-svc-pause-play]').forEach((b) => {
    b.onclick = (e) => {
      e.stopPropagation();
      togglePauseStart(b);
    };
  });
  body.querySelectorAll('[data-svc-pin-toggle]').forEach((b) => {
    b.onclick = (e) => {
      e.stopPropagation();
      togglePin(b);
    };
  });
  body.querySelectorAll('[data-svc-edit]').forEach((b) => {
    b.onclick = (e) => {
      e.stopPropagation();
      const svcId = b.dataset.svcEdit;
      const engineId = b.dataset.svcEngine;
      const nodeId = b.dataset.svcNode || undefined;
      const svc = (services || []).find((s) => String(s.id) === String(svcId));
      if (!svc) return;
      import('./services-edit.js').then((mod) => {
        mod.openEditModal(svc, { engineId, nodeId, onSaved: refreshServiceList });
      });
    };
  });
  body.querySelectorAll('[data-empty-cta]').forEach((b) => {
    b.onclick = () => Router.navigate('catalog');
  });

  // Aliases tab — M16 layout: filter chips, search, inline edit, drag-reorder.
  bindAliasFilterChips(body);
  bindAliasSearch(body);
  bindAliasRowActions(body);
  bindAliasInlineEditEvents(body);
  body.querySelectorAll('[data-new-alias]').forEach((b) => {
    b.onclick = () => openAliasModal(null);
  });
}

function bindAliasFilterChips(root) {
  root.querySelectorAll('[data-alias-filter]').forEach((chip) => {
    const trigger = () => {
      const id = chip.dataset.aliasFilter;
      if (!id || id === aliasFilter) return;
      aliasFilter = id;
      patchAliasesTab();
    };
    chip.onclick = trigger;
    chip.onkeydown = (e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        trigger();
      }
    };
  });
}

function bindAliasSearch(root) {
  const sb = root.querySelector('#svc-alias-search');
  if (!sb) return;
  // tf-searchbox emits "search" with debounce baked in. We re-patch the tab on
  // every keystroke; the grid is small (<200 rows typical) so it stays cheap.
  sb.addEventListener('search', (e) => {
    aliasSearch = String(e.detail?.value ?? sb.value ?? '');
    patchAliasesTab();
  });
}

function bindAliasRowActions(root) {
  root.querySelectorAll('[data-alias-edit]').forEach((b) => {
    b.onclick = (e) => {
      e.stopPropagation();
      const id = parseInt(b.dataset.aliasEdit, 10);
      if (aliasEditingId === id) {
        aliasEditingId = null;
        aliasEditDraft = null;
      } else {
        aliasEditingId = id;
        aliasEditDraft = null;
      }
      patchAliasesTab();
    };
  });
  root.querySelectorAll('[data-alias-delete]').forEach((b) => {
    b.onclick = (e) => {
      e.stopPropagation();
      deleteAlias(parseInt(b.dataset.aliasDelete, 10), b.dataset.aliasName);
    };
  });
  root.querySelectorAll('[data-alias-toggle]').forEach((tg) => {
    tg.addEventListener('change', async (e) => {
      const id = parseInt(tg.dataset.aliasToggle, 10);
      const checked = !!e.detail?.checked;
      await updateAliasActive(id, checked);
    });
  });
}

function bindAliasInlineEditEvents(root) {
  // Cancel button → close panel without applying draft.
  root.querySelectorAll('[data-alias-cancel]').forEach((b) => {
    b.onclick = (e) => {
      e.stopPropagation();
      aliasEditingId = null;
      aliasEditDraft = null;
      patchAliasesTab();
    };
  });
  // Save button → push update via binary RPC.
  root.querySelectorAll('[data-alias-save]').forEach((b) => {
    b.onclick = async (e) => {
      e.stopPropagation();
      const id = parseInt(b.dataset.aliasSave, 10);
      await saveAliasInline(id);
    };
  });
  // Primary target select → mirror into draft + re-render fallback candidate list.
  root.querySelectorAll('[id^="al-target-"]').forEach((sel) => {
    sel.addEventListener('change', (e) => {
      if (!aliasEditDraft) return;
      aliasEditDraft.targetModel = String(e.detail?.value ?? sel.value ?? '').trim();
      // Re-render so the fallback add-list excludes the newly-chosen primary.
      patchAliasesTab();
    });
  });
  // Strategy segmented → mirror into draft (no re-render needed; visual handled
  // by the component itself).
  root.querySelectorAll('[id^="al-strategy-"]').forEach((seg) => {
    seg.addEventListener('change', (e) => {
      if (!aliasEditDraft) return;
      aliasEditDraft.strategy = String(e.detail?.value ?? seg.value ?? 'first_available').toLowerCase();
    });
  });
  // Fallback add → push name into draft, re-render.
  root.querySelectorAll('[data-fallback-add]').forEach((sel) => {
    sel.addEventListener('change', (e) => {
      if (!aliasEditDraft) return;
      const val = String(e.detail?.value ?? sel.value ?? '').trim();
      if (!val || aliasEditDraft.fallbacks.includes(val)) return;
      if (val === aliasEditDraft.targetModel) return;
      aliasEditDraft.fallbacks.push(val);
      patchAliasesTab();
    });
  });
  // Fallback remove → splice by index, re-render.
  root.querySelectorAll('[data-fallback-remove]').forEach((b) => {
    b.onclick = (e) => {
      e.stopPropagation();
      if (!aliasEditDraft) return;
      const idx = parseInt(b.dataset.fallbackRemove, 10);
      if (Number.isNaN(idx)) return;
      aliasEditDraft.fallbacks.splice(idx, 1);
      patchAliasesTab();
    };
  });
  // Drag-and-drop reorder. We use the native HTML5 API on .svc-fallback-item
  // elements with draggable=true. Dropping reorders the draft array; the
  // primary row is non-draggable (no draggable attribute).
  root.querySelectorAll('[data-fallback-list]').forEach((list) => {
    attachFallbackDrag(list);
  });
}

function attachFallbackDrag(listEl) {
  let dragIdx = null;
  listEl.querySelectorAll('.svc-fallback-item[draggable="true"]').forEach((item) => {
    item.addEventListener('dragstart', (e) => {
      dragIdx = parseInt(item.dataset.fallbackIdx, 10);
      item.classList.add('dragging');
      // dataTransfer is required for Firefox to start the drag.
      e.dataTransfer?.setData('text/plain', String(dragIdx));
      if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
    });
    item.addEventListener('dragend', () => {
      item.classList.remove('dragging');
      listEl.querySelectorAll('.drag-over').forEach((el) => el.classList.remove('drag-over'));
      dragIdx = null;
    });
    item.addEventListener('dragover', (e) => {
      e.preventDefault();
      if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
      item.classList.add('drag-over');
    });
    item.addEventListener('dragleave', () => {
      item.classList.remove('drag-over');
    });
    item.addEventListener('drop', (e) => {
      e.preventDefault();
      item.classList.remove('drag-over');
      if (!aliasEditDraft || dragIdx === null) return;
      const dropIdx = parseInt(item.dataset.fallbackIdx, 10);
      if (Number.isNaN(dropIdx) || dragIdx === dropIdx) return;
      const arr = aliasEditDraft.fallbacks;
      const [moved] = arr.splice(dragIdx, 1);
      arr.splice(dropIdx, 0, moved);
      dragIdx = null;
      patchAliasesTab();
    });
  });
}

// Inline save — pushes ModelAliasUpdateRequest with the draft state. Keeps
// is_active and alias name unchanged (toggle handles is_active, rename is not
// part of the inline edit by design — rename forces a re-create flow).
async function saveAliasInline(id) {
  const alias = aliases.find((x) => x.id === id);
  if (!alias || !aliasEditDraft || aliasEditDraft.id !== id) return;
  const errEl = document.querySelector(`[data-edit-error="${CSS.escape(String(id))}"]`);
  const target = aliasEditDraft.targetModel.trim();
  if (!target) {
    if (errEl) { errEl.textContent = I18n.t('services.alias_required'); errEl.hidden = false; }
    return;
  }
  const fbJson = aliasEditDraft.fallbacks.length > 0
    ? JSON.stringify(aliasEditDraft.fallbacks)
    : null;
  try {
    await ApiBinary.action('modelAliasUpdateRequest', {
      id,
      alias: alias.alias,
      targetModel: target,
      isActive: alias.is_active,
      strategy: aliasEditDraft.strategy,
      fallbackTargets: fbJson,
    });
    toast(I18n.t('services.alias_updated'), 'success');
    aliasEditingId = null;
    aliasEditDraft = null;
    await loadAll();
  } catch (err) {
    if (errEl) { errEl.textContent = err.message; errEl.hidden = false; }
  }
}

// Toggle handler — flips is_active via the same update RPC. Optimistic UI:
// the toggle has already animated by the time we get here.
async function updateAliasActive(id, checked) {
  const alias = aliases.find((x) => x.id === id);
  if (!alias) return;
  try {
    await ApiBinary.action('modelAliasUpdateRequest', {
      id,
      alias: alias.alias,
      targetModel: alias.target_model,
      isActive: checked,
      strategy: alias.strategy ?? null,
      fallbackTargets: alias.fallback_targets ?? null,
    });
    alias.is_active = checked;
    toast(I18n.t('services.alias_updated'), 'success');
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    // Re-load to revert visual state if RPC failed.
    await loadAll();
  }
}

// ---- List tab -------------------------------------------------------------

function renderListTab() {
  if (services.length === 0) {
    return `
      <div class="empty-big">
        ${sprite('services')}
        <h3>${escapeHtml(I18n.t('services.empty'))}</h3>
        <p>${escapeHtml(I18n.t('services.empty_hint'))}</p>
        <tf-button variant="primary" icon="plus" data-empty-cta>${escapeHtml(I18n.t('services.empty_cta'))}</tf-button>
      </div>
    `;
  }
  return `
    <table class="data-table">
      <thead>
        <tr>
          <th>${escapeHtml(I18n.t('services.col_node'))}</th>
          <th>${escapeHtml(I18n.t('services.col_engine'))}</th>
          <th>${escapeHtml(I18n.t('services.col_display_name'))}</th>
          <th>${escapeHtml(I18n.t('services.col_category'))}</th>
          <th>${escapeHtml(I18n.t('services.col_status'))}</th>
          <th>${escapeHtml(I18n.t('services.col_endpoint'))}</th>
          <th>${escapeHtml(I18n.t('services.col_models'))}</th>
          <th>${escapeHtml(I18n.t('services.col_restart'))}</th>
          <th style="text-align:right;">${escapeHtml(I18n.t('services.col_actions'))}</th>
        </tr>
      </thead>
      <tbody>
        ${services.map(renderRow).join('')}
      </tbody>
    </table>
  `;
}

// Resolve a friendly label for a service.node_id by joining against the cached
// meshNodes list. Falls back to a 12-char short hex when hostname is missing.
function nodeLabelFor(nodeId) {
  if (!nodeId) return { label: '?', isLocal: false, hostname: null, full: '' };
  const node = meshNodes.find((n) => (n.node_id || n.id) === nodeId);
  const hostname = node?.hostname || null;
  const label = hostname || nodeId.slice(0, 12);
  return { label, isLocal: !!node?.is_local, hostname, full: nodeId };
}

// Maps services.status × paused flag onto the tf-chip palette. The paused flag
// wins over the underlying status because a paused row has no live process.
function mapStatusToChip(status, paused) {
  if (paused) return { variant: 'info', dot: false, key: 'paused' };
  const key = (status || '').toLowerCase();
  switch (key) {
    case 'running': return { variant: 'online', dot: true, key };
    case 'degraded': return { variant: 'pending', dot: true, key };
    case 'failed': return { variant: 'err', dot: true, key };
    case 'starting': return { variant: 'info', dot: true, key };
    case 'stopped': return { variant: 'offline', dot: false, key };
    default: return { variant: 'info', dot: false, key };
  }
}

// Color chip class for a category — reuses existing scope-chip palette so the
// list looks visually consistent with the catalog tiles.
function categoryChipClass(category) {
  switch ((category || '').toLowerCase()) {
    case 'llm': return 'chat';
    case 'embeddings':
    case 'embedding': return 'mesh-read';
    case 'stt':
    case 'tts': return 'deploy';
    case 'image-gen':
    case 'video-gen':
    case 'music-gen': return 'mesh-admin';
    case 'agents':
    case 'tools': return 'license';
    default: return 'mesh-read';
  }
}

function renderRow(s) {
  const paused = !!s.paused;
  const statusInfo = mapStatusToChip(s.status, paused);
  const statusLabel = I18n.t(`services.status.${statusInfo.key}`)
    || s.status || '—';
  // progress_message niesie krotki opis fazy startu od supervisor
  // heartbeat (np. "warming up — alive 30s, waiting for /v1/models").
  // Renderujemy pod chipem statusu, zeby user widzial PROGRES startu
  // serwisu a nie tylko "Starting" przez minuty.
  const progressMsg = typeof s.progress_message === 'string' && s.progress_message.length > 0
    ? s.progress_message
    : '';
  const progressBadge = progressMsg
    ? `<div style="font-size:11px;color:var(--text-3);margin-top:4px;line-height:1.3;">${escapeHtml(progressMsg)}</div>`
    : '';
  const endpoint = s.endpoint_url
    ? `<code style="font-size:11px;" title="${escapeAttr(s.endpoint_url)}">${escapeHtml(truncateMiddle(s.endpoint_url, 36))}</code>`
    : '<span style="color:var(--text-3);">—</span>';
  const models = Array.isArray(s.models) ? s.models : [];
  const modelChips = models.length === 0
    ? '<span style="color:var(--text-3);">—</span>'
    : models
        .map((m) => {
          const label = m.display_name || m.model_name || '';
          return `<tf-chip status="info">${escapeHtml(label)}</tf-chip>`;
        })
        .join(' ');
  const restartCount = Number.isFinite(s.restart_count) ? s.restart_count : 0;
  const restartTitle = s.health_last_err
    ? `title="${escapeAttr(s.health_last_err)}"`
    : '';
  const restartCell = restartCount > 0
    ? `<tf-chip status="warn" ${restartTitle}>${restartCount}</tf-chip>`
    : '<span style="color:var(--text-3);">0</span>';

  const displayName = s.display_name || s.engine_id || '';
  const engineLabel = s.engine_id || '';
  const category = s.category || '';
  const nodeInfo = nodeLabelFor(s.node_id);
  const nodeBadge = nodeInfo.isLocal
    ? `<span class="svc-node-local">${escapeHtml(I18n.t('services.node_local_badge'))}</span>`
    : '';
  const nodeCell = `
    <span class="svc-node-cell" title="${escapeAttr(nodeInfo.full)}">
      ${escapeHtml(nodeInfo.label)}${nodeBadge}
    </span>`;

  // Pause/Play toggle — paused row OR not-running ⇒ Play (start). Running ⇒ Pause.
  const isStarting = (s.status || '').toLowerCase() === 'starting';
  const isRunning = ['running', 'degraded'].includes((s.status || '').toLowerCase()) && !paused;
  const showPlay = paused || !isRunning;
  const ppIcon = isStarting ? 'rotate' : (showPlay ? 'play' : 'pause');
  const ppAction = showPlay ? 'start' : 'pause';
  const ppTooltip = showPlay
    ? I18n.t('services.btn_play')
    : I18n.t('services.btn_pause');

  // Pin toggle — outline by default, filled accent when pinned.
  const pinned = !!s.pinned;
  const pinTooltip = pinned
    ? I18n.t('services.tooltip_pin_on')
    : I18n.t('services.tooltip_pin_off');

  const svcId = escapeAttr(s.id);
  const svcNodeId = escapeAttr(s.node_id || '');
  const svcNodeLabel = escapeAttr(nodeInfo.label);

  return `
    <tr data-key="svc-${svcId}">
      <td data-label="${escapeAttr(I18n.t('services.col_node'))}">${nodeCell}</td>
      <td data-label="${escapeAttr(I18n.t('services.col_engine'))}">
        <strong style="color: var(--accent-2);">${escapeHtml(engineLabel)}</strong>
      </td>
      <td data-label="${escapeAttr(I18n.t('services.col_display_name'))}">
        ${escapeHtml(displayName)}
      </td>
      <td data-label="${escapeAttr(I18n.t('services.col_category'))}">
        <span class="scope-chip ${categoryChipClass(category)}">${escapeHtml(category.toUpperCase() || '—')}</span>
      </td>
      <td data-label="${escapeAttr(I18n.t('services.col_status'))}">
        <tf-chip status="${statusInfo.variant}"${statusInfo.dot ? ' dot' : ''}>${escapeHtml(statusLabel)}</tf-chip>
        ${progressBadge}
      </td>
      <td data-label="${escapeAttr(I18n.t('services.col_endpoint'))}">${endpoint}</td>
      <td data-label="${escapeAttr(I18n.t('services.col_models'))}">
        <div style="display:flex;flex-wrap:wrap;gap:4px;">${modelChips}</div>
      </td>
      <td data-label="${escapeAttr(I18n.t('services.col_restart'))}">${restartCell}</td>
      <td data-label="${escapeAttr(I18n.t('services.col_actions'))}" style="text-align:right;white-space:nowrap;">
        <tf-button variant="ghost" size="sm" icon="${ppIcon}"
          data-svc-pause-play="${svcId}"
          data-svc-action="${ppAction}"
          data-svc-node="${svcNodeId}"
          ${isStarting ? 'disabled' : ''}
          title="${escapeAttr(ppTooltip)}"></tf-button>
        <tf-button variant="ghost" size="sm" icon="pin"
          class="svc-pin-toggle${pinned ? ' pinned' : ''}"
          data-svc-pin-toggle="${svcId}"
          data-svc-pinned="${pinned ? 'true' : 'false'}"
          data-svc-node="${svcNodeId}"
          title="${escapeAttr(pinTooltip)}"></tf-button>
        <tf-button variant="ghost" size="sm" icon="edit"
          data-svc-edit="${svcId}"
          data-svc-engine="${escapeAttr(s.engine_id || '')}"
          data-svc-node="${svcNodeId}"
          title="${escapeAttr(I18n.t('services.btn_edit') || 'Edit')}"></tf-button>
        <tf-button variant="danger" size="sm" icon="trash"
          data-svc-delete="${svcId}"
          data-svc-name="${escapeAttr(displayName)}"
          data-svc-node="${svcNodeId}"
          data-svc-node-label="${svcNodeLabel}"
          title="${escapeAttr(I18n.t('services.btn_delete'))}"></tf-button>
        <span class="svc-row-error" data-svc-error="${svcId}" hidden></span>
      </td>
    </tr>
  `;
}

// Truncate a long string in the middle so both prefix and suffix remain visible.
function truncateMiddle(value, max) {
  if (!value || value.length <= max) return value || '';
  const half = Math.floor((max - 1) / 2);
  return value.slice(0, half) + '…' + value.slice(value.length - half);
}

// ---- Aliases tab ----------------------------------------------------------

function renderAliasesTab() {
  const title = escapeHtml(I18n.t('services.aliases_info_title'));
  // Strzalka w tresci jako ikona zamiast znaku → — ladniejsze wyrownanie
  // wertykalne, spojne z innymi strzalkami w UI.
  const arrowIcon = '<svg width="12" height="12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="display:inline-block;vertical-align:-1px;margin:0 4px;color:var(--accent-2)"><use href="#i-chevron-right"/></svg>';
  const body = escapeHtml(I18n.t('services.aliases_info_body')).replace('→', arrowIcon);
  if (aliases.length === 0) {
    return `
      <div class="info-card">
        ${sprite('info')}
        <div><strong>${title}</strong> — ${body}</div>
      </div>
      <div class="empty-big">
        ${sprite('share')}
        <h3>${escapeHtml(I18n.t('services.aliases_empty'))}</h3>
        <p>${escapeHtml(I18n.t('services.aliases_empty_hint'))}</p>
        <tf-button variant="primary" icon="plus" data-new-alias>${escapeHtml(I18n.t('services.new_alias'))}</tf-button>
      </div>
    `;
  }

  const counts = computeAliasFilterCounts(aliases);
  const filterChips = [
    { id: 'all', label: I18n.t('services.alias_filter_all'), count: counts.all },
    { id: 'addon', label: I18n.t('services.alias_filter_addon'), count: counts.addon },
    { id: 'manual', label: I18n.t('services.alias_filter_manual'), count: counts.manual },
    { id: 'active', label: I18n.t('services.alias_filter_active'), count: counts.active },
    { id: 'inactive', label: I18n.t('services.alias_filter_inactive'), count: counts.inactive },
    { id: 'fallback', label: I18n.t('services.alias_filter_with_fallback'), count: counts.fallback },
    { id: 'empty_target', label: I18n.t('services.alias_filter_empty_target'), count: counts.empty_target },
  ].map((c) => `
    <span class="svc-alias-filter-chip${aliasFilter === c.id ? ' active' : ''}"
          data-alias-filter="${escapeAttr(c.id)}"
          role="button" tabindex="0">
      ${escapeHtml(c.label)}<span class="count">${c.count}</span>
    </span>`).join('');

  const visible = filterAliases(aliases, aliasFilter, aliasSearch);

  const rowsHtml = visible.length === 0
    ? `<div class="svc-alias-empty-filter">${escapeHtml(I18n.t('services.alias_no_match'))}</div>`
    : visible.map(renderAliasRow).join('');

  return `
    <div class="info-card">
      ${sprite('info')}
      <div><strong>${title}</strong> — ${body}</div>
    </div>

    <div class="svc-alias-toolbar">
      <tf-searchbox id="svc-alias-search"
        placeholder="${escapeAttr(I18n.t('services.alias_search_placeholder'))}"
        value="${escapeAttr(aliasSearch)}"></tf-searchbox>
      <tf-button variant="primary" size="sm" icon="plus" data-new-alias>${escapeHtml(I18n.t('services.new_alias'))}</tf-button>
    </div>

    <div class="svc-alias-filters" id="svc-alias-filters">${filterChips}</div>

    <div class="svc-alias-grid">
      <div class="svc-alias-row h">
        <div>${escapeHtml(I18n.t('services.alias_col_name'))}</div>
        <div>${escapeHtml(I18n.t('services.alias_col_owner'))}</div>
        <div>${escapeHtml(I18n.t('services.alias_col_target'))}</div>
        <div>${escapeHtml(I18n.t('services.alias_col_strategy'))}</div>
        <div>${escapeHtml(I18n.t('services.alias_col_visibility'))}</div>
        <div>${escapeHtml(I18n.t('services.alias_col_active'))}</div>
        <div></div>
      </div>
      ${rowsHtml}
    </div>
  `;
}

// Filter chip counts. Single pass — order matches the chip array so the badge
// stays in sync as backend gradually populates owner/visibility fields.
function computeAliasFilterCounts(list) {
  const counts = { all: list.length, addon: 0, manual: 0, active: 0, inactive: 0, fallback: 0, empty_target: 0 };
  for (const a of list) {
    const owner = aliasOwnerInfo(a);
    if (owner.type === 'addon') counts.addon += 1;
    else counts.manual += 1;
    if (a.is_active) counts.active += 1;
    else counts.inactive += 1;
    const fbs = parseFallbackTargets(a.fallback_targets).values;
    if (fbs.length > 0) counts.fallback += 1;
    if (!a.target_model || !String(a.target_model).trim()) counts.empty_target += 1;
  }
  return counts;
}

function filterAliases(list, filter, search) {
  const q = (search || '').trim().toLowerCase();
  return list.filter((a) => {
    if (q && !String(a.alias || '').toLowerCase().includes(q)) return false;
    const owner = aliasOwnerInfo(a);
    const fbs = parseFallbackTargets(a.fallback_targets).values;
    const emptyTarget = !a.target_model || !String(a.target_model).trim();
    switch (filter) {
      case 'addon': return owner.type === 'addon';
      case 'manual': return owner.type === 'manual';
      case 'active': return !!a.is_active;
      case 'inactive': return !a.is_active;
      case 'fallback': return fbs.length > 0;
      case 'empty_target': return emptyTarget;
      case 'all':
      default: return true;
    }
  });
}

// Backend currently does NOT return owner info on ModelAliasEntry (the
// model_alias_owners table exists in DB but is not joined into the wire
// payload — see TODO in raport). We infer ownership only when an explicit
// `owner_type` / `owner_id` field is present (forward-compatible), otherwise
// default to `manual` so existing admin-created aliases keep their UI badge.
function aliasOwnerInfo(a) {
  const type = String(a.owner_type || a.ownerType || 'manual').toLowerCase();
  const id = a.owner_id || a.ownerId || '';
  if (type === 'addon') {
    return { type: 'addon', label: id || 'addon', icon: 'puzzle' };
  }
  return { type: 'manual', label: id || I18n.t('services.alias_owner_manual'), icon: 'users' };
}

// Visibility is also not exposed on ModelAliasEntry yet (model_alias_visibility
// table exists, no dispatch handler). We render an `unknown` chip so admin can
// see the column structure but cannot yet act on it from this screen.
function aliasVisibilityInfo(a) {
  const v = String(a.visibility || '').toLowerCase();
  if (v === 'private' || v === 'restricted' || v === 'public') {
    const iconMap = { private: 'lock', restricted: 'shield', public: 'unlock' };
    return { value: v, icon: iconMap[v], label: I18n.t(`services.alias_visibility_${v}`) };
  }
  return { value: 'unknown', icon: 'lock', label: I18n.t('services.alias_visibility_unknown') };
}

// fallback_targets na wire/DB to JSON array string (CLAUDE.md §9 — "No CSV").
// Migracja 69 konwertuje stare CSV-zapisy do JSON juz po stronie backendu,
// wiec klient parsuje wylacznie JSON. Zwraca `{ok, values}`:
//   - `ok=true` + values=[...]  — udane parsowanie (puste `[]` = ok+empty)
//   - `ok=true` + values=[]      — `null` / pusty string / "[]"
//   - `ok=false` + values=[]    — non-empty wartosc ktora NIE jest valid JSON
//     array (np. CSV ze stale taba). Wpis jest interpretowany jako "brak",
//     ale wywolujacy moze pokazac UI error przed zapisaniem null.
function parseFallbackTargets(raw) {
  if (raw === null || raw === undefined) return { ok: true, values: [] };
  const trimmed = String(raw).trim();
  if (!trimmed) return { ok: true, values: [] };
  if (!trimmed.startsWith('[')) return { ok: false, values: [] };
  try {
    const parsed = JSON.parse(trimmed);
    if (Array.isArray(parsed)) {
      return {
        ok: true,
        values: parsed.map((s) => String(s).trim()).filter(Boolean),
      };
    }
    return { ok: false, values: [] };
  } catch (_e) {
    return { ok: false, values: [] };
  }
}

function renderAliasRow(a) {
  const fallbacks = parseFallbackTargets(a.fallback_targets).values;
  const owner = aliasOwnerInfo(a);
  const vis = aliasVisibilityInfo(a);
  const emptyTarget = !a.target_model || !String(a.target_model).trim();
  const fallbackText = fallbacks.length === 0
    ? `<div class="fallbacks">${escapeHtml(I18n.t('services.alias_no_fallbacks'))}</div>`
    : `<div class="fallbacks">→ ${escapeHtml(fallbacks.join(' → '))}</div>`;
  const primary = emptyTarget
    ? `<div class="empty">${escapeHtml(I18n.t('services.alias_empty_target'))}</div>`
    : `<div class="primary">${escapeHtml(a.target_model)}</div>`;
  const strategyText = (a.strategy || 'first_available').toLowerCase();
  const rowClasses = [
    'svc-alias-row',
    !a.is_active ? 'inactive' : '',
    emptyTarget ? 'empty-target' : '',
  ].filter(Boolean).join(' ');
  const isEditing = aliasEditingId === a.id;
  const editPanel = isEditing ? renderAliasInlineEdit(a) : '';

  return `
    <div class="${rowClasses}" data-key="alias-${escapeAttr(a.id)}">
      <div class="svc-alias-name" title="${escapeAttr(a.alias)}">${escapeHtml(a.alias)}</div>
      <div>
        <span class="svc-alias-owner ${owner.type}" title="${escapeAttr(owner.type + ':' + owner.label)}">
          ${sprite(owner.icon)}${escapeHtml(owner.label)}
        </span>
      </div>
      <div class="svc-alias-target">${primary}${fallbackText}</div>
      <div><span class="svc-alias-strategy">${escapeHtml(strategyText)}</span></div>
      <div>
        <span class="svc-alias-vis ${vis.value}" title="${escapeAttr(I18n.t('services.alias_visibility_pending_label'))}">
          ${sprite(vis.icon)}${escapeHtml(vis.label)}
        </span>
      </div>
      <div>
        <tf-toggle ${a.is_active ? 'checked' : ''} data-alias-toggle="${escapeAttr(a.id)}"></tf-toggle>
      </div>
      <div class="svc-alias-actions">
        <tf-button variant="ghost" size="sm" icon="${isEditing ? 'chevron-down' : 'edit'}"
                   data-alias-edit="${escapeAttr(a.id)}"
                   title="${escapeAttr(I18n.t('common.edit'))}"></tf-button>
        <tf-button variant="danger" size="sm" icon="trash"
                   data-alias-delete="${escapeAttr(a.id)}"
                   data-alias-name="${escapeAttr(a.alias)}"
                   title="${escapeAttr(I18n.t('common.delete'))}"></tf-button>
      </div>
    </div>
    ${editPanel}
  `;
}

// Inline edit panel rendered directly under the row. Uses the per-alias draft
// state so unrelated row clicks do not clobber unsaved input. Drag handlers
// bind in `bindAliasInlineEditEvents` (called from bindTabEvents).
function renderAliasInlineEdit(a) {
  if (!aliasEditDraft || aliasEditDraft.id !== a.id) {
    const fbParse = parseFallbackTargets(a.fallback_targets);
    aliasEditDraft = {
      id: a.id,
      targetModel: a.target_model || '',
      strategy: (a.strategy || 'first_available').toLowerCase(),
      fallbacks: fbParse.values.slice(),
      parseFailed: !fbParse.ok,
    };
  }
  const owner = aliasOwnerInfo(a);
  const targets = buildTargetOptionList(a.alias);
  const targetOptions = targets.map((t) =>
    `<option value="${escapeAttr(t.value)}" ${t.value === aliasEditDraft.targetModel ? 'selected' : ''}>${escapeHtml(t.label)}</option>`,
  ).join('');
  // List of fallback targets EXCLUDING currently-selected primary and already-added
  // fallbacks, so the user cannot stack duplicates. Re-filtered on every render.
  const fbCandidates = targets.filter((t) =>
    t.value !== aliasEditDraft.targetModel
    && !aliasEditDraft.fallbacks.includes(t.value));
  const fbCandidateOpts = fbCandidates.map((t) =>
    `<option value="${escapeAttr(t.value)}">${escapeHtml(t.label)}</option>`,
  ).join('');

  return `
    <div class="svc-alias-edit" data-edit-for="${escapeAttr(a.id)}">
      <div class="edit-head">
        <div>
          ${sprite('edit')} ${escapeHtml(I18n.t('services.alias_edit'))}:
          <span class="name">${escapeHtml(a.alias)}</span>
        </div>
        <tf-button variant="ghost" size="sm" icon="x" data-alias-cancel="${escapeAttr(a.id)}"
                   title="${escapeAttr(I18n.t('services.alias_cancel'))}"></tf-button>
      </div>

      <div class="form-grid">
        <div class="form-row">
          <label>${escapeHtml(I18n.t('services.alias_col_target'))}</label>
          <tf-select id="al-target-${escapeAttr(a.id)}" value="${escapeAttr(aliasEditDraft.targetModel)}">
            <option value="">— ${escapeHtml(I18n.t('services.alias_target_placeholder'))} —</option>
            ${targetOptions}
          </tf-select>
        </div>
        <div class="form-row svc-strategy-segmented">
          <label>${escapeHtml(I18n.t('services.alias_col_strategy'))}</label>
          <tf-segmented value="${escapeAttr(aliasEditDraft.strategy)}" id="al-strategy-${escapeAttr(a.id)}">
            <option value="first_available">${escapeHtml(I18n.t('services.strategy_first_available'))}</option>
            <option value="round_robin">${escapeHtml(I18n.t('services.strategy_round_robin'))}</option>
            <option value="least_loaded">${escapeHtml(I18n.t('services.strategy_least_loaded'))}</option>
          </tf-segmented>
        </div>
      </div>

      <div class="form-row">
        <label>${escapeHtml(I18n.t('services.alias_col_fallback'))}</label>
        <div class="svc-fallback-builder" data-fallback-list="${escapeAttr(a.id)}">
          ${renderFallbackItems(aliasEditDraft)}
          <div class="add-row">
            <tf-select id="al-fb-add-${escapeAttr(a.id)}" data-fallback-add="${escapeAttr(a.id)}">
              <option value="">— ${escapeHtml(I18n.t('services.alias_add_fallback'))} —</option>
              ${fbCandidateOpts}
            </tf-select>
          </div>
        </div>
      </div>

      <div class="meta-block">
        <strong>${escapeHtml(I18n.t('services.alias_meta_owner'))}:</strong>
        <code>${escapeHtml(owner.type)}${owner.label && owner.type === 'addon' ? ':' + owner.label : ''}</code>
        · <strong>${escapeHtml(I18n.t('services.alias_meta_id'))}:</strong>
        <code>${escapeHtml(String(a.id))}</code>
      </div>

      ${aliasEditDraft.parseFailed
        ? `<div class="form-error">${escapeHtml(I18n.t('services.alias_fallback_parse_failed'))}</div>`
        : ''}
      <div class="form-error" hidden data-edit-error="${escapeAttr(a.id)}"></div>

      <div class="edit-foot">
        <tf-button variant="ghost" size="sm" icon="x" data-alias-cancel="${escapeAttr(a.id)}">${escapeHtml(I18n.t('services.alias_cancel'))}</tf-button>
        <div class="right">
          <tf-button variant="primary" icon="check" data-alias-save="${escapeAttr(a.id)}">${escapeHtml(I18n.t('services.alias_save'))}</tf-button>
        </div>
      </div>
    </div>
  `;
}

function renderFallbackItems(draft) {
  // Primary slot is the currently-selected target_model (read-only here — change
  // it via the "Primary target" select above). Indexed 1..N for the chain.
  const primaryLabel = draft.targetModel
    ? draft.targetModel
    : `— ${I18n.t('services.alias_target_placeholder')} —`;
  const items = [];
  items.push(`
    <div class="svc-fallback-item primary" data-fallback-primary>
      <span class="pos">${escapeHtml(I18n.t('services.alias_primary_target_pos'))}</span>
      <span></span>
      <span></span>
      <span class="name">${escapeHtml(primaryLabel)}</span>
      <span></span>
    </div>
  `);
  draft.fallbacks.forEach((name, idx) => {
    items.push(`
      <div class="svc-fallback-item" draggable="true"
           data-fallback-idx="${idx}" data-fallback-name="${escapeAttr(name)}">
        <span class="pos">${idx + 1}</span>
        <span class="grip" title="${escapeAttr(I18n.t('services.alias_drag_handle'))}">${sprite('grip')}</span>
        <span></span>
        <span class="name">${escapeHtml(name)}</span>
        <tf-button variant="ghost" size="sm" icon="trash"
                   data-fallback-remove="${idx}"
                   title="${escapeAttr(I18n.t('services.alias_fallback_remove'))}"></tf-button>
      </div>
    `);
  });
  return items.join('');
}

// ---- Models tab -----------------------------------------------------------

// Catalog entries with kind=ServiceModel carry per-node `instances`. Walk
// them and graft the model list onto the matching meshNodes row — peer
// snapshots take a few heartbeats to fill node.models, this fills the gap.
function mergeUnifiedModelsIntoNodes() {
  if (!Array.isArray(unifiedModels) || unifiedModels.length === 0) return;
  const byNode = new Map();
  for (const entry of unifiedModels) {
    const kindWrapper = entry && entry.kind;
    if (!kindWrapper || kindWrapper.kind !== 'service_model') continue;
    const alias = entry.id;
    if (!alias) continue;
    const surfaces = Array.isArray(entry.serviceSurfaces) ? entry.serviceSurfaces : [];
    const kindLabel = surfaces[0] || 'service';
    const instances = Array.isArray(kindWrapper.instances) ? kindWrapper.instances : [];
    for (const inst of instances) {
      const nid = inst.nodeId || inst.node_id;
      if (!nid) continue;
      if (!byNode.has(nid)) byNode.set(nid, []);
      const status = inst.status || '';
      byNode.get(nid).push({
        alias,
        kind: kindLabel,
        backend: inst.backend || '',
        size_mb: inst.sizeMb || inst.size_mb || 0,
        loaded: status === 'running' || status === 'ready',
      });
    }
  }
  for (const node of meshNodes) {
    const extra = byNode.get(node.node_id);
    if (!extra || extra.length === 0) continue;
    const existing = Array.isArray(node.models) ? node.models.slice() : [];
    const seen = new Set(existing.map((m) => m.alias).filter(Boolean));
    for (const m of extra) {
      if (!seen.has(m.alias)) {
        existing.push(m);
        seen.add(m.alias);
      }
    }
    node.models = existing;
  }
}


function collectUniqueModels() {
  // Source: modelListRequest (services_repo::models::list_alive) — JOIN of
  // model_registry + services WHERE status IN ('running','degraded'). Keys by
  // (model_name, engine_id) so the same alias served by multiple engines stays
  // distinguishable, and aggregates node_id into a chip list per row.
  const map = new Map();
  for (const m of modelsCache) {
    const alias = m.model_name || m.modelName || m.id;
    if (!alias) continue;
    const backend = m.engine_id || m.engineId || '';
    const kind = (m.category || '').toLowerCase();
    const key = `${alias}|${backend}`;
    const nodeInfo = nodeLabelFor(m.node_id || m.nodeId || '');
    const nodeLabel = nodeInfo.label;
    const isLoaded = (m.availability || '').toLowerCase() === 'running';
    if (!map.has(key)) {
      map.set(key, {
        alias,
        kind,
        backend,
        size_mb: 0,
        loaded: isLoaded,
        nodes: nodeLabel ? [nodeLabel] : [],
      });
    } else {
      const entry = map.get(key);
      if (nodeLabel && !entry.nodes.includes(nodeLabel)) entry.nodes.push(nodeLabel);
      entry.loaded = entry.loaded || isLoaded;
    }
  }
  return [...map.values()].sort((a, b) => a.alias.localeCompare(b.alias));
}

function renderModelsTab() {
  const models = collectUniqueModels();
  if (models.length === 0) {
    return `
      <div class="empty-big">
        ${sprite('model')}
        <h3>${escapeHtml(I18n.t('services.models_empty'))}</h3>
        <p>${escapeHtml(I18n.t('services.models_empty_hint'))}</p>
        <tf-button variant="primary" icon="plus" data-empty-cta>${escapeHtml(I18n.t('services.add_service'))}</tf-button>
      </div>
    `;
  }
  return `
    <table class="data-table">
      <thead>
        <tr>
          <th>${escapeHtml(I18n.t('services.model_col_alias'))}</th>
          <th>${escapeHtml(I18n.t('services.model_col_kind'))}</th>
          <th>${escapeHtml(I18n.t('services.model_col_backend'))}</th>
          <th>${escapeHtml(I18n.t('services.model_col_status'))}</th>
          <th>${escapeHtml(I18n.t('services.model_col_nodes'))}</th>
        </tr>
      </thead>
      <tbody>
        ${models.map(renderModelRow).join('')}
      </tbody>
    </table>
  `;
}

function renderModelRow(m) {
  const key = `${m.alias}|${m.backend}|${m.kind}`;
  const statusChip = m.loaded
    ? `<span class="tag-status online">● ${escapeHtml(I18n.t('services.model_loaded'))}</span>`
    : `<span class="tag-status offline">● ${escapeHtml(I18n.t('services.model_unloaded'))}</span>`;
  const nodeBadges = m.nodes.map((n) => `<span class="scope-chip mesh-read">${escapeHtml(n)}</span>`).join(' ');
  return `
    <tr data-key="model-${escapeAttr(key)}">
      <td data-label="${escapeAttr(I18n.t('services.model_col_alias'))}"><strong>${escapeHtml(m.alias)}</strong></td>
      <td data-label="${escapeAttr(I18n.t('services.model_col_kind'))}"><span class="scope-chip ${typeChipClass(m.kind)}">${escapeHtml(m.kind.toUpperCase() || '—')}</span></td>
      <td data-label="${escapeAttr(I18n.t('services.model_col_backend'))}"><code style="font-size:11px;">${escapeHtml(m.backend || '—')}</code></td>
      <td data-label="${escapeAttr(I18n.t('services.model_col_status'))}">${statusChip}</td>
      <td data-label="${escapeAttr(I18n.t('services.model_col_nodes'))}">${nodeBadges}</td>
    </tr>
  `;
}

// ---- Alias modal ----------------------------------------------------------

function buildTargetOptionList(_excludeAliasName) {
  // Plan v7 D.17: alias resolves to dokladnie jedna warstwa — service
  // model albo published flow + fallbacks. Domain validation w
  // `services::models` odrzuca alias-of-alias przy zapisie, wiec picker
  // celowo NIE pokazuje innych aliasow. Pokazujemy:
  //   1) service models (modelListRequest)
  //   2) published flows (catalogListRequest entries z kind=flow)
  const models = collectUniqueModels().map((m) => ({
    value: m.alias,
    label: m.alias + (m.backend ? ` · ${m.backend}` : '') + (m.kind ? ` (${m.kind})` : ''),
  }));
  const flows = (Array.isArray(unifiedModels) ? unifiedModels : [])
    .filter((e) => e && e.kind && e.kind.kind === 'flow' && typeof e.id === 'string')
    .map((e) => ({
      value: e.id,
      label: `${e.id} (flow)`,
    }));
  const seen = new Set();
  const out = [];
  for (const t of [...models, ...flows]) {
    if (!t.value || seen.has(t.value)) continue;
    seen.add(t.value);
    out.push(t);
  }
  return out.sort((a, b) => a.value.localeCompare(b.value));
}

function openAliasModal(alias) {
  const isEdit = !!alias;
  const targets = buildTargetOptionList(alias?.alias);

  // fallback_targets jest JSON-array stringiem (per CLAUDE.md "No CSV").
  // `parseFallbackTargets` zwraca `{ok, values}` zeby rozroznic walidne
  // puste `[]` od non-empty wartosci ktorej parser nie zrozumial (np.
  // CSV ze stale taba). `ok=false` blokuje save z UI errorem zeby
  // przypadkowo nie wymazac istniejacych fallbackow w bazie.
  const fallbackParse = isEdit
    ? parseFallbackTargets(alias.fallback_targets)
    : { ok: true, values: [] };
  const selectedFallbacks = fallbackParse.values;
  const fallbackParseFailed = !fallbackParse.ok;

  // Strategia w API: lowercase_snake (first_available | round_robin | least_loaded).
  const currentStrategy = (alias?.strategy || 'round_robin').toLowerCase();
  const currentTarget = alias?.target_model || '';

  const bodyEl = document.createElement('div');
  bodyEl.style.display = 'flex';
  bodyEl.style.flexDirection = 'column';
  bodyEl.style.gap = '14px';

  const targetOptionsHtml = targets.map((t) =>
    `<option value="${escapeAttr(t.value)}">${escapeHtml(t.label)}</option>`,
  ).join('');

  bodyEl.innerHTML = `
    <tf-input
      id="al-name"
      label="${escapeAttr(I18n.t('services.alias_col_name'))}"
      value="${escapeAttr(alias?.alias ?? '')}"
      placeholder="np. llm-fast"
      ${isEdit ? 'disabled' : ''}
    ></tf-input>

    <div class="form-row">
      <span class="tf-label">${escapeHtml(I18n.t('services.alias_col_target'))}</span>
      <tf-select id="al-target" value="${escapeAttr(currentTarget)}">
        <option value="">— ${escapeHtml(I18n.t('services.alias_target_placeholder'))} —</option>
        ${targetOptionsHtml}
      </tf-select>
    </div>

    <div class="form-row">
      <span class="tf-label">${escapeHtml(I18n.t('services.alias_col_strategy'))}</span>
      <tf-select id="al-strategy" value="${escapeAttr(currentStrategy)}">
        <option value="round_robin">${escapeHtml(I18n.t('services.strategy_round_robin'))}</option>
        <option value="first_available">${escapeHtml(I18n.t('services.strategy_first_available'))}</option>
        <option value="least_loaded">${escapeHtml(I18n.t('services.strategy_least_loaded'))}</option>
      </tf-select>
    </div>

    <div class="form-row">
      <span class="tf-label">${escapeHtml(I18n.t('services.alias_col_fallback'))}</span>
      <div id="al-fallback-chips" style="display:flex;flex-wrap:wrap;gap:6px;min-height:24px;margin-bottom:6px;"></div>
      <tf-select id="al-fallback-add">
        <option value="">— ${escapeHtml(I18n.t('services.alias_add_fallback'))} —</option>
        ${targetOptionsHtml}
      </tf-select>
      <span class="tf-hint">${escapeHtml(I18n.t('services.alias_fallback_hint'))}</span>
    </div>

    <div class="form-error" id="al-error" hidden style="color: var(--danger, #ef4444); font-size: 12px;"></div>
  `;

  const footerEl = document.createElement('div');
  footerEl.innerHTML = `
    <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
    <tf-button variant="primary" data-action="save" id="al-save-btn">${escapeHtml(I18n.t(isEdit ? 'common.save' : 'common.add'))}</tf-button>
  `;

  // Wlasne tf-window — walidacja i bledy API nie powinny zamykac okna.
  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t(isEdit ? 'services.alias_edit' : 'services.new_alias'));
  win.setAttribute('icon', isEdit ? 'settings' : 'plus');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('min-width', '460');
  win.setAttribute('min-height', '420');
  win.setAttribute('width', '520');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');

  const bodyWrap = document.createElement('div');
  bodyWrap.slot = 'body';
  bodyWrap.appendChild(bodyEl);
  win.appendChild(bodyWrap);

  const footWrap = document.createElement('div');
  footWrap.slot = 'footer';
  footWrap.appendChild(footerEl);
  win.appendChild(footWrap);

  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  document.body.appendChild(backdrop);
  document.body.appendChild(win);

  const cleanup = () => {
    if (win.isConnected) win.remove();
    if (backdrop.isConnected) backdrop.remove();
  };

  // Render chipow wybranych fallbackow
  function renderFallbackChips() {
    const c = bodyEl.querySelector('#al-fallback-chips');
    if (!c) return;
    if (selectedFallbacks.length === 0) {
      c.innerHTML = `<span style="color:var(--text-3);font-size:12px;">—</span>`;
      return;
    }
    c.innerHTML = selectedFallbacks.map((name, idx) => `
      <tf-chip status="info" style="cursor:default;">
        ${escapeHtml(name)}
        <span style="margin-left:6px;cursor:pointer;font-weight:bold;" data-rm="${idx}">×</span>
      </tf-chip>
    `).join('');
  }
  renderFallbackChips();

  // Dodawanie fallback z tf-select
  queueMicrotask(() => {
    const addSelect = bodyEl.querySelector('#al-fallback-add');
    if (addSelect) {
      addSelect.addEventListener('change', (e) => {
        const val = e.detail?.value ?? addSelect.value;
        const primaryTarget = bodyEl.querySelector('#al-target')?.value || '';
        if (val && !selectedFallbacks.includes(val) && val !== primaryTarget) {
          selectedFallbacks.push(val);
          renderFallbackChips();
        }
        // reset selekcji do placeholder
        addSelect.setAttribute('value', '');
        const innerSel = addSelect.querySelector('select');
        if (innerSel) innerSel.value = '';
      });
    }
  });

  // Usuwanie chipow przez delegacje
  bodyEl.querySelector('#al-fallback-chips').addEventListener('click', (e) => {
    const btn = e.target.closest('[data-rm]');
    if (!btn) return;
    const idx = parseInt(btn.dataset.rm, 10);
    if (!Number.isNaN(idx)) {
      selectedFallbacks.splice(idx, 1);
      renderFallbackChips();
    }
  });

  win.addEventListener('action', async (e) => {
    const action = e.detail?.action;
    if (action === 'close' || action === 'cancel') {
      cleanup();
      return;
    }
    if (action !== 'save') return;

    const nameInput = win.querySelector('#al-name');
    const targetSelect = win.querySelector('#al-target');
    const strategySelect = win.querySelector('#al-strategy');
    const errEl = win.querySelector('#al-error');

    const name = (nameInput?.value || '').trim();
    const target = (targetSelect?.value || '').trim();
    const strategy = (strategySelect?.value || 'round_robin').toLowerCase();
    // Backend kanonicznie trzyma fallback_targets jako JSON array string
    // (patrz "No CSV — always JSON" w CLAUDE.md). Pusta lista zapisuje
    // sie jako null zeby nie zaciemniac DB pustym '[]'.
    const fallback = selectedFallbacks.length > 0 ? JSON.stringify(selectedFallbacks) : null;

    if (!name || !target) {
      if (errEl) {
        errEl.textContent = I18n.t('services.alias_required');
        errEl.hidden = false;
      }
      return;
    }

    // R7.P3: `parseFallbackTargets` zwraca `ok=false` tylko gdy backend
    // dal cos non-empty czego nie umielismy zinterpretowac (np. CSV ze
    // stale taba). Walidne puste `[]` jest semantycznie OK — user moze
    // celowo skasowac wszystkie fallbacki. Blokujemy save tylko dla
    // ok=false zeby nie wymazac niewidocznych dla nas wartosci.
    if (fallbackParseFailed && selectedFallbacks.length === 0) {
      if (errEl) {
        errEl.textContent = I18n.t('services.alias_fallback_parse_failed');
        errEl.hidden = false;
      }
      return;
    }

    try {
      if (isEdit) {
        await ApiBinary.action('modelAliasUpdateRequest', {
          id: alias.id,
          alias: name,
          targetModel: target,
          isActive: true,
          strategy,
          fallbackTargets: fallback || null,
        });
      } else {
        await ApiBinary.action('modelAliasCreateRequest', {
          alias: name,
          targetModel: target,
          strategy,
          fallbackTargets: fallback || null,
        });
      }
      toast(I18n.t(isEdit ? 'services.alias_updated' : 'services.alias_created'), 'success');
      cleanup();
      await loadAll();
    } catch (err) {
      if (errEl) {
        errEl.textContent = err.message;
        errEl.hidden = false;
      }
    }
  });
}

async function deleteAlias(id, name) {
  const ok = await TfWindow.confirm({
    title: I18n.t('common.delete'),
    message: I18n.t('services.alias_delete_confirm', { name }),
    confirmLabel: I18n.t('common.delete'),
    cancelLabel: I18n.t('common.cancel'),
    danger: true,
  });
  if (!ok) return;
  try {
    await ApiBinary.action('modelAliasDeleteRequest', { id });
    toast(I18n.t('services.alias_deleted', { name }), 'success');
    await loadAll();
  } catch (e) {
    toast(`${I18n.t('common.error')}: ${e.message}`, 'error');
  }
}

// ---- Helpers --------------------------------------------------------------

async function stopService(id, name, nodeId, nodeLabel) {
  const ok = await TfWindow.confirm({
    title: I18n.t('services.confirm_delete_title'),
    message: I18n.t('services.confirm_delete_body', {
      name: name || id,
      node: nodeLabel || nodeId || '—',
    }),
    confirmLabel: I18n.t('services.btn_delete'),
    cancelLabel: I18n.t('common.cancel'),
    danger: true,
  });
  if (!ok) return;
  try {
    // ServiceDeleteRequest stops the runtime AND removes the row; FK cascade
    // wipes attached model_registry rows. When nodeId points at a remote peer
    // the dispatcher forwards the call as `MeshCommandType::ServiceDeleteRemote`.
    const resp = await ApiBinary.action('serviceDeleteRequest', {
      serviceId: id,
      nodeId: nodeId || undefined,
    });
    if (resp && resp.success === false) {
      throw new Error(resp.error || 'Unknown error');
    }
    await refreshServiceList();
  } catch (err) {
    showRowError(id, err.message);
  }
}

// Pause/Play toggle handler. Reads action from data-svc-action set by renderer
// and posts the matching binary RPC. The button's icon is updated inline (rotate
// = pending) until the refresh swaps the row in.
async function togglePauseStart(button) {
  const id = button.dataset.svcPausePlay;
  const action = button.dataset.svcAction;
  const nodeId = button.dataset.svcNode;
  if (!id || !action) return;
  button.setAttribute('disabled', '');
  button.setAttribute('icon', 'rotate');
  try {
    if (action === 'pause') {
      await ApiBinary.action('servicePauseRequest', {
        serviceId: id,
        nodeId: nodeId || undefined,
        paused: true,
      });
    } else {
      await ApiBinary.action('serviceStartRequest', {
        serviceId: id,
        nodeId: nodeId || undefined,
      });
    }
    await refreshServiceList();
  } catch (err) {
    showRowError(id, err.message);
    button.removeAttribute('disabled');
    button.setAttribute('icon', action === 'pause' ? 'pause' : 'play');
  }
}

// Pin toggle handler — flips data-svc-pinned, posts servicePinRequest with the
// new state, and refreshes the list. The .pinned class is rebuilt by render.
async function togglePin(button) {
  const id = button.dataset.svcPinToggle;
  const nodeId = button.dataset.svcNode;
  const current = button.dataset.svcPinned === 'true';
  if (!id) return;
  const next = !current;
  button.setAttribute('disabled', '');
  // Optimistic flip — keeps the icon state consistent during the round-trip.
  button.classList.toggle('pinned', next);
  button.dataset.svcPinned = next ? 'true' : 'false';
  try {
    await ApiBinary.action('servicePinRequest', {
      serviceId: id,
      nodeId: nodeId || undefined,
      pinned: next,
    });
    await refreshServiceList();
  } catch (err) {
    // Rollback optimistic flip.
    button.classList.toggle('pinned', current);
    button.dataset.svcPinned = current ? 'true' : 'false';
    button.removeAttribute('disabled');
    showRowError(id, err.message);
  }
}

// Pull a fresh service list and patch the table without flicker.
async function refreshServiceList() {
  const fresh = await ApiBinary.list('serviceListRequest', { arrayKey: 'services' })
    .catch(() => services);
  services = Array.isArray(fresh) ? fresh : [];
  patchListTab();
  updateSubtitle();
  updateTabCounts();
}

// Render a per-row error string under the action cell. Auto-hides via CSS
// animation (5s). Replaces any prior message so the user sees the latest.
function showRowError(serviceId, message) {
  const el = document.querySelector(`[data-svc-error="${CSS.escape(String(serviceId))}"]`);
  if (!el) return;
  el.textContent = message || I18n.t('common.error');
  el.hidden = false;
  // Restart CSS animation by reflow + class toggle.
  el.classList.remove('svc-row-error');
  void el.offsetWidth; // force reflow
  el.classList.add('svc-row-error');
  setTimeout(() => {
    if (el.isConnected) el.hidden = true;
  }, 5000);
}

function typeChipClass(t) {
  switch ((t || '').toLowerCase()) {
    case 'llm': return 'chat';
    case 'embedding':
    case 'embeddings': return 'mesh-read';
    case 'stt':
    case 'tts': return 'deploy';
    case 'agent':
    case 'tool': return 'mesh-admin';
    default: return 'license';
  }
}

export default ServicesScreen;
