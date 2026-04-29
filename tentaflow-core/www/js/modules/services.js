// =============================================================================
// File: modules/services.js — Services screen with 3 tabs (tf-tabs underline)
//   1) List     — deployed services table, REST /api/services, auto-refresh 5s
//   2) Aliases  — model alias CRUD (binary modelAlias*Request), tf-window editor
//   3) Models   — mesh-wide model aggregate (binary modelsUnifiedListRequest)
//   "New service" opens Catalog (target picker → wizard). Aliases edit + delete
//   confirmations use <tf-window>. Auto-refresh uses morphdom via /js/lib/patch.js
//   so the table does not flicker.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast, apiGet, apiDelete } from '/js/utils.js';
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
let refresher = null;
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
  },
};

// ---- Data loading ---------------------------------------------------------

async function loadAll() {
  try {
    const [svc, al, nodes, unified] = await Promise.all([
      apiGet('/api/services').catch(() => []),
      ApiBinary.list('modelAliasListRequest', { arrayKey: 'aliases' }).catch(() => []),
      ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' }).catch(() => []),
      ApiBinary.list('modelsUnifiedListRequest', { arrayKey: 'models' }).catch(() => []),
      ManifestStore.init().catch(() => false),
    ]);
    services = Array.isArray(svc) ? svc : [];
    aliases = al || [];
    meshNodes = nodes || [];
    unifiedModels = Array.isArray(unified) ? unified : [];
    // Lokalny service_registry jest swiezy — /api/mesh/nodes nie ma modeli
    // lokalnego noda az heartbeat nie ustabilizuje sie. Mergujemy z modelsUnifiedListRequest.
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
      const svc = await apiGet('/api/services');
      services = Array.isArray(svc) ? svc : [];
      patchListTab();
    } else if (currentTab === 'aliases') {
      aliases = await ApiBinary.list('modelAliasListRequest', { arrayKey: 'aliases' });
      patchAliasesTab();
    } else if (currentTab === 'models') {
      const [nodes, unified] = await Promise.all([
        ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' }).catch(() => []),
        ApiBinary.list('modelsUnifiedListRequest', { arrayKey: 'models' }).catch(() => []),
      ]);
      meshNodes = nodes || [];
      unifiedModels = Array.isArray(unified) ? unified : [];
      mergeUnifiedModelsIntoNodes();
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
          <th>${escapeHtml(I18n.t('services.col_engine'))}</th>
          <th>${escapeHtml(I18n.t('services.col_method'))}</th>
          <th>${escapeHtml(I18n.t('services.col_transport'))}</th>
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

// Maps services.status (running|degraded|failed|starting|stopped) onto the
// tf-chip status palette. degraded → pending (yellow), failed → err (red).
function mapStatusToChip(status) {
  switch ((status || '').toLowerCase()) {
    case 'running': return { variant: 'online', dot: true };
    case 'degraded': return { variant: 'pending', dot: true };
    case 'failed': return { variant: 'err', dot: true };
    case 'starting': return { variant: 'info', dot: true };
    case 'stopped': return { variant: 'offline', dot: false };
    default: return { variant: 'info', dot: false };
  }
}

// Translates the deploy_method db tag (docker|native_python_bundle|native_binary|
// embedded|external) to a short human label. Falls back to raw value.
function deployMethodLabel(method) {
  const raw = (method || '').toLowerCase();
  switch (raw) {
    case 'docker': return I18n.t('wizard.method.docker') || 'Docker';
    case 'native_python_bundle': return 'Native (Python)';
    case 'native_binary': return 'Native (binary)';
    case 'embedded': return I18n.t('wizard.method.embedded') || 'Embedded';
    case 'external': return I18n.t('wizard.method.external') || 'External';
    default: return raw || '—';
  }
}

// Transport tag (http_direct | quic_sidecar | embedded_inproc) → short label.
function transportLabel(transport) {
  const raw = (transport || '').toLowerCase();
  if (raw === 'http_direct') return 'HTTP';
  if (raw === 'quic_sidecar') return 'QUIC';
  if (raw === 'embedded_inproc') return 'inproc';
  return raw || '—';
}

function renderRow(s) {
  const statusInfo = mapStatusToChip(s.status);
  const statusLabel = I18n.t(`services.status.${(s.status || '').toLowerCase()}`)
    || s.status || '—';
  const endpoint = s.endpoint_url
    ? `<code style="font-size:11px;">${escapeHtml(s.endpoint_url)}</code>`
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
  const restartCell = restartCount > 0
    ? `<tf-chip status="warn">${restartCount}</tf-chip>`
    : '<span style="color:var(--text-3);">0</span>';
  const displayName = s.engine_id || '';
  return `
    <tr data-key="svc-${escapeAttr(s.id)}">
      <td data-label="${escapeAttr(I18n.t('services.col_engine'))}">
        <strong style="color: var(--accent-2);">${escapeHtml(displayName)}</strong>
      </td>
      <td data-label="${escapeAttr(I18n.t('services.col_method'))}">
        <span class="scope-chip mesh-admin">${escapeHtml(deployMethodLabel(s.deploy_method))}</span>
      </td>
      <td data-label="${escapeAttr(I18n.t('services.col_transport'))}">
        <span class="scope-chip mesh-read">${escapeHtml(transportLabel(s.transport))}</span>
      </td>
      <td data-label="${escapeAttr(I18n.t('services.col_status'))}">
        <tf-chip status="${statusInfo.variant}"${statusInfo.dot ? ' dot' : ''}>${escapeHtml(statusLabel)}</tf-chip>
      </td>
      <td data-label="${escapeAttr(I18n.t('services.col_endpoint'))}">${endpoint}</td>
      <td data-label="${escapeAttr(I18n.t('services.col_models'))}">
        <div style="display:flex;flex-wrap:wrap;gap:4px;">${modelChips}</div>
      </td>
      <td data-label="${escapeAttr(I18n.t('services.col_restart'))}">${restartCell}</td>
      <td data-label="${escapeAttr(I18n.t('services.col_actions'))}" style="text-align:right;white-space:nowrap;">
        <tf-button variant="danger" size="sm" icon="trash"
          data-svc-delete="${escapeAttr(s.id)}"
          data-svc-name="${escapeAttr(displayName)}"
          title="${escapeAttr(I18n.t('common.delete'))}"></tf-button>
      </td>
    </tr>
  `;
}

// ---- Aliases tab ----------------------------------------------------------

function renderAliasesTab() {
  const title = escapeHtml(I18n.t('services.aliases_info_title'));
  // Strzalka w tresci jako ikona zamiast znaku → — ladniejsze wyrownanie
  // wertykalne, spojne z innymi strzalkami w UI.
  const arrowIcon = '<svg width="12" height="12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="display:inline-block;vertical-align:-1px;margin:0 4px;color:var(--accent-2)"><use href="#i-chevron-right"/></svg>';
  const body = escapeHtml(I18n.t('services.aliases_info_body')).replace('→', arrowIcon);
  return `
    <div class="info-card">
      ${sprite('info')}
      <div><strong>${title}</strong> — ${body}</div>
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

// Dokleja modele z modelsUnifiedListRequest do odpowiednich nodow w meshNodes —
// konieczne, bo /api/mesh/nodes nie zawsze ma modele lokalnego noda (peer_store
// aktualizuje sie dopiero po heartbeat). Dedup po aliasie.
function mergeUnifiedModelsIntoNodes() {
  if (!Array.isArray(unifiedModels) || unifiedModels.length === 0) return;
  const byNode = new Map();
  for (const m of unifiedModels) {
    const alias = m.model_name || m.alias;
    const kind = m.service_type || m.kind;
    if (!alias) continue;
    const instances = Array.isArray(m.instances) ? m.instances : [];
    for (const inst of instances) {
      const nid = inst.node_id;
      if (!nid) continue;
      if (!byNode.has(nid)) byNode.set(nid, []);
      byNode.get(nid).push({
        alias,
        kind,
        backend: inst.backend || m.backend || '',
        size_mb: inst.size_mb || m.size_mb || 0,
        loaded: inst.status === 'running' || inst.status === 'ready',
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

function buildTargetOptionList(excludeAliasName) {
  // Modele ze wszystkich nodow — identyfikator to alias modelu (techniczna nazwa).
  const models = collectUniqueModels().map((m) => ({
    value: m.alias,
    label: m.alias + (m.backend ? ` · ${m.backend}` : '') + (m.kind ? ` (${m.kind})` : ''),
  }));
  // Inne aliasy — alias moze wskazywac na inny alias (chain routing), ale nie na siebie.
  const aliasTargets = (aliases || [])
    .filter((a) => a.alias && a.alias !== excludeAliasName)
    .map((a) => ({
      value: a.alias,
      label: `${a.alias} (alias)`,
    }));
  const seen = new Set();
  const out = [];
  for (const t of [...models, ...aliasTargets]) {
    if (seen.has(t.value)) continue;
    seen.add(t.value);
    out.push(t);
  }
  return out.sort((a, b) => a.value.localeCompare(b.value));
}

function openAliasModal(alias) {
  const isEdit = !!alias;
  const targets = buildTargetOptionList(alias?.alias);

  // Stan wybranych fallback targets (backend zwraca CSV string).
  let selectedFallbacks = [];
  if (isEdit && alias.fallback_targets) {
    selectedFallbacks = alias.fallback_targets
      .split(',').map((s) => s.trim()).filter(Boolean);
  }

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
    const fallback = selectedFallbacks.join(',');

    if (!name || !target) {
      if (errEl) {
        errEl.textContent = I18n.t('services.alias_required');
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
    // REST DELETE /api/services/:id stops the runtime and removes the row,
    // cascading to model_registry via FK ON DELETE CASCADE.
    await apiDelete(`/api/services/${encodeURIComponent(id)}`);
    toast(I18n.t('services.delete_success', { name }), 'success');
    const fresh = await apiGet('/api/services').catch(() => services);
    services = Array.isArray(fresh) ? fresh : [];
    patchListTab();
    updateSubtitle();
    updateTabCounts();
  } catch (err) {
    toast(I18n.t('services.delete_error', { error: err.message }), 'error');
  }
}

function typeChipClass(t) {
  switch ((t || '').toLowerCase()) {
    case 'llm': return 'chat';
    case 'embedding':
    case 'embeddings': return 'mesh-read';
    case 'stt':
    case 'tts': return 'deploy';
    case 'rag': return 'mesh-admin';
    case 'agent':
    case 'tool': return 'mesh-admin';
    default: return 'license';
  }
}

export default ServicesScreen;
