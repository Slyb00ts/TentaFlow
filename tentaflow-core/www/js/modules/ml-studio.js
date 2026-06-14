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
    shareBtn = `<button type="button" class="ml-studio-card-share" data-share-id="${escapeAttr(id)}" title="Udostępnij projekt" aria-label="Udostępnij projekt">${sprite('share')}</button>`;
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
