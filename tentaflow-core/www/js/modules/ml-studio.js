// ===== File: ml-studio.js — ML Studio dashboard module (project list, wizard, detail) =====
// Talks to Core over the binary protocol via MessageBody::MlStudioBody:
//   ProjectsListRequest / ProjectTypesListRequest / ProjectCreateRequest /
//   ProjectDetailRequest. Renders the project list (p00) and create wizard (p01)
//   with tf-* components only; project detail shows real backend data plus
//   type-aware tab placeholders for the later slices.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { Router } from '/js/router.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-badge.js';
import '/js/components/tf-input.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-modal.js';
import '/js/components/tf-radio.js';
import '/js/components/tf-select.js';
import '/js/components/tf-table.js';
import '/js/components/tf-file-input.js';
import '/js/components/tf-filter-chips.js';
import '/js/components/tf-stat-card.js';
import '/js/components/tf-detail-header.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-spinner.js';

let projects = [];
let projectTypes = [];
let activeTypeFilter = 'all';
let detailProjectId = null;
// Mesh node pool cached for the admin resources screen, so grant rows can resolve
// nodeId → hostname without a second MeshNodeList round-trip.
let resourceNodes = [];
// Cached current user (from AuthMeRequest) so the sharing screen can mark the
// "(Ty)" row and resolve self-identity for owner gating across mount/unmount.
let currentUser = null;

// Human label + sprite per project role. Owner is rendered as an accent badge,
// the rest as info badges / chips (matches p02 legend).
const ROLE_LABEL = {
  owner: 'Właściciel',
  editor: 'Edytor',
  viewer: 'Przeglądający',
};
const ROLE_ICON = {
  owner: 'crown',
  editor: 'edit',
  viewer: 'eye',
};

function roleLabel(role) {
  return ROLE_LABEL[String(role || '').toLowerCase()] || role || '—';
}

// Per-type icon (sprite id) and the placeholder tab map for the project detail.
// Slugs are the backend contract (ml_studio::models::ProjectType::slug).
const TYPE_ICON = {
  recognition: 'image',
  ft_llm: 'sparkle',
  ft_vision_audio: 'mic',
  tabular_anomaly: 'grid-rows',
  rag: 'rag-db',
  distillation: 'transform',
};

const TYPE_TABS = {
  recognition: ['Schemat', 'Dane', 'Anotacje', 'Treningi', 'Modele'],
  ft_llm: ['Model bazowy', 'Dane', 'Trening', 'Ewaluacja', 'Modele'],
  ft_vision_audio: ['Model bazowy', 'Dane', 'Trening', 'Ewaluacja', 'Modele'],
  tabular_anomaly: ['Dane', 'Cechy', 'AutoML', 'Anomalie', 'Modele'],
  rag: ['Korpus', 'Indeks', 'Reranker', 'Ewaluacja', 'Zapytania'],
  distillation: ['Nauczyciel', 'Uczeń', 'Dane', 'Trening', 'Modele'],
};

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

function typeIcon(slug) {
  return TYPE_ICON[slug] || 'brain';
}

function typeLabel(slug) {
  const t = projectTypes.find((pt) => pt.slug === slug);
  return t ? t.label : slug;
}

// Maps a free-form backend status string onto a tf-badge tone.
// tf-badge only accepts accent/danger/success/warning/info (tf-badge.js:7).
function statusTone(status) {
  const s = String(status || '').toLowerCase();
  if (s === 'active' || s === 'ready' || s === 'done' || s === 'gotowy') return 'success';
  if (s === 'training' || s === 'running' || s === 'trening') return 'warning';
  if (s === 'error' || s === 'failed' || s === 'blad') return 'danger';
  if (s === 'draft' || s === 'szkic') return 'info';
  return 'accent';
}

