// =============================================================================
// Plik: modules/catalog.js
// Opis: Service Catalog — flow:
//       1) User wybiera deploy target (local / trusted mesh node / cluster)
//       2) Katalog renderuje sie z filtrowaniem pod target (OS, GPU vendor)
//       3) Klik kafelka otwiera EngineDeployWizard albo NIM deploy modal z
//          preselekcja node'a.
//       Header pokazuje aktualny target + przycisk "Zmien".
// =============================================================================

import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { Router } from '/js/router.js';
import * as Manifest from '/js/modules/catalog/manifest-store.js';
import { render as renderIcon, categoryIconKey } from '/js/modules/catalog/catalog-icons.js';
import { openDeployWizard } from '/js/modules/catalog/engine-deploy-wizard.js';
import { openNimDeployModal } from '/js/modules/catalog/nim-deploy.js';

// ---- State ----------------------------------------------------------------

let activeTab = 'tentaflow';
let nodes = [];            // wszystkie peers (trusted + local)
let clusters = [];         // clusters z /api/clusters
let target = null;         // { kind: 'node'|'cluster', id, label, os, gpuNames }

// NIM state
let nimContainers = [];
let nimFilteredContainers = [];
let nimActiveCategory = 'all';
let nimSearchQuery = '';

const NIM_CATEGORIES = ['all', 'llm', 'vlm', 'embedding', 'reranker', 'stt', 'tts'];

// ---- Screen ---------------------------------------------------------------

const CatalogScreen = {
  get title() { return I18n.t('catalog.title'); },
  render() {
    return `<div class="catalog-shell" id="catalog-root"></div>`;
  },
  async mount() {
    await Manifest.init();
    await loadTargets();
    renderRoot();
  },
  unmount() {
    target = null;
    activeTab = 'tentaflow';
  },
};

async function loadTargets() {
  // Backend gwarantuje ze /api/mesh/nodes zawsze zwroci przynajmniej local
  // (seed_local w peer_store przed startem). Retry exponential backoff na
  // wypadek jesli GUI otworzy sie przed pierwszym hitem backendu.
  let attempt = 0;
  while (attempt < 3) {
    try {
      const [nodesResp, clustersResp] = await Promise.all([
        ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' }),
        ApiBinary.list('clusterListRequest', { arrayKey: 'clusters' }).catch(() => []),
      ]);
      const list = (nodesResp || []).filter((n) => n?.is_trusted === true || n?.is_local === true);
      if (list.length > 0) {
        nodes = list;
        clusters = clustersResp || [];
        return;
      }
    } catch (err) {
      console.warn('[catalog] loadTargets attempt', attempt, 'failed:', err.message);
    }
    attempt += 1;
    await new Promise((r) => setTimeout(r, 100 * attempt));
  }
  throw new Error('Backend nie zwrócił local node — sprawdź log daemona');
}

// ---- Root rendering -------------------------------------------------------

function renderRoot() {
  const root = byId('catalog-root');
  if (!root) return;

  if (!target) {
    root.innerHTML = renderTargetPicker();
    bindTargetPicker();
    return;
  }

  root.innerHTML = `
    <div class="page-header" id="catalog-page-header">
      <div>
        <h1>${iconSvg('catalog')} ${escapeHtml(I18n.t('catalog.title'))}</h1>
        <div class="sub">${renderTargetBadge()}</div>
      </div>
      <div class="actions">
        <tf-button variant="secondary" icon="refresh" id="catalog-change-target">${escapeHtml(I18n.t('catalog.change_target'))}</tf-button>
      </div>
    </div>

    <tf-tabs variant="underline" id="catalog-tabs" value="${escapeAttr(activeTab)}">
      <tf-tab id="tentaflow" icon="catalog" count="0">${escapeHtml(I18n.t('catalog.tab_tentaflow'))}</tf-tab>
      <tf-tab id="nim" icon="zap">${escapeHtml(I18n.t('catalog.tab_nim'))}</tf-tab>
    </tf-tabs>

    <div id="catalog-content">
      <div class="catalog-loading">${escapeHtml(I18n.t('common.loading'))}</div>
    </div>
  `;

  byId('catalog-change-target')?.addEventListener('click', () => {
    target = null;
    renderRoot();
  });
  byId('catalog-tabs')?.addEventListener('change', handleTabChange);

  renderActiveTab();
  updateCount();
}

