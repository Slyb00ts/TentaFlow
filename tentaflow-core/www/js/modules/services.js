// =============================================================================
// Plik: modules/services.js
// Opis: Ekran Services — 3 zakladki (tf-tabs underline):
//       1) Lista   — tabela deployowanych serwisow z auto-refresh 5s
//       2) Aliasy  — CRUD aliasow modeli (/api/model-aliases), edycja w tf-window
//       3) Modele  — zbiorcza lista modeli ze wszystkich nodow mesh
//      "Nowy serwis" otwiera Catalog (target picker → wizard). Edycja aliasu
//      oraz potwierdzenia usuwania korzystaja z komponentu <tf-window>.
//      Auto-refresh uzywa morphdom przez /js/lib/patch.js zeby nie migotac.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast, formatDate, apiGet, apiPost, apiDelete } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { Router } from '/js/router.js';
import { patchInner } from '/js/lib/patch.js';
import { TfWindow } from '/js/components/tf-window.js';

let services = [];
let aliases = [];
let meshNodes = [];
let quicStatusMap = {};
let refreshTimer = null;
let quicTimer = null;
let currentTab = 'list';
let lastQuery = '';

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

      <tf-tabs variant="underline" value="list" id="svc-tabs">
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
    refreshTimer = setInterval(() => {
      loadForCurrentTab();
    }, 5000);
    quicTimer = setInterval(loadQuicStatus, 5000);
  },
  unmount() {
    if (refreshTimer) clearInterval(refreshTimer);
    if (quicTimer) clearInterval(quicTimer);
    refreshTimer = null;
    quicTimer = null;
    services = [];
    aliases = [];
    meshNodes = [];
    quicStatusMap = {};
  },
};

// ---- Data loading ---------------------------------------------------------