const MlStudioScreen = {
  get title() { return 'ML Studio'; },

  render(params = {}) {
    if (params && params.admin === 'resources') {
      return `<div id="ml-studio-resources" class="ml-studio-resources"></div>`;
    }
    if (params && params.projectId && params.share) {
      return `<div id="ml-studio-share" class="ml-studio-share"></div>`;
    }
    if (params && params.projectId) {
      return `<div id="ml-studio-detail" class="ml-studio-detail"></div>`;
    }
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('brain')} ML Studio</h1>
          <div class="sub" id="ml-studio-sub">Projekty — jednostki pracy ML (dane, schemat, treningi, modele)</div>
        </div>
        <div class="actions" id="ml-studio-actions">
          <tf-button variant="ghost" icon="refresh" id="ml-studio-refresh">Odśwież</tf-button>
          <tf-button variant="primary" icon="plus" id="ml-studio-new">Nowy projekt</tf-button>
        </div>
      </div>

      <tf-filter-chips id="ml-studio-filters" mode="single"></tf-filter-chips>

      <div id="ml-studio-list" class="ml-studio-grid"></div>
    `;
  },

  async mount(params = {}) {
    if (params && params.admin === 'resources') {
      await showResourcesAdmin();
      return;
    }
    if (params && params.projectId && params.share) {
      await showShare(params.projectId);
      return;
    }
    if (params && params.projectId) {
      await showDetail(params.projectId);
      return;
    }
    byId('ml-studio-refresh')?.addEventListener('click', loadAll);
    byId('ml-studio-new')?.addEventListener('click', openCreateModal);

    const filters = byId('ml-studio-filters');
    filters?.addEventListener('change', (e) => {
      activeTypeFilter = e.detail.id;
      renderList();
    });

    await loadAll();
  },

  unmount() {
    // projectTypes is the backend type catalogue (slug → label) and is cached
    // across mount/unmount so the detail view keeps real type labels instead of
    // falling back to the raw slug when entered directly via the router.
    projects = [];
    activeTypeFilter = 'all';
    detailProjectId = null;
    resourceNodes = [];
  },
};

async function ensureProjectTypes() {
  if (projectTypes.length) return;
  try {
    const resp = await ApiBinary.one('mlStudioProjectTypesListRequest');
    projectTypes = Array.isArray(resp.types) ? resp.types : [];
  } catch (_) {
    projectTypes = [];
  }
}

async function loadAll() {
  const list = byId('ml-studio-list');
  if (list) list.innerHTML = '<div class="ml-studio-loading"><tf-spinner></tf-spinner></div>';
  try {
    const [typesResp, projectsResp] = await Promise.all([
      ApiBinary.one('mlStudioProjectTypesListRequest'),
      ApiBinary.one('mlStudioProjectsListRequest'),
      ensureCurrentUser(),
    ]);
    projectTypes = Array.isArray(typesResp.types) ? typesResp.types : [];
    projects = Array.isArray(projectsResp.projects) ? projectsResp.projects : [];
    renderAdminEntry();
    renderFilters();
    renderList();
    const sub = byId('ml-studio-sub');
    if (sub) {
      const owned = projects.filter(isOwnerProject).length;
      const shared = projects.length - owned;
      sub.textContent = `Moje (${owned}) · Udostępnione (${shared})`;
    }
  } catch (err) {
    projects = [];
    if (list) {
      list.innerHTML = '';
      const empty = document.createElement('tf-empty-state');
      empty.setAttribute('icon', 'alert');
      empty.setAttribute('title', 'Nie udało się wczytać projektów');
      empty.setAttribute('message', err.message || 'Błąd protokołu ML Studio.');
      const retry = document.createElement('tf-button');
      retry.setAttribute('variant', 'primary');
      retry.textContent = 'Spróbuj ponownie';
      retry.addEventListener('click', loadAll);
      empty.appendChild(retry);
      list.appendChild(empty);
    }
    toast(`ML Studio: ${err.message}`, 'error');
  }
}

function renderFilters() {
  const el = byId('ml-studio-filters');
  if (!el) return;
  const all = { id: 'all', label: 'Wszystkie', icon: 'list', count: projects.length, active: activeTypeFilter === 'all' };
  const typeFilters = projectTypes.map((t) => ({
    id: t.slug,
    label: t.label,
    icon: typeIcon(t.slug),
    count: projects.filter((p) => p.projectType === t.slug || p.project_type === t.slug).length,
    active: activeTypeFilter === t.slug,
  }));
  el.filters = [all, ...typeFilters];
}

function projectType(p) {
  return p.projectType ?? p.project_type ?? '';
}

function projectId(p) {
  return p.projectId ?? p.project_id ?? '';
}

// Ownership flag straight from the backend (ProjectsListResponse / detail carry
// `isOwner`). Falls back to comparing the project `role` to "owner" when the
// flag is absent so the split still works against older payloads.
function isOwnerProject(p) {
  if (typeof p.isOwner === 'boolean') return p.isOwner;
  if (typeof p.is_owner === 'boolean') return p.is_owner;
  return String(p.role || '').toLowerCase() === 'owner';
}

function projectRole(p) {
  return String(p.role ?? '').toLowerCase();
}

async function ensureCurrentUser() {
  if (currentUser) return currentUser;
  try {
    currentUser = await ApiBinary.one('authMeRequest');
  } catch (_) {
    currentUser = null;
  }
  return currentUser;
}

function currentUserId() {
  return currentUser ? String(currentUser.userId ?? currentUser.user_id ?? '') : '';
}

// Admin gating mirrors app.js: role comes from AuthMeRequest, admin === 'admin'.
// The resource allocation screen (§11.3) is admin-only; project members only
// read their own project's grants via mlStudioProjectResourcesRequest.
function isCurrentUserAdmin() {
  return String(currentUser?.role ?? '').toLowerCase() === 'admin';
}

// Adds the "Administracja › Zasoby" entry to the project list header — only for
// admins. Inserted once `currentUser` is known (after loadAll resolves AuthMe).
function renderAdminEntry() {
  const actions = byId('ml-studio-actions');
  if (!actions || !isCurrentUserAdmin()) return;
  if (byId('ml-studio-admin-resources')) return;
  const btn = document.createElement('tf-button');
  btn.id = 'ml-studio-admin-resources';
  btn.setAttribute('variant', 'outline');
  btn.setAttribute('icon', 'host');
  btn.textContent = 'Administracja › Zasoby';
  btn.addEventListener('click', () => Router.navigate('ml-studio', { admin: 'resources' }));
  actions.insertBefore(btn, actions.firstChild);
}

function renderList() {
  const host = byId('ml-studio-list');
  if (!host) return;

  const visible = activeTypeFilter === 'all'
    ? projects
    : projects.filter((p) => projectType(p) === activeTypeFilter);

  if (!visible.length) {
    host.innerHTML = '';
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', 'brain');
    empty.setAttribute('title', projects.length ? 'Brak projektów dla tego typu' : 'Brak projektów');
    empty.setAttribute('message', 'Utwórz pierwszy projekt kreatorem — typ projektu określa dalsze kroki.');
    const btn = document.createElement('tf-button');
    btn.setAttribute('variant', 'primary');
    btn.setAttribute('icon', 'plus');
    btn.textContent = 'Nowy projekt';
    btn.addEventListener('click', openCreateModal);
    empty.appendChild(btn);
    host.appendChild(empty);
    return;
  }

  const mine = visible.filter(isOwnerProject);
  const shared = visible.filter((p) => !isOwnerProject(p));

  // "Moje projekty" zawsze pokazuje kartę „Nowy projekt"; sekcja „Udostępnione
  // mi" pojawia się tylko gdy są jakieś projekty z rolą gościa.
  let html = sectionHead('crown', 'Moje projekty', mine.length, 'accent',
    'jesteś właścicielem — pełen dostęp i zarządzanie udostępnianiem');
  html += `<div class="ml-studio-grid">${mine.map((p) => projectCard(p, true)).join('')}${newProjectCard()}</div>`;

  if (shared.length) {
    html += sectionHead('share', 'Udostępnione mi', shared.length, 'info',
      'projekty, do których zaprosił Cię właściciel — działasz wg nadanej roli');
    html += `<div class="ml-studio-grid">${shared.map((p) => projectCard(p, false)).join('')}</div>`;
  }

  host.innerHTML = html;

  host.querySelectorAll('[data-share-id]').forEach((el) => {
    el.addEventListener('click', (e) => {
      e.stopPropagation();
      Router.navigate('ml-studio', { projectId: el.dataset.shareId, share: true });
    });
  });
  host.querySelectorAll('[data-project-id]').forEach((el) => {
    el.addEventListener('click', () => {
      Router.navigate('ml-studio', { projectId: el.dataset.projectId });
    });
  });
  host.querySelector('[data-new-project]')?.addEventListener('click', openCreateModal);
}

function sectionHead(icon, title, count, tone, sub) {
  return `
    <div class="ml-studio-section-head">
      <h3>${sprite(icon)} ${escapeHtml(title)}</h3>
      <tf-badge tone="${escapeAttr(tone)}" value="${count}"></tf-badge>
      <span class="ml-studio-section-sub">${escapeHtml(sub)}</span>
    </div>
  `;
}

function projectCard(p, owned) {
  const id = projectId(p);
  const slug = projectType(p);
  const datasetCount = p.datasetCount ?? p.dataset_count ?? 0;
  const modelCount = p.modelCount ?? p.model_count ?? 0;
  const created = formatDate(p.createdAt ?? p.created_at);
  const updated = formatDate(p.updatedAt ?? p.updated_at);

  // Owner strip: "Właściciel: Ty" with a share action for my projects; the
  // guest role chip (Edytor/Przeglądający) for shared-with-me projects.
  let ownerStrip;
  let shareBtn = '';
  if (owned) {
    ownerStrip = `<div class="ml-studio-card-owner">${sprite('crown')} Właściciel: Ty</div>`;
    shareBtn = `<tf-button variant="ghost" size="sm" icon="share" class="ml-studio-card-share" data-share-id="${escapeAttr(id)}" title="Udostępnij projekt" aria-label="Udostępnij projekt"></tf-button>`;
  } else {
    const role = projectRole(p);
    ownerStrip = `<div class="ml-studio-card-owner">${sprite('crown')} Właściciel: inny użytkownik
      <tf-chip status="info" icon="${escapeAttr(ROLE_ICON[role] || 'eye')}" label="${escapeAttr(roleLabel(role))}"></tf-chip></div>`;
  }

  return `
    <article class="ml-studio-card" data-project-id="${escapeAttr(id)}">
      ${shareBtn}
      <div class="ml-studio-card-top">
        <div class="ml-studio-card-ico">${sprite(typeIcon(slug))}</div>
        <div class="ml-studio-card-id">
          <div class="ml-studio-card-name">${escapeHtml(p.name || '(bez nazwy)')}</div>
          <div class="ml-studio-card-type">${escapeHtml(typeLabel(slug))}</div>
        </div>
        <tf-badge tone="${statusTone(p.status)}" value="${escapeAttr(p.status || '—')}"></tf-badge>
      </div>
      ${ownerStrip}
      <p class="ml-studio-card-desc">${escapeHtml(p.description || 'Bez opisu.')}</p>
      <div class="ml-studio-card-stats">
        <div class="ml-studio-stat"><div class="v">${datasetCount}</div><div class="l">datasety</div></div>
        <div class="ml-studio-stat"><div class="v">${modelCount}</div><div class="l">modele</div></div>
      </div>
      <div class="ml-studio-card-foot">
        <span class="ml-studio-card-meta">${sprite('clock')} utworzony ${escapeHtml(created)}</span>
        <span class="ml-studio-card-meta">edytowany ${escapeHtml(updated)}</span>
      </div>
    </article>
  `;
}

function newProjectCard() {
  return `
    <tf-button variant="outline" full-width class="ml-studio-new" data-new-project>
      <span class="ml-studio-new-ico">${sprite('plus')}</span>
      <span class="ml-studio-new-body">
        <span class="ml-studio-new-title">Nowy projekt</span>
        <span class="ml-studio-new-sub">Kreator: nazwa → opis → typ projektu</span>
      </span>
    </tf-button>
  `;
}

function openCreateModal() {
  if (!projectTypes.length) {
    toast('Lista typów projektów niedostępna — odśwież.', 'error');
    return;
  }
  const existing = byId('ml-studio-create-modal');
  if (existing) existing.remove();

  const modal = document.createElement('tf-modal');
  modal.id = 'ml-studio-create-modal';
  modal.setAttribute('title', 'Nowy projekt');
  modal.setAttribute('subtitle', 'Typ projektu określa dalsze kroki i całe „wnętrze” projektu.');
  modal.setAttribute('size', 'lg');

  const body = document.createElement('div');
  body.setAttribute('slot', 'body');
  body.innerHTML = `
    <div class="ml-studio-form">
      <tf-input id="ml-studio-name" label="Nazwa projektu" placeholder="np. Rozpoznawanie znaków ADR" required></tf-input>
      <tf-textarea id="ml-studio-desc" label="Opis" rows="3" placeholder="Krótko: cel projektu i dane wejściowe."></tf-textarea>
      <div class="ml-studio-type-field">
        <tf-radio-group
          id="ml-studio-types"
          name="ml-studio-type"
          label="Typ projektu"
          cards
          value="${escapeAttr(projectTypes[0].slug)}">
          ${projectTypes.map((t) => `
            <tf-radio card value="${escapeAttr(t.slug)}">
              <span class="tf-radio-card-group__icon">${sprite(typeIcon(t.slug))}</span>
              <span class="tf-radio-card-group__body">
                <span class="tf-radio-card-group__title">${escapeHtml(t.label)}</span>
                <span class="tf-radio-card-group__description">${escapeHtml(t.description)}</span>
              </span>
            </tf-radio>
          `).join('')}
        </tf-radio-group>
      </div>
    </div>
  `;

  const footer = document.createElement('div');
  footer.setAttribute('slot', 'footer');
  const cancel = document.createElement('tf-button');
  cancel.setAttribute('variant', 'ghost');
  cancel.textContent = 'Anuluj';
  const submit = document.createElement('tf-button');
  submit.setAttribute('variant', 'primary');
  submit.setAttribute('icon', 'check');
  submit.textContent = 'Utwórz projekt';
  footer.append(cancel, submit);

  modal.append(body, footer);
  document.body.appendChild(modal);
  modal.open = true;

  let selectedType = projectTypes[0].slug;

  // tf-radio-group emits `change` with detail.value (tf-radio.js:162-165).
  const typeGroup = body.querySelector('#ml-studio-types');
  typeGroup?.addEventListener('change', (e) => {
    selectedType = e.detail?.value || selectedType;
  });

  const close = () => { modal.open = false; modal.remove(); };
  cancel.addEventListener('click', close);
  modal.addEventListener('close', () => modal.remove());

  submit.addEventListener('click', async () => {
    const name = byId('ml-studio-name')?.value?.trim() || '';
    const description = byId('ml-studio-desc')?.value?.trim() || '';
    if (!name) {
      toast('Podaj nazwę projektu.', 'error');
      return;
    }
    submit.setAttribute('loading', '');
    try {
      const resp = await ApiBinary.one('mlStudioProjectCreateRequest', {
        name,
        description,
        projectType: selectedType,
      });
      const created = resp.project || {};
      toast('Projekt utworzony', 'success');
      close();
      await loadAll();
      const newId = created.projectId ?? created.project_id;
      if (newId) Router.navigate('ml-studio', { projectId: newId });
    } catch (err) {
      submit.removeAttribute('loading');
      toast(`Tworzenie projektu: ${err.message}`, 'error');
    }
  });
}

async function showDetail(projectId) {
  detailProjectId = projectId;
  const host = byId('ml-studio-detail');
  if (!host) return;
  host.innerHTML = '<div class="ml-studio-loading"><tf-spinner></tf-spinner></div>';

  try {
    const [resp] = await Promise.all([
      ApiBinary.one('mlStudioProjectDetailRequest', { projectId }),
      ensureProjectTypes(),
      ensureCurrentUser(),
    ]);
    const p = resp.project || {};
    renderDetail(host, p);
  } catch (err) {
    host.innerHTML = '';
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', 'alert');
    empty.setAttribute('title', 'Nie udało się wczytać projektu');
    empty.setAttribute('message', err.message || 'Błąd protokołu ML Studio.');
    const back = document.createElement('tf-button');
    back.setAttribute('variant', 'primary');
    back.textContent = 'Wróć do listy';
    back.addEventListener('click', () => Router.navigate('ml-studio'));
    empty.appendChild(back);
    host.appendChild(empty);
    toast(`ML Studio: ${err.message}`, 'error');
  }
}

function renderDetail(host, p) {
  const slug = p.projectType ?? p.project_type ?? '';
  const modelCount = p.modelCount ?? p.model_count ?? 0;
  const created = formatDate(p.createdAt ?? p.created_at);
  const updated = formatDate(p.updatedAt ?? p.updated_at);
  // Every project type gets a "Zasoby" tab (§11.3) appended after its type-aware
  // tabs — it shows the mesh resources allocated to this project.
  const tabs = [...(TYPE_TABS[slug] || ['Przegląd', 'Dane', 'Treningi', 'Modele']), 'Zasoby'];

  host.innerHTML = `
    <div class="ml-studio-detail-top">
      <tf-button variant="ghost" icon="chevron-left" id="ml-studio-back">Projekty</tf-button>
    </div>

    <tf-detail-header
      title="${escapeAttr(p.name || '(bez nazwy)')}"
      subtitle="${escapeAttr(typeLabel(slug))}"
      icon="${escapeAttr(typeIcon(slug))}">
      <span slot="badges"><tf-badge tone="${statusTone(p.status)}" value="${escapeAttr(p.status || '—')}"></tf-badge></span>
      ${isOwnerProject(p) ? `<span slot="actions"><tf-button variant="outline" icon="share" id="ml-studio-manage-access">Zarządzaj dostępem</tf-button></span>` : ''}
    </tf-detail-header>

    <p class="ml-studio-detail-desc">${escapeHtml(p.description || 'Bez opisu.')}</p>

    <div class="ml-studio-detail-stats">
      <tf-stat-card label="Modele" value="${modelCount}" icon="brain"></tf-stat-card>
      <tf-stat-card label="Status" value="${escapeAttr(p.status || '—')}" icon="check"></tf-stat-card>
      <tf-stat-card label="Utworzony" value="${escapeAttr(created)}" icon="clock"></tf-stat-card>
      <tf-stat-card label="Edytowany" value="${escapeAttr(updated)}" icon="clock"></tf-stat-card>
    </div>

    <tf-tabs id="ml-studio-tabs" value="ml-tab-0">
      ${tabs.map((t, i) => `<tf-tab id="ml-tab-${i}" label="${escapeAttr(t)}"></tf-tab>`).join('')}
    </tf-tabs>

    <div id="ml-studio-tab-panel" class="ml-studio-tab-panel"></div>
  `;

  byId('ml-studio-back')?.addEventListener('click', () => Router.navigate('ml-studio'));
  byId('ml-studio-manage-access')?.addEventListener('click', () => {
    Router.navigate('ml-studio', { projectId: projectId(p), share: true });
  });

  const tabsEl = byId('ml-studio-tabs');
  const renderPanel = (tabId) => {
    const panel = byId('ml-studio-tab-panel');
    if (!panel) return;
    const idx = Number(String(tabId ?? '').replace('ml-tab-', ''));
    const label = tabs[Number.isNaN(idx) ? 0 : idx] || tabs[0];
    if (label === 'Dane') {
      renderDataTab(panel, projectId(p));
      return;
    }
    if (label === 'Zasoby') {
      renderResourcesTab(panel, projectId(p));
      return;
    }
    panel.innerHTML = '';
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', typeIcon(slug));
    empty.setAttribute('title', label);
    empty.setAttribute('message', 'Ta zakładka jest w przygotowaniu — kolejny plaster modułu ML Studio podłączy tu dane i akcje dla tego typu projektu.');
    panel.appendChild(empty);
  };
  // tf-tabs re-emits the selection as `change` with detail.value = tab id
  // (tf-tabs.js:316-319); the underlying tf-tab fires tf-tab-click with
  // detail.id (tf-tabs.js:81-84). We bind the higher-level `change`.
  tabsEl?.addEventListener('change', (e) => {
    renderPanel(e.detail?.value);
  });
  renderPanel('ml-tab-0');
}

// =============================================================================
// "Dane" tab — data provenance: upload a tabular file, list datasets, and show
// the column profile (type/unique/missing/examples) read straight from the file.
// Everything shown here comes from the backend profile; nothing is predefined.
// =============================================================================

// Backend columnType slug → human label + chip status (mirrors t-dane type-pill).
const COLUMN_TYPE_LABEL = {
  categorical: 'kategoria',
  integer: 'całkowita',
  float: 'zmiennoprzecinkowa',
  date: 'data',
  text: 'tekst',
};
const COLUMN_TYPE_STATUS = {
  categorical: 'accent',
  integer: 'info',
  float: 'info',
  date: 'ok',
  text: 'info',
};

function columnTypeSlug(col) {
  return String(col.columnType ?? col.column_type ?? 'text').toLowerCase();
}

function datasetKindLabel(kind) {
  const k = String(kind || '').toLowerCase();
  if (k === 'csv') return 'CSV';
  if (k === 'xlsx') return 'XLSX';
  return kind ? String(kind).toUpperCase() : '—';
}

function nameFromFilename(filename) {
  const base = String(filename || '').replace(/\.[^.]+$/, '').trim();
  return base || String(filename || 'zbiór');
}

function renderDataTab(panel, pid) {
  panel.innerHTML = `
    <div class="ml-studio-data">
      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('cloud')} Źródło danych
          <span class="ml-studio-data-hint">Formaty: .csv · .xlsx · pierwszy wiersz = nagłówki kolumn</span>
        </div>
        <tf-file-input id="ml-studio-data-file" accept=".csv,.xlsx" label="Przeciągnij plik lub kliknij, aby wgrać"></tf-file-input>
      </section>

      <div class="ml-studio-data-origin">
        <div class="ml-studio-data-origin-ico">${sprite('info')}</div>
        <div>
          <div class="ml-studio-data-origin-title">Kolumny i typy są CZYTANE z Twojego pliku — nic nie jest predefiniowane.</div>
          <p class="ml-studio-data-origin-text">Każdy wiersz profilu poniżej to jedna kolumna z Twojego pliku. Typ, liczba unikalnych wartości, % braków i liczba klas zostały policzone z zawartości — to one są źródłem dalszych kroków (np. „wykryto N klas").</p>
        </div>
      </div>

      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('database')} Wgrane zbiory</div>
        <div id="ml-studio-datasets"></div>
      </section>

      <section class="ml-studio-data-card" id="ml-studio-profile-card" hidden>
        <div class="ml-studio-data-head">${sprite('grid-rows')} Profil kolumn — wykryto z nagłówków pliku
          <span class="ml-studio-data-hint" id="ml-studio-profile-meta"></span>
        </div>
        <div id="ml-studio-profile"></div>
      </section>
    </div>
  `;

  const fileInput = byId('ml-studio-data-file');
  // tf-file-input emits `change` with detail.files (FileList) — tf-file-input.js:70.
  fileInput?.addEventListener('change', async (e) => {
    const files = e.detail?.files;
    const file = files && files.length ? files[0] : null;
    if (file) await uploadDataset(pid, file);
  });

  loadDatasets(pid);
}

// inline upload bounded by WS frame limit (1 MiB); larger datasets need chunked upload (future)
const MAX_UPLOAD_BYTES = 900 * 1024;

async function uploadDataset(pid, file) {
  const filename = file.name || 'zbiór';
  if (file.size > MAX_UPLOAD_BYTES) {
    toast('Plik za duży (limit ~0,9 MB dla wgrywania w tej wersji). Większe zbiory: chunked upload w przygotowaniu.', 'error');
    return;
  }
  try {
    const buf = await file.arrayBuffer();
    const bytes = new Uint8Array(buf);
    const resp = await ApiBinary.one('mlStudioDatasetUploadRequest', {
      projectId: pid,
      name: nameFromFilename(filename),
      filename,
      bytes,
    });
    toast(`Wgrano „${filename}" — sprofilowano`, 'success');
    await loadDatasets(pid);
    const datasetId = resp.datasetId ?? resp.dataset_id
      ?? resp.dataset?.datasetId ?? resp.dataset?.dataset_id;
    if (resp.profile) {
      renderProfile(resp.profile);
    } else if (datasetId) {
      await loadProfile(datasetId);
    }
  } catch (err) {
    toast(`Wgrywanie pliku: ${err.message}`, 'error');
  }
}