function renderTargetBadge() {
  if (!target) return '';
  const osTxt = target.os ? ` · ${escapeHtml(target.os)}` : '';
  const gpuTxt = target.gpuNames?.length > 0
    ? ` · ${escapeHtml(target.gpuNames.slice(0, 2).join(', '))}${target.gpuNames.length > 2 ? ` +${target.gpuNames.length - 2}` : ''}`
    : '';
  return `${escapeHtml(I18n.t('catalog.target_label'))}: <strong>${escapeHtml(target.label)}</strong>${osTxt}${gpuTxt}`;
}

function iconSvg(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

// ---- Target picker --------------------------------------------------------

function renderTargetPicker() {
  const localNode = nodes.find((n) => n.is_local);
  const remoteNodes = nodes.filter((n) => !n.is_local);

  return `
    <div class="page-header">
      <div>
        <h1>${iconSvg('catalog')} ${escapeHtml(I18n.t('catalog.title'))}</h1>
        <div class="sub">${escapeHtml(I18n.t('catalog.subtitle'))}</div>
      </div>
    </div>

    <h3 class="catalog-section-title">${escapeHtml(I18n.t('catalog.select_target'))}</h3>
    <p class="catalog-target-hint">${escapeHtml(I18n.t('catalog.select_target_hint'))}</p>

    ${localNode ? renderTargetSection(I18n.t('catalog.target_local'), [localNode], 'local') : ''}
    ${remoteNodes.length > 0 ? renderTargetSection(I18n.t('catalog.target_nodes'), remoteNodes, 'node') : ''}
    ${clusters.length > 0 ? renderClustersSection(clusters) : ''}
  `;
}

function renderTargetSection(title, list, kind) {
  return `
    <h4 class="target-section-title">${escapeHtml(title)} <span class="section-count">${list.length}</span></h4>
    <div class="target-grid">
      ${list.map((n) => renderTargetCard(n, kind)).join('')}
    </div>
  `;
}

function renderTargetCard(node, kind) {
  const nodeId = node.node_id || node.id;
  const hostname = node.hostname || nodeId?.slice(0, 12) || I18n.t('mesh.unknown_host');
  const os = (node.platform || node.os || '').toLowerCase();
  const gpus = Array.isArray(node.gpus) ? node.gpus : [];
  const gpuNames = gpus.map((g) => g.name || '').filter(Boolean);
  const hasNvidia = gpus.some((g) => /nvidia|geforce|rtx|gtx|tesla|a100|h100|h200|l40|dgx|grace|blackwell|hopper|gb10|gh200|b200|b100/i.test(g.name || ''));

  const online = isOnline(node);
  const cpuCount = node.cpu_count ? `${node.cpu_count} cores` : '';
  const ramTotal = node.ram_total_mb ? `${Math.round(node.ram_total_mb / 1024)} GB RAM` : '';

  const badges = [];
  if (os) badges.push(`<span class="platform-badge">${escapeHtml(os)}</span>`);
  if (hasNvidia) badges.push(`<span class="platform-badge nvidia">NVIDIA</span>`);
  if (kind === 'local') badges.push(`<span class="platform-badge local">${escapeHtml(I18n.t('mesh.local'))}</span>`);

  const gpuList = gpuNames.length > 0
    ? gpuNames.slice(0, 2).join(', ') + (gpuNames.length > 2 ? ` +${gpuNames.length - 2}` : '')
    : I18n.t('mesh.no_gpu');

  const statusChip = online
    ? `<span class="tag-status online">● ${escapeHtml(I18n.t('mesh.online'))}</span>`
    : `<span class="tag-status offline">● ${escapeHtml(I18n.t('mesh.offline'))}</span>`;

  return `
    <div class="target-card${online ? '' : ' offline'}${hasNvidia ? ' has-nvidia' : ''}" data-target-kind="${kind}" data-target-id="${escapeAttr(nodeId)}">
      <div class="target-card-head">
        <div class="target-card-ico">${renderIcon(kind === 'local' ? 'grid' : 'cpu', 24)}</div>
        <div class="target-card-title">
          <div class="name-t">${escapeHtml(hostname)} ${statusChip}</div>
          <div class="details">${[cpuCount, ramTotal].filter(Boolean).join(' · ')}</div>
        </div>
      </div>
      <div class="target-card-gpu">
        <strong>GPU:</strong> ${escapeHtml(gpuList)}
      </div>
      <div class="target-card-badges">${badges.join('')}</div>
    </div>
  `;
}

function renderClustersSection(list) {
  return `
    <h4 class="target-section-title">${escapeHtml(I18n.t('catalog.target_clusters'))} <span class="section-count">${list.length}</span></h4>
    <div class="target-grid">
      ${list.map((c) => {
        const id = c.id || c.cluster_id;
        const label = c.name || id;
        const nodeCount = c.node_count || c.nodes?.length || 0;
        return `
          <div class="target-card cluster-card" data-target-kind="cluster" data-target-id="${escapeAttr(id)}">
            <div class="target-card-head">
              <div class="target-card-ico">${renderIcon('grid', 24)}</div>
              <div class="target-card-title">
                <div class="name-t">${escapeHtml(label)}</div>
                <div class="details">${nodeCount} ${escapeHtml(I18n.t(nodeCount === 1 ? 'mesh.count_node' : 'mesh.count_nodes'))}</div>
              </div>
            </div>
          </div>
        `;
      }).join('')}
    </div>
  `;
}

function bindTargetPicker() {
  const root = byId('catalog-root');
  if (!root) return;
  root.onclick = (e) => {
    const card = e.target.closest('.target-card[data-target-id]');
    if (!card) return;
    const kind = card.dataset.targetKind;
    const id = card.dataset.targetId;
    if (kind === 'cluster') {
      const c = clusters.find((x) => (x.id || x.cluster_id) === id);
      target = {
        kind: 'cluster',
        id,
        label: c?.name || id,
        os: null,
        gpuNames: [],
      };
    } else {
      const n = nodes.find((x) => (x.node_id || x.id) === id);
      const gpus = Array.isArray(n?.gpus) ? n.gpus : [];
      target = {
        kind: n?.is_local ? 'local' : 'node',
        id,
        label: n?.hostname || id,
        os: String(n?.platform || n?.os || '').toLowerCase(),
        gpuNames: gpus.map((g) => g.name || '').filter(Boolean),
        hasNvidia: gpus.some((g) => /nvidia|geforce|rtx|gtx|tesla|a100|h100|h200|l40|dgx|grace|blackwell|hopper|gb10|gh200|b200|b100/i.test(g.name || '')),
        nodeRef: n,
      };
    }
    renderRoot();
  };
}

function isOnline(node) {
  if (node.is_local) return true;
  const s = String(node.status || '').toLowerCase();
  return s === 'connected' || s === 'online' || s === 'active' || s === 'ready';
}

// ---- Tabs -----------------------------------------------------------------

function handleTabChange(e) {
  const id = e.detail?.value;
  if (!id || id === activeTab) return;
  activeTab = id;
  renderActiveTab();
}

function renderActiveTab() {
  const host = byId('catalog-content');
  if (!host) return;
  if (activeTab === 'nim') {
    host.innerHTML = `<div id="nim-catalog-container"><div class="catalog-loading">${escapeHtml(I18n.t('nim.loading_catalog'))}</div></div>`;
    loadNimCatalog();
  } else {
    host.innerHTML = renderTentaflowTab();
    bindCards(host);
  }
}

function updateCount() {
  if (!target) return;
  const targetOs = target.os || 'linux';
  const total = Manifest.all().filter((s) => Manifest.isEngineCompatible(s, targetOs)).length;
  const tab = document.querySelector('#catalog-tabs tf-tab#tentaflow');
  if (tab) tab.setAttribute('count', String(total));
}

// ---- TentaFlow tab --------------------------------------------------------

function renderTentaflowTab() {
  const targetOs = target?.os || 'linux';
  const categories = Manifest.nonEmptyCategories();

  let html = '';
  let rendered = 0;
  for (const cat of categories) {
    const engines = Manifest.byCategory(cat).filter((e) => Manifest.isEngineCompatible(e, targetOs));
    if (engines.length === 0) continue;
    rendered += engines.length;

    const categoryLabel = I18n.t(`category.${cat}`) !== `category.${cat}`
      ? I18n.t(`category.${cat}`)
      : cat.toUpperCase();

    html += `
      <h3 class="catalog-section-title">${escapeHtml(categoryLabel)} <span class="section-count">${engines.length}</span></h3>
      <div class="catalog-grid">
        ${engines.map((e) => renderEngineCard(e, targetOs)).join('')}
      </div>
    `;
  }

  if (rendered === 0) {
    return `
      <div class="empty-state">
        <div class="empty-state-text">${escapeHtml(I18n.t('catalog.noCompatible'))}</div>
        <div class="empty-state-hint">${escapeHtml(I18n.t('catalog.noCompatible_hint')).replace('{os}', escapeHtml(targetOs))}</div>
      </div>
    `;
  }

  return html;
}

function renderEngineCard(service, targetOs) {
  const e = service?.engine || {};
  const iconKey = e.icon || categoryIconKey[e.category] || 'cpu';
  const iconHtml = renderIcon(iconKey, 28);
  const desc = I18n.getLanguage() === 'pl' ? (e.description_pl || e.description_en) : (e.description_en || e.description_pl);
  const deployMethods = Manifest.availableDeployMethods(service, targetOs);
  const methodsLabel = deployMethods.map((m) => escapeHtml(I18n.t(`catalog.method_${m}`))).join(' · ') || '—';

  return `
    <div class="catalog-card" data-engine-id="${escapeAttr(e.id || '')}">
      <div class="catalog-card-head">
        <div class="catalog-card-ico">${iconHtml}</div>
        <div class="catalog-card-title">
          <div class="name-t">${escapeHtml(e.name || e.id || '—')}</div>
          <div class="details">${escapeHtml(desc || '')}</div>
        </div>
      </div>
      <div class="catalog-card-meta">
        <div class="meta-row methods">${escapeHtml(I18n.t('catalog.deploy_as'))}: ${methodsLabel}</div>
      </div>
      <div class="catalog-card-foot">
        <tf-button variant="primary" size="sm" icon="plus" data-engine-deploy="${escapeAttr(e.id || '')}">
          ${escapeHtml(I18n.t('catalog.deploy'))}
        </tf-button>
      </div>
    </div>
  `;
}

function bindCards(host) {
  host.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-engine-deploy]');
    const card = e.target.closest('[data-engine-id]');
    const engineId = btn?.dataset.engineDeploy || card?.dataset.engineId;
    if (!engineId || !target) return;
    if (target.kind === 'cluster') {
      toast(I18n.t('catalog.cluster_not_supported'), 'warning');
      return;
    }
    openDeployWizard(engineId, { nodeId: target.id, hostOs: target.os });
  });
}