async function loadAll() {
  try {
    const [svc, al, nodes] = await Promise.all([
      ApiBinary.list('serviceListRequest').catch(() => []),
      apiGet('/api/model-aliases').catch(() => []),
      apiGet('/api/mesh/nodes').catch(() => []),
    ]);
    services = svc || [];
    aliases = al || [];
    meshNodes = nodes || [];
    renderTab();
    updateSubtitle();
    updateTabCounts();
    await loadQuicStatus();
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

async function loadForCurrentTab() {
  try {
    if (currentTab === 'list') {
      services = await ApiBinary.list('serviceListRequest');
      patchListTab();
    } else if (currentTab === 'aliases') {
      aliases = await apiGet('/api/model-aliases');
      patchAliasesTab();
    } else if (currentTab === 'models') {
      meshNodes = await apiGet('/api/mesh/nodes');
      patchModelsTab();
    }
    updateSubtitle();
    updateTabCounts();
  } catch (err) {
    console.warn('[services] refresh failed:', err.message);
  }
}

async function loadQuicStatus() {
  try {
    const resp = await ApiBinary.one('serviceQuicStatusRequest');
    const next = {};
    for (const item of resp.statuses ?? []) next[item.name] = item.status;
    quicStatusMap = next;
    if (currentTab === 'list') updateQuicDots();
  } catch (err) {
    console.warn('[services] quic status:', err.message);
  }
}

function updateSubtitle() {
  const running = services.filter((s) => ['running', 'active', 'ready'].includes((s.status || '').toLowerCase())).length;
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
  if (currentTab === 'list') updateQuicDots();
}

function patchListTab() {
  if (currentTab !== 'list') return;
  const body = byId('svc-tab-body');
  if (!body) return;
  patchInner(body, renderListTab());
  bindTabEvents();
  updateQuicDots();
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

  // List tab
  body.querySelectorAll('[data-svc-delete]').forEach((b) => {
    b.onclick = (e) => {
      e.stopPropagation();
      stopService(b.dataset.svcDelete, b.dataset.svcName);
    };
  });
  body.querySelectorAll('[data-empty-cta]').forEach((b) => {
    b.onclick = () => Router.navigate('catalog');
  });

  // Aliases tab
  body.querySelectorAll('[data-alias-edit]').forEach((b) => {
    b.onclick = () => {
      const a = aliases.find((x) => String(x.id) === b.dataset.aliasEdit);
      if (a) openAliasModal(a);
    };
  });
  body.querySelectorAll('[data-alias-delete]').forEach((b) => {
    b.onclick = () => deleteAlias(b.dataset.aliasDelete, b.dataset.aliasName);
  });
  body.querySelectorAll('[data-new-alias]').forEach((b) => {
    b.onclick = () => openAliasModal(null);
  });
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
          <th>${escapeHtml(I18n.t('services.col_name'))}</th>
          <th>${escapeHtml(I18n.t('services.col_type'))}</th>
          <th>${escapeHtml(I18n.t('services.col_node'))}</th>
          <th>${escapeHtml(I18n.t('services.col_quic_address'))}</th>
          <th>${escapeHtml(I18n.t('services.col_quic_status'))}</th>
          <th>${escapeHtml(I18n.t('services.col_created'))}</th>
          <th style="text-align:right;">${escapeHtml(I18n.t('services.col_actions'))}</th>
        </tr>
      </thead>
      <tbody>
        ${services.map(renderRow).join('')}
      </tbody>
    </table>
  `;
}

function renderRow(s) {
  const cfg = parseConfig(s.configJson);
  const quicAddr = extractQuicAddr(cfg);
  const nodeLabel = s.nodeHostname
    || (s.nodeId ? `${s.nodeId.slice(0, 12)}…` : I18n.t('services.deploy_local'));
  return `
    <tr data-key="svc-${escapeAttr(s.id)}">
      <td data-label="${escapeAttr(I18n.t('services.col_name'))}"><strong style="color: var(--accent-2);">${escapeHtml(s.name)}</strong></td>
      <td data-label="${escapeAttr(I18n.t('services.col_type'))}"><span class="scope-chip ${typeChipClass(s.serviceType)}">${escapeHtml((s.serviceType || '').toUpperCase())}</span></td>
      <td data-label="${escapeAttr(I18n.t('services.col_node'))}" style="font-size: 12px;">${escapeHtml(nodeLabel)}</td>
      <td data-label="${escapeAttr(I18n.t('services.col_quic_address'))}">${quicAddr ? `<code style="font-size:11px;">${escapeHtml(quicAddr)}</code>` : '<span style="color:var(--text-3);">—</span>'}</td>
      <td data-label="${escapeAttr(I18n.t('services.col_quic_status'))}"><span data-quic-status="${escapeAttr(s.name)}"><span class="tag-status offline">${escapeHtml(I18n.t('services.status.none'))}</span></span></td>
      <td data-label="${escapeAttr(I18n.t('services.col_created'))}" style="font-size:11px;color:var(--text-3);">${s.createdAt ? escapeHtml(formatDateOnly(s.createdAt)) : '—'}</td>
      <td data-label="${escapeAttr(I18n.t('services.col_actions'))}" style="text-align:right;">
        <tf-button variant="danger" size="sm" icon="trash" data-svc-delete="${escapeAttr(s.id)}" data-svc-name="${escapeAttr(s.name)}" title="${escapeAttr(I18n.t('common.delete'))}"></tf-button>
      </td>
    </tr>
  `;
}

function updateQuicDots() {
  document.querySelectorAll('[data-quic-status]').forEach((el) => {
    const name = el.dataset.quicStatus;
    const raw = (quicStatusMap[name] || 'none').toLowerCase();
    let cls = 'offline', labelKey = 'services.status.none';
    if (raw === 'connected' || raw === 'ready') {
      cls = 'online';
      labelKey = raw === 'ready' ? 'services.status.ready' : 'services.status.connected';
    } else if (raw === 'connecting') { cls = 'pending'; labelKey = 'services.status.connecting'; }
    else if (raw === 'disconnected') { cls = 'offline'; labelKey = 'services.status.disconnected'; }
    else if (raw === 'config_error') { cls = 'offline'; labelKey = 'services.status.config_error'; }
    const target = `<span class="tag-status ${cls}">${escapeHtml(I18n.t(labelKey))}</span>`;
    if (el.innerHTML !== target) el.innerHTML = target;
  });
}

// ---- Aliases tab ----------------------------------------------------------

function renderAliasesTab() {
  return `
    <div class="info-card">
      <div>${sprite('info')}<strong>${escapeHtml(I18n.t('services.aliases_info_title'))}</strong> — ${escapeHtml(I18n.t('services.aliases_info_body'))}</div>
    </div>
    ${aliases.length === 0 ? `
      <div class="empty-big">
        ${sprite('share')}
        <h3>${escapeHtml(I18n.t('services.aliases_empty'))}</h3>
        <p>${escapeHtml(I18n.t('services.aliases_empty_hint'))}</p>
        <tf-button variant="primary" icon="plus" data-new-alias>${escapeHtml(I18n.t('services.new_alias'))}</tf-button>
      </div>
    ` : `
      <div class="svc-aliases-toolbar">
        <tf-button variant="primary" size="sm" icon="plus" data-new-alias>${escapeHtml(I18n.t('services.new_alias'))}</tf-button>
      </div>
      <table class="data-table">
        <thead>
          <tr>
            <th>${escapeHtml(I18n.t('services.alias_col_name'))}</th>
            <th>${escapeHtml(I18n.t('services.alias_col_target'))}</th>
            <th>${escapeHtml(I18n.t('services.alias_col_strategy'))}</th>
            <th>${escapeHtml(I18n.t('services.alias_col_fallback'))}</th>
            <th>${escapeHtml(I18n.t('services.alias_col_active'))}</th>
            <th style="text-align:right;">${escapeHtml(I18n.t('services.col_actions'))}</th>
          </tr>
        </thead>
        <tbody>
          ${aliases.map(renderAliasRow).join('')}
        </tbody>
      </table>
    `}
  `;
}

function renderAliasRow(a) {
  const fallbacks = (a.fallback_targets || '').split(',').map((x) => x.trim()).filter(Boolean);
  const activeBadge = a.is_active
    ? `<span class="tag-status online">● ${escapeHtml(I18n.t('services.alias_active'))}</span>`
    : `<span class="tag-status offline">● ${escapeHtml(I18n.t('services.alias_inactive'))}</span>`;
  return `
    <tr data-key="alias-${escapeAttr(a.id)}">
      <td data-label="${escapeAttr(I18n.t('services.alias_col_name'))}"><strong style="color:var(--accent-2);">${escapeHtml(a.alias)}</strong></td>
      <td data-label="${escapeAttr(I18n.t('services.alias_col_target'))}"><code style="font-size:11px;">${escapeHtml(a.target_model)}</code></td>
      <td data-label="${escapeAttr(I18n.t('services.alias_col_strategy'))}"><span class="scope-chip chat">${escapeHtml(a.strategy || 'FirstAvailable')}</span></td>
      <td data-label="${escapeAttr(I18n.t('services.alias_col_fallback'))}">${fallbacks.length > 0 ? fallbacks.map((f) => `<span class="scope-chip mesh-read">${escapeHtml(f)}</span>`).join(' ') : '<span style="color:var(--text-3);">—</span>'}</td>
      <td data-label="${escapeAttr(I18n.t('services.alias_col_active'))}">${activeBadge}</td>
      <td data-label="${escapeAttr(I18n.t('services.col_actions'))}" style="text-align:right;">
        <tf-button variant="ghost" size="sm" icon="settings" data-alias-edit="${escapeAttr(a.id)}" title="${escapeAttr(I18n.t('common.edit'))}"></tf-button>
        <tf-button variant="danger" size="sm" icon="trash" data-alias-delete="${escapeAttr(a.id)}" data-alias-name="${escapeAttr(a.alias)}" title="${escapeAttr(I18n.t('common.delete'))}"></tf-button>
      </td>
    </tr>
  `;
}

// ---- Models tab -----------------------------------------------------------

function collectUniqueModels() {
  // Zbiera z meshNodes[].models[] — grupuj po kluczu (alias|backend|kind).
  const map = new Map();
  for (const n of meshNodes) {
    const list = Array.isArray(n.models) ? n.models : [];
    const nodeLabel = n.hostname || (n.node_id ? n.node_id.slice(0, 12) : '?');
    for (const m of list) {
      const key = `${m.alias}|${m.backend || ''}|${m.kind || ''}`;
      if (!map.has(key)) {
        map.set(key, {
          alias: m.alias,
          kind: m.kind || '',
          backend: m.backend || '',
          size_mb: m.size_mb || 0,
          loaded: !!m.loaded,
          nodes: [nodeLabel],
        });
      } else {
        const entry = map.get(key);
        if (!entry.nodes.includes(nodeLabel)) entry.nodes.push(nodeLabel);
        entry.loaded = entry.loaded || m.loaded;
      }
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

function openAliasModal(alias) {
  const isEdit = !!alias;
  const availableTargets = collectUniqueModels().map((m) => m.alias);

  // Body formularza — tf-input dla pol tekstowych, tf-select dla strategii.
  const bodyEl = document.createElement('div');
  bodyEl.innerHTML = `
    <div class="form-row">
      <tf-input
        id="al-name"
        label="${escapeAttr(I18n.t('services.alias_col_name'))}"
        value="${escapeAttr(alias?.alias ?? '')}"
        placeholder="llm-fast"
        ${isEdit ? 'disabled' : ''}
      ></tf-input>
    </div>
    <div class="form-row">
      <tf-input
        id="al-target"
        label="${escapeAttr(I18n.t('services.alias_col_target'))}"
        value="${escapeAttr(alias?.target_model ?? '')}"
        placeholder="llm-chat"
      ></tf-input>
      <datalist id="al-target-list">
        ${availableTargets.map((t) => `<option value="${escapeAttr(t)}"></option>`).join('')}
      </datalist>
    </div>
    <div class="form-row">
      <span class="tf-label">${escapeHtml(I18n.t('services.alias_col_strategy'))}</span>
      <tf-select id="al-strategy" value="${escapeAttr(alias?.strategy || 'FirstAvailable')}">
        <option value="FirstAvailable">FirstAvailable</option>
        <option value="RoundRobin">RoundRobin</option>
        <option value="LeastLoaded">LeastLoaded</option>
      </tf-select>
    </div>
    <div class="form-row">
      <tf-input
        id="al-fallback"
        label="${escapeAttr(I18n.t('services.alias_col_fallback'))}"
        value="${escapeAttr(alias?.fallback_targets ?? '')}"
        placeholder="llm-big, llm-chat"
        hint="${escapeAttr(I18n.t('services.alias_fallback_hint'))}"
      ></tf-input>
    </div>
    <div class="form-error" id="al-error" hidden style="color: var(--color-danger, #ef4444); font-size: 12px; margin-top: var(--space-2);"></div>
  `;

  const footerEl = document.createElement('div');
  footerEl.innerHTML = `
    <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
    <tf-button variant="primary" data-action="save" id="al-save-btn">${escapeHtml(I18n.t(isEdit ? 'common.save' : 'common.add'))}</tf-button>
  `;

  // Tworzymy okno recznie — potrzebujemy kontroli nad zamknieciem (walidacja
  // + bledy API powinny pozostawic okno otwarte, a nie kazdy action je zamyka).
  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t(isEdit ? 'services.alias_edit' : 'services.new_alias'));
  win.setAttribute('icon', isEdit ? 'settings' : 'plus');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('min-width', '460');
  win.setAttribute('min-height', '380');
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

  // Natywny atrybut `list` nie jest przekazywany przez tf-input — ustawiamy
  // go recznie na wewnetrznym <input> aby zachowac autocomplete.
  queueMicrotask(() => {
    const targetNative = win.querySelector('#al-target input');
    if (targetNative) targetNative.setAttribute('list', 'al-target-list');
  });

  const cleanup = () => {
    if (win.isConnected) win.remove();
    if (backdrop.isConnected) backdrop.remove();
  };

  win.addEventListener('action', async (e) => {
    const action = e.detail?.action;
    if (action === 'close' || action === 'cancel') {
      cleanup();
      return;
    }
    if (action !== 'save') return;

    const nameInput = win.querySelector('#al-name');
    const targetInput = win.querySelector('#al-target');
    const strategySelect = win.querySelector('#al-strategy');
    const fallbackInput = win.querySelector('#al-fallback');
    const errEl = win.querySelector('#al-error');

    const name = (nameInput?.value || '').trim();
    const target = (targetInput?.value || '').trim();
    const strategy = strategySelect?.value || 'FirstAvailable';
    const fallback = (fallbackInput?.value || '').trim();

    if (!name || !target) {
      if (errEl) {
        errEl.textContent = I18n.t('services.alias_required');
        errEl.hidden = false;
      }
      return;
    }

    try {
      if (isEdit) {
        await fetch(`/api/model-aliases/${encodeURIComponent(alias.id)}`, {
          method: 'PUT',
          headers: authHeaders(true),
          body: JSON.stringify({ alias: name, target_model: target, is_active: true }),
        }).then(checkOk);
      } else {
        await apiPost('/api/model-aliases', {
          alias: name,
          target_model: target,
          strategy,
          fallback_targets: fallback || null,
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
    await apiDelete(`/api/model-aliases/${encodeURIComponent(id)}`);
    toast(I18n.t('services.alias_deleted', { name }), 'success');
    await loadAll();
  } catch (e) {
    toast(`${I18n.t('common.error')}: ${e.message}`, 'error');
  }
}

// ---- Helpers --------------------------------------------------------------

function authHeaders(json) {
  const jwt = localStorage.getItem('tentaflow_jwt');
  const h = {};
  if (json) h['Content-Type'] = 'application/json';
  if (jwt) h['Authorization'] = `Bearer ${jwt}`;
  return h;
}

async function checkOk(resp) {
  if (!resp.ok) {
    const text = await resp.text().catch(() => '');
    throw new Error(`${resp.status}${text ? `: ${text}` : ''}`);
  }
  return resp.json();
}

async function stopService(id, name) {
  const ok = await TfWindow.confirm({
    title: I18n.t('common.delete'),
    message: I18n.t('services.delete_confirm', { name }),
    confirmLabel: I18n.t('common.delete'),
    cancelLabel: I18n.t('common.cancel'),
    danger: true,
  });
  if (!ok) return;
  try {
    const r = await ApiBinary.action('serviceStopRequest', { serviceId: id });
    if (r.stopped) {
      toast(I18n.t('services.delete_success', { name }), 'success');
      services = await ApiBinary.list('serviceListRequest');
      patchListTab();
      updateSubtitle();
      updateTabCounts();
    } else {
      toast(I18n.t('services.delete_not_found'), 'warning');
    }
  } catch (err) {
    toast(I18n.t('services.delete_error', { error: err.message }), 'error');
  }
}

function parseConfig(json) {
  if (!json) return {};
  try { return JSON.parse(json); } catch { return {}; }
}

function extractQuicAddr(cfg) {
  if (!cfg) return '';
  if (cfg.quic_url) return String(cfg.quic_url).replace(/^quic:\/\//, '');
  if (cfg.quic_port && cfg.agent_domain) return `${cfg.agent_domain}:${cfg.quic_port}`;
  return '';
}

function typeChipClass(t) {
  switch ((t || '').toLowerCase()) {
    case 'llm': return 'chat';
    case 'embedding':
    case 'embeddings': return 'mesh-read';
    case 'stt':
    case 'tts': return 'deploy';
    case 'rag': return 'mesh-admin';
    default: return 'license';
  }
}

function formatDateOnly(s) {
  try {
    const d = new Date(s);
    if (isNaN(d.getTime())) return s;
    return d.toLocaleDateString(I18n.getLanguage(), { day: '2-digit', month: '2-digit', year: 'numeric' });
  } catch { return s; }
}

export default ServicesScreen;