async function loadDatasets(pid) {
  const host = byId('ml-studio-datasets');
  if (!host) return;
  host.innerHTML = '<div class="ml-studio-loading"><tf-spinner></tf-spinner></div>';
  try {
    const resp = await ApiBinary.one('mlStudioDatasetsListRequest', { projectId: pid });
    const datasets = Array.isArray(resp.datasets) ? resp.datasets : [];
    renderDatasetsTable(host, datasets);
  } catch (err) {
    host.innerHTML = '';
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', 'alert');
    empty.setAttribute('title', 'Nie udało się wczytać zbiorów');
    empty.setAttribute('message', err.message || 'Błąd protokołu ML Studio.');
    host.appendChild(empty);
  }
}

function renderDatasetsTable(host, datasets) {
  host.innerHTML = '';
  if (!datasets.length) {
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', 'database');
    empty.setAttribute('title', 'Brak danych');
    empty.setAttribute('message', 'Wgraj plik CSV/XLSX — system odczyta jego nagłówki i zbuduje profil kolumn.');
    host.appendChild(empty);
    return;
  }

  const table = document.createElement('tf-table');
  table.setAttribute('variant', 'lined');
  table.innerHTML = `
    <tf-column key="name" label="Nazwa"></tf-column>
    <tf-column key="kind" label="Typ pliku" renderer="html"></tf-column>
    <tf-column key="rowCount" label="Wiersze" renderer="num"></tf-column>
    <tf-column key="columnCount" label="Kolumny" renderer="num"></tf-column>
    <tf-column key="createdAt" label="Data"></tf-column>
  `;
  table.rows = datasets.map((d) => {
    const id = d.datasetId ?? d.dataset_id ?? '';
    return {
      _datasetId: String(id),
      name: d.name || '(bez nazwy)',
      kind: `<span class="tf-chip info">${escapeHtml(datasetKindLabel(d.kind))}</span>`,
      rowCount: formatNumber(d.rowCount ?? d.row_count),
      columnCount: formatNumber(d.columnCount ?? d.column_count),
      createdAt: formatDate(d.createdAt ?? d.created_at),
    };
  });
  table.addEventListener('row-click', (e) => {
    const id = e.detail?.row?._datasetId;
    if (id) loadProfile(id);
  });
  host.appendChild(table);
}