// ---- NIM tab --------------------------------------------------------------

async function loadNimCatalog() {
  const container = byId('nim-catalog-container');
  if (!container || !target) return;

  if (target.kind === 'cluster') {
    container.innerHTML = `
      <div class="empty-state">
        <div class="empty-state-text">${escapeHtml(I18n.t('catalog.cluster_nim_not_supported'))}</div>
      </div>
    `;
    return;
  }

  if (!target.hasNvidia) {
    const gpuList = target.gpuNames?.length > 0
      ? target.gpuNames.map((n) => escapeHtml(n)).join(', ')
      : escapeHtml(I18n.t('nim.no_gpu_detected'));
    container.innerHTML = `
      <div class="empty-state nim-empty">
        <div class="nim-empty-ico">${renderIcon('cpu', 48)}</div>
        <div class="empty-state-text">${escapeHtml(I18n.t('nim.not_supported'))}</div>
        <div class="empty-state-hint">${escapeHtml(I18n.t('nim.not_supported_hint'))}</div>
        <div class="empty-state-hint mono">${gpuList}</div>
      </div>
    `;
    return;
  }

  try {
    const data = await ApiBinary.one('nimCatalogListRequest');

    if (data.error === 'ngc_api_key_not_configured') {
      container.innerHTML = renderNimNoApiKey();
      bindNimSettingsLink();
      return;
    }
    if (data.error === 'ngc_auth_failed' || data.error === 'ngc_fetch_failed') {
      container.innerHTML = renderNimAuthFailed();
      bindNimSettingsLink();
      return;
    }
    if (data.error) {
      container.innerHTML = `
        <div class="empty-state nim-empty">
          <div class="nim-empty-ico">${renderIcon('cpu', 48)}</div>
          <div class="empty-state-text">${escapeHtml(data.error)}</div>
        </div>
      `;
      return;
    }

    nimContainers = (data.containers || []).map((c) => ({
      name: c.name,
      display_name: c.displayName,
      description: c.description,
      image: c.image,
      latest_tag: c.latestTag,
      publisher: c.publisher,
      category: c.category,
      min_gpu_memory_gb: c.minGpuMemoryGb,
      updated_at: c.updatedAt,
      self_hostable: c.selfHostable,
    }));
    nimActiveCategory = 'all';
    nimSearchQuery = '';
    applyNimFilters();
    container.innerHTML = renderNimContent();
    bindNimEvents();
  } catch (err) {
    container.innerHTML = `
      <div class="empty-state nim-empty">
        <div class="nim-empty-ico">${renderIcon('cpu', 48)}</div>
        <div class="empty-state-text">${escapeHtml(I18n.t('common.error'))}</div>
        <div class="empty-state-hint">${escapeHtml(err.message || '')}</div>
      </div>
    `;
  }
}

