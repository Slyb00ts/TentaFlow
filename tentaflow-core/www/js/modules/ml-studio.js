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
    if (params && params.projectId) {
      return `<div id="ml-studio-detail" class="ml-studio-detail"></div>`;
    }
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('brain')} ML Studio</h1>
          <div class="sub" id="ml-studio-sub">Projekty — jednostki pracy ML (dane, schemat, treningi, modele)</div>
        </div>
        <div class="actions">
          <tf-button variant="ghost" icon="refresh" id="ml-studio-refresh">Odśwież</tf-button>
          <tf-button variant="primary" icon="plus" id="ml-studio-new">Nowy projekt</tf-button>
        </div>
      </div>

      <tf-filter-chips id="ml-studio-filters" mode="single"></tf-filter-chips>

      <div id="ml-studio-list" class="ml-studio-grid"></div>
    `;
  },

  async mount(params = {}) {
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
    ]);
    projectTypes = Array.isArray(typesResp.types) ? typesResp.types : [];
    projects = Array.isArray(projectsResp.projects) ? projectsResp.projects : [];
    renderFilters();
    renderList();
    const sub = byId('ml-studio-sub');
    if (sub) sub.textContent = `${projects.length} ${plural(projects.length, 'projekt', 'projekty', 'projektów')}`;
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

  host.innerHTML = visible.map((p) => projectCard(p)).join('') + newProjectCard();

  host.querySelectorAll('[data-project-id]').forEach((el) => {
    el.addEventListener('click', () => {
      Router.navigate('ml-studio', { projectId: el.dataset.projectId });
    });
  });
  host.querySelector('[data-new-project]')?.addEventListener('click', openCreateModal);
}

function projectCard(p) {
  const id = p.projectId ?? p.project_id ?? '';
  const slug = projectType(p);
  const datasetCount = p.datasetCount ?? p.dataset_count ?? 0;
  const modelCount = p.modelCount ?? p.model_count ?? 0;
  const created = formatDate(p.createdAt ?? p.created_at);
  const updated = formatDate(p.updatedAt ?? p.updated_at);
  return `
    <article class="ml-studio-card" data-project-id="${escapeAttr(id)}">
      <div class="ml-studio-card-top">
        <div class="ml-studio-card-ico">${sprite(typeIcon(slug))}</div>
        <div class="ml-studio-card-id">
          <div class="ml-studio-card-name">${escapeHtml(p.name || '(bez nazwy)')}</div>
          <div class="ml-studio-card-type">${escapeHtml(typeLabel(slug))}</div>
        </div>
        <tf-badge tone="${statusTone(p.status)}" value="${escapeAttr(p.status || '—')}"></tf-badge>
      </div>
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
  const tabs = TYPE_TABS[slug] || ['Przegląd', 'Dane', 'Treningi', 'Modele'];

  host.innerHTML = `
    <div class="ml-studio-detail-top">
      <tf-button variant="ghost" icon="chevron-left" id="ml-studio-back">Projekty</tf-button>
    </div>

    <tf-detail-header
      title="${escapeAttr(p.name || '(bez nazwy)')}"
      subtitle="${escapeAttr(typeLabel(slug))}"
      icon="${escapeAttr(typeIcon(slug))}">
      <span slot="badges"><tf-badge tone="${statusTone(p.status)}" value="${escapeAttr(p.status || '—')}"></tf-badge></span>
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

  const tabsEl = byId('ml-studio-tabs');
  const renderPanel = (tabId) => {
    const panel = byId('ml-studio-tab-panel');
    if (!panel) return;
    const idx = Number(String(tabId ?? '').replace('ml-tab-', ''));
    const label = tabs[Number.isNaN(idx) ? 0 : idx] || tabs[0];
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