async function loadProfile(datasetId) {
  const card = byId('ml-studio-profile-card');
  const host = byId('ml-studio-profile');
  if (!host || !card) return;
  card.hidden = false;
  host.innerHTML = '<div class="ml-studio-loading"><tf-spinner></tf-spinner></div>';
  try {
    const resp = await ApiBinary.one('mlStudioDatasetProfileRequest', { datasetId });
    renderProfile(resp.profile || resp);
  } catch (err) {
    host.innerHTML = '';
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', 'alert');
    empty.setAttribute('title', 'Nie udało się wczytać profilu');
    empty.setAttribute('message', err.message || 'Błąd protokołu ML Studio.');
    host.appendChild(empty);
  }
}

function renderProfile(profile) {
  const card = byId('ml-studio-profile-card');
  const host = byId('ml-studio-profile');
  const meta = byId('ml-studio-profile-meta');
  if (!host || !card) return;
  card.hidden = false;

  const rowCount = profile.rowCount ?? profile.row_count ?? 0;
  const columnCount = profile.columnCount ?? profile.column_count ?? 0;
  const scannedRows = profile.scannedRows ?? profile.scanned_rows ?? 0;
  const truncated = profile.truncated ?? false;
  const format = profile.format ? datasetKindLabel(profile.format) : '—';
  const columns = Array.isArray(profile.columns) ? profile.columns : [];

  if (meta) {
    let text = `${formatNumber(rowCount)} wierszy · ${formatNumber(columnCount)} kolumn · ${format} · przeskanowano ${formatNumber(scannedRows)} wierszy`;
    if (truncated) text += ' (próbka obcięta)';
    meta.textContent = text;
  }

  host.innerHTML = '';
  if (!columns.length) {
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', 'grid-rows');
    empty.setAttribute('title', 'Brak kolumn w profilu');
    empty.setAttribute('message', 'Plik nie zawiera rozpoznawalnych kolumn.');
    host.appendChild(empty);
    return;
  }

  const table = document.createElement('tf-table');
  table.setAttribute('variant', 'lined');
  table.innerHTML = `
    <tf-column key="name" label="Kolumna" renderer="html"></tf-column>
    <tf-column key="type" label="Typ" renderer="html"></tf-column>
    <tf-column key="unique" label="Unikalne" renderer="num"></tf-column>
    <tf-column key="missing" label="% braków" renderer="html"></tf-column>
    <tf-column key="examples" label="Przykłady" renderer="html"></tf-column>
  `;
  table.rows = columns.map((col) => profileRow(col));
  host.appendChild(table);
}