function renderNimNoApiKey() {
  return `
    <div class="empty-state nim-empty">
      <div class="nim-empty-ico">${renderIcon('cpu', 48)}</div>
      <div class="empty-state-text">${escapeHtml(I18n.t('nim.no_api_key'))}</div>
      <div class="empty-state-hint">${escapeHtml(I18n.t('nim.no_api_key_hint'))}</div>
      <tf-button variant="primary" class="nim-go-settings" style="margin-top:14px;">${escapeHtml(I18n.t('nim.go_to_settings'))}</tf-button>
    </div>
  `;
}

function renderNimAuthFailed() {
  return `
    <div class="empty-state nim-empty">
      <div class="nim-empty-ico warning">⚠</div>
      <div class="empty-state-text">${escapeHtml(I18n.t('nim.auth_failed'))}</div>
      <div class="empty-state-hint">${escapeHtml(I18n.t('nim.auth_failed_hint'))}</div>
      <tf-button variant="primary" class="nim-go-settings" style="margin-top:14px;">${escapeHtml(I18n.t('nim.go_to_settings'))}</tf-button>
    </div>
  `;
}

function bindNimSettingsLink() {
  document.querySelectorAll('.nim-go-settings').forEach((btn) => {
    btn.addEventListener('click', () => Router.navigate('settings'));
  });
}