function profileRow(col) {
  const slug = columnTypeSlug(col);
  const typeLabel = COLUMN_TYPE_LABEL[slug] || slug;
  const status = COLUMN_TYPE_STATUS[slug] || 'info';
  const uniqueCount = col.uniqueCount ?? col.unique_count ?? 0;
  const uniqueCapped = col.uniqueCapped ?? col.unique_capped ?? false;
  const missingRatio = Number(col.missingRatio ?? col.missing_ratio ?? 0);
  const examples = Array.isArray(col.examples) ? col.examples : [];
  const classes = Array.isArray(col.classes) ? col.classes : [];

  const missingPct = (missingRatio * 100).toFixed(1).replace('.', ',');
  const missClass = missingRatio > 0.01 ? 'ml-studio-miss-warn' : 'ml-studio-miss-ok';

  // Categorical columns expose their detected classes — this is the provenance of
  // "wykryto N klas" downstream, so surface the value/count breakdown inline.
  let nameExtra = '';
  if (slug === 'categorical' && classes.length) {
    const list = classes
      .map((c) => `${escapeHtml(String(c.value ?? ''))} (${formatNumber(c.count ?? 0)})`)
      .join(', ');
    nameExtra = `<div class="ml-studio-col-classes">${sprite('info')} wykryto ${classes.length} ${plural(classes.length, 'klasę', 'klasy', 'klas')}: ${list}</div>`;
  }

  const uniqueText = `${formatNumber(uniqueCount)}${uniqueCapped ? '+' : ''}`;
  const examplesHtml = examples.length
    ? examples.slice(0, 4).map((v) => `<span class="ml-studio-col-example">${escapeHtml(String(v))}</span>`).join('')
    : '<span class="ml-studio-col-example ml-studio-col-example-empty">—</span>';

  return {
    name: `<span class="ml-studio-col-name">${escapeHtml(String(col.name ?? ''))}</span>${nameExtra}`,
    type: `<span class="tf-chip ${status}">${escapeHtml(typeLabel)}</span>`,
    unique: uniqueText,
    missing: `<span class="${missClass}">${missingPct}%</span>`,
    examples: `<div class="ml-studio-col-examples">${examplesHtml}</div>`,
  };
}

function formatNumber(value) {
  const n = Number(value);
  if (!Number.isFinite(n)) return String(value ?? '—');
  return n.toLocaleString('pl-PL');
}

// =============================================================================
// Sharing screen (p02) — members table, invite form, role legend.
// Owner-only controls (invite / remove / role change) are gated by the
// project's isOwner flag confirmed against the current user's membership.
// =============================================================================

async function showShare(pid) {
  detailProjectId = pid;
  const host = byId('ml-studio-share');
  if (!host) return;
  host.innerHTML = '<div class="ml-studio-loading"><tf-spinner></tf-spinner></div>';

  try {
    const [detailResp, membersResp] = await Promise.all([
      ApiBinary.one('mlStudioProjectDetailRequest', { projectId: pid }),
      ApiBinary.one('mlStudioProjectMembersListRequest', { projectId: pid }),
      ensureProjectTypes(),
      ensureCurrentUser(),
    ]);
    const project = detailResp.project || {};
    const members = Array.isArray(membersResp.members) ? membersResp.members : [];
    renderShare(host, project, members);
  } catch (err) {
    host.innerHTML = '';
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', 'alert');
    empty.setAttribute('title', 'Nie udało się wczytać udostępniania');
    empty.setAttribute('message', err.message || 'Błąd protokołu ML Studio.');
    const back = document.createElement('tf-button');
    back.setAttribute('variant', 'primary');
    back.textContent = 'Wróć do projektu';
    back.addEventListener('click', () => Router.navigate('ml-studio', { projectId: pid }));
    empty.appendChild(back);
    host.appendChild(empty);
    toast(`ML Studio: ${err.message}`, 'error');
  }
}

function renderShare(host, project, members) {
  const pid = projectId(project);
  const slug = projectType(project);
  const isOwner = isOwnerProject(project);
  const memberCount = members.filter((m) => String(m.status || 'active').toLowerCase() === 'active').length;
  const pendingCount = members.filter((m) => String(m.status || '').toLowerCase() === 'invited').length;

  host.innerHTML = `
    <div class="ml-studio-detail-top">
      <tf-button variant="ghost" icon="chevron-left" id="ml-studio-share-back">Wróć do projektu</tf-button>
    </div>

    <tf-detail-header
      title="Udostępnianie — ${escapeAttr(project.name || '(bez nazwy)')}"
      subtitle="Zarządzaj członkami projektu i ich rolami. Domyślnie projekt jest prywatny."
      icon="${escapeAttr(typeIcon(slug))}">
      <span slot="badges">
        <tf-badge tone="${isOwner ? 'accent' : 'info'}" value="${isOwner ? 'Właściciel: Ty' : roleLabel(projectRole(project))}"></tf-badge>
        <tf-badge tone="info" value="${memberCount} ${plural(memberCount, 'członek', 'członków', 'członków')}"></tf-badge>
        ${pendingCount ? `<tf-badge tone="warning" value="${pendingCount} oczekuje"></tf-badge>` : ''}
      </span>
    </tf-detail-header>

    <section class="ml-studio-share-card">
      <div class="ml-studio-share-head">${sprite('users')} Członkowie projektu</div>
      <tf-table id="ml-studio-members" variant="lined">
        <tf-column key="user" label="Użytkownik" renderer="html"></tf-column>
        <tf-column key="role" label="Rola" renderer="html"></tf-column>
        <tf-column key="status" label="Status" renderer="html"></tf-column>
        <tf-column key="invitedBy" label="Dodany przez"></tf-column>
        <tf-column key="createdAt" label="Data"></tf-column>
      </tf-table>
    </section>

    ${isOwner ? inviteSection() : ''}

    <section class="ml-studio-share-card">
      <div class="ml-studio-share-head">${sprite('info')} Co potrafi każda rola</div>
      <div class="ml-studio-role-legend">
        ${roleLegendItem('owner', 'Pełnia uprawnień + zarządzanie dostępem: zaprasza, usuwa członków, zmienia role, usuwa projekt. Jeden na projekt (twórca).')}
        ${roleLegendItem('editor', 'Pracuje na projekcie: dane, schemat, anotacja, uruchamianie treningu i eksport. Nie zarządza dostępem.')}
        ${roleLegendItem('viewer', 'Tylko podgląd: dane, schemat, wyniki i modele. Nie zmienia anotacji ani nie uruchamia treningu.')}
      </div>
      ${isOwner ? '' : '<p class="ml-studio-share-hint">' + sprite('info') + ' Zarządzanie dostępem (zaproszenia, role, usuwanie) jest dostępne tylko dla właściciela projektu.</p>'}
    </section>
  `;

  byId('ml-studio-share-back')?.addEventListener('click', () => Router.navigate('ml-studio', { projectId: pid }));

  renderMembersTable(pid, project, members, isOwner);
  if (isOwner) bindInviteForm(pid);
}

function inviteSection() {
  return `
    <section class="ml-studio-share-card">
      <div class="ml-studio-share-head">${sprite('mail')} Zaproś do projektu</div>
      <div class="ml-studio-invite-row">
        <tf-input id="ml-studio-invite-user" label="Identyfikator użytkownika"
          placeholder="np. user-1a2b lub login" hint="Podaj identyfikator użytkownika TentaFlow (lookup po nazwie dorobimy później)."></tf-input>
        <tf-select id="ml-studio-invite-role" label="Rola" value="editor">
          <option value="editor">Edytor</option>
          <option value="viewer">Przeglądający</option>
        </tf-select>
        <tf-button variant="primary" icon="send" id="ml-studio-invite-send">Wyślij zaproszenie</tf-button>
      </div>
      <p class="ml-studio-share-hint">${sprite('info')} Zaproszony zobaczy projekt na swoim koncie i będzie działał wg roli. Rola „Właściciel" nie jest nadawana przez zaproszenie.</p>
    </section>
  `;
}

function roleLegendItem(role, desc) {
  const tone = role === 'owner' ? 'accent' : 'info';
  return `
    <div class="ml-studio-role-item">
      <tf-chip status="${tone}" icon="${escapeAttr(ROLE_ICON[role])}" label="${escapeAttr(roleLabel(role))}"></tf-chip>
      <p class="ml-studio-role-desc">${escapeHtml(desc)}</p>
    </div>
  `;
}

function memberUserId(m) {
  return String(m.userId ?? m.user_id ?? '');
}

function renderMembersTable(pid, project, members, isOwner) {
  const table = byId('ml-studio-members');
  if (!table) return;
  const selfId = currentUserId();

  table.rows = members.map((m) => {
    const uid = memberUserId(m);
    const role = String(m.role ?? '').toLowerCase();
    const status = String(m.status ?? 'active').toLowerCase();
    const isSelf = selfId && uid === selfId;
    const invitedBy = m.invitedBy ?? m.invited_by;

    const userHtml = `
      <div class="ml-studio-member-cell">
        <span class="ml-studio-member-id">${escapeHtml(uid || '—')}${isSelf ? ' <span class="ml-studio-member-self">(Ty)</span>' : ''}</span>
      </div>`;

    // Owner role is always a static badge; for the rest the owner gets an inline
    // tf-select to change the role, everyone else sees a read-only chip.
    let roleHtml;
    if (role === 'owner') {
      roleHtml = `<tf-chip status="accent" icon="crown" label="Właściciel"></tf-chip>`;
    } else if (isOwner && status !== 'invited') {
      roleHtml = `<tf-select class="ml-studio-role-pick" data-user-id="${escapeAttr(uid)}" value="${escapeAttr(role || 'viewer')}">
          <option value="editor">Edytor</option>
          <option value="viewer">Przeglądający</option>
        </tf-select>`;
    } else {
      roleHtml = `<tf-chip status="info" icon="${escapeAttr(ROLE_ICON[role] || 'eye')}" label="${escapeAttr(roleLabel(role))}"></tf-chip>`;
    }

    const statusHtml = status === 'invited'
      ? `<tf-chip status="warn" dot label="oczekuje"></tf-chip>`
      : `<tf-chip status="ok" dot label="aktywny"></tf-chip>`;

    return {
      _userId: uid,
      _role: role,
      _status: status,
      user: userHtml,
      role: roleHtml,
      status: statusHtml,
      invitedBy: invitedBy ? String(invitedBy) : (role === 'owner' ? '— (twórca)' : '—'),
      createdAt: formatDate(m.createdAt ?? m.created_at),
    };
  });

  // The owner manages access; non-owners get a read-only table (no actions col).
  if (isOwner) {
    table.rowActions = (row) => {
      if (row._role === 'owner') {
        const note = document.createElement('span');
        note.className = 'ml-studio-member-note';
        note.textContent = 'nie można usunąć';
        return note;
      }
      const btn = document.createElement('tf-button');
      btn.setAttribute('variant', 'ghost');
      btn.setAttribute('icon', 'trash');
      btn.textContent = row._status === 'invited' ? 'Cofnij' : 'Usuń';
      btn.addEventListener('click', () => removeMember(pid, row._userId));
      return btn;
    };
  } else {
    table.rowActions = null;
  }

  // Inline role selects live in tf-table's (open) shadow DOM, so a single
  // delegated listener on the shadow root catches their bubbling `change`
  // CustomEvent (detail.value, tf-select.js:114-117) — host-level binding would
  // never see them across the shadow boundary.
  if (isOwner && table.shadowRoot && !table._mlRoleBound) {
    table._mlRoleBound = true;
    table.shadowRoot.addEventListener('change', (e) => {
      const sel = e.target?.closest?.('.ml-studio-role-pick');
      if (!sel) return;
      const uid = sel.dataset.userId;
      const role = e.detail?.value;
      if (uid && role) setMemberRole(pid, uid, role);
    });
  }
}

function bindInviteForm(pid) {
  const sendBtn = byId('ml-studio-invite-send');
  sendBtn?.addEventListener('click', async () => {
    const inviteeUserId = byId('ml-studio-invite-user')?.value?.trim() || '';
    const role = byId('ml-studio-invite-role')?.value || 'editor';
    if (!inviteeUserId) {
      toast('Podaj identyfikator użytkownika.', 'error');
      return;
    }
    sendBtn.setAttribute('loading', '');
    try {
      await ApiBinary.one('mlStudioProjectInviteRequest', { projectId: pid, inviteeUserId, role });
      toast('Zaproszenie wysłane', 'success');
      await showShare(pid);
    } catch (err) {
      sendBtn.removeAttribute('loading');
      toast(`Zaproszenie: ${err.message}`, 'error');
    }
  });
}

async function removeMember(pid, userId) {
  try {
    await ApiBinary.one('mlStudioProjectMemberRemoveRequest', { projectId: pid, userId });
    toast('Członek usunięty', 'success');
    await showShare(pid);
  } catch (err) {
    toast(`Usuwanie: ${err.message}`, 'error');
  }
}

async function setMemberRole(pid, userId, role) {
  try {
    await ApiBinary.one('mlStudioProjectMemberRoleSetRequest', { projectId: pid, userId, role });
    toast('Rola zmieniona', 'success');
    await showShare(pid);
  } catch (err) {
    toast(`Zmiana roli: ${err.message}`, 'error');
  }
}

// =============================================================================
// Mesh resource allocation (§11.3) — admin screen: pool of mesh nodes, allocate
// a resource to a subject (user/group/project), and the active grants table.
// Pool comes from MeshNodeListRequest; grants from the ML Studio backend.
// =============================================================================

const SUBJECT_KIND_LABEL = {
  user: 'Osoba',
  group: 'Grupa',
  project: 'Projekt',
};
const RESOURCE_KIND_LABEL = {
  gpu: 'GPU',
  cpu: 'CPU',
  ram: 'RAM',
};

// Mesh node fields arrive snake_case over the wire (mesh.js reads vram_total_mb,
// gpus[].name); the §11.3 contract documents camelCase. Read both so the screen
// survives either casing.
function nodeIdOf(n) {
  return String(n.nodeId ?? n.node_id ?? '');
}
function nodeHostname(n) {
  const id = nodeIdOf(n);
  return n.hostname || (id ? id.slice(0, 12) : '(nieznany)');
}
function nodeGpus(n) {
  return Array.isArray(n.gpus) ? n.gpus : [];
}
function gpuName(g) {
  return g.name || g.model || '(GPU)';
}
function gpuVramMb(g) {
  return Number(g.vramTotalMb ?? g.vram_total_mb ?? 0);
}
function nodeCpuCount(n) {
  return Number(n.cpuCount ?? n.cpu_count ?? 0);
}
function nodeRamMb(n) {
  return Number(n.ramTotalMb ?? n.ram_total_mb ?? 0);
}

function formatMbGb(mb) {
  const n = Number(mb);
  if (!Number.isFinite(n) || n <= 0) return '—';
  if (n >= 1024) return `${(n / 1024).toLocaleString('pl-PL', { maximumFractionDigits: 1 })} GB`;
  return `${formatNumber(n)} MB`;
}