function applyNimFilters() {
  nimFilteredContainers = nimContainers.filter((c) => {
    if (nimActiveCategory !== 'all' && c.category !== nimActiveCategory) return false;
    if (nimSearchQuery) {
      const q = nimSearchQuery.toLowerCase();
      const name = (c.display_name || c.name || '').toLowerCase();
      const desc = (c.description || '').toLowerCase();
      const pub = (c.publisher || '').toLowerCase();
      if (!name.includes(q) && !desc.includes(q) && !pub.includes(q)) return false;
    }
    return true;
  });
}

function nimCategoryLabel(cat) {
  if (cat === 'all') return I18n.t('nim.category_all');
  return cat.toUpperCase();
}

function publisherColor(publisher) {
  const p = (publisher || '').toLowerCase();
  if (p === 'nvidia') return '#22c55e';
  if (p === 'meta') return '#1877F2';
  if (p === 'mistralai') return '#FF7000';
  return '#94a3b8';
}

function renderNimContent() {
  const toolbar = `
    <div class="nim-toolbar">
      <tf-searchbox class="nim-search" placeholder="${escapeAttr(I18n.t('nim.search'))}" value="${escapeAttr(nimSearchQuery)}" debounce="200"></tf-searchbox>
      <tf-tabs class="nim-filters" variant="soft" value="${escapeAttr(nimActiveCategory)}">
        ${NIM_CATEGORIES.map((cat) => `
          <tf-tab id="${escapeAttr(cat)}">${escapeHtml(nimCategoryLabel(cat))}</tf-tab>
        `).join('')}
      </tf-tabs>
    </div>
  `;

  if (nimFilteredContainers.length === 0) {
    return toolbar + `<div class="empty-state" style="margin-top:20px;"><div class="empty-state-text">${escapeHtml(I18n.t('nim.no_results'))}</div></div>`;
  }

  const cards = nimFilteredContainers.map((c) => {
    const color = publisherColor(c.publisher);
    const vram = c.min_gpu_memory_gb ? `${c.min_gpu_memory_gb} GB ${I18n.t('nim.vram')}` : '';
    return `
      <div class="catalog-card nim-card" data-nim-image="${escapeAttr(c.image)}">
        <div class="catalog-card-head">
          <div class="catalog-card-ico">${renderIcon('cpu', 28)}</div>
          <div class="catalog-card-title">
            <div class="name-t">${escapeHtml(c.display_name || c.name)}</div>
            <span class="nim-publisher" style="background:${color}26;color:${color};border-color:${color}66;">${escapeHtml(c.publisher || '')}</span>
          </div>
        </div>
        <div class="catalog-card-title">
          <div class="details">${escapeHtml(c.description || '')}</div>
        </div>
        <div class="catalog-card-meta">
          <div class="meta-row">
            <span class="platform-badge">${escapeHtml((c.category || '').toUpperCase())}</span>
            ${vram ? `<span class="platform-badge">${escapeHtml(vram)}</span>` : ''}
            ${c.latest_tag ? `<span class="platform-badge">v${escapeHtml(c.latest_tag)}</span>` : ''}
          </div>
        </div>
        <div class="catalog-card-foot">
          <tf-button variant="primary" size="sm" class="nim-deploy-btn">${escapeHtml(I18n.t('nim.deploy'))}</tf-button>
        </div>
      </div>
    `;
  }).join('');

  return toolbar + `<div class="catalog-grid">${cards}</div>`;
}

function bindNimEvents() {
  const search = document.querySelector('tf-searchbox.nim-search');
  if (search) {
    search.addEventListener('search', (e) => {
      nimSearchQuery = String(e.detail?.value || '').trim();
      applyNimFilters();
      refreshNimGrid();
    });
  }
  const filters = document.querySelector('tf-tabs.nim-filters');
  if (filters) {
    filters.addEventListener('change', (e) => {
      nimActiveCategory = e.detail?.value || 'all';
      applyNimFilters();
      refreshNimGrid();
    });
  }
  document.querySelectorAll('.nim-card[data-nim-image]').forEach((card) => {
    const open = (e) => {
      e.stopPropagation();
      const img = card.dataset.nimImage;
      const c = nimContainers.find((x) => x.image === img);
      if (c && target) openNimDeployModal(c, target.id);
    };
    card.querySelector('.nim-deploy-btn')?.addEventListener('click', open);
    card.addEventListener('click', open);
  });
}

function refreshNimGrid() {
  const container = byId('nim-catalog-container');
  if (!container) return;
  container.innerHTML = renderNimContent();
  bindNimEvents();
}

export default CatalogScreen;