async function showResourcesAdmin() {
  const host = byId('ml-studio-resources');
  if (!host) return;
  host.innerHTML = '<div class="ml-studio-loading"><tf-spinner></tf-spinner></div>';

  await ensureCurrentUser();
  if (!isCurrentUserAdmin()) {
    host.innerHTML = '';
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', 'lock');
    empty.setAttribute('title', 'Tylko dla administratora');
    empty.setAttribute('message', 'Przydział zasobów mesh jest dostępny wyłącznie dla administratora.');
    const back = document.createElement('tf-button');
    back.setAttribute('variant', 'primary');
    back.textContent = 'Wróć do projektów';
    back.addEventListener('click', () => Router.navigate('ml-studio'));
    empty.appendChild(back);
    host.appendChild(empty);
    return;
  }

  try {
    const [nodesResp, grantsResp] = await Promise.all([
      ApiBinary.one('meshNodeListRequest'),
      ApiBinary.one('mlStudioResourceGrantsListRequest'),
      ensureProjectTypes(),
    ]);
    if (!projects.length) {
      const projResp = await ApiBinary.one('mlStudioProjectsListRequest').catch(() => null);
      if (projResp) projects = Array.isArray(projResp.projects) ? projResp.projects : [];
    }
    resourceNodes = Array.isArray(nodesResp.nodes) ? nodesResp.nodes : [];
    const grants = Array.isArray(grantsResp.grants) ? grantsResp.grants : [];
    renderResourcesAdmin(host, resourceNodes, grants);
  } catch (err) {
    host.innerHTML = '';
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', 'alert');
    empty.setAttribute('title', 'Nie udało się wczytać zasobów');
    empty.setAttribute('message', err.message || 'Błąd protokołu ML Studio.');
    const back = document.createElement('tf-button');
    back.setAttribute('variant', 'primary');
    back.textContent = 'Wróć do projektów';
    back.addEventListener('click', () => Router.navigate('ml-studio'));
    empty.appendChild(back);
    host.appendChild(empty);
    toast(`Zasoby: ${err.message}`, 'error');
  }
}

function renderResourcesAdmin(host, nodes, grants) {
  const totalGpus = nodes.reduce((s, n) => s + nodeGpus(n).length, 0);

  host.innerHTML = `
    <div class="ml-studio-detail-top">
      <tf-button variant="ghost" icon="chevron-left" id="ml-studio-res-back">Projekty</tf-button>
    </div>

    <tf-detail-header
      title="Zasoby mesh"
      subtitle="Przydzielanie zasobów obliczeniowych z puli mesh osobom, grupom i projektom. Domyślnie nikt nie ma zasobów."
      icon="host">
      <span slot="badges">
        <tf-badge tone="info" value="${nodes.length} ${plural(nodes.length, 'node', 'nody', 'nodów')}"></tf-badge>
        <tf-badge tone="accent" value="${totalGpus} GPU"></tf-badge>
      </span>
    </tf-detail-header>

    <section class="ml-studio-res-card">
      <div class="ml-studio-res-head">${sprite('host')} Pula zasobów mesh — wszystkie nody</div>
      <div id="ml-studio-res-pool" class="ml-studio-res-pool"></div>
    </section>

    <section class="ml-studio-res-card">
      <div class="ml-studio-res-head">${sprite('plus')} Przydziel zasób</div>
      <div class="ml-studio-res-form">
        <tf-select id="ml-studio-res-subject-kind" label="Komu" value="project">
          <option value="user">Osoba</option>
          <option value="group">Grupa</option>
          <option value="project">Projekt</option>
        </tf-select>
        <div id="ml-studio-res-subject-field"></div>
        <tf-select id="ml-studio-res-node" label="Node"></tf-select>
        <tf-select id="ml-studio-res-kind" label="Rodzaj zasobu" value="gpu">
          <option value="gpu">GPU</option>
          <option value="cpu">CPU</option>
          <option value="ram">RAM</option>
        </tf-select>
        <div id="ml-studio-res-ref-field"></div>
        <tf-input id="ml-studio-res-quota" label="Limit (quota)" placeholder="np. 1 GPU / 10 h / 8 GB" hint="Jednostka zależy od rodzaju zasobu."></tf-input>
        <tf-button variant="primary" icon="check" id="ml-studio-res-grant">Przydziel</tf-button>
      </div>
    </section>

    <section class="ml-studio-res-card">
      <div class="ml-studio-res-head">${sprite('list')} Aktywne przydziały</div>
      <div id="ml-studio-res-grants"></div>
    </section>
  `;

  byId('ml-studio-res-back')?.addEventListener('click', () => Router.navigate('ml-studio'));

  renderResourcePool(byId('ml-studio-res-pool'), nodes);
  bindResourceForm(nodes);
  renderGrantsTable(byId('ml-studio-res-grants'), grants);
}

function renderResourcePool(pool, nodes) {
  if (!pool) return;
  if (!nodes.length) {
    pool.innerHTML = '';
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', 'host');
    empty.setAttribute('title', 'Brak nodów w mesh');
    empty.setAttribute('message', 'Sparuj węzeł w sekcji Mesh, aby udostępnić jego zasoby do przydziału.');
    pool.appendChild(empty);
    return;
  }

  pool.innerHTML = nodes.map((n) => {
    const gpus = nodeGpus(n);
    const gpuRows = gpus.length
      ? gpus.map((g) => `
          <div class="ml-studio-res-gpu">
            <span class="ml-studio-res-gpu-name">${sprite('chip')} ${escapeHtml(gpuName(g))}</span>
            <tf-chip status="info" label="${escapeAttr(formatMbGb(gpuVramMb(g)))} VRAM"></tf-chip>
          </div>`).join('')
      : `<div class="ml-studio-res-gpu ml-studio-res-gpu-empty">${sprite('info')} brak GPU</div>`;
    return `
      <article class="ml-studio-res-node">
        <div class="ml-studio-res-node-top">
          <span class="ml-studio-res-node-name">${sprite('host')} ${escapeHtml(nodeHostname(n))}</span>
          <tf-badge tone="accent" value="${gpus.length} GPU"></tf-badge>
        </div>
        <div class="ml-studio-res-gpus">${gpuRows}</div>
        <div class="ml-studio-res-node-foot">
          <tf-chip status="ok" icon="cpu" label="${nodeCpuCount(n) || '—'} CPU"></tf-chip>
          <tf-chip status="ok" icon="ram" label="${escapeAttr(formatMbGb(nodeRamMb(n)))} RAM"></tf-chip>
        </div>
      </article>
    `;
  }).join('');
}

// Subject field switches between a project picker (for kind=project) and a free
// identifier input (user/group). Resource-ref field shows a GPU picker only when
// the selected node has GPUs and the resource kind is gpu.
function bindResourceForm(nodes) {
  const kindSel = byId('ml-studio-res-subject-kind');
  const nodeSel = byId('ml-studio-res-node');
  const resKindSel = byId('ml-studio-res-kind');

  if (nodeSel) {
    const opts = nodes.map((n) => ({ value: nodeIdOf(n), label: nodeHostname(n) }));
    nodeSel.setOptions(opts, nodes.length ? nodeIdOf(nodes[0]) : null);
  }

  const renderSubjectField = () => {
    const field = byId('ml-studio-res-subject-field');
    if (!field) return;
    const kind = kindSel?.value || 'project';
    if (kind === 'project') {
      const opts = projects.map((p) => `<option value="${escapeAttr(projectId(p))}">${escapeHtml(p.name || '(bez nazwy)')}</option>`).join('');
      field.innerHTML = `<tf-select id="ml-studio-res-subject" label="Projekt">${opts}</tf-select>`;
    } else {
      const hint = kind === 'user' ? 'id użytkownika' : 'id grupy';
      field.innerHTML = `<tf-input id="ml-studio-res-subject" label="Identyfikator" placeholder="np. user-1a2b" hint="${escapeAttr(hint)}"></tf-input>`;
    }
  };

  const renderRefField = () => {
    const field = byId('ml-studio-res-ref-field');
    if (!field) return;
    const resKind = resKindSel?.value || 'gpu';
    const node = nodes.find((n) => nodeIdOf(n) === (nodeSel?.value || ''));
    const gpus = node ? nodeGpus(node) : [];
    if (resKind === 'gpu' && gpus.length) {
      const opts = gpus.map((g, i) => {
        const ref = String(g.uuid ?? g.gpuId ?? g.gpu_id ?? g.index ?? i);
        return `<option value="${escapeAttr(ref)}">${escapeHtml(gpuName(g))} · ${escapeHtml(formatMbGb(gpuVramMb(g)))}</option>`;
      }).join('');
      field.innerHTML = `<tf-select id="ml-studio-res-ref" label="Karta GPU">${opts}</tf-select>`;
    } else {
      field.innerHTML = '';
    }
  };

  kindSel?.addEventListener('change', renderSubjectField);
  resKindSel?.addEventListener('change', renderRefField);
  nodeSel?.addEventListener('change', renderRefField);
  renderSubjectField();
  renderRefField();

  const grantBtn = byId('ml-studio-res-grant');
  grantBtn?.addEventListener('click', async () => {
    const subjectKind = kindSel?.value || 'project';
    const subjectId = byId('ml-studio-res-subject')?.value?.trim() || '';
    const nodeId = nodeSel?.value || '';
    const resourceKind = resKindSel?.value || 'gpu';
    const resourceRef = byId('ml-studio-res-ref')?.value || '';
    const quota = byId('ml-studio-res-quota')?.value?.trim() || '';
    if (!subjectId) {
      toast('Wskaż podmiot przydziału (osobę, grupę lub projekt).', 'error');
      return;
    }
    if (!nodeId) {
      toast('Wybierz node z puli mesh.', 'error');
      return;
    }
    grantBtn.setAttribute('loading', '');
    try {
      await ApiBinary.one('mlStudioResourceGrantCreateRequest', {
        subjectKind, subjectId, nodeId, resourceKind, resourceRef, quota,
      });
      toast('Zasób przydzielony', 'success');
      await showResourcesAdmin();
    } catch (err) {
      grantBtn.removeAttribute('loading');
      toast(`Przydział: ${err.message}`, 'error');
    }
  });
}

function grantNodeLabel(g) {
  const nid = String(g.nodeId ?? g.node_id ?? '');
  const node = resourceNodes.find((n) => nodeIdOf(n) === nid);
  return node ? nodeHostname(node) : (nid ? nid.slice(0, 12) : '—');
}

function renderGrantsTable(host, grants) {
  if (!host) return;
  host.innerHTML = '';
  if (!grants.length) {
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', 'lock');
    empty.setAttribute('title', 'Brak przydziałów');
    empty.setAttribute('message', 'Domyślnie nikt nie ma zasobów. Użyj formularza powyżej, aby przydzielić zasób mesh.');
    host.appendChild(empty);
    return;
  }

  const table = document.createElement('tf-table');
  table.setAttribute('variant', 'lined');
  table.innerHTML = `
    <tf-column key="subject" label="Podmiot" renderer="html"></tf-column>
    <tf-column key="resource" label="Zasób" renderer="html"></tf-column>
    <tf-column key="quota" label="Limit"></tf-column>
    <tf-column key="grantedBy" label="Przydzielił"></tf-column>
    <tf-column key="createdAt" label="Data"></tf-column>
  `;
  table.rows = grants.map((g) => {
    const kind = String(g.subjectKind ?? g.subject_kind ?? '').toLowerCase();
    const subjectId = String(g.subjectId ?? g.subject_id ?? '');
    const resourceKind = String(g.resourceKind ?? g.resource_kind ?? '').toLowerCase();
    const resourceRef = String(g.resourceRef ?? g.resource_ref ?? '');
    const subjectHtml = `<span class="ml-studio-res-subject-cell"><tf-chip status="info" label="${escapeAttr(SUBJECT_KIND_LABEL[kind] || kind || '—')}"></tf-chip> ${escapeHtml(subjectId || '—')}</span>`;
    const resLabel = RESOURCE_KIND_LABEL[resourceKind] || resourceKind || '—';
    const resourceHtml = `<span class="ml-studio-res-resource-cell">${sprite('host')} ${escapeHtml(grantNodeLabel(g))} · <strong>${escapeHtml(resLabel)}</strong>${resourceRef ? ` · ${escapeHtml(resourceRef)}` : ''}</span>`;
    return {
      _grantId: String(g.grantId ?? g.grant_id ?? ''),
      subject: subjectHtml,
      resource: resourceHtml,
      quota: g.quota ? String(g.quota) : '—',
      grantedBy: String(g.grantedBy ?? g.granted_by ?? '—'),
      createdAt: formatDate(g.createdAt ?? g.created_at),
    };
  });
  table.rowActions = (row) => {
    const btn = document.createElement('tf-button');
    btn.setAttribute('variant', 'ghost');
    btn.setAttribute('icon', 'trash');
    btn.textContent = 'Cofnij';
    btn.addEventListener('click', () => revokeGrant(row._grantId));
    return btn;
  };
  host.appendChild(table);
}

async function revokeGrant(grantId) {
  if (!grantId) return;
  try {
    await ApiBinary.one('mlStudioResourceGrantRevokeRequest', { grantId });
    toast('Przydział cofnięty', 'success');
    await showResourcesAdmin();
  } catch (err) {
    toast(`Cofanie: ${err.message}`, 'error');
  }
}

// Project detail "Zasoby" section — read-only view of the grants allocated to a
// project. Non-admins see a hint to ask an admin; admins get a shortcut to the
// allocation screen.
function renderResourcesTab(panel, pid) {
  panel.innerHTML = '<div class="ml-studio-loading"><tf-spinner></tf-spinner></div>';
  ApiBinary.one('mlStudioProjectResourcesRequest', { projectId: pid })
    .then((resp) => {
      const grants = Array.isArray(resp.grants) ? resp.grants : [];
      panel.innerHTML = '';
      if (!grants.length) {
        const empty = document.createElement('tf-empty-state');
        empty.setAttribute('icon', 'host');
        empty.setAttribute('title', 'Brak przydzielonych zasobów');
        empty.setAttribute('message', isCurrentUserAdmin()
          ? 'Ten projekt nie ma przydzielonych zasobów mesh. Przejdź do ekranu Zasoby, aby je przydzielić.'
          : 'Ten projekt nie ma przydzielonych zasobów mesh — poproś administratora o przydział.');
        if (isCurrentUserAdmin()) {
          const btn = document.createElement('tf-button');
          btn.setAttribute('variant', 'primary');
          btn.setAttribute('icon', 'host');
          btn.textContent = 'Przejdź do Zasobów';
          btn.addEventListener('click', () => Router.navigate('ml-studio', { admin: 'resources' }));
          empty.appendChild(btn);
        }
        panel.appendChild(empty);
        return;
      }
      const table = document.createElement('tf-table');
      table.setAttribute('variant', 'lined');
      table.innerHTML = `
        <tf-column key="resource" label="Zasób" renderer="html"></tf-column>
        <tf-column key="quota" label="Limit"></tf-column>
        <tf-column key="grantedBy" label="Przydzielił"></tf-column>
        <tf-column key="createdAt" label="Data"></tf-column>
      `;
      table.rows = grants.map((g) => {
        const resourceKind = String(g.resourceKind ?? g.resource_kind ?? '').toLowerCase();
        const resourceRef = String(g.resourceRef ?? g.resource_ref ?? '');
        const nid = String(g.nodeId ?? g.node_id ?? '');
        const resLabel = RESOURCE_KIND_LABEL[resourceKind] || resourceKind || '—';
        const nodeLabel = g.hostname || (nid ? nid.slice(0, 12) : '—');
        return {
          resource: `<span class="ml-studio-res-resource-cell">${sprite('host')} ${escapeHtml(nodeLabel)} · <strong>${escapeHtml(resLabel)}</strong>${resourceRef ? ` · ${escapeHtml(resourceRef)}` : ''}</span>`,
          quota: g.quota ? String(g.quota) : '—',
          grantedBy: String(g.grantedBy ?? g.granted_by ?? '—'),
          createdAt: formatDate(g.createdAt ?? g.created_at),
        };
      });
      panel.appendChild(table);
    })
    .catch((err) => {
      panel.innerHTML = '';
      const empty = document.createElement('tf-empty-state');
      empty.setAttribute('icon', 'alert');
      empty.setAttribute('title', 'Nie udało się wczytać zasobów projektu');
      empty.setAttribute('message', err.message || 'Błąd protokołu ML Studio.');
      panel.appendChild(empty);
    });
}

function formatDate(value) {
  if (!value) return '—';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString('pl-PL');
}

function plural(n, one, few, many) {
  const mod10 = n % 10;
  const mod100 = n % 100;
  if (n === 1) return one;
  if (mod10 >= 2 && mod10 <= 4 && (mod100 < 10 || mod100 >= 20)) return few;
  return many;
}

export default MlStudioScreen;
