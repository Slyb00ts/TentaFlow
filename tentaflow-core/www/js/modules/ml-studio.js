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
import { I18n } from '/js/i18n.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-badge.js';
import '/js/components/tf-input.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-radio.js';
import '/js/components/tf-modal.js';
import '/js/components/tf-select.js';
import '/js/components/tf-table.js';
import '/js/components/tf-file-input.js';
import '/js/components/tf-filter-chips.js';
import '/js/components/tf-stat-card.js';
import '/js/components/tf-detail-header.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-spinner.js';
import '/js/components/tf-progress-bar.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-tag-input.js';

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

// Konfiguracja fine-tuningu LLM per projekt (zakładka „Model bazowy"), żeby
// zakładka „Trening" mogła odczytać wybór modelu/metody/hiperparametrów. Klucz =
// projectId. Trzymane w pamięci modułu — krótko żyjący wybór kreatora, nie stan
// backendu (ten powstaje dopiero po starcie treningu jako runId).
const ftConfig = {};

// Interwał pollingu statusu treningu FT (jeden na raz w widoku). Czyszczony przy
// każdym przełączeniu zakładki (renderPanel) i przy opuszczeniu ekranu (unmount),
// żeby żaden timer nie wisiał po wyjściu z widoku LIVE.
let ftPollTimer = null;

function stopFtPolling() {
  if (ftPollTimer !== null) {
    clearInterval(ftPollTimer);
    ftPollTimer = null;
  }
}

// Interwał pollingu statusu eksportu modelu FT do GGUF (jeden modal na raz).
// Czyszczony przy zamknięciu modala oraz w unmount, żeby żaden timer nie wisiał
// po wyjściu z widoku — analogicznie do stopFtPolling treningu live.
let ftExportPollTimer = null;

function stopFtExportPolling() {
  if (ftExportPollTimer !== null) {
    clearInterval(ftExportPollTimer);
    ftExportPollTimer = null;
  }
}

// Interwał auto-odświeżania panelu jobów ML Studio (widok „Joby"). Czyszczony
// przy opuszczeniu widoku (unmount) oraz przy ponownym wejściu, żeby nie było
// dwóch równoległych timerów odpytujących mlStudioJobsOverviewRequest.
let jobsPollTimer = null;

function stopJobsPolling() {
  if (jobsPollTimer !== null) {
    clearInterval(jobsPollTimer);
    jobsPollTimer = null;
  }
}

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
  distillation: 'transform',
};

const TYPE_TABS = {
  recognition: ['Schemat', 'Dane', 'Anotacje', 'Trening', 'Treningi', 'Modele'],
  ft_llm: ['Model bazowy', 'Dane', 'Trening', 'Ewaluacja', 'Modele'],
  ft_vision_audio: ['Model bazowy', 'Dane', 'Trening', 'Ewaluacja', 'Modele'],
  tabular_anomaly: ['Dane', 'Trenuj', 'Cechy', 'Anomalie', 'Modele'],
  // Flow destylacji: Model bazowy (student + metoda LoRA) -> Dane (teacher generuje
  // pary Q→A) -> Trening (student uczy sie na tych parach) -> Modele. Teacher
  // wybierany w Dane; osobne zakladki Nauczyciel/Uczen byly zbedne (renderowaly pustke).
  distillation: ['Model bazowy', 'Dane', 'Trening', 'Modele'],
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

// Polish label for a backend status slug, shown on the status pill. Unknown
// values fall back to the raw string so we never hide real backend states.
const STATUS_LABEL = {
  active: 'aktywny',
  ready: 'gotowy',
  done: 'gotowy',
  draft: 'szkic',
  training: 'trening',
  running: 'w toku',
  error: 'błąd',
  failed: 'błąd',
  archived: 'zarchiwizowany',
  paused: 'wstrzymany',
};

function statusLabel(status) {
  const s = String(status || '').toLowerCase();
  if (!s) return '—';
  return STATUS_LABEL[s] || status;
}

const MlStudioScreen = {
  get title() { return 'ML Studio'; },

  render(params = {}) {
    if (params && params.admin === 'resources') {
      return `<div id="ml-studio-resources" class="ml-studio-resources"></div>`;
    }
    if (params && params.jobs) {
      return `<div id="ml-studio-jobs-view" class="ml-studio-jobs-view"></div>`;
    }
    if (params && params.projectId && params.share) {
      return `<div id="ml-studio-share" class="ml-studio-share"></div>`;
    }
    if (params && params.projectId) {
      return `<div id="ml-studio-detail" class="ml-studio-detail"></div>`;
    }
    if (params && params.create) {
      return `<div id="ml-studio-wizard" class="ml-studio-wizard"></div>`;
    }
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('brain')} ML Studio</h1>
          <div class="sub" id="ml-studio-sub">Projekty — jednostki pracy ML (dane, schemat, treningi, modele)</div>
        </div>
        <div class="actions" id="ml-studio-actions">
          <tf-button variant="ghost" icon="refresh" id="ml-studio-refresh">Odśwież</tf-button>
          <tf-button variant="outline" icon="cpu" id="ml-studio-jobs">Joby</tf-button>
          <tf-button variant="primary" icon="plus" id="ml-studio-new">Nowy projekt</tf-button>
        </div>
      </div>

      <tf-filter-chips id="ml-studio-filters" mode="single"></tf-filter-chips>

      <div id="ml-studio-list" class="ml-studio-list"></div>
    `;
  },

  async mount(params = {}) {
    if (params && params.admin === 'resources') {
      await showResourcesAdmin();
      return;
    }
    if (params && params.jobs) {
      await showJobsOverview();
      return;
    }
    if (params && params.projectId && params.share) {
      await showShare(params.projectId);
      return;
    }
    if (params && params.projectId) {
      await showDetail(params.projectId, { runId: params.runId, kind: params.kind });
      return;
    }
    if (params && params.create) {
      await showCreateWizard();
      return;
    }
    byId('ml-studio-refresh')?.addEventListener('click', loadAll);
    byId('ml-studio-new')?.addEventListener('click', () => Router.navigate('ml-studio', { create: '1' }));
    byId('ml-studio-jobs')?.addEventListener('click', () => Router.navigate('ml-studio', { jobs: '1' }));

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
    stopFtPolling();
    stopFtExportPolling();
    stopJobsPolling();
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

// Inicjały do awatara member-chip. Działa dla nazwy wyświetlanej z wielu słów
// ("Anna Kowalska" → "AK") oraz dla identyfikatorów/tokenów typu "user-1a2b".
// Bierzemy pierwsze litery maks. dwóch pierwszych słów, a dla jednego słowa
// (lub UUID bez separatorów) dwa pierwsze znaki.
function initialsFromId(id) {
  const s = String(id || '').trim();
  if (!s) return '?';
  const words = s.split(/\s+/).filter(Boolean);
  if (words.length >= 2) return (words[0][0] + words[1][0]).toUpperCase();
  const parts = s.split(/[-_]+/).filter(Boolean);
  if (parts.length >= 2) return (parts[0][0] + parts[1][0]).toUpperCase();
  return s.slice(0, 2).toUpperCase();
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

// authMe niesie userId jako 16 SUROWYCH BAJTÓW, a członkowie projektu mają
// user_id jako kanoniczny string UUID — bez konwersji porównanie uid === selfId
// nigdy by nie pasowało, więc tu zamieniamy bajty na kanoniczny UUID.
function uuidFromBytes(bytes) {
  const hex = Array.from(bytes, (b) => (b & 0xff).toString(16).padStart(2, '0'));
  return [
    hex.slice(0, 4).join(''),
    hex.slice(4, 6).join(''),
    hex.slice(6, 8).join(''),
    hex.slice(8, 10).join(''),
    hex.slice(10, 16).join(''),
  ].join('-');
}

function currentUserId() {
  if (!currentUser) return '';
  const raw = currentUser.userId ?? currentUser.user_id ?? '';
  if (raw == null) return '';
  if (typeof raw === 'string') return raw.trim();
  let bytes = null;
  if (raw instanceof Uint8Array) {
    bytes = raw;
  } else if (raw instanceof ArrayBuffer) {
    bytes = new Uint8Array(raw);
  } else if (Array.isArray(raw)) {
    bytes = raw;
  }
  if (bytes && bytes.length === 16) return uuidFromBytes(bytes);
  return String(raw);
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
    btn.addEventListener('click', () => Router.navigate('ml-studio', { create: '1' }));
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
  host.querySelector('[data-new-project]')?.addEventListener('click', () => Router.navigate('ml-studio', { create: '1' }));
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
  const trainingCount = p.trainingCount ?? p.training_count ?? 0;
  const updated = formatRelative(p.updatedAt ?? p.updated_at);

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
        <tf-badge tone="${statusTone(p.status)}" value="${escapeAttr(statusLabel(p.status))}"></tf-badge>
      </div>
      ${ownerStrip}
      <p class="ml-studio-card-desc">${escapeHtml(p.description || 'Bez opisu.')}</p>
      <div class="ml-studio-card-stats">
        <div class="ml-studio-stat"><div class="v">${datasetCount}</div><div class="l">datasety</div></div>
        <div class="ml-studio-stat"><div class="v">${modelCount}</div><div class="l">modele</div></div>
        <div class="ml-studio-stat"><div class="v">${trainingCount}</div><div class="l">treningi</div></div>
      </div>
      <div class="ml-studio-card-foot">
        <span class="ml-studio-card-meta">${sprite('clock')} edytowany ${escapeHtml(updated)}</span>
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

// =============================================================================
// Kreator projektu (p01) — 4-krokowy wizard: nazwa/opis → typ → dane → podsumowanie.
// Typ projektu determinuje dalsze kroki WEWNĄTRZ projektu (TYPE_TABS), więc krok 2
// pokazuje edukacyjną mapę "co dalej". Stan żyje lokalnie w domknięciu, dzięki czemu
// re-render kroku zachowuje wpisane wartości.
// =============================================================================

// Typy fine-tuningowe wybierają model bazowy/nauczyciela dopiero w samym projekcie,
// nie w kreatorze — krok 4 dodaje dla nich osobną notkę zamiast udawać wybór modelu.
const FT_TYPES = new Set(['ft_llm', 'ft_vision_audio', 'distillation']);

async function showCreateWizard() {
  const host = byId('ml-studio-wizard');
  if (!host) return;
  host.innerHTML = '<div class="ml-studio-loading"><tf-spinner></tf-spinner></div>';

  await ensureProjectTypes();
  if (!projectTypes.length) {
    host.innerHTML = '';
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', 'alert');
    empty.setAttribute('title', 'Lista typów projektów niedostępna');
    empty.setAttribute('message', 'Nie udało się pobrać typów projektów z Core — wróć do listy i odśwież.');
    const back = document.createElement('tf-button');
    back.setAttribute('variant', 'primary');
    back.textContent = 'Wróć do projektów';
    back.addEventListener('click', () => Router.navigate('ml-studio'));
    empty.appendChild(back);
    host.appendChild(empty);
    return;
  }

  const state = {
    step: 1,
    name: '',
    description: '',
    type: projectTypes[0].slug,
    file: null,
  };

  const STEP_META = [
    { label: 'Nazwa i opis', desc: 'Krok 1' },
    { label: 'Typ projektu', desc: 'Krok 2' },
    { label: 'Dane i źródła', desc: 'Krok 3' },
    { label: 'Schemat / model', desc: 'Krok 4' },
  ];

  host.innerHTML = `
    <div class="ml-studio-detail-top">
      <div class="ml-studio-breadcrumb">
        <span class="ml-studio-crumb">ML Studio</span>
        ${sprite('chevron-right')}
        <span class="ml-studio-crumb current">Nowy projekt</span>
      </div>
    </div>

    <section class="ml-studio-wizard-card">
      <div id="ml-studio-stepper" class="ml-studio-stepper"></div>
      <div id="ml-studio-wizard-body" class="ml-studio-wizard-body"></div>
      <div id="ml-studio-wizard-actions" class="ml-studio-wizard-actions"></div>
    </section>
  `;

  const stepperEl = byId('ml-studio-stepper');
  const bodyEl = byId('ml-studio-wizard-body');
  const actionsEl = byId('ml-studio-wizard-actions');

  // Plan kroków wewnątrz projektu dla wybranego typu — używany i w mapie "co dalej"
  // (krok 2), i w podsumowaniu (krok 4). To realny TYPE_TABS, nie wymyślona lista.
  function typePlan(slug) {
    return TYPE_TABS[slug] || ['Dane', 'Trening', 'Modele'];
  }

  function planFlowHtml(slug) {
    return typePlan(slug)
      .map((s) => `<span class="ml-studio-ns-pill">${escapeHtml(s)}</span>`)
      .join(`<span class="ml-studio-ns-arrow">${sprite('chevron-right')}</span>`);
  }

  function renderStepper() {
    stepperEl.innerHTML = STEP_META.map((m, i) => {
      const n = i + 1;
      const stateClass = n < state.step ? 'ml-studio-step--done'
        : n === state.step ? 'ml-studio-step--active' : '';
      const numInner = n < state.step ? sprite('check') : String(n);
      const lineClass = n < state.step ? 'ml-studio-s-line--done' : '';
      const line = i < STEP_META.length - 1
        ? `<div class="ml-studio-s-line ${lineClass}"></div>` : '';
      return `
        <div class="ml-studio-step ${stateClass}">
          <div class="ml-studio-step-num">${numInner}</div>
          <div class="ml-studio-step-meta">
            <div class="ml-studio-s-label">${escapeHtml(m.label)}</div>
            <div class="ml-studio-s-desc">${escapeHtml(m.desc)}</div>
          </div>
        </div>
        ${line}
      `;
    }).join('');
  }

  // Krok 2 — panel "co dalej" pod kafelkami: aktualizuje się przy zmianie typu.
  function renderNextSteps() {
    const panel = byId('ml-studio-wiz-nextsteps');
    if (!panel) return;
    const lbl = typeLabel(state.type);
    panel.innerHTML = `
      <div class="ml-studio-ns-title">${sprite('info')} Co dalej dla typu „${escapeHtml(lbl)}”</div>
      <div class="ml-studio-ns-flow">${planFlowHtml(state.type)}</div>
      <p class="ml-studio-ns-note">Wybrany typ determinuje kolejne kroki wewnątrz projektu — inny typ prowadzi do innej ścieżki pracy.</p>
    `;
  }

  function renderStep() {
    if (state.step === 1) {
      bodyEl.innerHTML = `
        <div class="ml-studio-form">
          <tf-input id="ml-studio-wiz-name" label="Nazwa projektu" required
            placeholder="np. Rozpoznawanie znaków ADR" value="${escapeAttr(state.name)}"></tf-input>
          <tf-textarea id="ml-studio-wiz-desc" label="Opis" rows="3"
            placeholder="Krótko: cel projektu i dane wejściowe." value="${escapeAttr(state.description)}"></tf-textarea>
        </div>
      `;
      byId('ml-studio-wiz-name')?.addEventListener('input', (e) => { state.name = e.target.value; });
      byId('ml-studio-wiz-desc')?.addEventListener('input', (e) => { state.description = e.target.value; });
      return;
    }

    if (state.step === 2) {
      bodyEl.innerHTML = `
        <div class="ml-studio-type-field">
          <tf-radio-group id="ml-studio-wiz-types" name="ml-studio-wiz-type" cards
            value="${escapeAttr(state.type)}">
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
        <div id="ml-studio-wiz-nextsteps" class="ml-studio-next-steps"></div>
      `;
      // tf-radio-group emituje `change` z detail.value (tf-radio.js).
      byId('ml-studio-wiz-types')?.addEventListener('change', (e) => {
        state.type = e.detail?.value || state.type;
        renderNextSteps();
      });
      renderNextSteps();
      return;
    }

    if (state.step === 3) {
      const hasFile = !!state.file;
      bodyEl.innerHTML = `
        <div class="ml-studio-wiz-data">
          <tf-file-input id="ml-studio-wiz-file" accept=".csv,.xlsx"
            label="Przeciągnij plik lub kliknij"></tf-file-input>
          <div id="ml-studio-wiz-file-info" class="ml-studio-wiz-file-info" ${hasFile ? '' : 'hidden'}></div>
          <div class="ml-studio-wiz-sources">
            <tf-chip status="ok" icon="check" label="CSV"></tf-chip>
            <tf-chip status="ok" icon="check" label="XLSX"></tf-chip>
            <tf-chip status="info" label="Baza danych · wkrótce" class="ml-studio-wiz-source-soon"></tf-chip>
            <tf-chip status="info" label="API · wkrótce" class="ml-studio-wiz-source-soon"></tf-chip>
          </div>
          <p class="ml-studio-wiz-hint">${sprite('info')} Dane możesz też dodać później w zakładce Dane — ten krok jest opcjonalny.</p>
        </div>
      `;
      const fileInfo = byId('ml-studio-wiz-file-info');
      const renderFileInfo = () => {
        if (!state.file) { fileInfo.hidden = true; fileInfo.innerHTML = ''; return; }
        fileInfo.hidden = false;
        fileInfo.innerHTML = `
          <span class="ml-studio-wiz-file-name">${sprite('database')} ${escapeHtml(state.file.name)}</span>
          <span class="ml-studio-wiz-file-size">${formatFileSize(state.file.size)}</span>
          <tf-button variant="ghost" size="sm" icon="trash" id="ml-studio-wiz-file-remove">Usuń</tf-button>
        `;
        byId('ml-studio-wiz-file-remove')?.addEventListener('click', () => {
          state.file = null;
          renderFileInfo();
        });
      };
      // tf-file-input emituje `change` z detail.files (FileList) — tf-file-input.js.
      byId('ml-studio-wiz-file')?.addEventListener('change', (e) => {
        const files = e.detail?.files;
        state.file = files && files.length ? files[0] : null;
        renderFileInfo();
      });
      renderFileInfo();
      return;
    }

    // Krok 4 — podsumowanie. Zero fałszywych kontrolek: schemat kolumn wykrywa się
    // dopiero z pliku po utworzeniu projektu (zakładka Dane), więc piszemy to wprost.
    const dataStatus = state.file
      ? `plik „${escapeHtml(state.file.name)}” (${formatFileSize(state.file.size)}) zostanie wgrany i sprofilowany po utworzeniu projektu`
      : 'dane dodasz później w zakładce Dane';
    const ftNote = FT_TYPES.has(state.type)
      ? `<div class="ml-studio-wiz-summary-note">${sprite('info')} ${
          state.type === 'distillation'
            ? 'Model bazowy studenta wybierzesz w zakładce „Model bazowy”, a nauczyciela (generuje odpowiedzi) w zakładce „Dane”.'
            : 'Model bazowy wybierzesz już w projekcie — w zakładce „Model bazowy”.'
        }</div>`
      : '';
    bodyEl.innerHTML = `
      <div class="ml-studio-wiz-summary">
        <div class="ml-studio-wiz-summary-title">${sprite('check')} Co otrzymasz</div>
        <dl class="ml-studio-wiz-summary-list">
          <dt>Nazwa projektu</dt>
          <dd>${escapeHtml(state.name || '—')}</dd>
          <dt>Typ projektu</dt>
          <dd><tf-badge tone="accent" value="${escapeAttr(typeLabel(state.type))}"></tf-badge></dd>
          <dt>Plan kroków</dt>
          <dd><div class="ml-studio-ns-flow">${planFlowHtml(state.type)}</div></dd>
          <dt>Dane</dt>
          <dd>${dataStatus}</dd>
        </dl>
        ${ftNote}
        <p class="ml-studio-wiz-hint">${sprite('info')} Schemat kolumn jest wykrywany z pliku po utworzeniu projektu (w zakładce Dane) — nic nie jest predefiniowane na tym etapie.</p>
      </div>
    `;
  }

  function renderActions() {
    const isFirst = state.step === 1;
    const isLast = state.step === 4;
    actionsEl.innerHTML = `
      <tf-button variant="ghost" id="ml-studio-wiz-cancel">Anuluj</tf-button>
      <div class="ml-studio-wiz-actions-right">
        <tf-button variant="ghost" icon="chevron-left" id="ml-studio-wiz-back" ${isFirst ? 'disabled' : ''}>Wstecz</tf-button>
        ${isLast
          ? `<tf-button variant="primary" icon="check" id="ml-studio-wiz-create">Utwórz projekt</tf-button>`
          : `<tf-button variant="primary" trailing-icon="chevron-right" id="ml-studio-wiz-next">Dalej</tf-button>`}
      </div>
    `;
    byId('ml-studio-wiz-cancel')?.addEventListener('click', () => Router.navigate('ml-studio'));
    byId('ml-studio-wiz-back')?.addEventListener('click', () => {
      if (state.step > 1) { state.step -= 1; renderAll(); }
    });
    byId('ml-studio-wiz-next')?.addEventListener('click', () => {
      if (state.step === 1 && !state.name.trim()) {
        toast('Podaj nazwę projektu.', 'error');
        return;
      }
      state.step += 1;
      renderAll();
    });
    byId('ml-studio-wiz-create')?.addEventListener('click', createFromWizard);
  }

  async function createFromWizard() {
    const createBtn = byId('ml-studio-wiz-create');
    if (!state.name.trim()) {
      state.step = 1;
      renderAll();
      toast('Podaj nazwę projektu.', 'error');
      return;
    }
    createBtn?.setAttribute('loading', '');
    createBtn?.setAttribute('disabled', '');
    try {
      const resp = await ApiBinary.one('mlStudioProjectCreateRequest', {
        name: state.name.trim(),
        description: state.description.trim(),
        projectType: state.type,
      });
      const created = resp.project || {};
      const newId = created.projectId ?? created.project_id;
      if (state.file && newId) {
        await uploadDataset(newId, state.file);
      }
      toast('Projekt utworzony', 'success');
      if (newId) Router.navigate('ml-studio', { projectId: newId });
      else Router.navigate('ml-studio');
    } catch (err) {
      createBtn?.removeAttribute('loading');
      createBtn?.removeAttribute('disabled');
      toast(`Tworzenie projektu: ${err.message}`, 'error');
    }
  }

  function renderAll() {
    renderStepper();
    renderStep();
    renderActions();
  }

  renderAll();
}

function formatFileSize(bytes) {
  const n = Number(bytes);
  if (!Number.isFinite(n) || n <= 0) return '—';
  if (n >= 1024 * 1024) return `${(n / (1024 * 1024)).toLocaleString('pl-PL', { maximumFractionDigits: 1 })} MB`;
  if (n >= 1024) return `${(n / 1024).toLocaleString('pl-PL', { maximumFractionDigits: 1 })} KB`;
  return `${formatNumber(n)} B`;
}

async function showDetail(projectId, focus = {}) {
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
    renderDetail(host, p, focus);
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

function renderDetail(host, p, focus = {}) {
  const slug = p.projectType ?? p.project_type ?? '';
  // Wejście „w szczegóły joba" z panelu Joby: przekazany runId ma otworzyć
  // zakładkę Trening i wznowić dla niego widok LIVE (zamiast formularza startu).
  // Konsumowane jednorazowo — kolejne ręczne wejścia w Trening pokażą już setup.
  const focusRunId = focus && focus.runId ? String(focus.runId) : '';
  const focusKind = focus && focus.kind ? String(focus.kind) : '';
  let pendingFocusRunId = focusRunId;
  // "Przegląd" jest zawsze pierwszą zakładką (stan projektu na jednym ekranie),
  // a "Zasoby" zawsze ostatnią (§11.3 — zasoby mesh przydzielone projektowi).
  // Żaden wpis TYPE_TABS nie zawiera "Przegląd", więc bez duplikatów.
  const tabs = ['Przegląd', ...(TYPE_TABS[slug] || ['Dane', 'Treningi', 'Modele']), 'Zasoby'];
  const recognition = slug === 'recognition';

  // Per-tab indices used for tab counts (set once recog metrics resolve) and
  // for the header action buttons that jump straight into a sibling tab.
  const tabIndex = (label) => tabs.indexOf(label);

  // Header badges: project type, created-at provenance, schema size and mesh
  // sync state. Synced is signalled by the backend `synced`/`isShared` flag.
  const createdRel = formatRelative(p.createdAt ?? p.created_at);
  const synced = p.synced === true || p.is_synced === true;
  const headerBadges = [
    `<tf-chip status="accent" icon="${escapeAttr(typeIcon(slug))}" label="typ: ${escapeAttr(typeLabel(slug))}"></tf-chip>`,
    createdRel !== '—' ? `<tf-chip icon="clock" label="utworzony ${escapeAttr(createdRel)}"></tf-chip>` : '',
    `<tf-chip status="info" icon="grid-2x2" label="schemat: —"></tf-chip>`,
    synced ? `<tf-chip status="ok" icon="network" label="zsynchronizowano z mesh"></tf-chip>` : '',
  ].join('');

  // Recognition gets data-centric header actions (add photos → Dane, annotate →
  // Anotacje); other types keep the owner-only access action.
  const headerActions = recognition
    ? `<span slot="actions">
         <tf-button variant="outline" icon="plus" id="ml-studio-hdr-data">Dodaj zdjęcia</tf-button>
         <tf-button variant="primary" icon="edit" id="ml-studio-hdr-annot">Anotuj</tf-button>
       </span>`
    : (isOwnerProject(p)
      ? `<span slot="actions"><tf-button variant="outline" icon="share" id="ml-studio-manage-access">Zarządzaj dostępem</tf-button></span>`
      : '');

  host.innerHTML = `
    <div class="ml-studio-detail-top">
      <tf-button variant="ghost" icon="chevron-left" id="ml-studio-back">Projekty</tf-button>
    </div>

    <tf-detail-header
      title="${escapeAttr(p.name || '(bez nazwy)')}"
      subtitle="${escapeAttr(p.description || typeLabel(slug))}"
      icon="${escapeAttr(typeIcon(slug))}">
      <span slot="badges" id="ml-studio-hdr-badges">
        <tf-badge tone="${statusTone(p.status)}" value="${escapeAttr(statusLabel(p.status))}"></tf-badge>
        ${headerBadges}
      </span>
      ${headerActions}
    </tf-detail-header>

    <tf-tabs id="ml-studio-tabs" value="ml-tab-0">
      ${tabs.map((t, i) => `<tf-tab id="ml-tab-${i}" label="${escapeAttr(t)}"></tf-tab>`).join('')}
    </tf-tabs>

    <div id="ml-studio-tab-panel" class="ml-studio-tab-panel"></div>
  `;

  byId('ml-studio-back')?.addEventListener('click', () => Router.navigate('ml-studio'));
  byId('ml-studio-manage-access')?.addEventListener('click', () => {
    Router.navigate('ml-studio', { projectId: projectId(p), share: true });
  });
  byId('ml-studio-hdr-data')?.addEventListener('click', () => selectTab('Dane'));
  byId('ml-studio-hdr-annot')?.addEventListener('click', () => selectTab('Anotacje'));

  // Recognition tab counts + header schema badge come from the COCO dataset, so
  // they are resolved once asynchronously and patched into the already-rendered
  // header/tabs (degrades silently when unavailable).
  if (recognition) {
    fetchRecogStats(projectId(p)).then((stats) => {
      if (!stats) return;
      const setCount = (label, value) => {
        const i = tabIndex(label);
        if (i >= 0 && value != null) byId('ml-tab-' + i)?.setAttribute('count', String(value));
      };
      setCount('Dane', stats.images);
      if (stats.images) setCount('Anotacje', `${stats.annotated}/${stats.images}`);
      const schemaBadge = byId('ml-studio-hdr-badges')?.querySelector('tf-chip[icon="grid-2x2"]');
      if (schemaBadge && stats.classes != null) {
        schemaBadge.setAttribute('label', `schemat: ${stats.classes} ${plural(stats.classes, 'klasa', 'klasy', 'klas')}`);
      }
    });
  }

  const tabsEl = byId('ml-studio-tabs');
  // Pozwala skrótom z zakładki "Przegląd" przełączać aktywną zakładkę:
  // tf-tabs przyjmuje value="ml-tab-N" i sam wyemituje `change`, ale tutaj
  // wołamy renderPanel wprost, bo ustawienie value nie zawsze re-emituje event.
  const selectTab = (label) => {
    const i = tabs.indexOf(label);
    if (i >= 0) {
      const el = byId('ml-studio-tabs');
      if (el) el.value = 'ml-tab-' + i;
      renderPanel('ml-tab-' + i);
    }
  };
  const renderPanel = (tabId) => {
    const panel = byId('ml-studio-tab-panel');
    if (!panel) return;
    // Każde przełączenie zakładki zatrzymuje polling treningu FT oraz eksportu
    // GGUF — żaden interwał nie przeżywa wyjścia z zakładki.
    stopFtPolling();
    stopFtExportPolling();
    const idx = Number(String(tabId ?? '').replace('ml-tab-', ''));
    const label = tabs[Number.isNaN(idx) ? 0 : idx] || tabs[0];
    if (label === 'Przegląd') {
      renderOverviewTab(panel, p, { tabs, selectTab });
      return;
    }
    if (label === 'Model bazowy' && (slug === 'ft_llm' || slug === 'distillation')) {
      renderFtModelTab(panel, p);
      return;
    }
    if (label === 'Schemat' && slug === 'recognition') {
      renderRecogSchemaTab(panel, p, { selectTab });
      return;
    }
    if (label === 'Trening' && slug === 'recognition') {
      const opts = { selectTab };
      if (pendingFocusRunId) {
        opts.focusRunId = pendingFocusRunId;
        opts.focusKind = focusKind;
        pendingFocusRunId = '';
      }
      renderRecogTrainTab(panel, p, opts);
      return;
    }
    if (label === 'Dane' && slug === 'recognition') {
      renderRecogDataTab(panel, p);
      return;
    }
    if (label === 'Anotacje' && slug === 'recognition') {
      renderRecogAnnotateTab(panel, p);
      return;
    }
    if (label === 'Dane' && slug === 'distillation') {
      renderDistillDataTab(panel, p);
      return;
    }
    if (label === 'Dane') {
      renderDataTab(panel, projectId(p));
      return;
    }
    if (label === 'Trening' && (slug === 'ft_llm' || slug === 'distillation')) {
      renderFtTrainTab(panel, p, { selectTab });
      return;
    }
    if (label === 'Trenuj') {
      renderTrainTab(panel, projectId(p));
      return;
    }
    if (label === 'Modele') {
      renderModelsTab(panel, p);
      return;
    }
    if (label === 'Treningi') {
      renderRunsTab(panel, p);
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
  // Domyślnie „Przegląd"; przy wejściu w szczegóły joba — od razu „Trening"
  // (wznowienie widoku LIVE dla przekazanego runId).
  const initialTab = (focusRunId && tabs.indexOf('Trening') >= 0) ? 'Trening' : 'Przegląd';
  if (initialTab !== 'Przegląd') selectTab(initialTab);
  else renderPanel('ml-tab-0');
}

// =============================================================================
// Zakładka "Przegląd" — stan projektu na jednym ekranie: KPI z pochodzeniem,
// członkowie, przydzielone zasoby mesh, ostatnie joby, modele projektu i skróty
// do pozostałych zakładek. Każde wywołanie API jest izolowane w try/catch, więc
// niedostępny endpoint degraduje do pustej sekcji zamiast wywalać całą zakładkę.
// =============================================================================

// Status joba/modelu → para {tone, label} dla tf-badge. Reużywa istniejących
// helperów statusTone/statusLabel, ale domyka kilka slugów specyficznych dla
// jobów treningowych (succeeded/finished), których nie pokrywa STATUS_LABEL.
function runBadge(status) {
  const s = String(status || '').toLowerCase();
  if (s === 'succeeded' || s === 'finished' || s === 'completed') return { tone: 'success', label: 'gotowy' };
  return { tone: statusTone(s), label: statusLabel(s) };
}

// Parse a metrics-bearing JSON blob (string or object) into a plain object,
// tolerating the snake/camel field pair. Returns {} when unparseable.
function parseMetrics(raw) {
  if (!raw) return {};
  let m = raw;
  if (typeof m === 'string') { try { m = JSON.parse(m); } catch (_) { return {}; } }
  return (m && typeof m === 'object') ? m : {};
}

function metricNum(m, ...keys) {
  for (const k of keys) {
    const v = m[k];
    if (v != null && Number.isFinite(Number(v))) return Number(v);
  }
  return null;
}

// Progress + headline result for a training run row. `done` drives the green
// progress style; `metric` (if present) becomes the success chip text. Falls
// back to a 0/100 % bar derived only from the run status when no numbers exist.
function runProgressMeta(r) {
  const m = parseMetrics(r.metricsJson ?? r.metrics_json);
  const status = String(r.status ?? '').toLowerCase();
  const terminal = status === 'succeeded' || status === 'finished' || status === 'completed';
  let pct = metricNum({ ...r, ...m }, 'progress', 'progressPct', 'progress_pct');
  if (pct != null && pct <= 1) pct = Math.round(pct * 100);
  if (pct == null) pct = terminal ? 100 : (status === 'failed' || status === 'error' ? 0 : 0);
  pct = Math.max(0, Math.min(100, Math.round(pct)));
  const map50 = metricNum(m, 'map50', 'mAP50', 'map_50', 'mAP@50');
  let metric = null;
  if (terminal && map50 != null) metric = `mAP@50 ${map50.toFixed(3)}`;
  else if (terminal) {
    const summary = modelMetricsSummary(m);
    if (summary) metric = summary;
  }
  return { pct, done: terminal, metric };
}

// Metric tiles for a project model card (mAP@50 / mAP@50-95 / klasy). Missing
// values render as an em-dash so the tile layout stays stable.
function modelMetricTiles(model) {
  const m = parseMetrics(model.metricsJson ?? model.metrics_json);
  const fmt = (v) => (v == null ? '—' : v.toFixed(3));
  const map50 = metricNum(m, 'map50', 'mAP50', 'map_50', 'mAP@50');
  const map5095 = metricNum(m, 'map5095', 'map50_95', 'map_50_95', 'mAP50-95', 'mAP@50-95');
  const classes = metricNum(m, 'num_classes', 'numClasses', 'classes');
  return [
    { val: fmt(map50), lbl: 'mAP@50' },
    { val: fmt(map5095), lbl: 'mAP@50-95' },
    { val: classes == null ? '—' : String(classes), lbl: 'klasy' },
  ];
}

// Recognition project stats from the COCO dataset: image count, class count
// (background id 0 excluded, mirroring the annotate tab) and annotated-image
// count (images with at least one annotation). Returns null on any failure so
// callers degrade to "—" instead of throwing.
async function fetchRecogStats(pid) {
  try {
    const dsResp = await ApiBinary.one('mlStudioDatasetsListRequest', { projectId: pid });
    const datasets = (Array.isArray(dsResp.datasets) ? dsResp.datasets : [])
      .filter((d) => (d.kind ?? '') === 'coco_path');
    if (!datasets.length) return { images: 0, classes: 0, annotated: 0 };
    const datasetId = datasets[0].datasetId ?? datasets[0].dataset_id;
    const imgResp = await ApiBinary.one('mlStudioRecogImagesListRequest', { datasetId });
    const images = JSON.parse(imgResp.imagesJson ?? imgResp.images_json ?? '[]');
    const categories = JSON.parse(imgResp.categoriesJson ?? imgResp.categories_json ?? '[]');
    const classes = categories.filter((c) => c.id !== 0).length || categories.length;
    const annotated = images.filter((im) => Number(im.ann_count ?? 0) > 0).length;
    return { images: images.length, classes, annotated };
  } catch (_) {
    return null;
  }
}

async function renderOverviewTab(panel, p, { tabs, selectTab }) {
  const pid = projectId(p);
  panel.innerHTML = '<div class="ml-studio-loading"><tf-spinner></tf-spinner></div>';

  // KPI domenowe z payloadu projektu — liczba członków uzupełniana po pobraniu.
  const datasetCount = p.datasetCount ?? p.dataset_count ?? 0;
  const modelCount = p.modelCount ?? p.model_count ?? 0;
  const trainingCount = p.trainingCount ?? p.training_count ?? 0;

  // Członkowie — potrzebni także do KPI "Członkowie", więc pobieramy najpierw.
  let members = [];
  try {
    const resp = await ApiBinary.one('mlStudioProjectMembersListRequest', { projectId: pid });
    members = Array.isArray(resp.members) ? resp.members : [];
  } catch (_) {
    members = [];
  }

  const owner = isOwnerProject(p);
  const selfId = currentUserId();
  const shareNav = () => Router.navigate('ml-studio', { projectId: pid, share: true });
  const adminNav = () => Router.navigate('ml-studio', { admin: 'resources' });

  const slug = p.projectType ?? p.project_type ?? '';
  const recognition = slug === 'recognition';

  // value can carry an inline <span class="small"> suffix (e.g. "42%"), so it is
  // injected as raw HTML — callers must pre-escape any dynamic text.
  const kpi = (icon, label, valueHtml, deltaText, deltaClass = '') => `
    <div class="ml-studio-kpi">
      <div class="label">${sprite(icon)}${escapeHtml(label)}</div>
      <div class="value">${valueHtml}</div>
      <div class="delta${deltaClass ? ' ' + deltaClass : ''}">${escapeHtml(deltaText)}</div>
    </div>`;

  // Recognition KPI (Zdjęcia/Klasy/Oznaczone/Modele) need the COCO dataset, so
  // they start as placeholders and are patched once fetchRecogStats resolves.
  const kpiGrid = recognition
    ? `<div class="ml-studio-kpi-grid">
        ${kpi('image', 'Zdjęcia', '<span id="ml-studio-kpi-images">—</span>', 'z zakładki Dane')}
        ${kpi('grid-2x2', 'Klasy', '<span id="ml-studio-kpi-classes">—</span>', 'ze schematu projektu')}
        ${kpi('chart-line', 'Oznaczone', '<span id="ml-studio-kpi-annot">—</span>', 'wczytywanie…', 'up')}
        ${kpi('catalog', 'Modele', escapeHtml(String(modelCount)), 'wersje wytrenowane w projekcie')}
      </div>`
    : `<div class="ml-studio-kpi-grid">
        ${kpi('image', 'Datasety', escapeHtml(String(datasetCount)), 'z zakładki Dane')}
        ${kpi('catalog', 'Modele', escapeHtml(String(modelCount)), 'wytrenowane wersje w projekcie')}
        ${kpi('brain', 'Treningi', escapeHtml(String(trainingCount)), 'uruchomione joby treningowe')}
        ${kpi('users', 'Członkowie', escapeHtml(String(members.length)), 'właściciel + osoby z dostępem')}
      </div>`;

  // Mini-lista członków: awatar z inicjałów, nazwa = displayName (fallback userId), rola jako tf-chip.
  const memberChips = members.map((m) => {
    const uid = String(m.userId ?? m.user_id ?? '');
    const name = m.displayName ?? m.display_name ?? uid;
    const role = String(m.role ?? '').toLowerCase();
    const isSelf = (selfId && uid === selfId) || role === 'owner' && !selfId;
    const tone = role === 'owner' ? 'accent' : 'info';
    return `
      <div class="ml-studio-member-chip">
        <div class="m-av">${escapeHtml(initialsFromId(name))}</div>
        <div>
          <div class="m-nm">${escapeHtml(name || '—')}${isSelf ? ' <span class="ml-studio-member-self">(Ty)</span>' : ''}</div>
          <div class="m-rl">${escapeHtml(formatRelative(m.createdAt ?? m.created_at))}</div>
        </div>
        <tf-chip status="${tone}" icon="${escapeAttr(ROLE_ICON[role] || 'eye')}" label="${escapeAttr(roleLabel(role))}"></tf-chip>
      </div>`;
  }).join('');

  const membersSection = `
    <div class="ml-studio-section-card">
      <div class="ml-studio-section-card-head">
        <div class="title">${sprite('users')} Członkowie projektu <span class="ml-studio-section-sub">— właściciel i osoby z dostępem</span></div>
        ${owner ? '<tf-button variant="ghost" icon="share" id="ml-studio-ov-share">Zarządzaj dostępem</tf-button>' : ''}
      </div>
      <div class="ml-studio-members-row" id="ml-studio-ov-members">
        ${memberChips || '<span class="ml-studio-section-sub">Brak członków do wyświetlenia.</span>'}
      </div>
    </div>`;

  panel.innerHTML = `
    ${kpiGrid}
    ${membersSection}
    <div class="ml-studio-section-card" id="ml-studio-ov-resources-card">
      <div class="ml-studio-section-card-head">
        <div class="title">${sprite('host')} Zasoby przydzielone <span class="ml-studio-section-sub">— GPU/nody dostępne dla treningu</span></div>
        <tf-button variant="ghost" icon="host" id="ml-studio-ov-resources-admin">Zarządzaj (admin)</tf-button>
      </div>
      <div id="ml-studio-ov-resources"></div>
    </div>
    <div class="ml-studio-section-card" id="ml-studio-ov-runs-card">
      <div class="ml-studio-section-card-head">
        <div class="title">${sprite('clock')} Ostatnie joby <span class="ml-studio-section-sub">— tylko tego projektu</span></div>
      </div>
      <div id="ml-studio-ov-runs"></div>
    </div>
    <div class="ml-studio-section-card" id="ml-studio-ov-models-card">
      <div class="ml-studio-section-card-head">
        <div class="title">${sprite('catalog')} Modele projektu <span class="ml-studio-section-sub">— wersje wyprodukowane tutaj</span></div>
      </div>
      <div id="ml-studio-ov-models"></div>
    </div>
    <div class="ml-studio-section-card">
      <div class="ml-studio-section-card-head">
        <div class="title">${sprite('play')} Szybkie skróty</div>
      </div>
      <div class="ml-studio-shortcut-grid" id="ml-studio-ov-shortcuts"></div>
    </div>
  `;

  // Akcje członków (owner): zarządzanie dostępem + "Zaproś" — oba na ekran share.
  byId('ml-studio-ov-share')?.addEventListener('click', shareNav);
  if (owner) {
    const inviteBtn = document.createElement('tf-button');
    inviteBtn.setAttribute('variant', 'outline');
    inviteBtn.setAttribute('icon', 'plus');
    inviteBtn.textContent = 'Zaproś';
    inviteBtn.addEventListener('click', shareNav);
    byId('ml-studio-ov-members')?.appendChild(inviteBtn);
  }
  byId('ml-studio-ov-resources-admin')?.addEventListener('click', adminNav);

  // Recognition KPI patch — fill image/class counts and the annotated-% tile
  // once the COCO dataset stats resolve (placeholders stay "—" on failure).
  if (recognition) {
    fetchRecogStats(pid).then((stats) => {
      if (!stats) return;
      const imagesEl = byId('ml-studio-kpi-images');
      const classesEl = byId('ml-studio-kpi-classes');
      const annotEl = byId('ml-studio-kpi-annot');
      if (imagesEl) imagesEl.textContent = String(stats.images);
      if (classesEl) classesEl.textContent = String(stats.classes);
      if (annotEl) {
        const pct = stats.images ? Math.round((stats.annotated / stats.images) * 100) : 0;
        annotEl.innerHTML = `${pct}<span class="small">%</span>`;
        const deltaEl = annotEl.closest('.ml-studio-kpi')?.querySelector('.delta');
        if (deltaEl) deltaEl.textContent = `${stats.annotated} / ${stats.images} zdjęć z anotacji`;
      }
    });
  }

  // Zasoby przydzielone — istniejący endpoint member-dostępny.
  try {
    const resp = await ApiBinary.one('mlStudioProjectResourcesRequest', { projectId: pid });
    const grants = Array.isArray(resp.grants) ? resp.grants : [];
    const host = byId('ml-studio-ov-resources');
    if (host) {
      if (!grants.length) {
        const empty = document.createElement('tf-empty-state');
        empty.setAttribute('icon', 'host');
        empty.setAttribute('title', 'Brak przydzielonych zasobów mesh');
        empty.setAttribute('message', 'Ten projekt nie ma jeszcze przydzielonych zasobów. Przydziela je administrator z puli mesh.');
        const btn = document.createElement('tf-button');
        btn.setAttribute('variant', 'primary');
        btn.setAttribute('icon', 'host');
        btn.textContent = 'Przejdź do Zasobów';
        btn.addEventListener('click', adminNav);
        empty.appendChild(btn);
        host.appendChild(empty);
      } else {
        const table = document.createElement('tf-table');
        table.setAttribute('variant', 'lined');
        table.innerHTML = `
          <tf-column key="resource" label="Zasób"></tf-column>
          <tf-column key="node" label="Node"></tf-column>
          <tf-column key="quota" label="Limit"></tf-column>
          <tf-column key="status" label="Status" renderer="html"></tf-column>
        `;
        table.rows = grants.map((g) => {
          const resourceRef = String(g.resourceRef ?? g.resource_ref ?? '');
          const resourceKind = String(g.resourceKind ?? g.resource_kind ?? '');
          const nid = String(g.nodeId ?? g.node_id ?? '');
          return {
            resource: resourceRef || resourceKind || '—',
            node: g.hostname || (nid ? nid.slice(0, 12) : '—'),
            quota: g.quota ? String(g.quota) : '—',
            status: '<tf-badge tone="success" value="przydzielone"></tf-badge>',
          };
        });
        host.appendChild(table);
      }
    }
  } catch (_) {
    const host = byId('ml-studio-ov-resources');
    if (host) {
      const empty = document.createElement('tf-empty-state');
      empty.setAttribute('icon', 'host');
      empty.setAttribute('title', 'Brak przydzielonych zasobów mesh');
      empty.setAttribute('message', 'Nie udało się odczytać przydziałów zasobów dla tego projektu.');
      host.appendChild(empty);
    }
  }

  // Ostatnie joby treningowe — max 5 najnowszych.
  try {
    const resp = await ApiBinary.one('mlStudioTrainingRunsListRequest', { projectId: pid });
    const runs = Array.isArray(resp.runs) ? resp.runs : [];
    const host = byId('ml-studio-ov-runs');
    if (host) {
      if (!runs.length) {
        const empty = document.createElement('tf-empty-state');
        empty.setAttribute('icon', 'brain');
        empty.setAttribute('title', 'Brak treningów');
        empty.setAttribute('message', 'Uruchom trening w zakładce Trenuj/Trening — joby tego projektu pojawią się tutaj.');
        host.appendChild(empty);
      } else {
        const table = document.createElement('tf-table');
        table.setAttribute('variant', 'lined');
        table.innerHTML = `
          <tf-column key="job" label="Job" renderer="html"></tf-column>
          <tf-column key="progress" label="Postęp" renderer="html"></tf-column>
          <tf-column key="result" label="Wynik" renderer="html"></tf-column>
          <tf-column key="time" label="Czas"></tf-column>
        `;
        table.rows = runs.slice(0, 5).map((r) => {
          const runId = String(r.runId ?? r.run_id ?? '');
          const b = runBadge(r.status);
          const meta = runProgressMeta(r);
          return {
            job: `<span class="ml-studio-mono">${escapeHtml(runId.slice(0, 8) || '—')}</span>`,
            progress: `<div class="ml-studio-progress-cell"><div class="ml-studio-progress${meta.done ? ' done' : ''}"><span style="width:${meta.pct}%"></span></div><span class="pct">${meta.pct}%</span></div>`,
            result: meta.metric != null
              ? `<tf-chip status="ok" icon="check" label="${escapeAttr(meta.metric)}"></tf-chip>`
              : `<tf-badge tone="${b.tone}" value="${escapeAttr(b.label)}"></tf-badge>`,
            time: formatRelative(r.finishedAt ?? r.finished_at ?? r.startedAt ?? r.started_at),
          };
        });
        host.appendChild(table);
      }
    }
  } catch (_) {
    const host = byId('ml-studio-ov-runs');
    if (host) {
      const empty = document.createElement('tf-empty-state');
      empty.setAttribute('icon', 'brain');
      empty.setAttribute('title', 'Brak treningów');
      empty.setAttribute('message', 'Uruchom trening w zakładce Trenuj/Trening — joby tego projektu pojawią się tutaj.');
      host.appendChild(empty);
    }
  }

  // Modele projektu.
  try {
    const resp = await ApiBinary.one('mlStudioModelsListRequest', { projectId: pid });
    const models = Array.isArray(resp.models) ? resp.models : [];
    const host = byId('ml-studio-ov-models');
    if (host) {
      if (!models.length) {
        const empty = document.createElement('tf-empty-state');
        empty.setAttribute('icon', 'catalog');
        empty.setAttribute('title', 'Brak modeli');
        empty.setAttribute('message', 'Modele pojawią się tutaj po udanym treningu.');
        host.appendChild(empty);
      } else {
        // Model cards: icon + name + status chip + framework/base sub + a row of
        // metric tiles (mAP@50 / mAP@50-95 / klasy) + an origin provenance line.
        const grid = document.createElement('div');
        grid.className = 'ml-studio-model-card-grid';
        grid.innerHTML = models.map((m) => {
          const b = runBadge(m.status);
          const name = String(m.name ?? m.modelId ?? m.model_id ?? '—');
          const framework = String(m.framework ?? '') || '—';
          const base = String(m.baseModel ?? m.base_model ?? '').trim();
          const sub = base ? `${framework} · ${base}` : framework;
          const tiles = modelMetricTiles(m).map((t) =>
            `<div class="mc-metric"><div class="mm-val">${escapeHtml(t.val)}</div><div class="mm-lbl">${escapeHtml(t.lbl)}</div></div>`).join('');
          return `
            <div class="ml-studio-model-card">
              <div class="mc-head">
                <div class="mc-ico">${sprite('model')}</div>
                <div>
                  <div class="mc-name">${escapeHtml(name)} <tf-badge tone="${b.tone}" value="${escapeAttr(b.label)}"></tf-badge></div>
                  <div class="mc-sub">${escapeHtml(sub)}</div>
                </div>
              </div>
              <div class="mc-metrics">${tiles}</div>
              <div class="ml-studio-origin">${sprite('external-link')} Rejestr modeli · źródło: ten projekt</div>
            </div>`;
        }).join('');
        host.appendChild(grid);
      }
    }
  } catch (_) {
    const host = byId('ml-studio-ov-models');
    if (host) {
      const empty = document.createElement('tf-empty-state');
      empty.setAttribute('icon', 'catalog');
      empty.setAttribute('title', 'Brak modeli');
      empty.setAttribute('message', 'Modele pojawią się tutaj po udanym treningu.');
      host.appendChild(empty);
    }
  }

  // Szybkie skróty do pozostałych zakładek (bez "Przegląd"). Ikona + opis skrótu.
  const shortcutsHost = byId('ml-studio-ov-shortcuts');
  if (shortcutsHost) {
    const shortcutIcon = (label) => {
      if (label === 'Dane') return 'image';
      if (label === 'Schemat') return 'grid-2x2';
      if (label === 'Anotacje') return 'edit';
      if (label === 'Zasoby') return 'host';
      if (label === 'Treningi' || label === 'Trening' || label === 'Trenuj') return 'brain';
      if (label === 'Modele') return 'catalog';
      if (label === 'Ewaluacja') return 'check';
      return typeIcon(slug);
    };
    const shortcutDesc = (label) => {
      if (label === 'Dane') {
        return slug === 'distillation' ? 'generowanie par Q→A (teacher) / import' : 'import i profil danych';
      }
      if (label === 'Schemat') return 'klasy i atrybuty';
      if (label === 'Anotacje') return 'studio anotacji';
      if (label === 'Zasoby') return 'GPU/nody mesh projektu';
      if (label === 'Treningi' || label === 'Trening') return 'uruchom i śledź treningi';
      if (label === 'Trenuj') return 'uruchom nowy trening';
      if (label === 'Modele') return 'wytrenowane wersje';
      if (label === 'Ewaluacja') return 'metryki i porównania';
      if (label === 'Model bazowy') return 'wybór modelu i metody';
      return 'przejdź do zakładki';
    };
    tabs.filter((t) => t !== 'Przegląd').forEach((label) => {
      // Kafel-skrót jest blokiem nawigacyjnym (jak <a> w mockupie), nie prymitywem
      // UI — daje pełną kontrolę nad układem ikona+tytuł. Dostępność: role=button
      // + obsługa klawiatury (Enter/Spacja), bo to element interaktywny.
      const card = document.createElement('div');
      card.className = 'ml-studio-shortcut-card';
      card.setAttribute('role', 'button');
      card.setAttribute('tabindex', '0');
      card.innerHTML = `
        <div class="sc-ico">${sprite(shortcutIcon(label))}</div>
        <div class="sc-text">
          <div class="sc-title">${escapeHtml(label)}</div>
          <div class="sc-desc">${escapeHtml(shortcutDesc(label))}</div>
        </div>`;
      const go = () => selectTab(label);
      card.addEventListener('click', go);
      card.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); go(); }
      });
      shortcutsHost.appendChild(card);
    });
  }
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
        <tf-file-input id="ml-studio-data-file" accept=".csv,.xlsx,.jsonl,.json,.zip" label="Przeciągnij plik lub kliknij, aby wgrać"></tf-file-input>
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

// Pojedyncza ramka WS ma limit ~1 MiB. Pliki ≤ CHUNK_SIZE idą jednym żądaniem,
// większe są dzielone na fragmenty i sklejane po stronie serwera (chunked upload).
const CHUNK_SIZE = 256 * 1024;

async function uploadDataset(pid, file) {
  const filename = file.name || 'zbiór';
  try {
    if (file.size <= CHUNK_SIZE) {
      const buf = await file.arrayBuffer();
      const bytes = new Uint8Array(buf);
      const resp = await ApiBinary.one('mlStudioDatasetUploadRequest', {
        projectId: pid,
        name: nameFromFilename(filename),
        filename,
        bytes,
      });
      await onDatasetUploaded(pid, filename, resp);
      return;
    }

    const buf = await file.arrayBuffer();
    const all = new Uint8Array(buf);
    const totalChunks = Math.ceil(all.length / CHUNK_SIZE);
    const uploadId = (crypto.randomUUID && crypto.randomUUID())
      || `up-${Date.now()}-${Math.floor(Math.random() * 1e9)}`;
    const name = nameFromFilename(filename);

    let resp = null;
    for (let seq = 0; seq < totalChunks; seq += 1) {
      const start = seq * CHUNK_SIZE;
      const slice = all.subarray(start, Math.min(start + CHUNK_SIZE, all.length));
      resp = await ApiBinary.one('mlStudioDatasetUploadChunkRequest', {
        projectId: pid,
        name,
        filename,
        uploadId,
        seq,
        totalChunks,
        bytes: slice,
      });
      const pct = Math.round(((seq + 1) / totalChunks) * 100);
      toast(`Wgrywanie „${filename}": ${pct}% (${seq + 1}/${totalChunks})`, 'info', 1500);
    }
    await onDatasetUploaded(pid, filename, resp);
  } catch (err) {
    toast(`Wgrywanie pliku: ${err.message}`, 'error');
  }
}

// Stages ONE raw media file server-side via the recognition staging endpoint,
// always chunked over the WS frame limit. Does NOT create a dataset (that is the
// separate build step). Reuses CHUNK_SIZE.
async function stageRecogMedia(pid, file) {
  const filename = file.name || 'media';
  const buf = await file.arrayBuffer();
  const all = new Uint8Array(buf);
  const totalChunks = Math.max(1, Math.ceil(all.length / CHUNK_SIZE));
  const uploadId = (crypto.randomUUID && crypto.randomUUID())
    || `stg-${Date.now()}-${Math.floor(Math.random() * 1e9)}`;
  for (let seq = 0; seq < totalChunks; seq += 1) {
    const start = seq * CHUNK_SIZE;
    const slice = all.subarray(start, Math.min(start + CHUNK_SIZE, all.length));
    await ApiBinary.one('mlStudioRecogStageMediaRequest', {
      projectId: pid,
      filename,
      uploadId,
      seq,
      totalChunks,
      bytes: slice,
    });
  }
}

// Odpytuje status asynchronicznej budowy datasetu COCO do zakończenia. Pokazuje
// postęp ("Przetwarzanie N / total", liczba klatek), na końcu toast i odświeża
// listę zbiorów. Rozwiązuje się dopiero po stanie terminalnym (succeeded/failed).
function pollRecogBuild(pid, buildId, prog) {
  return new Promise((resolve) => {
    const tick = async () => {
      let st;
      try {
        st = await ApiBinary.one('mlStudioRecogBuildStatusRequest', { buildId });
      } catch (err) {
        if (prog) prog.textContent = '';
        toast(`Budowa datasetu: ${err.message}`, 'error');
        resolve();
        return;
      }
      const status = st.status || 'running';
      const total = st.filesTotal ?? st.files_total ?? 0;
      const doneN = st.filesDone ?? st.files_done ?? 0;
      const frames = st.framesExtracted ?? st.frames_extracted ?? 0;
      if (status === 'succeeded') {
        const imgs = st.imageCount ?? st.image_count ?? 0;
        const cats = st.categoryCount ?? st.category_count ?? 0;
        if (prog) prog.textContent = '';
        toast(`Dataset zbudowany: ${imgs} obrazów, ${cats} klas.`, 'success');
        loadDatasets(pid);
        resolve();
        return;
      }
      if (status === 'failed') {
        if (prog) prog.textContent = '';
        toast(`Budowa datasetu: ${st.error || 'nieznany błąd'}`, 'error');
        resolve();
        return;
      }
      if (prog) {
        prog.textContent = total
          ? `Przetwarzanie ${doneN} / ${total} (klatek: ${frames})…`
          : 'Budowanie datasetu (dekodowanie HEIC, klatki z wideo)…';
      }
      setTimeout(tick, 2000);
    };
    tick();
  });
}

// Polling postępu auto-etykietowania datasetu (po jobId). `prog` to element na
// tekst statusu; `onDone` wywoływane po sukcesie (odświeżenie obrazów).
function pollRecogAutolabel(jobId, prog, onDone) {
  return new Promise((resolve) => {
    const tick = async () => {
      let st;
      try {
        st = await ApiBinary.one('mlStudioRecogAutolabelStatusRequest', { jobId });
      } catch (err) {
        if (prog) prog.textContent = '';
        toast(`Auto-etykietowanie: ${err.message}`, 'error');
        resolve();
        return;
      }
      const status = st.status || 'running';
      const total = st.imagesTotal ?? st.images_total ?? 0;
      const doneN = st.imagesDone ?? st.images_done ?? 0;
      const dets = st.detections ?? 0;
      const skippedUnknown = st.skippedUnknown ?? st.skipped_unknown ?? 0;
      if (status === 'succeeded') {
        if (prog) prog.textContent = '';
        if (dets === 0) {
          const hint = skippedUnknown > 0
            ? `0 wykryć — ${skippedUnknown} pominięto (klasy spoza datasetu), sprawdź model`
            : '0 wykryć — sprawdź próg/model';
          toast(`Auto-etykietowanie zakończone: ${hint}.`, 'warning');
        } else {
          toast(`Auto-etykietowanie zakończone: ${dets} wykryć na ${total} obrazach.`, 'success');
        }
        if (onDone) { try { await onDone(); } catch (_) {} }
        resolve();
        return;
      }
      if (status === 'failed') {
        if (prog) prog.textContent = '';
        toast(`Auto-etykietowanie: ${st.error || 'nieznany błąd'}`, 'error');
        resolve();
        return;
      }
      if (prog) prog.textContent = `Auto-etykietowanie ${doneN} / ${total} (wykryć: ${dets})…`;
      setTimeout(tick, 2000);
    };
    tick();
  });
}

async function onDatasetUploaded(pid, filename, resp) {
  toast(`Wgrano „${filename}" — sprofilowano`, 'success');
  await loadDatasets(pid);
  const datasetId = resp?.datasetId ?? resp?.dataset_id
    ?? resp?.dataset?.datasetId ?? resp?.dataset?.dataset_id;
  if (resp?.profile) {
    renderProfile(resp.profile);
  } else if (datasetId) {
    await loadProfile(datasetId);
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
// "Trenuj" tab — tabular training: pick dataset → pick target column (origin of
// "wykryto N klas" from the dataset profile) → engine card → leaderboard with
// real metrics from mlStudioTabularTrainRequest. Everything is read from the
// backend; the leaderboard rows are the actual trained models.
// =============================================================================

// Continuous numeric columns map to regression; everything else (categorical,
// low-cardinality ints, text) defaults to classification. The user can override.
function isRegressionColumn(col) {
  return columnTypeSlug(col) === 'float';
}

function taskLabel(task) {
  return task === 'regression' ? 'regresja' : 'klasyfikacja';
}

// =============================================================================
// Fine-tuning LLM (typ projektu ft_llm) — zakładki „Model bazowy" (f00+f01) i
// „Trening" (f02). Wybór modelu/metody/hiperparametrów trafia do ftConfig[pid],
// a zakładka Trening startuje asynchroniczny job (mlStudioFtTrainStartRequest) i
// odpytuje postęp co 2 s (mlStudioFtTrainStatusRequest) z krzywą loss.
// =============================================================================

// Presety modeli bazowych zgodne z manifestem ml-training. „source" rozróżnia
// model dostępny w serwisie od pobieranego z HuggingFace (chip pochodzenia w f00).
const FT_BASE_MODELS = [
  {
    // Qwen2.5 (arch qwen2) działa z pinowanym transformers==4.46.3 i jest bez bramki.
    // Qwen3.5 (qwen3_5) wymagałby nowszego transformers (łańcuch trl/peft) — świadomie
    // rekomendujemy model, który trenuje się out-of-box na obecnym backendzie.
    id: 'Qwen/Qwen2.5-0.5B-Instruct',
    name: 'Qwen2.5-0.5B',
    sub: 'Mały, szybki, bez bramki — zalecany start (działa out-of-box)',
    params: '0.5 B',
    context: '32k',
    license: 'Apache-2.0',
    source: 'hf',
    recommended: true,
  },
  {
    id: 'meta-llama/Llama-3.2-1B',
    name: 'Llama-3.2-1B',
    sub: 'Kompaktowy bazowy Llama — dobry kompromis jakość/koszt',
    params: '1 B',
    context: '128k',
    license: 'Llama 3.2 Comm.',
    source: 'hf',
  },
  {
    id: 'Qwen/Qwen2.5-7B',
    name: 'Qwen2.5-7B',
    sub: 'Większy bazowy — wyższa jakość kosztem VRAM i czasu',
    params: '7 B',
    context: '128k',
    license: 'Apache-2.0',
    source: 'hf',
  },
];

// Parametryzacja (oś 2) — karty z badge szacowanej VRAM. Wartości jak w f01.
const FT_METHODS = [
  { id: 'qlora', name: 'QLoRA', desc: '4-bit baza + adaptery LoRA. Najniższa VRAM. Wymaga GPU CUDA (Linux/Windows) — na Apple/CPU degraduje do LoRA.', vram: '~8 GB', tone: 'low', lora: true },
  { id: 'lora', name: 'LoRA', desc: 'Baza 16-bit + adaptery. Lepsza jakość niż QLoRA.', vram: '~16 GB', tone: 'mid', lora: true },
  { id: 'dora', name: 'DoRA', desc: 'LoRA z dekompozycją wagi — wyższa wierność.', vram: '~18 GB', tone: 'high', lora: true },
  { id: 'full', name: 'Full', desc: 'Pełny fine-tune wszystkich wag. Najlepsza jakość.', vram: '~24 GB', tone: 'max', lora: false },
];

// Cel treningu (oś 1).
// W projekcie destylacji WSZYSTKIE trzy cele są destylacją (uczeń uczy się od
// nauczyciela) — różnią się SYGNAŁEM: odpowiedzi (SFT) / preferencje (DPO) /
// rozkład-logity (KD). To nie są alternatywy dla destylacji, tylko jej tryby.
const FT_OBJECTIVES = [
  { id: 'sft', name: 'SFT', desc: 'Na ODPOWIEDZIACH — uczeń imituje odpowiedzi nauczyciela (pary wejście→wyjście).' },
  { id: 'dpo', name: 'DPO', desc: 'Na PREFERENCJACH — uczeń uczy się „lepsza>gorsza" (chosen/rejected od nauczyciela).' },
  { id: 'kd', name: 'KD', desc: 'Na LOGITACH — uczeń dopasowuje rozkład nauczyciela (soft labels, GKD; nauczyciel obecny przy treningu).' },
];

// Hiperparametry: domyślne + zakres dla tf-input type=number. lora=true → pole
// chowane przy method=full (nie ma adapterów LoRA przy pełnym fine-tunie).
const FT_HYPERPARAMS = [
  { key: 'learningRate', label: 'learning rate', def: 2e-4, step: '0.00001', min: 0 },
  { key: 'batchSize', label: 'batch size', def: 8, step: '1', min: 1 },
  { key: 'gradAccumSteps', label: 'grad accum', def: 4, step: '1', min: 1 },
  { key: 'epochs', label: 'epoki', def: 3, step: '1', min: 1 },
  { key: 'loraR', label: 'LoRA r (rank)', def: 16, step: '1', min: 1, lora: true },
  { key: 'loraAlpha', label: 'LoRA alpha', def: 32, step: '1', min: 1, lora: true },
  { key: 'loraDropout', label: 'LoRA dropout', def: 0.05, step: '0.01', min: 0, lora: true },
  { key: 'maxSeqLen', label: 'max seq len', def: 512, step: '1', min: 1 },
];

// Domyślna konfiguracja FT dla projektu, w którym jej jeszcze nie ma.
function defaultFtConfig() {
  const hyperparams = {};
  for (const h of FT_HYPERPARAMS) hyperparams[h.key] = h.def;
  return {
    baseModel: FT_BASE_MODELS[0].id,
    customRepo: '',
    method: 'qlora',
    objective: 'sft',
    mergeAdapter: false,
    hyperparams,
  };
}

// Konfiguracja fine-tuningu persystuje w localStorage per projekt — bez tego
// „Zapisz konfigurację" ginęło po reloadzie (config był tylko w RAM). Merge z
// defaultami toleruje starsze zapisy bez nowych pól.
const FT_CONFIG_LS_PREFIX = 'ml-studio-ft-config:';

function persistFtConfig(pid) {
  try {
    if (ftConfig[pid]) localStorage.setItem(FT_CONFIG_LS_PREFIX + pid, JSON.stringify(ftConfig[pid]));
  } catch (_) {
    // localStorage niedostępny (tryb prywatny) — config zostaje w pamięci sesji.
  }
}

// Ładuje zapisany config z localStorage do pamięci (bez defaultów). Zwraca true,
// gdy istniał zapis — Trening używa tego, by odróżnić „nigdy nie konfigurowano"
// (pusty stan) od „skonfigurowano wcześniej" (hydratacja po reloadzie).
function hydrateFtConfig(pid) {
  if (ftConfig[pid]) return true;
  try {
    const raw = localStorage.getItem(FT_CONFIG_LS_PREFIX + pid);
    if (raw) {
      const saved = JSON.parse(raw);
      const def = defaultFtConfig();
      const savedHp = saved && typeof saved.hyperparams === 'object' && saved.hyperparams;
      // Deep-merge `hyperparams`: częściowy/stary zapis (albo null) nie może wyzerować
      // nowych pól — inaczej summary/payload dostają undefined, a null crashuje zakładkę.
      ftConfig[pid] = { ...def, ...saved, hyperparams: { ...def.hyperparams, ...(savedHp || {}) } };
      return true;
    }
  } catch (_) {
    // uszkodzony/niedostępny wpis — traktuj jak brak konfiguracji
  }
  return false;
}

function getFtConfig(pid) {
  if (!ftConfig[pid]) {
    hydrateFtConfig(pid);
  }
  if (!ftConfig[pid]) ftConfig[pid] = defaultFtConfig();
  return ftConfig[pid];
}

// Etykieta źródła modelu (chip pochodzenia).
function ftSourceChip(source) {
  if (source === 'serwis') return '<span class="ml-studio-ft-origin serwis">dostępny w serwisie</span>';
  return '<span class="ml-studio-ft-origin hf">pobierany z HuggingFace</span>';
}

function methodLabel(id) {
  return (FT_METHODS.find((m) => m.id === id) || {}).name || id;
}

function objectiveLabel(id) {
  return (FT_OBJECTIVES.find((o) => o.id === id) || {}).name || String(id).toUpperCase();
}

// =============================================================================
// Zakładka „Dane" (destylacja) — generowanie datasetu par (question, answer).
// Źródło pytań: generacja modelem z celu usera ALBO import istniejącego datasetu.
// Wybrany TEACHER generuje odpowiedzi. Backend: MlStudioDistillGenerate + polling.
// =============================================================================
async function renderDistillDataTab(panel, p) {
  const pid = projectId(p);
  panel.innerHTML = `<div class="ml-studio-loading">Ładowanie…</div>`;

  let models = [];
  let datasets = [];
  try {
    const [ml, dss] = await Promise.all([
      ApiBinary.list('modelListRequest', { arrayKey: 'models' }).catch(() => []),
      // .one (nie .list) — .list nie forwarduje projectId do żądania, przez co
      // padało „project not found" i selektory datasetów (import + edytor) były puste.
      ApiBinary.one('mlStudioDatasetsListRequest', { projectId: pid })
        .then((r) => (Array.isArray(r.datasets) ? r.datasets : []))
        .catch(() => []),
    ]);
    // Modele generacyjne (kategoria 'llm') — teacher i model generujący pytania
    // odpowiadają na prompt. Datalist to PODPOWIEDZI; pole przyjmuje też dowolny
    // alias/model spoza listy (np. zewnętrzny endpoint).
    const catOf = (m) => String(m.category || m.service_type || '').toLowerCase();
    models = (Array.isArray(ml) ? ml : []).filter((m) => catOf(m) === 'llm');
    datasets = Array.isArray(dss) ? dss : [];
  } catch (e) {
    /* best effort — pola przyjmują dowolny alias/model */
  }

  const modelNames = [...new Set(models.map((m) => m.name || m.id).filter(Boolean))];
  const modelOptions = modelNames.map((n) => `<option value="${escapeAttr(n)}"></option>`).join('');
  const datasetOptions = datasets
    .map((d) => `<option value="${escapeAttr(d.datasetId || d.dataset_id || d.id || '')}">${escapeHtml(d.name || '')}</option>`)
    .join('');

  panel.innerHTML = `
    <div class="ml-studio-distill-data">
      <h3>Generowanie datasetu destylacji</h3>
      <p class="ml-studio-hint">Zbierz PYTANIA (wygeneruj modelem z celu albo zaimportuj z datasetu), a wybrany TEACHER wygeneruje ODPOWIEDZI. Wynik: pary (pytanie, odpowiedź) do treningu ucznia.</p>

      <label class="ml-studio-field-label">Źródło pytań</label>
      <div class="ml-studio-source-toggle" style="display:flex;gap:8px;margin-bottom:8px;">
        <tf-button id="ml-distill-src-generate" variant="primary" size="sm">Generuj modelem</tf-button>
        <tf-button id="ml-distill-src-import" variant="ghost" size="sm">Import z datasetu</tf-button>
      </div>

      <div id="ml-distill-generate-box">
        <label class="ml-studio-field-label">Cel / co wygenerować (prompt dla modelu)</label>
        <tf-textarea id="ml-distill-prompt" rows="3" placeholder="np. Wygeneruj pytania o ekstrakcję encji i relacji z polskiego tekstu prawnego"></tf-textarea>
        <div style="display:flex;gap:12px;">
          <div style="flex:2;"><label class="ml-studio-field-label">Model generujący pytania</label>
            <input list="ml-distill-models" id="ml-distill-qmodel" class="ml-studio-model-input" style="width:100%;box-sizing:border-box;display:block;padding:8px;margin:2px 0 8px;border:1px solid var(--border);border-radius:6px;background:var(--surface-2);color:var(--text);" placeholder="alias/model (puste = teacher)"></div>
          <div style="flex:1;"><label class="ml-studio-field-label">Ile pytań</label>
            <tf-input id="ml-distill-num" type="number" value="10"></tf-input></div>
        </div>
      </div>

      <div id="ml-distill-import-box" hidden>
        <label class="ml-studio-field-label">Dataset źródłowy (pytania)</label>
        <select id="ml-distill-srcds" class="ml-studio-model-input" style="width:100%;box-sizing:border-box;display:block;padding:8px;margin:2px 0 8px;border:1px solid var(--border);border-radius:6px;background:var(--surface-2);color:var(--text);">${datasetOptions || '<option value="">— brak datasetów —</option>'}</select>
        <label class="ml-studio-field-label">Pole/kolumna z pytaniem</label>
        <tf-input id="ml-distill-field" placeholder="question / prompt (puste = auto)"></tf-input>
      </div>

      <hr style="margin:14px 0;border:none;border-top:1px solid var(--border);">
      <label class="ml-studio-field-label">Wariant treningu (decyduje o kształcie danych)</label>
      <div style="display:flex;gap:8px;margin:2px 0 6px;">
        <tf-button id="ml-distill-obj-sft" variant="primary" size="small">SFT</tf-button>
        <tf-button id="ml-distill-obj-kd" variant="ghost" size="small">KD</tf-button>
        <tf-button id="ml-distill-obj-dpo" variant="ghost" size="small">DPO</tf-button>
      </div>
      <p class="ml-studio-hint" id="ml-distill-obj-hint" style="margin:0 0 10px;">SFT/KD: pary pytanie→odpowiedź (teacher). Wybierz ten sam wariant co w zakładce „Model bazowy".</p>

      <label class="ml-studio-field-label">Teacher — model generujący ODPOWIEDZI (etykiety treningowe)</label>
      <input list="ml-distill-models" id="ml-distill-teacher" class="ml-studio-model-input" style="width:100%;box-sizing:border-box;display:block;padding:8px;margin:2px 0 8px;border:1px solid var(--border);border-radius:6px;background:var(--surface-2);color:var(--text);" placeholder="np. gpt-5-5 albo dowolny alias/model tentaflow">
      <datalist id="ml-distill-models">${modelOptions}</datalist>

      <label class="ml-studio-field-label">Instrukcja dla teachera (opcjonalna)</label>
      <tf-textarea id="ml-distill-instr" rows="2" placeholder="np. Wyodrębnij encje i relacje w formacie E|nazwa|typ / R|head|rel|tail"></tf-textarea>

      <div id="ml-distill-dpo-box" hidden>
        <label class="ml-studio-field-label">Model ODRZUCAJĄCY — generuje GORSZĄ odpowiedź (DPO)</label>
        <input list="ml-distill-models" id="ml-distill-rejmodel" class="ml-studio-model-input" style="width:100%;box-sizing:border-box;display:block;padding:8px;margin:2px 0 8px;border:1px solid var(--border);border-radius:6px;background:var(--surface-2);color:var(--text);" placeholder="słabszy/bazowy model (puste = teacher z instrukcją „gorzej")">
        <label class="ml-studio-field-label">Instrukcja dla modelu odrzucającego (opcjonalna)</label>
        <tf-textarea id="ml-distill-rejinstr" rows="2" placeholder="np. Odpowiedz krótko i ogólnikowo, pomiń szczegóły i uzasadnienie"></tf-textarea>
      </div>
      <div style="display:flex;gap:12px;">
        <div style="flex:1;"><label class="ml-studio-field-label">Temperatura</label><tf-input id="ml-distill-temp" type="number" value="0.2"></tf-input></div>
        <div style="flex:1;"><label class="ml-studio-field-label">Max tokenów</label><tf-input id="ml-distill-maxtok" type="number" value="768"></tf-input></div>
      </div>

      <label class="ml-studio-field-label">Nazwa datasetu</label>
      <tf-input id="ml-distill-name" value="destylacja-${Date.now()}"></tf-input>

      <div class="ml-studio-actions" style="margin-top:12px;">
        <tf-button id="ml-distill-go" variant="primary">Generuj dataset</tf-button>
      </div>
      <div id="ml-distill-progress" style="margin-top:14px;" hidden></div>

      <hr style="margin:20px 0;border:none;border-top:1px solid var(--border);">
      <label class="ml-studio-field-label">Podgląd i ręczna edycja datasetu</label>
      <p class="ml-studio-hint" style="margin:0 0 8px;">Wczytaj istniejący dataset, żeby zobaczyć wygenerowane pary i ręcznie dodać/poprawić/usunąć wiersze. Kolumny dobierają się do formatu (SFT/KD: pytanie·odpowiedź; DPO: prompt·lepsza·gorsza).</p>
      <div style="display:flex;gap:8px;align-items:center;margin:2px 0 8px;">
        <tf-select id="ml-distill-edit-ds" style="flex:1;"></tf-select>
        <tf-button id="ml-distill-edit-load" variant="outline">Wczytaj</tf-button>
      </div>
      <div id="ml-distill-edit-meta" class="ml-studio-hint" style="margin:0 0 8px;display:none;padding:8px;border:1px solid var(--border);border-radius:6px;background:var(--surface-2);"></div>
      <div id="ml-distill-edit-rows"></div>
      <div id="ml-distill-edit-actions" style="display:none;gap:8px;margin-top:10px;">
        <tf-button id="ml-distill-edit-add" variant="ghost">+ Dodaj wiersz</tf-button>
        <tf-button id="ml-distill-edit-save" variant="primary">Zapisz zmiany</tf-button>
      </div>
      <div id="ml-distill-edit-status" class="ml-studio-hint" style="margin-top:6px;"></div>
    </div>`;

  let source = 'generate';
  const setSource = (s) => {
    source = s;
    byId('ml-distill-generate-box').hidden = s !== 'generate';
    byId('ml-distill-import-box').hidden = s !== 'import';
    byId('ml-distill-src-generate')?.setAttribute('variant', s === 'generate' ? 'primary' : 'ghost');
    byId('ml-distill-src-import')?.setAttribute('variant', s === 'import' ? 'primary' : 'ghost');
  };
  byId('ml-distill-src-generate')?.addEventListener('click', () => setSource('generate'));
  byId('ml-distill-src-import')?.addEventListener('click', () => setSource('import'));

  // Wariant treningu: SFT/KD -> pary Q&A; DPO -> trójki (prompt, chosen, rejected).
  // Synchronizujemy z ZAPISANĄ konfiguracją „Model bazowy" (spójny cel treningu),
  // ale przez hydrateFtConfig — NIE getFtConfig, bo ten tworzy domyślny config i
  // omijałby guard „Skonfiguruj fine-tuning" w Treningu. hydrateFtConfig ładuje
  // tylko realny zapis z localStorage (albo nic → zostajemy na sft, bez mutacji).
  let objective = 'sft';
  if (hydrateFtConfig(pid) && ftConfig[pid] && ftConfig[pid].objective) {
    objective = String(ftConfig[pid].objective).toLowerCase();
  }
  if (!['sft', 'kd', 'dpo'].includes(objective)) objective = 'sft';
  const OBJ_HINTS = {
    sft: 'SFT — destylacja na ODPOWIEDZIACH: generujemy pary pytanie→odpowiedź (teacher). Uczeń imituje odpowiedzi.',
    kd: 'KD — destylacja na LOGITACH: generujemy pary pytanie→odpowiedź; przy treningu teacher (wskaż go jako nauczyciela) dopasowuje rozkład.',
    dpo: 'DPO — destylacja na PREFERENCJACH: generujemy trójki (prompt, lepsza, gorsza) — teacher lepszą, model odrzucający gorszą.',
  };
  const setObjective = (o) => {
    objective = o;
    byId('ml-distill-dpo-box').hidden = o !== 'dpo';
    const hint = byId('ml-distill-obj-hint');
    if (hint) hint.textContent = OBJ_HINTS[o] || '';
    ['sft', 'kd', 'dpo'].forEach((k) =>
      byId('ml-distill-obj-' + k)?.setAttribute('variant', k === o ? 'primary' : 'ghost'));
  };
  ['sft', 'kd', 'dpo'].forEach((k) =>
    byId('ml-distill-obj-' + k)?.addEventListener('click', () => setObjective(k)));
  setObjective(objective); // odzwierciedl stan początkowy (z Model bazowy) w UI

  byId('ml-distill-go')?.addEventListener('click', async () => {
    const teacher = String(byId('ml-distill-teacher')?.value || '').trim();
    if (!teacher) {
      toast('Podaj model teacher', 'error');
      return;
    }
    const payload = {
      projectId: pid,
      datasetName: String(byId('ml-distill-name')?.value || '').trim() || `destylacja-${Date.now()}`,
      questionSource: source,
      teacherModel: teacher,
      answerInstruction: String(byId('ml-distill-instr')?.value || '').trim() || undefined,
      temperature: Math.max(0, Math.min(2, Number(byId('ml-distill-temp')?.value ?? 0.2) || 0)),
      maxTokens: Math.max(16, Math.min(8192, Number(byId('ml-distill-maxtok')?.value || 768) | 0)),
      objective,
    };
    if (objective === 'dpo') {
      payload.rejectedModel = String(byId('ml-distill-rejmodel')?.value || '').trim() || undefined;
      payload.rejectedInstruction = String(byId('ml-distill-rejinstr')?.value || '').trim() || undefined;
    }
    if (source === 'generate') {
      payload.generatePrompt = String(byId('ml-distill-prompt')?.value || '').trim();
      payload.questionModel = String(byId('ml-distill-qmodel')?.value || '').trim() || undefined;
      payload.numQuestions = Math.max(1, Math.min(500, Number(byId('ml-distill-num')?.value || 10) | 0));
      if (!payload.generatePrompt) {
        toast('Podaj cel/prompt', 'error');
        return;
      }
    } else {
      payload.sourceDatasetId = String(byId('ml-distill-srcds')?.value || '');
      payload.questionField = String(byId('ml-distill-field')?.value || '').trim() || undefined;
      if (!payload.sourceDatasetId) {
        toast('Wybierz dataset źródłowy', 'error');
        return;
      }
    }
    byId('ml-distill-go')?.setAttribute('disabled', 'true');
    try {
      const resp = await ApiBinary.one('mlStudioDistillGenerateRequest', payload);
      pollDistill(resp.datasetId || resp.dataset_id);
    } catch (e) {
      toast(`Błąd startu: ${e.message}`, 'error');
      byId('ml-distill-go')?.removeAttribute('disabled');
    }
  });

  function pollDistill(datasetId) {
    const box = byId('ml-distill-progress');
    if (!box) return;
    box.hidden = false;
    const tick = async () => {
      let st;
      try {
        st = await ApiBinary.one('mlStudioDistillGenerateStatusRequest', { datasetId });
      } catch (e) {
        box.innerHTML = `<span class="ml-studio-err">Błąd pollingu: ${escapeHtml(e.message)}</span>`;
        return;
      }
      const done = st.done || 0;
      const total = st.total || 0;
      const pct = total ? Math.round((done / total) * 100) : 0;
      const samples = Array.isArray(st.samples) ? st.samples : [];
      box.innerHTML = `
        <div>Status: <strong>${escapeHtml(st.status || '')}</strong> ${total ? `(${done}/${total}, ${pct}%)` : ''}</div>
        ${st.error ? `<div class="ml-studio-err">${escapeHtml(st.error)}</div>` : ''}
        <div class="ml-studio-progress-bar" style="height:8px;background:var(--surface-2);border-radius:4px;overflow:hidden;margin:6px 0;"><div style="width:${pct}%;height:100%;background:var(--accent);"></div></div>
        ${samples.length ? `<div class="ml-studio-distill-samples">${samples.map((s) => `<div class="qa" style="border:1px solid var(--border);border-radius:6px;padding:8px;margin:6px 0;"><div style="font-weight:600;">${escapeHtml(s.question || '')}</div><div style="opacity:0.85;margin-top:4px;white-space:pre-wrap;">${s.rejected ? '✓ ' : ''}${escapeHtml(s.answer || '')}</div>${s.rejected ? `<div style="opacity:0.7;margin-top:4px;white-space:pre-wrap;color:var(--danger,#c66);">✗ ${escapeHtml(s.rejected)}</div>` : ''}</div>`).join('')}</div>` : ''}`;
      if (st.status === 'completed') {
        toast('Dataset gotowy — zakładka Trening', 'success');
        byId('ml-distill-go')?.removeAttribute('disabled');
        return;
      }
      if (st.status === 'failed' || st.status === 'unknown') {
        byId('ml-distill-go')?.removeAttribute('disabled');
        return;
      }
      setTimeout(tick, 2000);
    };
    tick();
  }

  // ---- Podgląd i ręczna edycja datasetu (wiersze JSONL) ----
  const editRowsBox = byId('ml-distill-edit-rows');
  const editActions = byId('ml-distill-edit-actions');
  const editStatus = byId('ml-distill-edit-status');
  let editCols = ['question', 'answer']; // wykrywane z danych; fallback SFT
  let editDatasetId = '';
  let editTruncated = false; // true gdy załadowano tylko część dużego datasetu
  let editPending = false; // true gdy dataset w trakcie generacji (distill_status=pending)

  const rowFieldHtml = (col, val) =>
    `<div style="flex:1;min-width:0;"><div class="ml-studio-hint" style="margin-bottom:2px;">${escapeHtml(col)}</div>` +
    `<textarea data-col="${escapeAttr(col)}" rows="2" style="width:100%;box-sizing:border-box;padding:6px;border:1px solid var(--border);border-radius:6px;background:var(--surface-2);color:var(--text);">${escapeHtml(val)}</textarea></div>`;

  const addRowEl = (obj) => {
    const row = document.createElement('div');
    row.className = 'ml-distill-edit-row';
    row.style = 'display:flex;gap:8px;align-items:flex-start;margin:6px 0;padding:8px;border:1px solid var(--border);border-radius:6px;';
    row.innerHTML = editCols.map((c) => rowFieldHtml(c, obj && obj[c] != null ? String(obj[c]) : '')).join('');
    const del = document.createElement('tf-button');
    del.setAttribute('variant', 'ghost');
    del.setAttribute('icon', 'trash');
    del.setAttribute('title', 'Usuń wiersz');
    del.style.alignSelf = 'center';
    del.addEventListener('click', () => row.remove());
    row.appendChild(del);
    editRowsBox.appendChild(row);
  };

  const collectRows = () => {
    const out = [];
    editRowsBox.querySelectorAll('.ml-distill-edit-row').forEach((row) => {
      const o = {};
      let any = false;
      row.querySelectorAll('textarea[data-col]').forEach((ta) => {
        const v = ta.value.trim();
        o[ta.getAttribute('data-col')] = v;
        if (v) any = true;
      });
      if (any) out.push(JSON.stringify(o)); // pomijamy całkowicie puste wiersze
    });
    return out;
  };

  const editMeta = byId('ml-distill-edit-meta');
  const OBJ_LABEL = { sft: 'SFT (na odpowiedziach)', dpo: 'DPO (na preferencjach)', kd: 'KD (na logitach)' };
  const renderMeta = (metaStr) => {
    let m = null;
    try { m = metaStr ? JSON.parse(metaStr) : null; } catch (_) { m = null; }
    if (!m || typeof m !== 'object') { editMeta.style.display = 'none'; return; }
    const parts = [];
    if (m.objective) parts.push(`wariant: <strong>${escapeHtml(OBJ_LABEL[m.objective] || String(m.objective).toUpperCase())}</strong>`);
    if (m.teacher_model) parts.push(`nauczyciel: <strong>${escapeHtml(m.teacher_model)}</strong>`);
    if (m.question_source) parts.push(`źródło pytań: ${escapeHtml(m.question_source)}${m.question_model ? ` (model: ${escapeHtml(m.question_model)})` : ''}`);
    if (m.rejected_model) parts.push(`model odrzucający: ${escapeHtml(m.rejected_model)}`);
    if (m.generate_prompt) parts.push(`cel: „${escapeHtml(String(m.generate_prompt).slice(0, 140))}”`);
    editMeta.innerHTML = parts.length ? `Pochodzenie — ${parts.join(' · ')}` : '';
    editMeta.style.display = parts.length ? 'block' : 'none';
  };

  byId('ml-distill-edit-load')?.addEventListener('click', async () => {
    const dsid = String(byId('ml-distill-edit-ds')?.value || '');
    if (!dsid) { toast('Wybierz dataset', 'error'); return; }
    editStatus.textContent = 'Wczytywanie…';
    try {
      // limit chroni przeglądarkę (rozmiar odpowiedzi + liczba textarea). Dobrany
      // wysoko, bo datasety destylacji są zwykle małe; gdy total > limit, ładujemy
      // tylko część i BLOKUJEMY zapis (zapis nadpisuje całość → utrata reszty).
      const LIMIT = 5000;
      const resp = await ApiBinary.one('mlStudioDatasetRowsRequest', { datasetId: dsid, limit: LIMIT });
      const raw = Array.isArray(resp.rows) ? resp.rows : [];
      const parsed = raw.map((r) => { try { return JSON.parse(r); } catch (_) { return null; } })
        .filter((o) => o && typeof o === 'object');
      // UNIA kluczy ze WSZYSTKICH wierszy (nie tylko [0]) — inaczej pola obecne
      // tylko w dalszych wierszach nie byłyby renderowane i przepadłyby przy zapisie.
      const keySet = new Set();
      parsed.forEach((o) => Object.keys(o).forEach((k) => keySet.add(k)));
      editCols = keySet.size ? [...keySet] : ['question', 'answer'];
      editDatasetId = dsid;
      editRowsBox.innerHTML = '';
      parsed.forEach((o) => addRowEl(o));
      if (!parsed.length) addRowEl(null); // pusty dataset → jeden pusty wiersz do wypełnienia
      editActions.style.display = 'flex';
      renderMeta(resp.meta);
      const total = resp.total ?? parsed.length;
      // Blokujemy zapis w dwóch przypadkach: (a) załadowano tylko część dużego
      // datasetu (zapis nadpisałby resztę); (b) dataset w trakcie generacji
      // (pending) — zapis oznaczyłby go jako completed i kolidował z tłem.
      editTruncated = total > parsed.length;
      editPending = !!resp.pending;
      const saveBtn = byId('ml-distill-edit-save');
      if (editTruncated || editPending) saveBtn?.setAttribute('disabled', 'true');
      else saveBtn?.removeAttribute('disabled');
      editStatus.textContent = editPending
        ? 'Dataset w trakcie generacji — podgląd tylko; edycja/zapis po zakończeniu.'
        : editTruncated
          ? `Wczytano ${parsed.length} z ${total} — dataset za duży na pełną edycję w GUI; ZAPIS WYŁĄCZONY (nadpisałby resztę).`
          : `Wczytano ${parsed.length} wierszy (kolumny: ${editCols.join(' · ')}).`;
    } catch (e) {
      editStatus.textContent = 'Błąd wczytywania: ' + (e.message || e);
    }
  });

  byId('ml-distill-edit-add')?.addEventListener('click', () => addRowEl(null));

  byId('ml-distill-edit-save')?.addEventListener('click', async () => {
    if (!editDatasetId) { toast('Najpierw wczytaj dataset', 'error'); return; }
    if (editTruncated) { toast('Zapis wyłączony — załadowano tylko część datasetu (nadpisałby resztę).', 'error'); return; }
    if (editPending) { toast('Dataset w trakcie generacji — edycja możliwa po zakończeniu.', 'error'); return; }
    const rows = collectRows();
    editStatus.textContent = 'Zapisywanie…';
    try {
      const resp = await ApiBinary.one('mlStudioDatasetRowsSaveRequest', { datasetId: editDatasetId, rows });
      editStatus.textContent = `Zapisano ${resp.rowCount ?? resp.row_count ?? rows.length} wierszy.`;
      toast('Dataset zapisany', 'success');
    } catch (e) {
      editStatus.textContent = 'Błąd zapisu: ' + (e.message || e);
      toast('Błąd zapisu datasetu', 'error');
    }
  });

  // Populacja tf-select datasetów (tf-select przyjmuje opcje przez setOptions).
  byId('ml-distill-edit-ds')?.setOptions?.(
    [{ label: '— wybierz dataset —', value: '' }].concat(
      datasets.map((d) => ({
        label: d.name || '(bez nazwy)',
        value: String(d.datasetId || d.dataset_id || d.id || ''),
      })),
    ),
    '',
  );
}

// =============================================================================
// Zakładka „Model bazowy" — f00 (wybór modelu) + f01 (metoda i hiperparametry).
// Auto-zapis do ftConfig[pid] przy każdej zmianie, plus jawny przycisk „Zapisz".
// =============================================================================
function renderFtModelTab(panel, p) {
  const pid = projectId(p);
  const cfg = getFtConfig(pid);
  const isDistill = (p.projectType ?? p.project_type ?? '') === 'distillation';

  const modelCards = FT_BASE_MODELS.map((m) => `
    <button type="button" class="ml-studio-train-engine-card ml-studio-ft-model-card${cfg.baseModel === m.id ? ' selected' : ''}"
            data-model="${escapeAttr(m.id)}" aria-pressed="${cfg.baseModel === m.id}">
      <div class="ml-studio-train-engine-ico">${sprite('brain')}</div>
      <div class="ml-studio-train-engine-body">
        <div class="ml-studio-train-engine-title">${escapeHtml(m.name)}${m.recommended ? ' <span class="ml-studio-ft-rec">zalecany</span>' : ''}</div>
        <p class="ml-studio-train-engine-text">${escapeHtml(m.sub)}</p>
        <div class="ml-studio-ft-spec">
          <span class="sp">parametry<b>${escapeHtml(m.params)}</b></span>
          <span class="sp">kontekst<b>${escapeHtml(m.context)}</b></span>
          <span class="sp">licencja<b>${escapeHtml(m.license)}</b></span>
        </div>
        <div class="ml-studio-ft-origin-row">${ftSourceChip(m.source)}<span class="ml-studio-ft-cap">capability: generacja LLM</span></div>
      </div>
    </button>
  `).join('');

  const customSelected = cfg.baseModel === '__custom__';
  const customCard = `
    <button type="button" class="ml-studio-train-engine-card ml-studio-ft-model-card${customSelected ? ' selected' : ''}"
            data-model="__custom__" aria-pressed="${customSelected}">
      <div class="ml-studio-train-engine-ico">${sprite('plus')}</div>
      <div class="ml-studio-train-engine-body">
        <div class="ml-studio-train-engine-title">Własny z HuggingFace</div>
        <p class="ml-studio-train-engine-text">Wskaż dowolne repo HF zdolne do generacji LLM.</p>
        <tf-input id="ml-studio-ft-custom-repo" placeholder="np. Qwen/Qwen2.5-0.5B-Instruct"
                  value="${escapeAttr(cfg.customRepo || '')}"></tf-input>
        <div class="ml-studio-ft-origin-row">${ftSourceChip('hf')}<span class="ml-studio-ft-cap">capability sprawdzany po podaniu repo</span></div>
      </div>
    </button>
  `;

  const objectiveCards = FT_OBJECTIVES.map((o) => `
    <button type="button" class="ml-studio-ft-axis-card${cfg.objective === o.id ? ' selected' : ''}"
            data-objective="${escapeAttr(o.id)}" aria-pressed="${cfg.objective === o.id}">
      <div class="ml-studio-ft-axis-name">${escapeHtml(o.name)}</div>
      <p class="ml-studio-ft-axis-desc">${escapeHtml(o.desc)}</p>
    </button>
  `).join('');

  const methodCards = FT_METHODS.map((m) => `
    <button type="button" class="ml-studio-train-engine-card ml-studio-ft-method-card${cfg.method === m.id ? ' selected' : ''}"
            data-method="${escapeAttr(m.id)}" aria-pressed="${cfg.method === m.id}">
      <div class="ml-studio-train-engine-body">
        <div class="ml-studio-train-engine-title">${escapeHtml(m.name)}</div>
        <p class="ml-studio-train-engine-text">${escapeHtml(m.desc)}</p>
        <span class="ml-studio-ft-vram ${escapeAttr(m.tone)}">${escapeHtml(m.vram)} VRAM</span>
      </div>
    </button>
  `).join('');

  const hpInputs = FT_HYPERPARAMS.map((h) => `
    <div class="ml-studio-ft-hp-field${h.lora ? ' ml-studio-ft-lora-field' : ''}">
      <tf-input type="number" label="${escapeAttr(h.label)}" id="ml-studio-ft-hp-${escapeAttr(h.key)}"
                value="${escapeAttr(String(cfg.hyperparams[h.key]))}" min="${escapeAttr(String(h.min))}" step="${escapeAttr(h.step)}"></tf-input>
    </div>
  `).join('');

  panel.innerHTML = `
    <div class="ml-studio-ft">
      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('package')} Model bazowy
          <span class="ml-studio-data-hint">model, który dotrenujemy na danych z zakładki „Dane"</span>
        </div>
        <div class="ml-studio-train-engine-grid ml-studio-ft-model-grid" id="ml-studio-ft-models">
          ${modelCards}
          ${customCard}
        </div>
      </section>

      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('tune')} Metoda treningu
          <span class="ml-studio-data-hint">dwie osie: cel × parametryzacja</span>
        </div>
        <div class="ml-studio-ft-axis-label">Oś 1 — Cel${isDistill ? ' <span class="ml-studio-data-hint">— wszystkie to tryby DESTYLACJI, różni je sygnał od nauczyciela (odpowiedzi / preferencje / logity)</span>' : ''}</div>
        <div class="ml-studio-ft-axis-grid" id="ml-studio-ft-objectives">${objectiveCards}</div>
        <div class="ml-studio-ft-teacher-field" id="ml-studio-ft-teacher-field"
             style="${cfg.objective === 'kd' ? '' : 'display:none'};margin:8px 0 4px">
          <tf-input label="Model-nauczyciel (repo HF)" id="ml-studio-ft-teacher"
                    value="${escapeAttr(cfg.teacherModel || '')}"
                    placeholder="np. Qwen/Qwen2.5-7B-Instruct"
                    hint="Większy, mocniejszy model — student uczy się jego rozkładu (KD)."></tf-input>
        </div>
        <div class="ml-studio-ft-axis-label">Oś 2 — Parametryzacja</div>
        <div class="ml-studio-train-engine-grid ml-studio-ft-method-grid" id="ml-studio-ft-methods">${methodCards}</div>
        <div class="ml-studio-ft-combo" id="ml-studio-ft-combo"></div>
      </section>

      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('tune')} Hiperparametry
          <span class="ml-studio-data-hint">sensowne domyślne — dostosuj w razie potrzeby</span>
        </div>
        <div class="ml-studio-ft-hp-grid" id="ml-studio-ft-hp">${hpInputs}</div>
      </section>

      <div class="ml-studio-data-origin">
        <div class="ml-studio-data-origin-ico">${sprite('info')}</div>
        <div>
          <div class="ml-studio-data-origin-title">Dane treningowe wgrywasz w zakładce „Dane".</div>
          <p class="ml-studio-data-origin-text">Fine-tuning używa wgranego zbioru (JSONL lub CSV z kolumnami <code>text</code> albo <code>prompt</code>+<code>response</code>). Backend sam parsuje format. Trening uruchamiasz w zakładce „Trening".</p>
        </div>
      </div>

      <div class="ml-studio-train-actions">
        <tf-button variant="primary" icon="check" id="ml-studio-ft-save">Zapisz konfigurację</tf-button>
      </div>
    </div>
  `;

  const comboEl = byId('ml-studio-ft-combo');
  const updateCombo = () => {
    if (!comboEl) return;
    const m = FT_METHODS.find((x) => x.id === cfg.method);
    comboEl.innerHTML = `
      <span class="ml-studio-ft-combo-eq">${escapeHtml(objectiveLabel(cfg.objective))} <span class="op">×</span> ${escapeHtml(methodLabel(cfg.method))}</span>
      <span class="ml-studio-ft-combo-vram">${escapeHtml(m ? m.vram : '')} VRAM</span>
    `;
  };

  // Pola LoRA nie mają sensu przy Full fine-tune — chowamy je w tym trybie.
  const updateLoraVisibility = () => {
    const hide = cfg.method === 'full';
    panel.querySelectorAll('.ml-studio-ft-lora-field').forEach((el) => {
      el.hidden = hide;
    });
  };

  // Wybór karty modelu.
  byId('ml-studio-ft-models')?.querySelectorAll('.ml-studio-ft-model-card').forEach((card) => {
    card.addEventListener('click', () => {
      cfg.baseModel = card.getAttribute('data-model');
      panel.querySelectorAll('.ml-studio-ft-model-card').forEach((c) => {
        const on = c === card;
        c.classList.toggle('selected', on);
        c.setAttribute('aria-pressed', String(on));
      });
    });
  });
  // Repo HF: zapis do customRepo; wpisanie repo nie kradnie zaznaczenia karty,
  // ale klik w pole zaznacza kartę „Własny".
  const customRepoInput = byId('ml-studio-ft-custom-repo');
  customRepoInput?.addEventListener('input', () => {
    cfg.customRepo = customRepoInput.value || '';
  });
  customRepoInput?.addEventListener('focus', () => {
    cfg.baseModel = '__custom__';
    panel.querySelectorAll('.ml-studio-ft-model-card').forEach((c) => {
      const on = c.getAttribute('data-model') === '__custom__';
      c.classList.toggle('selected', on);
      c.setAttribute('aria-pressed', String(on));
    });
  });

  // Wybór celu (oś 1).
  byId('ml-studio-ft-objectives')?.querySelectorAll('.ml-studio-ft-axis-card').forEach((card) => {
    card.addEventListener('click', () => {
      cfg.objective = card.getAttribute('data-objective');
      panel.querySelectorAll('.ml-studio-ft-axis-card').forEach((c) => {
        const on = c === card;
        c.classList.toggle('selected', on);
        c.setAttribute('aria-pressed', String(on));
      });
      // Pole nauczyciela tylko dla KD.
      const tf = byId('ml-studio-ft-teacher-field');
      if (tf) tf.style.display = cfg.objective === 'kd' ? '' : 'none';
      updateCombo();
    });
  });

  // Auto-zapis modelu-nauczyciela (KD).
  byId('ml-studio-ft-teacher')?.addEventListener('input', (e) => {
    cfg.teacherModel = (e.target.value || '').trim();
  });

  // Wybór parametryzacji (oś 2).
  byId('ml-studio-ft-methods')?.querySelectorAll('.ml-studio-ft-method-card').forEach((card) => {
    card.addEventListener('click', () => {
      cfg.method = card.getAttribute('data-method');
      panel.querySelectorAll('.ml-studio-ft-method-card').forEach((c) => {
        const on = c === card;
        c.classList.toggle('selected', on);
        c.setAttribute('aria-pressed', String(on));
      });
      updateCombo();
      updateLoraVisibility();
    });
  });

  // Auto-zapis hiperparametrów (liczby).
  for (const h of FT_HYPERPARAMS) {
    const el = byId('ml-studio-ft-hp-' + h.key);
    el?.addEventListener('input', () => {
      const num = Number(el.value);
      if (!Number.isNaN(num)) cfg.hyperparams[h.key] = num;
    });
  }

  byId('ml-studio-ft-save')?.addEventListener('click', () => {
    if (cfg.baseModel === '__custom__' && !String(cfg.customRepo || '').trim()) {
      toast('Podaj repo HuggingFace dla modelu „Własny".', 'error');
      return;
    }
    persistFtConfig(pid);
    toast('Zapisano konfigurację fine-tuningu.', 'success');
  });

  updateCombo();
  updateLoraVisibility();
}

// Efektywny identyfikator modelu bazowego (repo HF gdy wybrano „Własny").
function ftEffectiveBaseModel(cfg) {
  if (cfg.baseModel === '__custom__') return String(cfg.customRepo || '').trim();
  return cfg.baseModel;
}

// =============================================================================
// Zakładka „Trening" (f02) — podsumowanie konfiguracji, wybór datasetu, start
// jobu i widok LIVE z pollingiem statusu (pasek postępu, KPI, krzywa loss).
// =============================================================================
function renderFtTrainTab(panel, p, { selectTab }) {
  const pid = projectId(p);
  panel.innerHTML = '<div class="ml-studio-loading"><tf-spinner></tf-spinner></div>';
  ApiBinary.one('mlStudioDatasetsListRequest', { projectId: pid })
    .then((resp) => {
      const datasets = Array.isArray(resp.datasets) ? resp.datasets : [];
      renderFtTrainContent(panel, p, pid, datasets, { selectTab });
    })
    .catch((err) => {
      panel.innerHTML = '';
      const empty = document.createElement('tf-empty-state');
      empty.setAttribute('icon', 'alert');
      empty.setAttribute('title', 'Nie udało się wczytać zbiorów');
      empty.setAttribute('message', err.message || 'Błąd protokołu ML Studio.');
      panel.appendChild(empty);
    });
}

function renderFtTrainContent(panel, p, pid, datasets, { selectTab }) {
  // Brak danych → kierujemy do zakładki Dane.
  if (!datasets.length) {
    panel.innerHTML = '';
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', 'database');
    empty.setAttribute('title', 'Brak zbioru treningowego');
    empty.setAttribute('message', 'Najpierw wgraj zbiór treningowy w zakładce „Dane" (JSONL lub CSV z kolumnami text albo prompt+response).');
    panel.appendChild(empty);
    return;
  }
  // Brak konfiguracji → kierujemy do zakładki Model bazowy. Najpierw próba
  // hydratacji zapisanego configu (localStorage) — po reloadzie/nowej sesji
  // wcześniej zapisana konfiguracja wraca zamiast pustego stanu.
  hydrateFtConfig(pid);
  if (!ftConfig[pid]) {
    panel.innerHTML = '';
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', 'sparkle');
    empty.setAttribute('title', 'Skonfiguruj fine-tuning');
    empty.setAttribute('message', 'Skonfiguruj model i metodę w zakładce „Model bazowy", zanim uruchomisz trening.');
    panel.appendChild(empty);
    return;
  }

  const cfg = ftConfig[pid];
  const datasetOptions = datasets.map((d) => ({
    value: String(d.datasetId ?? d.dataset_id ?? ''),
    label: d.name || '(bez nazwy)',
  }));
  const onlyOne = datasetOptions.length === 1;

  panel.innerHTML = `
    <div class="ml-studio-ft">
      <section class="ml-studio-data-card" id="ml-studio-ft-setup">
        <div class="ml-studio-data-head">${sprite('cpu')} Konfiguracja treningu
          <span class="ml-studio-data-hint">podsumowanie z zakładki „Model bazowy"</span>
        </div>
        <div class="ml-studio-ft-summary">
          <div class="ml-studio-ft-sum-line"><span class="k">Model bazowy</span><span class="v">${escapeHtml(ftEffectiveBaseModel(cfg) || '(brak)')}</span></div>
          <div class="ml-studio-ft-sum-line"><span class="k">Metoda</span><span class="v">${escapeHtml(objectiveLabel(cfg.objective))} × ${escapeHtml(methodLabel(cfg.method))}</span></div>
          <div class="ml-studio-ft-sum-line"><span class="k">Hiperparametry</span><span class="v">lr ${escapeHtml(String(cfg.hyperparams.learningRate))} · batch ${escapeHtml(String(cfg.hyperparams.batchSize))} · ${escapeHtml(String(cfg.hyperparams.epochs))} epoki</span></div>
        </div>
        ${onlyOne ? '' : '<tf-select id="ml-studio-ft-dataset" label="Zbiór treningowy"></tf-select>'}
        <div class="ml-studio-ft-resources" style="display:flex;gap:10px;flex-wrap:wrap;align-items:flex-end;margin-top:8px">
          <tf-select id="ml-studio-ft-node" label="Węzeł treningowy" style="flex:1;min-width:200px"></tf-select>
          <tf-input type="number" id="ml-studio-ft-gpus" label="GPU (0 = wszystkie)" value="0" min="0" max="64" step="1" style="width:150px"></tf-input>
          <tf-toggle id="ml-studio-ft-multirig" label="Multi-rig (rozproszony)"></tf-toggle>
        </div>
        <div class="ml-studio-train-actions">
          <tf-button variant="primary" icon="play" id="ml-studio-ft-run">Uruchom trening</tf-button>
        </div>
      </section>

      <div id="ml-studio-ft-live"></div>
    </div>
  `;

  let datasetId = datasetOptions[0].value;
  if (!onlyOne) {
    const sel = byId('ml-studio-ft-dataset');
    sel?.setOptions(datasetOptions, datasetId);
    sel?.addEventListener('change', (e) => {
      datasetId = e.detail?.value || sel.value || datasetId;
    });
  }

  // Węzeł treningowy: „Lokalnie" + węzły mesh (zaufane). Pusty target = lokalnie.
  cfg.targetNodeId = cfg.targetNodeId || '';
  cfg.numGpus = cfg.numGpus ?? 0;
  cfg.multiRig = Boolean(cfg.multiRig);
  const nodeSel = byId('ml-studio-ft-node');
  if (nodeSel) {
    nodeSel.setOptions([{ value: '', label: 'Lokalnie (ten węzeł)' }], cfg.targetNodeId);
    ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' }).then((nodes) => {
      const opts = [{ value: '', label: 'Lokalnie (ten węzeł)' }];
      (nodes || []).forEach((n) => {
        const id = n.nodeId || n.node_id;
        if (!id) return;
        const host = n.hostname || n.host || '';
        opts.push({ value: id, label: `${host || id.slice(0, 12)} (${id.slice(0, 8)}…)` });
      });
      nodeSel.setOptions(opts, cfg.targetNodeId);
    }).catch(() => {});
    nodeSel.addEventListener('change', (e) => { cfg.targetNodeId = e.detail?.value || nodeSel.value || ''; });
  }
  byId('ml-studio-ft-gpus')?.addEventListener('change', (e) => {
    cfg.numGpus = Math.max(0, parseInt(e.detail?.value ?? e.target?.value ?? '0', 10) || 0);
  });
  byId('ml-studio-ft-multirig')?.addEventListener('change', (e) => {
    cfg.multiRig = Boolean(e.detail?.checked ?? e.target?.checked);
  });

  byId('ml-studio-ft-run')?.addEventListener('click', async () => {
    const baseModel = ftEffectiveBaseModel(cfg);
    if (!baseModel) {
      toast('Brak modelu bazowego — uzupełnij go w zakładce „Model bazowy".', 'error');
      return;
    }
    if (!datasetId) {
      toast('Wybierz zbiór treningowy.', 'error');
      return;
    }
    const teacherModel = (cfg.teacherModel || '').trim();
    if (cfg.objective === 'kd' && !teacherModel) {
      toast('KD wymaga modelu-nauczyciela (repo HF).', 'error');
      return;
    }
    if (cfg.multiRig && !cfg.targetNodeId) {
      toast('Multi-rig (rozproszony) wymaga wybrania węzła zdalnego jako master.', 'error');
      return;
    }
    const runBtn = byId('ml-studio-ft-run');
    runBtn?.setAttribute('disabled', '');
    try {
      const payload = {
        projectId: pid,
        datasetId,
        baseModel,
        method: cfg.method,
        objective: cfg.objective,
        teacherModel: cfg.objective === 'kd' ? teacherModel : null,
        mergeAdapter: cfg.method !== 'full' && Boolean(cfg.mergeAdapter),
        targetNodeId: cfg.targetNodeId || undefined,
        numGpus: cfg.numGpus || 0,
        // Multi-rig: A wypełni masterAddr (LAN-IP węzła zdalnego); UI deklaruje
        // tylko liczbę węzłów i port rendezvous. nnodes=2 (A + wybrany rig).
        dist: (cfg.multiRig && cfg.targetNodeId)
          ? { nnodes: 2, nodeRank: 0, masterAddr: '', masterPort: 29500 }
          : undefined,
        hyperparams: {
          learningRate: cfg.hyperparams.learningRate,
          batchSize: cfg.hyperparams.batchSize,
          gradAccumSteps: cfg.hyperparams.gradAccumSteps,
          epochs: cfg.hyperparams.epochs,
          loraR: cfg.hyperparams.loraR,
          loraAlpha: cfg.hyperparams.loraAlpha,
          loraDropout: cfg.hyperparams.loraDropout,
          maxSeqLen: cfg.hyperparams.maxSeqLen,
        },
      };
      const resp = await ApiBinary.one('mlStudioFtTrainStartRequest', payload);
      const runId = resp.runId ?? resp.run_id;
      if (!runId) throw new Error('Backend nie zwrócił runId.');
      // Po starcie chowamy konfigurację i pokazujemy widok LIVE z pollingiem.
      const setup = byId('ml-studio-ft-setup');
      if (setup) setup.hidden = true;
      startFtLive(byId('ml-studio-ft-live'), runId, { selectTab });
    } catch (err) {
      runBtn?.removeAttribute('disabled');
      toast(`Start treningu: ${err.message}`, 'error');
    }
  });
}

// Widok LIVE + polling co 2 s. Renderuje pasek postępu, KPI i krzywą loss (SVG)
// z danych statusu. Interwał czyszczony przez stopFtPolling (przy zakończeniu,
// przełączeniu zakładki lub unmount ekranu).
function startFtLive(host, runId, { selectTab }) {
  if (!host) return;
  stopFtPolling();
  host.innerHTML = `
    <section class="ml-studio-data-card ml-studio-ft-live">
      <div class="ml-studio-data-head">${sprite('cpu')} Trening na żywo
        <span class="ml-studio-ft-status" id="ml-studio-ft-status-badge"><tf-badge tone="warning" value="trening trwa"></tf-badge></span>
      </div>
      <div class="ml-studio-ft-progress">
        <div class="ml-studio-ft-progress-meta" id="ml-studio-ft-progress-meta">krok 0</div>
        <tf-progress-bar id="ml-studio-ft-progress-bar" value="0" tone="accent"></tf-progress-bar>
      </div>
      <div class="ml-studio-ft-kpi-grid" id="ml-studio-ft-kpi"></div>
      <div class="ml-studio-ft-chart-wrap">
        <div class="ml-studio-ft-chart-head">
          <span class="ml-studio-ft-chart-title">Krzywa loss</span>
          <span class="ml-studio-ft-chart-legend">
            <span class="lg"><span class="sw train"></span>train</span>
            <span class="lg"><span class="sw eval"></span>eval</span>
          </span>
        </div>
        <div id="ml-studio-ft-chart"></div>
      </div>
      <div class="ml-studio-ft-done" id="ml-studio-ft-done" hidden></div>
    </section>
  `;

  const renderStatus = (st) => {
    const status = String(st.status || 'running');
    // Faza transferu datasetu przez mesh (trening na zdalnym węźle) — pasek B/s.
    if (status === 'syncing') {
      const syncPhase = String(st.syncPhase ?? st.sync_phase ?? 'syncing');
      const sent = Number(st.syncBytesSent ?? st.sync_bytes_sent ?? 0);
      const tot = Number(st.syncBytesTotal ?? st.sync_bytes_total ?? 0);
      const rate = Number(st.syncRateBps ?? st.sync_rate_bps ?? 0);
      const pct = tot > 0 ? Math.max(0, Math.min(100, Math.round((sent / tot) * 100))) : 0;
      const phaseLabel = syncPhase === 'zipping' ? 'pakowanie datasetu'
        : syncPhase === 'starting' ? 'uruchamianie treningu na węźle'
        : 'transfer datasetu przez mesh';
      const meta = byId('ml-studio-ft-progress-meta');
      const bar = byId('ml-studio-ft-progress-bar');
      if (meta) {
        meta.innerHTML = syncPhase === 'syncing'
          ? `${phaseLabel} · ${fmtBytes(sent)} / ${fmtBytes(tot)} · ${pct}% · ${fmtRate(rate)}`
          : `<tf-spinner size="sm"></tf-spinner> ${phaseLabel}`;
      }
      if (bar) bar.setAttribute('value', String(pct));
      const kpi = byId('ml-studio-ft-kpi');
      if (kpi) {
        kpi.innerHTML = `
          <div class="ml-studio-ft-kpi"><div class="lbl">wysłano</div><div class="val">${fmtBytes(sent)}</div></div>
          <div class="ml-studio-ft-kpi"><div class="lbl">rozmiar</div><div class="val">${fmtBytes(tot)}</div></div>
          <div class="ml-studio-ft-kpi"><div class="lbl">prędkość</div><div class="val">${fmtRate(rate)}</div></div>`;
      }
      const badge = byId('ml-studio-ft-badge');
      if (badge) badge.innerHTML = '<tf-badge tone="info" value="transfer danych"></tf-badge>';
      return;
    }
    const step = Number(st.step ?? 0);
    const totalSteps = Number(st.totalSteps ?? st.total_steps ?? 0);
    const trainLoss = st.trainLoss ?? st.train_loss;
    const evalLoss = st.evalLoss ?? st.eval_loss;
    const curve = Array.isArray(st.lossCurve ?? st.loss_curve) ? (st.lossCurve ?? st.loss_curve) : [];

    // Pasek postępu — gdy znamy totalSteps liczymy procent, inaczej spinner.
    const meta = byId('ml-studio-ft-progress-meta');
    const bar = byId('ml-studio-ft-progress-bar');
    if (totalSteps > 0) {
      const pct = Math.max(0, Math.min(100, Math.round((step / totalSteps) * 100)));
      if (meta) meta.textContent = `krok ${step} / ${totalSteps} · ${pct}%`;
      if (bar) bar.setAttribute('value', String(pct));
    } else if (meta) {
      meta.innerHTML = `<tf-spinner size="sm"></tf-spinner> trwa — krok ${step}`;
      if (bar) bar.setAttribute('value', '0');
    }

    // KPI: tylko realne wartości ze statusu.
    const kpi = byId('ml-studio-ft-kpi');
    if (kpi) {
      kpi.innerHTML = `
        <div class="ml-studio-ft-kpi"><div class="lbl">train loss</div><div class="val">${trainLoss != null ? Number(trainLoss).toFixed(4) : '—'}</div></div>
        <div class="ml-studio-ft-kpi"><div class="lbl">eval loss</div><div class="val">${evalLoss != null ? Number(evalLoss).toFixed(4) : '—'}</div></div>
        <div class="ml-studio-ft-kpi"><div class="lbl">krok</div><div class="val">${step}${totalSteps > 0 ? ' / ' + totalSteps : ''}</div></div>
      `;
    }

    // Krzywa loss (inline SVG).
    const chart = byId('ml-studio-ft-chart');
    if (chart) chart.innerHTML = renderLossChart(curve);

    // Badge statusu.
    const badge = byId('ml-studio-ft-status-badge');
    if (badge) {
      if (status === 'succeeded') badge.innerHTML = '<tf-badge tone="success" value="zakończony"></tf-badge>';
      else if (status === 'failed') badge.innerHTML = '<tf-badge tone="danger" value="błąd"></tf-badge>';
      else badge.innerHTML = '<tf-badge tone="warning" value="trening trwa"></tf-badge>';
    }

    if (status === 'succeeded') {
      stopFtPolling();
      const done = byId('ml-studio-ft-done');
      if (done) {
        done.hidden = false;
        done.innerHTML = `
          <div class="ml-studio-ft-done-msg">${sprite('check')} Trening zakończony — model dostępny w zakładce „Modele".</div>
          <tf-button variant="outline" icon="layers" id="ml-studio-ft-goto-models">Przejdź do Modele</tf-button>
        `;
        byId('ml-studio-ft-goto-models')?.addEventListener('click', () => selectTab && selectTab('Modele'));
      }
      toast('Fine-tuning zakończony.', 'success');
    } else if (status === 'failed') {
      stopFtPolling();
      toast(`Trening nieudany: ${st.error || 'nieznany błąd'}`, 'error');
      const done = byId('ml-studio-ft-done');
      if (done) {
        done.hidden = false;
        done.innerHTML = `<div class="ml-studio-ft-done-msg error">${sprite('alert')} ${escapeHtml(st.error || 'Trening zakończył się błędem.')}</div>`;
      }
    }
  };

  const poll = async () => {
    try {
      const st = await ApiBinary.one('mlStudioFtTrainStatusRequest', { runId });
      renderStatus(st);
    } catch (err) {
      stopFtPolling();
      toast(`Polling statusu: ${err.message}`, 'error');
    }
  };

  poll();
  ftPollTimer = setInterval(poll, 2000);
}

// Krzywa loss jako inline SVG: oś X = step, Y = loss; dwie linie (train/eval).
// Skalowanie do min/max obu serii. Brak punktów → komunikat „brak danych".
function renderLossChart(curve) {
  const points = (curve || [])
    .map((c) => ({
      step: Number(c.step ?? 0),
      train: c.trainLoss ?? c.train_loss,
      evalv: c.evalLoss ?? c.eval_loss,
    }))
    .filter((c) => Number.isFinite(c.step));
  if (points.length < 2) {
    return '<div class="ml-studio-ft-chart-empty">Krzywa pojawi się po pierwszych krokach treningu.</div>';
  }

  const W = 600;
  const H = 220;
  const padX = 8;
  const padY = 12;
  const steps = points.map((p) => p.step);
  const minStep = Math.min(...steps);
  const maxStep = Math.max(...steps);
  const losses = [];
  for (const p of points) {
    if (Number.isFinite(p.train)) losses.push(p.train);
    if (Number.isFinite(p.evalv)) losses.push(p.evalv);
  }
  const minLoss = Math.min(...losses);
  const maxLoss = Math.max(...losses);
  const spanStep = maxStep - minStep || 1;
  const spanLoss = maxLoss - minLoss || 1;

  const x = (s) => padX + ((s - minStep) / spanStep) * (W - 2 * padX);
  const y = (l) => padY + (1 - (l - minLoss) / spanLoss) * (H - 2 * padY);

  const pathFor = (sel) => {
    const segs = [];
    let started = false;
    for (const p of points) {
      const v = sel(p);
      if (!Number.isFinite(v)) continue;
      segs.push(`${started ? 'L' : 'M'}${x(p.step).toFixed(1)},${y(v).toFixed(1)}`);
      started = true;
    }
    return segs.join(' ');
  };

  const trainPath = pathFor((p) => p.train);
  const evalPath = pathFor((p) => p.evalv);
  const grid = [40, 100, 160].map((gy) => `<line x1="0" y1="${gy}" x2="${W}" y2="${gy}" class="grid"/>`).join('');

  return `
    <svg class="ml-studio-ft-loss-svg" viewBox="0 0 ${W} ${H}" preserveAspectRatio="none" role="img" aria-label="Krzywa loss">
      <g>${grid}</g>
      ${trainPath ? `<path class="line train" d="${trainPath}"/>` : ''}
      ${evalPath ? `<path class="line eval" d="${evalPath}"/>` : ''}
    </svg>
  `;
}

// =============================================================================
// Recognition (RF-DETR) — zakładki detekcji obiektów. Dataset COCO podajemy
// PRZEZ ŚCIEŻKĘ do katalogu na serwerze (zbiory obrazów >> limit ramki WS).
// =============================================================================

const RECOG_VARIANTS = [
  { id: 'nano', name: 'Nano', desc: 'Najszybszy, najmniejszy — szybki PoC.' },
  { id: 'small', name: 'Small', desc: 'Kompromis szybkość/jakość.' },
  { id: 'medium', name: 'Medium', desc: 'Lepsza jakość, większy koszt.' },
  { id: 'base', name: 'Base', desc: 'Domyślny — dobra jakość (560px).' },
  { id: 'large', name: 'Large', desc: 'Najlepsza jakość, najwięcej VRAM.' },
];

const RECOG_HP = [
  { key: 'epochs', label: 'epoki', def: 50, step: '1', min: 1 },
  { key: 'batchSize', label: 'batch size', def: 4, step: '1', min: 1 },
  { key: 'gradAccum', label: 'grad accum', def: 4, step: '1', min: 1 },
  { key: 'learningRate', label: 'learning rate', def: 1e-4, step: '0.00001', min: 0 },
  { key: 'resolution', label: 'rozdzielczość', def: 576, step: '32', min: 224 },
];

// Warianty backbone'u klasyfikatora atrybutu (timm). Kolejność = od najlżejszego
// do najlepszej jakości; opisy trzymają się konwencji kart RF-DETR.
const CLF_VARIANTS = [
  { id: 'mobilenetv4', name: 'MobileNetV4', desc: 'Lekki — najszybszy, najmniejszy model.' },
  { id: 'efficientnet_b0', name: 'EfficientNet-B0', desc: 'Kompromis szybkość/jakość.' },
  { id: 'resnet50', name: 'ResNet-50', desc: 'Najlepsza jakość, większy koszt.' },
];

// Hiperparametry klasyfikatora (osobny zestaw niż detekcja). freezeBackbone jest
// przełącznikiem bool i trzymany jest poza tą listą (renderowany jako tf-toggle).
const CLF_HP = [
  { key: 'epochs', label: 'epoki', def: 40, step: '1', min: 1 },
  { key: 'batchSize', label: 'batch size', def: 32, step: '1', min: 1 },
  { key: 'learningRate', label: 'learning rate', def: 0.0003, step: '0.0001', min: 0 },
  { key: 'imageSize', label: 'rozmiar obrazu', def: 224, step: '32', min: 96 },
];

const recogCfg = {};
function defaultClfHyperparams() {
  const hp = {};
  for (const h of CLF_HP) hp[h.key] = h.def;
  hp.freezeBackbone = false;
  return hp;
}
function defaultRecogCfg() {
  const hyperparams = {};
  for (const h of RECOG_HP) hyperparams[h.key] = h.def;
  return {
    datasetId: '', target: 'detection', variant: 'base',
    attribute: '', sourceClass: '', clfVariant: 'mobilenetv4',
    clfHyperparams: defaultClfHyperparams(),
    targetNodeId: '', earlyStopping: true, hyperparams,
  };
}
function getRecogCfg(pid) {
  if (!recogCfg[pid]) recogCfg[pid] = defaultRecogCfg();
  return recogCfg[pid];
}

// Zakładka "Dane" dla recognition — układ z mockupu a-dane.html:
// źródło danych (upload/folder/kamera) → dropzone/ścieżka → galeria miniatur →
// podział train/val/test (liczony jak trener) → tabela zarejestrowanych zbiorów.
// Wszystkie realne ścieżki ingestu (build-from-files, build-from-folder,
// rejestracja COCO, lista datasetów, miniatury) są zachowane, tylko przełożone
// na kształt mockupu.
function renderRecogDataTab(panel, p) {
  const pid = projectId(p);
  panel.innerHTML = `
    <div class="ml-studio-data">

      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('cloud')} Źródło danych
          <span class="ml-studio-data-hint">upload · folder na serwerze · kamera TentaFlow</span>
        </div>
        <p class="ml-studio-data-origin-text" style="margin:0 0 12px">Wybierz skąd pochodzą zdjęcia. Obrazy są kopiowane, HEIC dekodowane, a z wideo wycinane klatki — powstaje dataset COCO gotowy do anotacji i treningu.</p>

        <div class="ml-studio-source-tiles" id="ml-studio-source-tiles">
          <button type="button" class="ml-studio-source-tile" data-source="upload">
            <span class="st-ico">${sprite('cloud')}</span>
            <span class="st-name">Upload plików</span>
            <span class="st-desc">Przeciągnij JPG/PNG/HEIC lub wskaż pliki z dysku.</span>
          </button>
          <button type="button" class="ml-studio-source-tile" data-source="folder">
            <span class="st-ico">${sprite('folder')}</span>
            <span class="st-name">Z folderu (na serwerze)</span>
            <span class="st-desc">Import całego katalogu obrazów/wideo z węzła.</span>
          </button>
          <button type="button" class="ml-studio-source-tile" data-source="camera">
            <span class="st-ico">${sprite('record')}</span>
            <span class="st-name">Kamera TentaFlow</span>
            <span class="st-desc">Klatki z kamery na żywo przez TentaVision.</span>
          </button>
        </div>

        <div class="ml-studio-source-body" id="ml-studio-source-upload">
          <div class="ml-studio-dropzone" id="ml-studio-dropzone">
            <span class="dz-ico">${sprite('cloud')}</span>
            <span class="dz-title">Upuść zdjęcia tutaj lub kliknij, aby wczytać</span>
            <span class="dz-sub">Obsługiwane: JPG, PNG, HEIC, MP4, MOV · z wideo wycinane są klatki</span>
            <span class="dz-formats">
              <span class="tf-chip">JPG</span><span class="tf-chip">PNG</span><span class="tf-chip">HEIC</span><span class="tf-chip">MP4</span><span class="tf-chip">MOV</span>
            </span>
            <tf-file-input id="ml-studio-recog-build-files" class="ml-studio-dropzone-input" accept=".jpg,.jpeg,.png,.heic,.mp4,.mov" multiple label="Wybierz pliki"></tf-file-input>
          </div>
        </div>

        <div class="ml-studio-source-body" id="ml-studio-source-folder" hidden>
          <tf-input id="ml-studio-recog-build-srcdir" label="Folder na serwerze (ścieżka)" placeholder="np. /mnt/dane/adr"></tf-input>
          <p class="ml-studio-data-hint" style="margin:6px 0 0">Core czyta media wprost z dysku węzła — nic nie jest wgrywane przez przeglądarkę.</p>
        </div>

        <div class="ml-studio-source-body" id="ml-studio-source-camera" hidden>
          <div class="ml-studio-callout">
            <span class="co-ico">${sprite('info')}</span>
            <p>Pobieranie klatek z kamer na żywo odbywa się w <strong>TentaVision</strong> — tam wybierzesz kamerę i zapiszesz nagrania jako materiał źródłowy, a następnie wczytasz je tutaj jako folder na serwerze. Bezpośrednie wpięcie kamery w ML Studio nie jest jeszcze podłączone.</p>
          </div>
        </div>

        <div class="ml-studio-source-fields" id="ml-studio-source-fields">
          <tf-input id="ml-studio-recog-build-name" label="Nazwa datasetu" placeholder="np. ADR z terenu" style="flex:1;min-width:200px"></tf-input>
          <tf-input id="ml-studio-recog-build-fps" type="number" label="Klatki/s z wideo" value="5" min="1" max="60" style="min-width:140px"></tf-input>
          <tf-button variant="primary" icon="plus" id="ml-studio-recog-build">Zbuduj dataset</tf-button>
        </div>
        <div id="ml-studio-recog-build-progress" class="ml-studio-data-hint" style="margin-top:8px"></div>

        <details class="ml-studio-coco-register">
          <summary>${sprite('database')} Mam już katalog COCO na serwerze — zarejestruj bez budowania</summary>
          <p class="ml-studio-data-origin-text" style="margin:10px 0">Zbiory detekcji to dziesiątki/setki MB obrazów — podajesz ŚCIEŻKĘ do katalogu COCO (z <code>_annotations.coco.json</code>), nie wgrywasz bajtów. Klasy i liczba obrazów są czytane z plików COCO.</p>
          <div class="ml-studio-source-fields">
            <tf-input id="ml-studio-recog-path" label="Ścieżka katalogu COCO" placeholder="/home/.../dataset_aug" style="flex:1;min-width:260px"></tf-input>
            <tf-input id="ml-studio-recog-name" label="Nazwa (opcjonalnie)" placeholder="np. ADR" style="min-width:180px"></tf-input>
            <tf-button variant="secondary" icon="plus" id="ml-studio-recog-register">Zarejestruj dataset</tf-button>
          </div>
        </details>
      </section>

      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('image')} Galeria podglądu
          <span class="ml-studio-data-hint" id="ml-studio-gallery-meta">—</span>
        </div>
        <div class="ml-studio-gallery" id="ml-studio-gallery"></div>
      </section>

      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('grid-2x2')} Podział TRAIN / VAL / TEST
          <span class="ml-studio-data-hint">liczony przez trener (deterministyczny stride, bez RNG)</span>
        </div>
        <p class="ml-studio-data-origin-text" style="margin:0 0 12px">Trener tworzy efemeryczny split: co 7. obraz trafia do <strong>val</strong>, przedostatni z siódemki do <strong>test</strong>, reszta to <strong>train</strong>. Liczby poniżej są wyliczone z liczby wgranych obrazów — nie wpisywane ręcznie.</p>
        <div class="ml-studio-split-bar" id="ml-studio-split-bar">
          <span class="seg-train" style="width:0%"></span>
          <span class="seg-val" style="width:0%"></span>
          <span class="seg-test" style="width:0%"></span>
        </div>
        <div class="ml-studio-split-stats" id="ml-studio-split-stats"></div>
      </section>

      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('database')} Zarejestrowane zbiory</div>
        <div id="ml-studio-datasets"></div>
      </section>
    </div>
  `;

  // ----- Wybór źródła (segmentowane kafelki) -----
  const sourceBodies = {
    upload: byId('ml-studio-source-upload'),
    folder: byId('ml-studio-source-folder'),
    camera: byId('ml-studio-source-camera'),
  };
  const buildFields = byId('ml-studio-source-fields');
  const buildProgress = byId('ml-studio-recog-build-progress');
  function selectSource(src) {
    byId('ml-studio-source-tiles')?.querySelectorAll('.ml-studio-source-tile').forEach((el) => {
      el.classList.toggle('selected', el.getAttribute('data-source') === src);
    });
    for (const [key, el] of Object.entries(sourceBodies)) { if (el) el.hidden = key !== src; }
    // Kamera nie buduje datasetu w ML Studio — chowamy pola budowy, by nie udawać akcji.
    const cameraOnly = src === 'camera';
    if (buildFields) buildFields.hidden = cameraOnly;
    if (buildProgress) buildProgress.hidden = cameraOnly;
  }
  byId('ml-studio-source-tiles')?.querySelectorAll('.ml-studio-source-tile').forEach((el) => {
    el.addEventListener('click', () => selectSource(el.getAttribute('data-source')));
  });
  selectSource('upload');

  let recogBuildFiles = [];
  byId('ml-studio-recog-build-files')?.addEventListener('change', (e) => {
    const files = e.detail?.files;
    recogBuildFiles = files && files.length ? Array.from(files) : [];
    const prog = byId('ml-studio-recog-build-progress');
    if (prog) prog.textContent = recogBuildFiles.length ? `Wybrano plików: ${recogBuildFiles.length}` : '';
  });

  byId('ml-studio-recog-build')?.addEventListener('click', async () => {
    const name = (byId('ml-studio-recog-build-name')?.value || '').trim();
    const fps = Math.max(1, parseInt(byId('ml-studio-recog-build-fps')?.value || '5', 10) || 5);
    // Read the current selection straight from the live <input> at click time. The
    // change-captured `recogBuildFiles` can be stale if the tab re-rendered (e.g. the
    // datasets list refreshed) after the user picked files, so the DOM is the source
    // of truth here; fall back to the captured list if the input is gone.
    const liveInput = byId('ml-studio-recog-build-files')?.querySelector('input[type="file"]');
    const files = liveInput && liveInput.files && liveInput.files.length
      ? Array.from(liveInput.files)
      : recogBuildFiles;
    // A server folder path makes the file picker optional: when given, Core reads
    // media straight from disk and no upload happens.
    const sourceDir = (byId('ml-studio-recog-build-srcdir')?.value || '').trim();
    if (!sourceDir && !files.length) { toast('Wybierz pliki lub podaj folder na serwerze.', 'error'); return; }
    if (!name) { toast('Podaj nazwę datasetu.', 'error'); return; }
    const btn = byId('ml-studio-recog-build');
    const prog = byId('ml-studio-recog-build-progress');
    btn?.setAttribute('disabled', '');
    try {
      if (!sourceDir) {
        const total = files.length;
        for (let i = 0; i < total; i += 1) {
          if (prog) prog.textContent = `Wysyłanie ${i + 1} / ${total}: ${files[i].name}`;
          await stageRecogMedia(pid, files[i]);
        }
      }
      if (prog) prog.textContent = 'Uruchamianie budowy datasetu…';
      const resp = await ApiBinary.one('mlStudioRecogBuildDatasetRequest', {
        projectId: pid, datasetName: name, fps, sourceDir: sourceDir || undefined,
      });
      const buildId = resp.buildId ?? resp.build_id ?? '';
      if (resp.error || !buildId) {
        toast(`Budowa datasetu: ${resp.error || 'nie udało się uruchomić'}`, 'error');
        if (prog) prog.textContent = '';
        return;
      }
      // Budowa biegnie ASYNCHRONICZNIE w tle — odpytuj postęp do zakończenia.
      await pollRecogBuild(pid, buildId, prog);
      recogBuildFiles = [];
      loadDataPreview(pid);
    } catch (err) {
      toast(`Budowa datasetu: ${err.message}`, 'error');
      if (prog) prog.textContent = '';
    } finally {
      btn?.removeAttribute('disabled');
    }
  });

  byId('ml-studio-recog-register')?.addEventListener('click', async () => {
    const path = (byId('ml-studio-recog-path')?.value || '').trim();
    const name = (byId('ml-studio-recog-name')?.value || '').trim();
    if (!path) { toast('Podaj ścieżkę katalogu COCO.', 'error'); return; }
    const btn = byId('ml-studio-recog-register');
    btn?.setAttribute('disabled', '');
    try {
      const resp = await ApiBinary.one('mlStudioRecogDatasetRegisterRequest', { projectId: pid, name, path });
      const d = resp.dataset || {};
      const classes = d.columnCount ?? d.column_count ?? 0;
      const imgs = d.rowCount ?? d.row_count ?? 0;
      toast(`Dataset zarejestrowany: ${imgs} obrazów, ${classes} klas.`, 'success');
      loadDatasets(pid);
      loadDataPreview(pid);
    } catch (err) {
      toast(`Rejestracja datasetu: ${err.message}`, 'error');
    } finally {
      btn?.removeAttribute('disabled');
    }
  });
  loadDatasets(pid);
  loadDataPreview(pid);
}

// Liczba obrazów per split, identyczna z trenerem `prepare_dataset_with_valid`
// (train_recognition.rs): co VALID_HOLDOUT_STRIDE-ty obraz → val, przedostatni
// z okna → test, reszta → train; każdy split eval ma min. 1 obraz dla małych
// zbiorów. Liczymy z samej liczby obrazów (kolejność stride jest deterministyczna).
function recogSplitCounts(total) {
  const STRIDE = 7;
  if (total <= 0) return { total: 0, train: 0, val: 0, test: 0 };
  let val = 0;
  let test = 0;
  for (let i = 0; i < total; i += 1) {
    const m = i % STRIDE;
    if (m === STRIDE - 1) val += 1;
    else if (m === STRIDE - 2) test += 1;
  }
  if (val === 0 && total >= 1) val = 1;
  if (test === 0 && total - val >= 1) test = 1;
  const train = Math.max(0, total - val - test);
  return { total, train, val, test };
}

// Ładuje podgląd danych dla zakładki „Dane": galeria miniatur z pierwszego
// datasetu COCO (mlStudioRecogImagesListRequest + mlStudioRecogImageRequest)
// oraz podział train/val/test policzony z liczby obrazów. Galeria jest ograniczona
// do pierwszych 23 miniatur + kafelek „+N" (jak w mockupie).
async function loadDataPreview(pid) {
  const gallery = byId('ml-studio-gallery');
  const galleryMeta = byId('ml-studio-gallery-meta');
  const splitStats = byId('ml-studio-split-stats');
  const splitBar = byId('ml-studio-split-bar');
  if (!gallery && !splitStats) return;

  const renderSplit = (total, datasetName) => {
    const s = recogSplitCounts(total);
    if (splitBar) {
      const pct = (n) => (s.total ? (n / s.total) * 100 : 0);
      const segs = splitBar.querySelectorAll('span');
      if (segs[0]) segs[0].style.width = `${pct(s.train)}%`;
      if (segs[1]) segs[1].style.width = `${pct(s.val)}%`;
      if (segs[2]) segs[2].style.width = `${pct(s.test)}%`;
    }
    if (splitStats) {
      const card = (lbl, val, foot, cls) => `
        <div class="ml-studio-split-stat${cls ? ' ' + cls : ''}">
          <div class="ss-lbl">${lbl}</div>
          <div class="ss-val">${formatNumber(val)}</div>
          <div class="ss-foot">${foot}</div>
        </div>`;
      splitStats.innerHTML = total > 0
        ? card('Wgrane', s.total, datasetName ? escapeHtml(datasetName) : 'wszystkie obrazy')
          + card('Train', s.train, 'do treningu RF-DETR', 'train')
          + card('Val', s.val, 'metryki w czasie treningu', 'val')
          + card('Test', s.test, 'końcowa ewaluacja', 'test')
        : `<div class="ml-studio-data-hint">Podział pojawi się po wczytaniu pierwszego datasetu.</div>`;
    }
  };

  try {
    const resp = await ApiBinary.one('mlStudioDatasetsListRequest', { projectId: pid });
    const datasets = (Array.isArray(resp.datasets) ? resp.datasets : [])
      .filter((d) => (d.kind ?? '') === 'coco_path');
    if (!datasets.length) {
      if (gallery) {
        const empty = document.createElement('tf-empty-state');
        empty.setAttribute('icon', 'image');
        empty.setAttribute('title', 'Brak obrazów');
        empty.setAttribute('message', 'Wgraj pliki, wskaż folder na serwerze lub zarejestruj katalog COCO — miniatury pojawią się tutaj.');
        gallery.innerHTML = '';
        gallery.appendChild(empty);
      }
      if (galleryMeta) galleryMeta.textContent = '0 obrazów';
      renderSplit(0, '');
      return;
    }
    const ds = datasets[0];
    const datasetId = ds.datasetId ?? ds.dataset_id;
    const datasetName = ds.name || '';
    if (gallery) gallery.innerHTML = '<div class="ml-studio-loading"><tf-spinner></tf-spinner></div>';

    const imgResp = await ApiBinary.one('mlStudioRecogImagesListRequest', { datasetId });
    const images = JSON.parse(imgResp.imagesJson ?? imgResp.images_json ?? '[]');
    const total = images.length;
    if (galleryMeta) galleryMeta.textContent = `${formatNumber(total)} obrazów${datasetName ? ' · ' + escapeHtml(datasetName) : ''}`;
    renderSplit(total, datasetName);

    if (gallery) {
      const MAX_THUMBS = 23;
      const shown = images.slice(0, MAX_THUMBS);
      const overflow = total - shown.length;
      gallery.innerHTML = shown.map((im) => `
        <div class="ml-studio-thumb" data-image-id="${escapeAttr(String(im.image_id))}" title="${escapeAttr(im.file_name || '')}">
          ${sprite('image')}
          <span class="it-name">${escapeHtml(im.file_name || '')}</span>
        </div>`).join('')
        + (overflow > 0 ? `<div class="ml-studio-thumb more">+${overflow}</div>` : '');

      // Lazy-load miniatur (base64) — pojedyncze żądania per obraz, bez blokowania.
      for (const im of shown) {
        const cell = gallery.querySelector(`.ml-studio-thumb[data-image-id="${CSS.escape(String(im.image_id))}"]`);
        if (!cell) continue;
        ApiBinary.one('mlStudioRecogImageRequest', { datasetId, imageId: im.image_id }).then((r) => {
          const b64 = r.imageB64 ?? r.image_b64;
          if (!b64 || !cell.isConnected) return;
          cell.style.backgroundImage = `url(data:${r.mime || 'image/jpeg'};base64,${b64})`;
          cell.classList.add('has-img');
        }).catch(() => {});
      }
    }
  } catch (err) {
    if (gallery) gallery.innerHTML = `<div class="ml-studio-ft-done-msg error">${escapeHtml(err.message)}</div>`;
    renderSplit(0, '');
  }
}

// Zakładka "Anotacje" — edytor bboxów COCO: galeria obrazów + płótno z rysowaniem,
// przesuwaniem, resize (uchwyty), zmianą klasy, usuwaniem; zapis do COCO.
// Zakładka "Anotacje" rozpoznawania — układ 3-kolumnowy (port z mockupu
// r02-anotacja.html, namespaced prefiksami ml-studio-annotate-):
//   LEWA   — postęp datasetu + lista obrazów ze statusem ramek,
//   CENTRUM— płótno z edytowalnymi ramkami (rysowanie/przesuwanie/skala),
//   PRAWA  — pre-label modelem (RF-DETR działa, OWLv2/Qwen3-VL „wkrótce"),
//            klasy ze schematu (kolor + skrót cyfrowy) oraz panel atrybutów
//            zaznaczonej ramki generowany z atrybutów klasy w schemacie.
// Klasy i atrybuty pochodzą ze schematu projektu (mlStudioSchemaGetRequest);
// gdy schemat jest pusty, klasy spadają do kategorii COCO datasetu.
function renderRecogAnnotateTab(panel, p) {
  const pid = projectId(p);
  // Stan edytora. `boxes[i]` niesie też `score`/`predicted` (ramki z pre-labelu)
  // oraz `attributes` (wartości atrybutów ze schematu) — round-trip przez COCO.
  const S = {
    datasetId: '', images: [], categories: [], curIdx: -1,
    origW: 0, origH: 0, boxes: [], sel: -1, dirty: false,
    drag: null, // {mode:'new'|'move'|'resize', handle, startX, startY, orig}
    schemaClasses: [], dicts: [], prelabelModel: 'rf-detr', threshold: 0.5,
  };

  // Modele pre-label: tylko RF-DETR jest podpięty pod wbudowany autolabel.
  // OWLv2 i Qwen3-VL pojawiają się w pickerze (zgodnie z mockupem), ale są
  // oznaczone „wkrótce" i wyłączone — żaden klik nie udaje działania.
  const PRELABEL_MODELS = [
    { id: 'rf-detr', name: 'RF-DETR', meta: 'detekcja boxów · ramki + klasa', ready: true },
    { id: 'owlv2', name: 'OWLv2', meta: 'open-vocab · po opisie tekstowym', ready: false },
    { id: 'qwen3-vl', name: 'Qwen3-VL', meta: 'multimodal · ramki + atrybuty (OCR)', ready: false },
  ];

  panel.innerHTML = `
    <div class="ml-studio-annotate">
      <div class="ml-studio-annotate-layout">

        <aside class="ml-studio-annotate-tasks">
          <div class="ml-studio-annotate-progress-row">
            <tf-progress-bar id="ml-studio-annotate-progress" value="0"></tf-progress-bar>
            <span class="ml-studio-annotate-pct" id="ml-studio-annotate-pct">0%</span>
          </div>
          <div class="ml-studio-annotate-progress-meta" id="ml-studio-annotate-progress-meta">0 z 0 oznaczonych</div>
          <tf-select id="ml-studio-annotate-dataset" label="Dataset COCO"></tf-select>
          <div class="ml-studio-annotate-task-list" id="ml-studio-annotate-task-list"></div>
        </aside>

        <section class="ml-studio-annotate-canvas">
          <div class="ml-studio-annotate-toolbar" id="ml-studio-annotate-toolbar"></div>
          <div class="ml-studio-annotate-stage" id="ml-studio-annotate-stage"></div>
        </section>

        <aside class="ml-studio-annotate-side" id="ml-studio-annotate-side">
          <tf-button class="ml-studio-prelabel-btn" id="ml-studio-annotate-prelabel" variant="primary" icon="zap">Pre-oznacz modelem</tf-button>
          <span class="ml-studio-data-hint" id="ml-studio-annotate-prelabel-prog"></span>

          <div class="ml-studio-annotate-card">
            <div class="ml-studio-annotate-card-title">Model wstępnie oznaczający (z serwisu)</div>
            <div class="ml-studio-annotate-models" id="ml-studio-annotate-models"></div>
            <tf-input id="ml-studio-annotate-threshold" type="number" label="Próg detekcji" value="0.5" min="0.5" max="1" step="0.05"></tf-input>
            <p class="ml-studio-annotate-card-note">Predykcje pojawią się jako kropkowane ramki — akceptuj (klik) lub popraw.</p>
          </div>

          <div class="ml-studio-annotate-card">
            <div class="ml-studio-annotate-card-title">${sprite('grid-2x2')} Klasy (ze schematu)</div>
            <div class="ml-studio-annotate-classes" id="ml-studio-annotate-classes"></div>
          </div>

          <div class="ml-studio-annotate-card" id="ml-studio-annotate-attrs">
            <div class="ml-studio-annotate-card-title">Atrybuty zaznaczonej ramki</div>
            <div id="ml-studio-annotate-attrs-body"></div>
          </div>
        </aside>

      </div>
    </div>
  `;

  renderModelPicker();
  byId('ml-studio-annotate-threshold')?.addEventListener('change', (e) => {
    const v = Number(e.detail?.value ?? 0.5);
    S.threshold = v >= 0.5 && v <= 1 ? v : 0.5;
  });

  // Lista datasetów coco_path do selecta + ładowanie schematu/słowników raz.
  (async () => {
    await Promise.all([loadSchema(), loadDicts()]);
    const sel = byId('ml-studio-annotate-dataset');
    try {
      const resp = await ApiBinary.one('mlStudioDatasetsListRequest', { projectId: pid });
      const list = (resp.datasets || []).filter((d) => (d.kind || '') === 'coco_path');
      const opts = list.map((d) => ({ value: d.datasetId ?? d.dataset_id, label: `${d.name} (${d.rowCount ?? d.row_count ?? 0} obr.)` }));
      if (sel?.setOptions) sel.setOptions(opts, opts.length ? opts[0].value : null);
      if (opts.length) { S.datasetId = opts[0].value; await loadImages(); }
      sel?.addEventListener('change', async (e) => { S.datasetId = e.detail?.value || sel.value; await loadImages(); });
    } catch (err) { byId('ml-studio-annotate-task-list').innerHTML = `<div class="ml-studio-ft-done-msg error">${escapeHtml(err.message)}</div>`; }
  })();

  async function loadSchema() {
    try {
      const resp = await ApiBinary.one('mlStudioSchemaGetRequest', { projectId: pid });
      const schema = JSON.parse(resp.schemaJson ?? resp.schema_json ?? '{}');
      S.schemaClasses = Array.isArray(schema.classes) ? schema.classes : [];
    } catch { S.schemaClasses = []; }
  }

  async function loadDicts() {
    try {
      const resp = await ApiBinary.one('mlStudioLookupDictsListRequest', { projectId: pid });
      S.dicts = JSON.parse(resp.dictsJson ?? resp.dicts_json ?? '[]');
    } catch { S.dicts = []; }
  }

  // Klasy do panelu prawego + mapy nazwa↔kolor: ze schematu, a gdy pusty —
  // z kategorii COCO datasetu (zgodnie z zasiewem zakładki Schemat).
  function effectiveClasses() {
    if (S.schemaClasses.length) {
      return S.schemaClasses.map((c, i) => ({
        name: c.name, color: c.color || RECOG_SCHEMA_PALETTE[i % RECOG_SCHEMA_PALETTE.length],
        attributes: Array.isArray(c.attributes) ? c.attributes : [],
      }));
    }
    const cats = S.categories.length > 1 ? S.categories.filter((c) => c.id !== 0) : S.categories;
    return cats.map((c, i) => ({ name: c.name, color: RECOG_SCHEMA_PALETTE[i % RECOG_SCHEMA_PALETTE.length], attributes: [] }));
  }

  function schemaClassFor(box) {
    const name = catName(box.category_id);
    return effectiveClasses().find((c) => c.name === name) || null;
  }

  function classColor(name) {
    const c = effectiveClasses().find((x) => x.name === name);
    return c ? c.color : null;
  }

  // ----- Picker modeli pre-label -----
  function renderModelPicker() {
    const host = byId('ml-studio-annotate-models');
    if (!host) return;
    host.innerHTML = PRELABEL_MODELS.map((m) => `
      <div class="ml-studio-model-pick${m.id === S.prelabelModel ? ' selected' : ''}${m.ready ? '' : ' disabled'}" data-model="${escapeAttr(m.id)}">
        <div>
          <div class="m-name">${escapeHtml(m.name)}</div>
          <div class="m-meta">${escapeHtml(m.meta)}</div>
        </div>
        ${m.ready
          ? (m.id === S.prelabelModel ? `<span class="m-check">${sprite('check')}</span>` : '')
          : '<tf-chip status="warning" label="wkrótce"></tf-chip>'}
      </div>`).join('');
    host.querySelectorAll('.ml-studio-model-pick').forEach((el) => {
      const id = el.getAttribute('data-model');
      const model = PRELABEL_MODELS.find((m) => m.id === id);
      if (!model?.ready) return;
      el.addEventListener('click', () => { S.prelabelModel = id; renderModelPicker(); });
    });
  }

  // ----- Pre-label (RF-DETR) -----
  byId('ml-studio-annotate-prelabel')?.addEventListener('click', async () => {
    if (S.prelabelModel !== 'rf-detr') { toast('Wybrany model nie jest jeszcze podpięty.', 'error'); return; }
    if (!S.datasetId) { toast('Wybierz dataset COCO.', 'error'); return; }
    const btn = byId('ml-studio-annotate-prelabel');
    const prog = byId('ml-studio-annotate-prelabel-prog');
    btn.setAttribute('disabled', '');
    try {
      const resp = await ApiBinary.one('mlStudioRecogAutolabelRequest', { datasetId: S.datasetId, threshold: S.threshold, mode: 'only_empty' });
      if (resp.status === 'failed' || resp.error) throw new Error(resp.error || 'start nieudany');
      const jobId = resp.jobId ?? resp.job_id;
      if (prog) prog.textContent = 'Pre-oznaczanie…';
      await pollRecogAutolabel(jobId, prog, async () => {
        await loadImages();
        if (S.curIdx >= 0) await selectImage(S.curIdx);
      });
    } catch (err) {
      if (prog) prog.textContent = '';
      toast(`Pre-oznaczanie: ${err.message}`, 'error');
    } finally {
      btn.removeAttribute('disabled');
    }
  });

  async function loadImages() {
    const host = byId('ml-studio-annotate-task-list');
    host.innerHTML = '<tf-spinner></tf-spinner>';
    try {
      const resp = await ApiBinary.one('mlStudioRecogImagesListRequest', { datasetId: S.datasetId });
      S.images = JSON.parse(resp.imagesJson ?? resp.images_json ?? '[]');
      S.categories = JSON.parse(resp.categoriesJson ?? resp.categories_json ?? '[]');
      renderProgress();
      renderTaskList();
      renderClassPanel();
      if (S.images.length) selectImage(0);
      else { byId('ml-studio-annotate-stage').innerHTML = '<div class="ml-studio-ft-chart-empty">Brak obrazów w datasecie.</div>'; }
    } catch (err) { host.innerHTML = `<div class="ml-studio-ft-done-msg error">${escapeHtml(err.message)}</div>`; }
  }

  function renderProgress() {
    const total = S.images.length;
    const labeled = S.images.filter((im) => (im.ann_count || 0) > 0).length;
    const approved = S.images.filter((im) => im.approved === true).length;
    const pct = total ? Math.round((labeled / total) * 100) : 0;
    byId('ml-studio-annotate-progress')?.setAttribute('value', String(pct));
    byId('ml-studio-annotate-pct').textContent = `${pct}%`;
    byId('ml-studio-annotate-progress-meta').textContent = I18n.t('ml_studio.annotate.progress_meta', { labeled, total, approved });
  }

  function imageStatus(im, i) {
    if (i === S.curIdx) {
      const n = S.boxes.length ? S.boxes.length : (im.ann_count || 0);
      return `${I18n.t('ml_studio.annotate.editing_now')}${n ? ` · ${boxCount(n)}` : ''}`;
    }
    const n = im.ann_count || 0;
    if (im.approved) return `${I18n.t('ml_studio.annotate.approved')}${n ? ` · ${boxCount(n)}` : ''}`;
    return n > 0 ? boxCount(n) : I18n.t('ml_studio.annotate.unlabeled');
  }

  // Polski zachowuje deklinację (ramka/ramki/ramek); pozostałe języki używają
  // zlokalizowanego szablonu „{n} boxes".
  function plRamki(n) { return n === 1 ? 'ramka' : (n >= 2 && n <= 4 ? 'ramki' : 'ramek'); }
  function boxCount(n) {
    return I18n.getLanguage() === 'pl' ? `${n} ${plRamki(n)}` : I18n.t('ml_studio.annotate.boxes', { n });
  }

  function renderTaskList() {
    const host = byId('ml-studio-annotate-task-list');
    host.innerHTML = S.images.map((im, i) => {
      const labeled = (im.ann_count || 0) > 0;
      const approved = im.approved === true;
      const active = i === S.curIdx;
      const metaClass = approved && !active ? ' approved' : (labeled && !active ? ' ok' : '');
      return `
        <div class="ml-studio-task-item${active ? ' active' : ''}${approved ? ' approved' : ''}" data-idx="${i}">
          <div class="t-thumb">${sprite('image')}</div>
          <div class="t-body">
            <div class="t-name">${escapeHtml(im.file_name)}</div>
            <div class="t-meta${metaClass}">${active ? '' : (approved || labeled ? sprite('check') : '')}${escapeHtml(imageStatus(im, i))}</div>
          </div>
        </div>`;
    }).join('');
    host.querySelectorAll('.ml-studio-task-item').forEach((el) => {
      el.addEventListener('click', () => maybeLeave(() => selectImage(Number(el.getAttribute('data-idx')))));
    });
  }

  // ----- Panel klas (prawa kolumna) ze skrótami 1-9 -----
  function renderClassPanel() {
    const host = byId('ml-studio-annotate-classes');
    if (!host) return;
    const classes = effectiveClasses();
    host.innerHTML = classes.map((c, i) => `
      <div class="ml-studio-class-row" data-class="${escapeAttr(c.name)}">
        <span class="c-swatch" style="background:${escapeAttr(c.color)}"></span>
        <span class="c-name">${escapeHtml(c.name)}</span>
        ${i < 9 ? `<span class="c-key">${i + 1}</span>` : ''}
      </div>`).join('')
      + `<a class="ml-studio-class-add" href="#/ml-studio/${escapeAttr(pid)}?tab=schemat">${sprite('plus')} Dodaj klasę w schemacie</a>`;
    host.querySelectorAll('.ml-studio-class-row').forEach((el) => {
      el.addEventListener('click', () => assignClass(el.getAttribute('data-class')));
    });
  }

  // Przypisuje klasę o danej nazwie zaznaczonej ramce (po cat_id z kategorii COCO).
  function assignClass(name) {
    if (S.sel < 0) { toast('Najpierw zaznacz ramkę.', 'error'); return; }
    const cat = S.categories.find((c) => c.name === name);
    if (!cat) { toast('Klasa nie ma odpowiednika w kategoriach datasetu.', 'error'); return; }
    S.boxes[S.sel].category_id = cat.id; S.dirty = true;
    drawBoxes(); renderAttrPanel();
  }

  function maybeLeave(fn) {
    if (S.dirty && !confirm('Masz niezapisane zmiany. Porzucić je?')) return;
    fn();
  }

  async function selectImage(idx) {
    if (idx < 0 || idx >= S.images.length) return;
    S.curIdx = idx; S.sel = -1; S.dirty = false;
    renderTaskList();
    const stage = byId('ml-studio-annotate-stage');
    stage.innerHTML = '<tf-spinner></tf-spinner>';
    try {
      const im = S.images[idx];
      const resp = await ApiBinary.one('mlStudioRecogImageRequest', { datasetId: S.datasetId, imageId: im.image_id });
      if (resp.error) throw new Error(resp.error);
      S.origW = resp.origWidth ?? resp.orig_width ?? im.width;
      S.origH = resp.origHeight ?? resp.orig_height ?? im.height;
      const anns = JSON.parse(resp.annotationsJson ?? resp.annotations_json ?? '[]');
      S.boxes = anns.map((a) => ({
        category_id: a.category_id, x: a.bbox[0], y: a.bbox[1], w: a.bbox[2], h: a.bbox[3],
        score: typeof a.score === 'number' ? a.score : null,
        predicted: a.predicted === true,
        attributes: a.attributes && typeof a.attributes === 'object' ? a.attributes : {},
      }));
      renderStage(`data:${resp.mime || 'image/jpeg'};base64,${resp.imageB64 ?? resp.image_b64}`);
      renderToolbar();
      renderAttrPanel();
    } catch (err) { stage.innerHTML = `<div class="ml-studio-ft-done-msg error">${escapeHtml(err.message)}</div>`; }
  }

  function catName(id) { const c = S.categories.find((c) => c.id === id); return c ? c.name : String(id); }
  function catColor(id) {
    const col = classColor(catName(id));
    return col || `hsl(${(id * 67) % 360} 85% 55%)`;
  }
  function defaultCat() { return S.categories.length ? S.categories[S.categories.length > 1 && S.categories[0].id === 0 ? 1 : 0].id : 0; }

  function renderToolbar() {
    const tb = byId('ml-studio-annotate-toolbar');
    const im = S.images[S.curIdx];
    tb.innerHTML = `
      <tf-button id="ml-studio-annotate-prev" variant="ghost" icon="chevron-left"></tf-button>
      <span class="ml-studio-annotate-pos">${S.curIdx + 1}/${S.images.length}</span>
      <tf-button id="ml-studio-annotate-next" variant="ghost" icon="chevron-right"></tf-button>
      <span class="ml-studio-annotate-filemeta">${escapeHtml(im?.file_name || '')} · ${S.origW}×${S.origH}</span>
      <div class="ml-studio-annotate-save-group">
        <tf-button id="ml-studio-annotate-save" variant="secondary" icon="download">${escapeHtml(I18n.t('ml_studio.annotate.save'))}</tf-button>
        <tf-button id="ml-studio-annotate-save-approve" variant="primary" icon="check">${escapeHtml(I18n.t('ml_studio.annotate.save_and_approve'))}</tf-button>
      </div>`;
    byId('ml-studio-annotate-prev').addEventListener('click', () => maybeLeave(() => selectImage(S.curIdx - 1)));
    byId('ml-studio-annotate-next').addEventListener('click', () => maybeLeave(() => selectImage(S.curIdx + 1)));
    byId('ml-studio-annotate-save').addEventListener('click', () => saveAnns(false));
    byId('ml-studio-annotate-save-approve').addEventListener('click', () => saveAnns(true));
  }

  function renderStage(src) {
    const stage = byId('ml-studio-annotate-stage');
    // The SVG overlay must cover the image's RENDERED rectangle exactly, not the
    // whole stage — otherwise letterboxing (which changes on window resize) shifts
    // and scales the boxes off the picture. A shrink-to-image wrapper makes the SVG
    // share the image's box, so boxes stay pinned to the picture at any size.
    stage.innerHTML = `
      <div class="ml-studio-annotate-imgwrap">
        <img id="annot-img" src="${src}" class="ml-studio-annotate-img"/>
        <svg id="annot-svg" viewBox="0 0 ${S.origW} ${S.origH}" preserveAspectRatio="none" class="ml-studio-annotate-svg"></svg>
      </div>`;
    const svg = byId('annot-svg');
    // Property assignment (NOT addEventListener) so re-rendering the stage can never
    // stack duplicate handlers — stacked pointerdown listeners would create one box
    // per past render on a single click. Pointer capture routes move/up back to the
    // svg even when the pointer leaves it, so no leaky global window listener is needed.
    svg.onpointerdown = (ev) => { try { svg.setPointerCapture(ev.pointerId); } catch (_) {} onDown(ev); };
    svg.onpointermove = onMove;
    svg.onpointerup = onUp;
    svg.onpointercancel = onUp;
    drawBoxes();
  }

  // Konwersja: klient px → współrzędne oryginału (viewBox).
  function toOrig(ev) {
    const svg = byId('annot-svg'); const r = svg.getBoundingClientRect();
    return { x: ((ev.clientX - r.left) / r.width) * S.origW, y: ((ev.clientY - r.top) / r.height) * S.origH };
  }

  function drawBoxes() {
    const svg = byId('annot-svg'); if (!svg) return;
    const hs = Math.max(6, S.origW / 120); // rozmiar uchwytu w jednostkach oryginału
    let html = '';
    S.boxes.forEach((b, i) => {
      const col = catColor(b.category_id); const seld = i === S.sel;
      // Stroke widths are in SCREEN pixels (vector-effect: non-scaling-stroke) so a thin
      // 2px outline stays 2px on any image resolution. Predicted (autolabeled) boxes are
      // dashed with a confidence label until accepted; confirmed boxes are solid.
      const dash = b.predicted ? ' stroke-dasharray="6 4"' : '';
      html += `<rect data-box="${i}" x="${b.x}" y="${b.y}" width="${Math.max(0, b.w)}" height="${Math.max(0, b.h)}"
        fill="transparent" stroke="${col}" stroke-width="2"${dash} vector-effect="non-scaling-stroke" style="pointer-events:all;cursor:move"/>`;
      const label = b.predicted && typeof b.score === 'number'
        ? `${catName(b.category_id)} ${b.score.toFixed(2)}`
        : catName(b.category_id);
      html += `<text x="${b.x}" y="${Math.max(hs, b.y - 4)}" fill="${col}" font-size="${Math.max(11, S.origW / 55)}" font-weight="700" style="pointer-events:none">${escapeHtml(label)}</text>`;
      if (seld) {
        const corners = [[b.x, b.y, 'nw'], [b.x + b.w, b.y, 'ne'], [b.x, b.y + b.h, 'sw'], [b.x + b.w, b.y + b.h, 'se']];
        for (const [cx, cy, h] of corners) {
          html += `<rect class="annot-handle" data-box="${i}" data-handle="${h}" x="${cx - hs / 2}" y="${cy - hs / 2}" width="${hs}" height="${hs}" fill="#fff" stroke="${col}" stroke-width="2" vector-effect="non-scaling-stroke" style="cursor:nwse-resize"/>`;
        }
      }
    });
    svg.innerHTML = html;
  }

  function onDown(ev) {
    ev.preventDefault();
    const t = ev.target; const p0 = toOrig(ev);
    if (t.classList && t.classList.contains('annot-handle')) {
      S.sel = Number(t.getAttribute('data-box'));
      S.drag = { mode: 'resize', handle: t.getAttribute('data-handle'), orig: { ...S.boxes[S.sel] } };
    } else if (t.hasAttribute && t.hasAttribute('data-box')) {
      S.sel = Number(t.getAttribute('data-box'));
      // Klik w predykcję akceptuje ją (solid) — pierwszy klik zaznacza, akceptacja
      // dzieje się przy zwolnieniu bez przeciągnięcia (onUp).
      S.drag = { mode: 'move', startX: p0.x, startY: p0.y, orig: { ...S.boxes[S.sel] }, wasPredicted: S.boxes[S.sel].predicted };
      renderAttrPanel();
    } else {
      // Nowa ramka.
      const b = { category_id: Number(byId('annot-cat')?.value ?? defaultCat()), x: p0.x, y: p0.y, w: 0, h: 0, score: null, predicted: false, attributes: {} };
      S.boxes.push(b); S.sel = S.boxes.length - 1;
      S.drag = { mode: 'new', startX: p0.x, startY: p0.y };
    }
    drawBoxes();
  }

  function onMove(ev) {
    if (!S.drag) return;
    const p1 = toOrig(ev); const b = S.boxes[S.sel]; if (!b) return;
    if (S.drag.mode === 'new') {
      b.x = Math.min(S.drag.startX, p1.x); b.y = Math.min(S.drag.startY, p1.y);
      b.w = Math.abs(p1.x - S.drag.startX); b.h = Math.abs(p1.y - S.drag.startY);
    } else if (S.drag.mode === 'move') {
      const dx = p1.x - S.drag.startX, dy = p1.y - S.drag.startY;
      b.x = clamp(S.drag.orig.x + dx, 0, S.origW - b.w); b.y = clamp(S.drag.orig.y + dy, 0, S.origH - b.h);
    } else if (S.drag.mode === 'resize') {
      const o = S.drag.orig; const h = S.drag.handle;
      let x1 = o.x, y1 = o.y, x2 = o.x + o.w, y2 = o.y + o.h;
      if (h.includes('w')) x1 = p1.x; if (h.includes('e')) x2 = p1.x;
      if (h.includes('n')) y1 = p1.y; if (h.includes('s')) y2 = p1.y;
      b.x = Math.min(x1, x2); b.y = Math.min(y1, y2); b.w = Math.abs(x2 - x1); b.h = Math.abs(y2 - y1);
    }
    S.dirty = true; drawBoxes();
  }

  function onUp(ev) {
    if (!S.drag) return;
    const b = S.boxes[S.sel];
    if (S.drag.mode === 'new' && b && (b.w < 3 || b.h < 3)) { S.boxes.splice(S.sel, 1); S.sel = -1; } // za mała = anuluj
    else if (S.drag.mode === 'move' && b && S.drag.wasPredicted) {
      // Klik bez przeciągnięcia w ramkę-predykcję = akceptacja (staje się solid).
      const moved = ev && (Math.abs((toOrig(ev).x) - S.drag.startX) > 2 || Math.abs((toOrig(ev).y) - S.drag.startY) > 2);
      if (!moved) { b.predicted = false; b.score = null; S.dirty = true; toast('Ramka zaakceptowana.', 'success'); }
    }
    S.drag = null; drawBoxes(); renderAttrPanel();
  }

  function clamp(v, lo, hi) { return Math.max(lo, Math.min(hi, v)); }

  // ----- Panel atrybutów zaznaczonej ramki (z atrybutów klasy ze schematu) -----
  function renderAttrPanel() {
    const host = byId('ml-studio-annotate-attrs-body');
    if (!host) return;
    if (S.sel < 0 || !S.boxes[S.sel]) {
      host.innerHTML = '<p class="ml-studio-annotate-card-note">Zaznacz ramkę, aby ustawić jej klasę i atrybuty.</p>';
      return;
    }
    const box = S.boxes[S.sel];
    const cls = schemaClassFor(box);
    const swatch = catColor(box.category_id);
    const catOpts = S.categories.map((c) => `<option value="${c.id}"${c.id === box.category_id ? ' selected' : ''}>${escapeHtml(c.name)}</option>`).join('');
    const attrs = cls?.attributes || [];
    const fieldsHtml = attrs.map((a, ai) => recogAttrFieldHtml(a, ai, box)).join('')
      || '<p class="ml-studio-annotate-card-note">Ta klasa nie ma atrybutów w schemacie.</p>';
    host.innerHTML = `
      <div class="ml-studio-annotate-attr-head">
        <span class="c-swatch" style="background:${escapeAttr(swatch)}"></span>
        <span>Ramka: <strong>${escapeHtml(catName(box.category_id))}</strong>${box.predicted ? ' <tf-chip status="warning" label="predykcja"></tf-chip>' : ''}</span>
        <tf-button id="ml-studio-annotate-del" variant="ghost" icon="trash" class="ml-studio-annotate-attr-del"></tf-button>
      </div>
      <tf-select id="ml-studio-annot-cat" label="Klasa ramki">${catOpts}</tf-select>
      <div class="ml-studio-annotate-attr-fields">${fieldsHtml}</div>
      <tf-button id="ml-studio-annotate-confirm" variant="primary" icon="check" class="ml-studio-annotate-confirm">Zatwierdź ramkę</tf-button>`;
    byId('ml-studio-annot-cat')?.addEventListener('change', (e) => {
      const v = Number(e.detail?.value ?? box.category_id);
      box.category_id = v; box.attributes = {}; S.dirty = true; drawBoxes(); renderAttrPanel();
    });
    byId('ml-studio-annotate-del')?.addEventListener('click', () => {
      S.boxes.splice(S.sel, 1); S.sel = -1; S.dirty = true; drawBoxes(); renderAttrPanel();
    });
    byId('ml-studio-annotate-confirm')?.addEventListener('click', () => {
      box.predicted = false; box.score = null; S.dirty = true; drawBoxes(); renderAttrPanel();
      toast('Ramka zatwierdzona.', 'success');
    });
    bindAttrFields(attrs, box);
  }

  // Pole atrybutu wg typu (list/text/number/classifier/ocr). Atrybut OCR ma
  // dodatkowo żywy lookup ze słownika (gdy attr.ocr.lookup_dict_id wskazuje słownik).
  function recogAttrFieldHtml(a, ai, box) {
    const v = box.attributes?.[a.name] ?? '';
    const typeBadge = `<span class="ml-studio-annotate-attr-type ${escapeAttr(a.type)}">${escapeHtml(recogAttrTypeLabel(a.type))}</span>`;
    let field = '';
    if (a.type === 'list') {
      const opts = (a.list?.values || []).map((o) => `<option value="${escapeAttr(o)}"${o === v ? ' selected' : ''}>${escapeHtml(o)}</option>`).join('');
      field = `<tf-select data-attr="${escapeAttr(a.name)}"><option value="">—</option>${opts}</tf-select>`;
    } else if (a.type === 'classifier') {
      const opts = (a.classifier?.values || []).map((o) => `<option value="${escapeAttr(o)}"${o === v ? ' selected' : ''}>${escapeHtml(o)}</option>`).join('');
      field = `<tf-select data-attr="${escapeAttr(a.name)}"><option value="">—</option>${opts}</tf-select>`;
    } else if (a.type === 'number') {
      const n = a.number || {};
      const minA = n.min != null && n.min !== '' ? ` min="${escapeAttr(n.min)}"` : '';
      const maxA = n.max != null && n.max !== '' ? ` max="${escapeAttr(n.max)}"` : '';
      const unit = n.unit ? ` <span class="ml-studio-annotate-attr-unit">${escapeHtml(n.unit)}</span>` : '';
      field = `<tf-input type="number" data-attr="${escapeAttr(a.name)}" value="${escapeAttr(v)}"${minA}${maxA}></tf-input>${unit}`;
    } else { // text + ocr
      field = `<tf-input data-attr="${escapeAttr(a.name)}" value="${escapeAttr(v)}"></tf-input>`;
    }
    const lookup = a.type === 'ocr' ? `<div class="ml-studio-annotate-attr-lookup" data-lookup="${escapeAttr(a.name)}"></div>` : '';
    return `
      <div class="ml-studio-annotate-attr-field">
        <div class="ml-studio-annotate-attr-label">${escapeHtml(a.name)} ${typeBadge}</div>
        ${field}
        ${lookup}
      </div>`;
  }

  function bindAttrFields(attrs, box) {
    byId('ml-studio-annotate-attrs-body')?.querySelectorAll('[data-attr]').forEach((el) => {
      const name = el.getAttribute('data-attr');
      const attr = attrs.find((a) => a.name === name);
      // Capture on both input and change so live typing is persisted even if the
      // user never blurs the field (tf-input emits detail.value; native bubbling
      // events without detail fall back to the element's value getter).
      const capture = (e) => {
        box.attributes = box.attributes || {};
        box.attributes[name] = e.detail?.value ?? el.value ?? '';
        S.dirty = true;
        if (attr?.type === 'ocr') renderOcrLookup(attr, box.attributes[name]);
      };
      el.addEventListener('change', capture);
      el.addEventListener('input', capture);
    });
    // Wyrenderuj istniejący wynik lookup dla wartości wczytanych z zapisu.
    attrs.filter((a) => a.type === 'ocr').forEach((a) => renderOcrLookup(a, box.attributes?.[a.name] ?? ''));
  }

  // Żywy lookup OCR: gdy wpisana wartość pasuje do klucza wiersza słownika,
  // pokaż zmapowane pola (np. 33 → UN1203 · Benzyna).
  function renderOcrLookup(attr, value) {
    const host = byId('ml-studio-annotate-attrs-body')?.querySelector(`[data-lookup="${CSS.escape(attr.name)}"]`);
    if (!host) return;
    const dictId = attr.ocr?.lookup_dict_id;
    if (!dictId || !value) { host.innerHTML = ''; return; }
    const dict = S.dicts.find((d) => d.dictId === dictId || d.dict_id === dictId);
    if (!dict) { host.innerHTML = ''; return; }
    let body; try { body = JSON.parse(dict.rowsJson || dict.rows_json || '{}'); } catch { body = {}; }
    const cols = body.columns || []; const rows = body.rows || [];
    if (!cols.length) { host.innerHTML = ''; return; }
    const keyCol = cols[0].key;
    const row = rows.find((r) => String(r[keyCol] ?? '').trim() === String(value).trim());
    if (!row) { host.innerHTML = `<span class="ml-studio-annotate-attr-lookup-miss">brak w słowniku „${escapeHtml(dict.name)}"</span>`; return; }
    const mapped = cols.slice(1).map((c) => escapeHtml(String(row[c.key] ?? ''))).filter(Boolean).join(' · ');
    host.innerHTML = `<span class="ml-studio-annotate-attr-lookup-hit">${sprite('check')} ${escapeHtml(String(value))} → ${mapped}</span>`;
  }

  async function saveAnns(approve = false) {
    const btn = byId('ml-studio-annotate-save');
    const btnApprove = byId('ml-studio-annotate-save-approve');
    btn?.setAttribute('disabled', ''); btnApprove?.setAttribute('disabled', '');
    try {
      const anns = S.boxes.filter((b) => b.w >= 3 && b.h >= 3).map((b) => {
        const out = {
          category_id: b.category_id, bbox: [Math.round(b.x), Math.round(b.y), Math.round(b.w), Math.round(b.h)],
        };
        if (b.predicted) { out.predicted = true; if (typeof b.score === 'number') out.score = b.score; }
        if (b.attributes && Object.keys(b.attributes).length) out.attributes = b.attributes;
        return out;
      });
      const resp = await ApiBinary.one('mlStudioRecogSaveAnnotationsRequest', {
        datasetId: S.datasetId, imageId: S.images[S.curIdx].image_id, annotationsJson: JSON.stringify(anns), approve,
      });
      if (!resp.ok) throw new Error(resp.error || 'zapis nieudany');
      S.dirty = false;
      S.images[S.curIdx].ann_count = anns.length;
      if (approve) S.images[S.curIdx].approved = true;
      renderProgress(); renderTaskList();
      toast(I18n.t(approve ? 'ml_studio.annotate.saved_approved' : 'ml_studio.annotate.saved'), 'success');
    } catch (err) { toast(`${I18n.t('ml_studio.annotate.save')}: ${err.message}`, 'error'); }
    finally { btn?.removeAttribute('disabled'); btnApprove?.removeAttribute('disabled'); }
  }

  // Skróty klawiszowe: Delete usuwa zaznaczoną ramkę, 1-9 przypisuje klasę.
  // Ignorowane gdy fokus jest w polu formularza (wpisywanie wartości atrybutu).
  const keyHandler = (e) => {
    if (!byId('annot-svg')) return;
    const tag = (e.target?.tagName || '').toLowerCase();
    if (tag === 'input' || tag === 'textarea' || tag === 'select' || e.target?.isContentEditable) return;
    if ((e.key === 'Delete' || e.key === 'Backspace') && S.sel >= 0) {
      S.boxes.splice(S.sel, 1); S.sel = -1; S.dirty = true; drawBoxes(); renderAttrPanel(); e.preventDefault();
    } else if (/^[1-9]$/.test(e.key) && S.sel >= 0) {
      const classes = effectiveClasses(); const idx = Number(e.key) - 1;
      if (idx < classes.length) { assignClass(classes[idx].name); e.preventDefault(); }
    }
  };
  document.addEventListener('keydown', keyHandler);
}

// Zakładka "Schemat" dla recognition: edytor klas i atrybutów rozpoznawania.
// Lewa kolumna to lista klas (kolor/kształt/liczba atrybutów), prawa to edytor
// wybranej klasy + jej atrybuty (lista/tekst/OCR/liczba/klasyfikator). Schemat
// jest opakiem w backendzie (CBOR string) — frontend jest jego właścicielem i
// serializuje go do `schemaJson`. Gdy schemat jest pusty, klasy są zasiewane z
// istniejących kategorii COCO projektu, żeby użytkownik od razu widział swoje
// realne klasy (zasiew żyje w pamięci do kliknięcia "Zapisz schemat").
//
// Słowniki lookup (OCR) są osobnym zasobem backendu i edytowane przez modal:
// nazwa + tabela kolumn/wierszy zapisywana jako `rowsJson`.

// Stała paleta kolorów klas — zgodna z mockupem r01-schemat. Cyklowana przy
// zasiewie z kategorii COCO, oferowana jako szybki wybór w edytorze klasy.
const RECOG_SCHEMA_PALETTE = ['#a78bfa', '#60a5fa', '#22c55e', '#f59e0b', '#ef4444'];

// Typy atrybutów: id + etykieta + ikona + krótki opis (dla pickera) oraz ton
// badge'a (klasa CSS .ml-studio-attr-type-badge.<id>) używanego w wierszu atrybutu.
const RECOG_ATTR_TYPES = [
  { id: 'list', label: 'Lista wyboru', icon: 'list', desc: 'jedna z wartości' },
  { id: 'text', label: 'Tekst', icon: 'file-text', desc: 'dowolny ciąg' },
  { id: 'ocr', label: 'OCR', icon: 'search', desc: 'odczyt z ramki' },
  { id: 'number', label: 'Liczba', icon: 'pi', desc: 'wartość num.' },
  { id: 'classifier', label: 'Klasyfikator', icon: 'model', desc: 'osobny model' },
];

// Wyprowadza z schematu projektu listę CELÓW treningu — w jednym projekcie może
// powstać wiele modeli. Zawsze zwraca detekcję, a dodatkowo po jednym celu na
// każdy atrybut nadający się do osobnego modelu:
//   - klasyfikator: atrybut typu `list` lub `classifier` o ≥2 wartościach,
//   - OCR: atrybut typu `ocr` (faza 2 — w UI element może być disabled).
// Atrybut o tej samej nazwie może wystąpić w wielu klasach — agregujemy jego
// klasy źródłowe (cropy trenujemy z ramek tych klas) i sumę wartości.
// `cocoCategories` (opcjonalne) zawęża klasy źródłowe do realnie istniejących
// kategorii datasetu; gdy puste — bierzemy wszystkie klasy schematu.
function deriveTrainTargets(schema, cocoCategories) {
  const targets = [{ task: 'detection' }];
  const classes = (schema && Array.isArray(schema.classes)) ? schema.classes : [];
  const cocoNames = Array.isArray(cocoCategories)
    ? new Set(cocoCategories.map((c) => (typeof c === 'string' ? c : c && c.name)).filter(Boolean))
    : null;
  const byAttr = new Map();
  for (const c of classes) {
    const attrs = Array.isArray(c.attributes) ? c.attributes : [];
    for (const a of attrs) {
      if (!a || !a.name || !a.type) continue;
      let entry = byAttr.get(a.name);
      if (!entry) { entry = { name: a.name, type: a.type, values: [], sourceClasses: new Set() }; byAttr.set(a.name, entry); }
      if (!cocoNames || cocoNames.has(c.name)) entry.sourceClasses.add(c.name);
      const vals = a.type === 'list' ? (a.list?.values || [])
        : a.type === 'classifier' ? (a.classifier?.values || []) : [];
      for (const v of vals) if (!entry.values.includes(v)) entry.values.push(v);
    }
  }
  for (const entry of byAttr.values()) {
    const sourceClasses = [...entry.sourceClasses];
    if ((entry.type === 'list' || entry.type === 'classifier') && entry.values.length >= 2) {
      targets.push({ task: 'classifier', attribute: entry.name, sourceClasses, values: entry.values });
    } else if (entry.type === 'ocr') {
      targets.push({ task: 'ocr', attribute: entry.name, sourceClasses });
    }
  }
  return targets;
}

// Kształt ramki klasy → ikona (segmented control + ikona przy wierszu klasy).
const RECOG_SHAPES = [
  { id: 'box', label: 'Box', icon: 'grid-2x2' },
  { id: 'polygon', label: 'Poligon', icon: 'branch' },
  { id: 'point', label: 'Punkt', icon: 'record-dot' },
];

function recogShapeIcon(shape) {
  const s = RECOG_SHAPES.find((x) => x.id === shape);
  return s ? s.icon : 'grid-2x2';
}

function recogAttrTypeLabel(type) {
  const t = RECOG_ATTR_TYPES.find((x) => x.id === type);
  return t ? t.label : type;
}

// Krótki opis atrybutu dla wiersza (preview) — zależny od typu, bez OCR słownika
// (ten dostaje osobny blok detail + podgląd tabeli).
function recogAttrSummary(attr) {
  switch (attr.type) {
    case 'list':
      return (attr.list?.values || []).join(' / ') || '—';
    case 'classifier': {
      const vals = (attr.classifier?.values || []).join(' / ');
      const m = attr.classifier?.model || '';
      return [m && `model: ${m}`, vals].filter(Boolean).join(' · ') || '—';
    }
    case 'number': {
      const n = attr.number || {};
      const range = [n.min, n.max].every((v) => v != null && v !== '') ? `${n.min}…${n.max}` : '';
      return [range, n.unit].filter(Boolean).join(' ') || '—';
    }
    case 'text':
      return 'dowolny tekst';
    default:
      return '';
  }
}

function renderRecogSchemaTab(panel, p, { selectTab }) {
  const pid = projectId(p);
  // Stan edytora schematu — żyje w domknięciu zakładki. `classes` jest in-memory
  // do zapisu; `selected` to indeks wybranej klasy; `dicts`/`models` to cache
  // zasobów backendu; `adding` przechowuje roboczy stan formularza atrybutu.
  const S = {
    classes: [], selected: 0, dirty: false,
    dicts: [], ocrModels: [], classifierModels: [],
    adding: null, // {name, type, ocr:{...}, list:{values}, classifier:{...}, number:{...}}
  };

  panel.innerHTML = `
    <div class="ml-studio-schema">
      <div class="ml-studio-schema-toolbar">
        <span class="ml-studio-schema-toolbar-hint" id="ml-studio-schema-dirty"></span>
        <tf-button variant="primary" icon="download" id="ml-studio-schema-save">Zapisz schemat</tf-button>
      </div>
      <div class="ml-studio-schema-layout">
        <aside class="ml-studio-schema-classes">
          <div class="ml-studio-schema-classes-head" id="ml-studio-schema-classes-head">Klasy (0)</div>
          <div id="ml-studio-schema-class-list"></div>
          <button type="button" class="ml-studio-schema-add-class" id="ml-studio-schema-add-class">
            ${sprite('plus')} Dodaj klasę
          </button>
          <p class="ml-studio-schema-classes-note">Nowa klasa: nazwa (snake_case), kolor i kształt ramki. Atrybuty dodasz po jej wybraniu.</p>
        </aside>
        <div class="ml-studio-schema-editor" id="ml-studio-schema-editor"></div>
      </div>
    </div>
  `;

  const markDirty = () => {
    S.dirty = true;
    const el = byId('ml-studio-schema-dirty');
    if (el) el.textContent = 'Niezapisane zmiany';
  };

  function renderClassList() {
    const head = byId('ml-studio-schema-classes-head');
    if (head) head.textContent = `Klasy (${S.classes.length})`;
    const host = byId('ml-studio-schema-class-list');
    if (!host) return;
    host.innerHTML = S.classes.map((c, i) => `
      <button type="button" class="ml-studio-schema-class${i === S.selected ? ' active' : ''}" data-idx="${i}">
        <span class="ml-studio-schema-class-swatch" style="background:${escapeAttr(c.color)}"></span>
        <span class="ml-studio-schema-class-name">${escapeHtml(c.name)}</span>
        <span class="ml-studio-schema-class-shape">${sprite(recogShapeIcon(c.shape))}</span>
        <span class="ml-studio-schema-class-attrs">${(c.attributes || []).length} atr.</span>
      </button>`).join('');
    host.querySelectorAll('.ml-studio-schema-class').forEach((el) => {
      el.addEventListener('click', () => {
        S.selected = Number(el.getAttribute('data-idx'));
        S.adding = null;
        renderClassList();
        renderEditor();
      });
    });
  }

  function renderEditor() {
    const host = byId('ml-studio-schema-editor');
    if (!host) return;
    const c = S.classes[S.selected];
    if (!c) {
      host.innerHTML = '';
      const empty = document.createElement('tf-empty-state');
      empty.setAttribute('icon', 'grid-2x2');
      empty.setAttribute('title', 'Brak klas w schemacie');
      empty.setAttribute('message', 'Dodaj pierwszą klasę po lewej, aby zdefiniować co model wykrywa i jakie ma atrybuty.');
      host.appendChild(empty);
      return;
    }

    const colorPicks = RECOG_SCHEMA_PALETTE.map((col) => `
      <button type="button" class="ml-studio-color-pick${c.color === col ? ' active' : ''}"
              data-color="${escapeAttr(col)}" style="background:${escapeAttr(col)}" aria-label="${escapeAttr(col)}"></button>`).join('');

    const attrRows = (c.attributes || []).map((a, ai) => recogAttrRowHtml(a, ai)).join('')
      || '<p class="ml-studio-schema-classes-note" style="padding:4px 2px">Brak atrybutów. Dodaj pierwszy poniżej.</p>';

    host.innerHTML = `
      <section class="ml-studio-data-card">
        <div class="ml-studio-schema-editor-head">
          <div class="ml-studio-schema-editor-title">
            <span class="ml-studio-schema-class-swatch" style="background:${escapeAttr(c.color)}"></span>
            Klasa: <span class="mono" style="color:var(--accent-2)">${escapeHtml(c.name)}</span>
          </div>
          <tf-button variant="danger" icon="trash" id="ml-studio-schema-del-class">Usuń klasę</tf-button>
        </div>
        <div class="ml-studio-schema-class-props">
          <tf-input id="ml-studio-schema-class-name" class="mono" label="Nazwa klasy" value="${escapeAttr(c.name)}"
                    hint="Identyfikator techniczny (snake_case). Trafi do eksportu COCO/JSONL."></tf-input>
          <div class="ml-studio-schema-field">
            <label class="ml-studio-schema-label">Kolor ramki</label>
            <div class="ml-studio-color-swatches">${colorPicks}</div>
          </div>
          <div class="ml-studio-schema-field">
            <label class="ml-studio-schema-label">Kształt</label>
            <tf-segmented id="ml-studio-schema-shape" value="${escapeAttr(c.shape)}">
              ${RECOG_SHAPES.map((s) => `<option value="${escapeAttr(s.id)}">${escapeHtml(s.label)}</option>`).join('')}
            </tf-segmented>
          </div>
        </div>
      </section>

      <section class="ml-studio-data-card">
        <div class="ml-studio-schema-editor-head">
          <div class="ml-studio-schema-editor-title">${sprite('list')} Atrybuty klasy
            <span class="text-3" style="font-weight:400">— odczytywane dla każdej ramki</span>
          </div>
          <tf-button variant="outline" icon="plus" id="ml-studio-schema-add-attr">Dodaj atrybut</tf-button>
        </div>
        <div id="ml-studio-schema-attr-list">${attrRows}</div>
        <div id="ml-studio-schema-attr-form"></div>
      </section>
    `;

    byId('ml-studio-schema-del-class')?.addEventListener('click', () => {
      if (!confirm(`Usunąć klasę „${c.name}" wraz z atrybutami?`)) return;
      S.classes.splice(S.selected, 1);
      S.selected = Math.max(0, S.selected - 1);
      S.adding = null;
      markDirty();
      renderClassList();
      renderEditor();
    });

    const nameInput = byId('ml-studio-schema-class-name');
    nameInput?.addEventListener('change', () => {
      const v = String(nameInput.value || '').trim();
      if (v) { c.name = v; markDirty(); renderClassList(); }
    });

    host.querySelectorAll('.ml-studio-color-pick').forEach((el) => {
      el.addEventListener('click', () => {
        c.color = el.getAttribute('data-color');
        markDirty();
        renderClassList();
        renderEditor();
      });
    });

    byId('ml-studio-schema-shape')?.addEventListener('change', (e) => {
      c.shape = e.detail?.value || 'box';
      markDirty();
      renderClassList();
      renderEditor();
    });

    bindAttrRowActions(c);

    byId('ml-studio-schema-add-attr')?.addEventListener('click', () => {
      S.adding = { name: '', type: 'list', ocr: { model: '', format_regex: '', lookup_dict_id: '', on_missing: 'keep_raw', lookup_enabled: false }, list: { values: [] }, classifier: { model: '', values: [] }, number: { min: '', max: '', unit: '' } };
      renderAttrForm(c);
    });
  }

  // Wiersz istniejącego atrybutu: nazwa + badge typu + (dla OCR) detale modelu/
  // regexu/słownika i podgląd tabeli lookup. Edytuj/usuń obsługiwane delegacją.
  function recogAttrRowHtml(a, ai) {
    const typeMeta = RECOG_ATTR_TYPES.find((t) => t.id === a.type) || RECOG_ATTR_TYPES[0];
    let detail = '';
    if (a.type === 'ocr') {
      const o = a.ocr || {};
      const dict = o.lookup_dict_id ? S.dicts.find((d) => d.dictId === o.lookup_dict_id) : null;
      detail = `
        <div class="ml-studio-attr-detail">
          <span class="dk">Model OCR</span><span class="dv mono">${escapeHtml(o.model || '—')}</span>
          <span class="dk">Walidacja formatu</span><span class="dv mono">${escapeHtml(o.format_regex || '—')}</span>
          <span class="dk">Lookup / słownik</span><span class="dv">${dict ? escapeHtml(dict.name) : 'brak'}${dict ? ` · braki: ${o.on_missing === 'reject' ? 'odrzuć ramkę' : 'zapisz surowy kod'}` : ''}</span>
        </div>
        ${dict ? recogLookupPreviewHtml(dict) : ''}`;
    } else {
      const summary = recogAttrSummary(a);
      if (summary) {
        detail = `<div class="ml-studio-attr-detail"><span class="dk">${escapeHtml(a.type === 'number' ? 'zakres' : a.type === 'classifier' ? 'klasyfikator' : a.type === 'list' ? 'wartości' : 'wartość')}</span><span class="dv">${escapeHtml(summary)}</span></div>`;
      }
    }
    return `
      <div class="ml-studio-attr-row" data-attr-idx="${ai}">
        <div class="ml-studio-attr-head">
          <span class="ml-studio-attr-name">${escapeHtml(a.name)}</span>
          <span class="ml-studio-attr-type-badge ${escapeAttr(a.type)}">${sprite(typeMeta.icon)}${escapeHtml(typeMeta.label)}</span>
          <span class="ml-studio-attr-actions">
            <tf-button variant="ghost" icon="trash" data-attr-del="${ai}" aria-label="Usuń atrybut"></tf-button>
          </span>
        </div>
        ${detail}
      </div>`;
  }

  function recogLookupPreviewHtml(dict) {
    let body;
    try { body = JSON.parse(dict.rowsJson || '{}'); } catch { body = {}; }
    const cols = body.columns || [];
    const rows = body.rows || [];
    if (!cols.length) return '';
    const head = cols.map((col) => `<th>${escapeHtml(col.label || col.key)}</th>`).join('');
    const preview = rows.slice(0, 4).map((r) =>
      `<tr>${cols.map((col) => `<td>${escapeHtml(String(r[col.key] ?? ''))}</td>`).join('')}</tr>`).join('');
    return `
      <div class="ml-studio-lookup-wrap">
        <div class="ml-studio-lookup-head">${sprite('catalog')} Słownik lookup: ${escapeHtml(dict.name)} (${Math.min(4, rows.length)} z ${rows.length})</div>
        <table class="ml-studio-lookup-table"><thead><tr>${head}</tr></thead><tbody>${preview}</tbody></table>
      </div>`;
  }

  function bindAttrRowActions(c) {
    byId('ml-studio-schema-attr-list')?.querySelectorAll('[data-attr-del]').forEach((btn) => {
      btn.addEventListener('click', () => {
        const ai = Number(btn.getAttribute('data-attr-del'));
        c.attributes.splice(ai, 1);
        markDirty();
        renderClassList();
        renderEditor();
      });
    });
  }

  // Formularz dodawania atrybutu — nazwa + picker typu + pola zależne od typu.
  function renderAttrForm(c) {
    const host = byId('ml-studio-schema-attr-form');
    if (!host || !S.adding) { if (host) host.innerHTML = ''; return; }
    const a = S.adding;

    const typeOpts = RECOG_ATTR_TYPES.map((t) => `
      <button type="button" class="ml-studio-attr-type-opt${a.type === t.id ? ' selected' : ''}" data-type="${escapeAttr(t.id)}">
        <span class="ato-ico">${sprite(t.icon)}</span>
        <span class="ato-name">${escapeHtml(t.label)}</span>
        <span class="ato-desc">${escapeHtml(t.desc)}</span>
      </button>`).join('');

    host.innerHTML = `
      <div class="ml-studio-attr-add">
        <h4 class="ml-studio-attr-add-title">${sprite('plus')} Nowy atrybut</h4>
        <p class="ml-studio-schema-classes-note" style="margin:0 0 12px">Krok 1: nadaj nazwę. Krok 2: wybierz typ — od typu zależą dodatkowe pola.</p>
        <tf-input id="ml-studio-schema-attr-name" class="mono" label="Nazwa atrybutu" value="${escapeAttr(a.name)}"
                  placeholder="np. numer, klasa, stan…" hint="snake_case. Pojawi się w panelu anotacji dla każdej ramki tej klasy."></tf-input>
        <div class="ml-studio-schema-field">
          <label class="ml-studio-schema-label">Typ atrybutu</label>
          <div class="ml-studio-attr-type-pick">${typeOpts}</div>
        </div>
        <div id="ml-studio-schema-attr-config"></div>
        <div class="ml-studio-attr-add-foot">
          <tf-button variant="ghost" id="ml-studio-schema-attr-cancel">Anuluj</tf-button>
          <tf-button variant="primary" icon="check" id="ml-studio-schema-attr-confirm">Dodaj atrybut do klasy</tf-button>
        </div>
      </div>
    `;

    const nameEl = byId('ml-studio-schema-attr-name');
    nameEl?.addEventListener('input', () => { a.name = String(nameEl.value || '').trim(); });

    host.querySelectorAll('.ml-studio-attr-type-opt').forEach((el) => {
      el.addEventListener('click', () => {
        a.type = el.getAttribute('data-type');
        renderAttrForm(c);
      });
    });

    byId('ml-studio-schema-attr-cancel')?.addEventListener('click', () => {
      S.adding = null;
      renderEditor();
    });

    byId('ml-studio-schema-attr-confirm')?.addEventListener('click', () => {
      if (!a.name) { toast('Podaj nazwę atrybutu.', 'error'); return; }
      if ((c.attributes || []).some((x) => x.name === a.name)) { toast('Atrybut o tej nazwie już istnieje.', 'error'); return; }
      c.attributes = c.attributes || [];
      c.attributes.push(buildAttrFromForm(a));
      S.adding = null;
      markDirty();
      renderClassList();
      renderEditor();
    });

    renderAttrConfig(c);
  }

  function buildAttrFromForm(a) {
    const out = { name: a.name, type: a.type };
    if (a.type === 'ocr') {
      out.ocr = {
        model: a.ocr.model || '',
        format_regex: a.ocr.format_regex || '',
        lookup_dict_id: a.ocr.lookup_enabled ? (a.ocr.lookup_dict_id || '') : '',
        on_missing: a.ocr.on_missing || 'keep_raw',
      };
    } else if (a.type === 'classifier') {
      out.classifier = { model: a.classifier.model || '', values: a.classifier.values.slice() };
    } else if (a.type === 'list') {
      out.list = { values: a.list.values.slice() };
    } else if (a.type === 'number') {
      const n = a.number;
      out.number = {
        min: n.min === '' ? null : Number(n.min),
        max: n.max === '' ? null : Number(n.max),
        unit: n.unit || '',
      };
    } else {
      out.text = {};
    }
    return out;
  }

  // Pola zależne od typu w formularzu dodawania. Renderowane do osobnego kontenera,
  // żeby zmiana typu nie gubiła już wpisanej nazwy atrybutu.
  function renderAttrConfig(c) {
    const host = byId('ml-studio-schema-attr-config');
    if (!host || !S.adding) return;
    const a = S.adding;

    if (a.type === 'text') { host.innerHTML = '<p class="ml-studio-schema-classes-note" style="margin:0">Tekst nie wymaga dodatkowych pól.</p>'; return; }

    if (a.type === 'list') {
      host.innerHTML = `
        <div class="ml-studio-schema-field">
          <label class="ml-studio-schema-label">Wartości listy</label>
          <tf-tag-input id="ml-studio-schema-list-values" placeholder="dopisz wartość i Enter"></tf-tag-input>
        </div>`;
      const ti = byId('ml-studio-schema-list-values');
      if (ti) { ti.tags = a.list.values; ti.addEventListener('change', (e) => { a.list.values = e.detail?.tags || []; }); }
      return;
    }

    if (a.type === 'number') {
      const n = a.number;
      host.innerHTML = `
        <div class="ml-studio-schema-config-grid">
          <tf-input id="ml-studio-schema-num-min" type="number" label="Min" value="${escapeAttr(String(n.min))}"></tf-input>
          <tf-input id="ml-studio-schema-num-max" type="number" label="Max" value="${escapeAttr(String(n.max))}"></tf-input>
          <tf-input id="ml-studio-schema-num-unit" label="Jednostka" value="${escapeAttr(n.unit)}" placeholder="np. °C"></tf-input>
        </div>`;
      byId('ml-studio-schema-num-min')?.addEventListener('input', (e) => { n.min = e.target?.value ?? e.detail?.value ?? ''; });
      byId('ml-studio-schema-num-max')?.addEventListener('input', (e) => { n.max = e.target?.value ?? e.detail?.value ?? ''; });
      byId('ml-studio-schema-num-unit')?.addEventListener('input', (e) => { n.unit = e.target?.value ?? e.detail?.value ?? ''; });
      return;
    }

    if (a.type === 'classifier') {
      host.innerHTML = `
        <div class="ml-studio-schema-config-card">
          <div class="ml-studio-schema-config-grid two">
            <tf-select id="ml-studio-schema-cls-model" label="Model klasyfikatora"></tf-select>
            <div class="ml-studio-schema-field">
              <label class="ml-studio-schema-label">Wartości (klasy)</label>
              <tf-tag-input id="ml-studio-schema-cls-values" placeholder="dopisz wartość i Enter"></tf-tag-input>
            </div>
          </div>
        </div>`;
      const sel = byId('ml-studio-schema-cls-model');
      const opts = [{ value: '', label: '— wybierz model —' }].concat(S.classifierModels.map((m) => ({ value: m.id, label: `${m.name} (${m.source || 'serwis'})` })));
      sel?.setOptions(opts, a.classifier.model || '');
      sel?.addEventListener('change', (e) => { a.classifier.model = e.detail?.value || ''; });
      const ti = byId('ml-studio-schema-cls-values');
      if (ti) { ti.tags = a.classifier.values; ti.addEventListener('change', (e) => { a.classifier.values = e.detail?.tags || []; }); }
      return;
    }

    // OCR
    const o = a.ocr;
    const dictOpts = [{ value: '', label: '— wybierz słownik —' }]
      .concat(S.dicts.map((d) => ({ value: d.dictId, label: d.name })))
      .concat([{ value: '__new__', label: '+ Utwórz nowy słownik…' }]);
    host.innerHTML = `
      <div class="ml-studio-schema-config-card">
        <div class="ml-studio-schema-config-badge"><span class="ml-studio-attr-type-badge ocr">${sprite('search')}Konfiguracja OCR</span></div>
        <div class="ml-studio-schema-config-grid two">
          <tf-select id="ml-studio-schema-ocr-model" label="Model OCR"></tf-select>
          <tf-input id="ml-studio-schema-ocr-regex" class="mono" label="Walidacja formatu (regex, opcjonalnie)" value="${escapeAttr(o.format_regex)}" placeholder="^\\d{2,3}$"></tf-input>
        </div>
        <div class="ml-studio-schema-toggle-row">
          <tf-toggle id="ml-studio-schema-ocr-lookup"${o.lookup_enabled ? ' checked' : ''}></tf-toggle>
          <span><strong>Dołącz słownik (lookup)</strong> — mapuje odczytany kod na dodatkowe pola</span>
        </div>
        <div id="ml-studio-schema-ocr-lookup-fields"></div>
      </div>`;

    const modelSel = byId('ml-studio-schema-ocr-model');
    const modelOpts = [{ value: '', label: '— wybierz model —' }].concat(S.ocrModels.map((m) => ({ value: m.id, label: `${m.name} (${m.source || 'serwis'})` })));
    modelSel?.setOptions(modelOpts, o.model || '');
    modelSel?.addEventListener('change', (e) => { o.model = e.detail?.value || ''; });
    byId('ml-studio-schema-ocr-regex')?.addEventListener('input', (e) => { o.format_regex = e.target?.value ?? e.detail?.value ?? ''; });
    byId('ml-studio-schema-ocr-lookup')?.addEventListener('change', (e) => {
      o.lookup_enabled = !!e.detail?.checked;
      renderOcrLookupFields(o);
    });
    renderOcrLookupFields(o);

    function renderOcrLookupFields(ocr) {
      const lh = byId('ml-studio-schema-ocr-lookup-fields');
      if (!lh) return;
      if (!ocr.lookup_enabled) { lh.innerHTML = ''; return; }
      lh.innerHTML = `
        <div class="ml-studio-schema-config-grid two" style="margin-top:10px">
          <tf-select id="ml-studio-schema-ocr-dict" label="Tabela słownika"></tf-select>
          <tf-select id="ml-studio-schema-ocr-onmissing" label="Zachowanie przy braku w słowniku">
            <option value="keep_raw">Zapisz surowy kod, oznacz „nieznany"</option>
            <option value="reject">Odrzuć ramkę do ręcznej weryfikacji</option>
          </tf-select>
        </div>`;
      const dictSel = byId('ml-studio-schema-ocr-dict');
      dictSel?.setOptions(dictOpts, ocr.lookup_dict_id || '');
      dictSel?.addEventListener('change', (e) => {
        const v = e.detail?.value || '';
        if (v === '__new__') { openDictEditor(null, (newId) => { ocr.lookup_dict_id = newId; renderAttrConfig(c); }); return; }
        ocr.lookup_dict_id = v;
      });
      const omSel = byId('ml-studio-schema-ocr-onmissing');
      if (omSel) { omSel.value = ocr.on_missing || 'keep_raw'; omSel.addEventListener('change', (e) => { ocr.on_missing = e.detail?.value || 'keep_raw'; }); }
    }
  }

  // Modal edytora słownika lookup: nazwa + edytowalna tabela kolumn/wierszy.
  // Po zapisie woła mlStudioLookupDictSave, odświeża cache i wraca dictId.
  function openDictEditor(existing, onSaved) {
    const dict = existing
      ? { dictId: existing.dictId, name: existing.name, body: safeParse(existing.rowsJson) }
      : { dictId: '', name: '', body: { columns: [{ key: 'code', label: 'kod' }, { key: 'un', label: 'numer UN' }, { key: 'material', label: 'nazwa materiału' }], rows: [] } };

    const modal = document.createElement('tf-modal');
    modal.setAttribute('open', '');
    modal.setAttribute('title', existing ? 'Edytuj słownik lookup' : 'Nowy słownik lookup');
    modal.innerHTML = `
      <div slot="body" class="ml-studio-dict-editor">
        <tf-input id="ml-studio-dict-name" label="Nazwa słownika" value="${escapeAttr(dict.name)}" placeholder="np. un_kody"></tf-input>
        <div class="ml-studio-schema-field">
          <label class="ml-studio-schema-label">Kolumny (klucz : etykieta)</label>
          <div id="ml-studio-dict-cols"></div>
          <tf-button variant="ghost" icon="plus" id="ml-studio-dict-add-col">Dodaj kolumnę</tf-button>
        </div>
        <div class="ml-studio-schema-field">
          <label class="ml-studio-schema-label">Wiersze</label>
          <div id="ml-studio-dict-rows"></div>
          <tf-button variant="ghost" icon="plus" id="ml-studio-dict-add-row">Dodaj wiersz</tf-button>
        </div>
      </div>
      <div slot="footer">
        <tf-button variant="ghost" id="ml-studio-dict-cancel">Anuluj</tf-button>
        <tf-button variant="primary" icon="download" id="ml-studio-dict-save">Zapisz słownik</tf-button>
      </div>
    `;
    document.body.appendChild(modal);

    const renderCols = () => {
      const h = modal.querySelector('#ml-studio-dict-cols');
      h.innerHTML = dict.body.columns.map((col, i) => `
        <div class="ml-studio-dict-col-row" data-col="${i}">
          <tf-input class="mono ml-studio-dict-col-key" value="${escapeAttr(col.key)}" placeholder="klucz"></tf-input>
          <tf-input class="ml-studio-dict-col-label" value="${escapeAttr(col.label)}" placeholder="etykieta"></tf-input>
          <tf-button variant="ghost" icon="trash" data-del-col="${i}" aria-label="Usuń kolumnę"></tf-button>
        </div>`).join('');
      h.querySelectorAll('[data-del-col]').forEach((b) => b.addEventListener('click', () => {
        dict.body.columns.splice(Number(b.getAttribute('data-del-col')), 1); renderCols(); renderRows();
      }));
      h.querySelectorAll('.ml-studio-dict-col-row').forEach((row) => {
        const i = Number(row.getAttribute('data-col'));
        row.querySelector('.ml-studio-dict-col-key')?.addEventListener('input', (e) => { dict.body.columns[i].key = (e.target?.value ?? '').trim(); });
        row.querySelector('.ml-studio-dict-col-label')?.addEventListener('input', (e) => { dict.body.columns[i].label = e.target?.value ?? ''; });
      });
    };
    const renderRows = () => {
      const h = modal.querySelector('#ml-studio-dict-rows');
      h.innerHTML = dict.body.rows.map((r, ri) => `
        <div class="ml-studio-dict-data-row" data-row="${ri}" style="grid-template-columns:repeat(${dict.body.columns.length}, 1fr) auto">
          ${dict.body.columns.map((col) => `<tf-input class="mono" data-key="${escapeAttr(col.key)}" value="${escapeAttr(String(r[col.key] ?? ''))}" placeholder="${escapeAttr(col.label || col.key)}"></tf-input>`).join('')}
          <tf-button variant="ghost" icon="trash" data-del-row="${ri}" aria-label="Usuń wiersz"></tf-button>
        </div>`).join('');
      h.querySelectorAll('[data-del-row]').forEach((b) => b.addEventListener('click', () => {
        dict.body.rows.splice(Number(b.getAttribute('data-del-row')), 1); renderRows();
      }));
      h.querySelectorAll('.ml-studio-dict-data-row').forEach((row) => {
        const ri = Number(row.getAttribute('data-row'));
        row.querySelectorAll('tf-input[data-key]').forEach((inp) => {
          inp.addEventListener('input', (e) => { dict.body.rows[ri][inp.getAttribute('data-key')] = e.target?.value ?? ''; });
        });
      });
    };
    renderCols();
    renderRows();

    modal.querySelector('#ml-studio-dict-add-col')?.addEventListener('click', () => { dict.body.columns.push({ key: '', label: '' }); renderCols(); renderRows(); });
    modal.querySelector('#ml-studio-dict-add-row')?.addEventListener('click', () => {
      const row = {}; dict.body.columns.forEach((c) => { row[c.key] = ''; }); dict.body.rows.push(row); renderRows();
    });
    const close = () => modal.remove();
    modal.querySelector('#ml-studio-dict-cancel')?.addEventListener('click', close);
    modal.addEventListener('close', close);
    modal.querySelector('#ml-studio-dict-save')?.addEventListener('click', async () => {
      const name = String(modal.querySelector('#ml-studio-dict-name')?.value || '').trim();
      if (!name) { toast('Podaj nazwę słownika.', 'error'); return; }
      try {
        const resp = await ApiBinary.one('mlStudioLookupDictSaveRequest', {
          projectId: pid, dictId: dict.dictId || '', name, rowsJson: JSON.stringify(dict.body),
        });
        const newId = resp.dictId ?? resp.dict_id ?? dict.dictId;
        await loadDicts();
        toast('Słownik zapisany.', 'success');
        close();
        if (onSaved) onSaved(newId);
      } catch (err) { toast(`Słownik: ${err.message}`, 'error'); }
    });
  }

  function safeParse(s) {
    try { const v = JSON.parse(s || '{}'); return (v && v.columns) ? v : { columns: [], rows: [] }; }
    catch { return { columns: [], rows: [] }; }
  }

  async function loadDicts() {
    try {
      const resp = await ApiBinary.one('mlStudioLookupDictsListRequest', { projectId: pid });
      S.dicts = JSON.parse(resp.dictsJson ?? resp.dicts_json ?? '[]');
    } catch { S.dicts = []; }
  }

  async function loadModels() {
    try {
      const ocr = await ApiBinary.one('mlStudioServiceModelsListRequest', { capability: 'ocr' });
      S.ocrModels = JSON.parse(ocr.modelsJson ?? ocr.models_json ?? '[]');
    } catch { S.ocrModels = []; }
    try {
      const cls = await ApiBinary.one('mlStudioServiceModelsListRequest', { capability: 'classifier' });
      S.classifierModels = JSON.parse(cls.modelsJson ?? cls.models_json ?? '[]');
    } catch { S.classifierModels = []; }
  }

  // Zasiew klas z istniejących kategorii COCO projektu, gdy schemat jest pusty.
  // Pomija kategorię id 0 jeśli istnieje więcej niż jedna (placeholder/tło) —
  // zgodnie z logiką defaultCat w zakładce Anotacje.
  async function seedClassesFromCoco() {
    try {
      const dsResp = await ApiBinary.one('mlStudioDatasetsListRequest', { projectId: pid });
      const list = (dsResp.datasets || []).filter((d) => (d.kind || '') === 'coco_path');
      if (!list.length) return [];
      const datasetId = list[0].datasetId ?? list[0].dataset_id;
      const imgResp = await ApiBinary.one('mlStudioRecogImagesListRequest', { datasetId });
      const cats = JSON.parse(imgResp.categoriesJson ?? imgResp.categories_json ?? '[]');
      const usable = cats.length > 1 ? cats.filter((c) => c.id !== 0) : cats;
      return usable.map((cat, i) => ({
        name: cat.name, color: RECOG_SCHEMA_PALETTE[i % RECOG_SCHEMA_PALETTE.length],
        shape: 'box', attributes: [],
      }));
    } catch { return []; }
  }

  byId('ml-studio-schema-add-class')?.addEventListener('click', () => {
    const idx = S.classes.length;
    S.classes.push({ name: `klasa_${idx + 1}`, color: RECOG_SCHEMA_PALETTE[idx % RECOG_SCHEMA_PALETTE.length], shape: 'box', attributes: [] });
    S.selected = idx;
    S.adding = null;
    markDirty();
    renderClassList();
    renderEditor();
  });

  byId('ml-studio-schema-save')?.addEventListener('click', async () => {
    const btn = byId('ml-studio-schema-save');
    btn?.setAttribute('disabled', '');
    try {
      await ApiBinary.one('mlStudioSchemaSaveRequest', { projectId: pid, schemaJson: JSON.stringify({ classes: S.classes }) });
      S.dirty = false;
      const el = byId('ml-studio-schema-dirty');
      if (el) el.textContent = 'Zapisano';
      toast('Schemat zapisany.', 'success');
    } catch (err) { toast(`Zapis schematu: ${err.message}`, 'error'); }
    finally { btn?.removeAttribute('disabled'); }
  });

  (async () => {
    await Promise.all([loadDicts(), loadModels()]);
    let schema = {};
    try {
      const resp = await ApiBinary.one('mlStudioSchemaGetRequest', { projectId: pid });
      schema = JSON.parse(resp.schemaJson ?? resp.schema_json ?? '{}');
    } catch { schema = {}; }
    if (Array.isArray(schema.classes) && schema.classes.length) {
      S.classes = schema.classes.map((c) => ({
        name: c.name || 'klasa', color: c.color || RECOG_SCHEMA_PALETTE[0], shape: c.shape || 'box',
        attributes: Array.isArray(c.attributes) ? c.attributes : [],
      }));
    } else {
      S.classes = await seedClassesFromCoco();
    }
    S.selected = 0;
    renderClassList();
    renderEditor();
  })();
}

// Etykiety celu treningu w segmented control (kolejność jak w deriveTrainTargets).
const TRAIN_TARGET_LABELS = {
  detection: 'Detekcja',
  classifier: 'Klasyfikator',
  ocr: 'OCR',
};

// Zakładka "Trening" dla recognition: wybór CELU (detekcja / klasyfikator / OCR)
// wyprowadzonego ze schematu projektu, potem wybór datasetu + wariantu +
// hiperparametry + start treningu. Po starcie przechodzi w widok LIVE.
function renderRecogTrainTab(panel, p, { selectTab, focusRunId = '', focusKind = '' } = {}) {
  const pid = projectId(p);
  const cfg = getRecogCfg(pid);
  // Cele wyprowadzone ze schematu; wypełniane asynchronicznie. Do czasu wczytania
  // dostępna jest tylko detekcja (zawsze obecna).
  let trainTargets = [{ task: 'detection' }];
  let datasetList = [];

  panel.innerHTML = `
    <div class="ml-studio-ft">
      <div id="ml-studio-recog-setup">
        <section class="ml-studio-data-card">
          <div class="ml-studio-data-head">${sprite('model')} Cel treningu
            <span class="ml-studio-data-hint">wybierz co trenujesz — w jednym projekcie może powstać wiele modeli</span>
          </div>
          <div class="ml-studio-target-seg" id="ml-studio-train-target" role="tablist"></div>
        </section>
        <section class="ml-studio-data-card">
          <div class="ml-studio-data-head">${sprite('database')} Zbiór treningowy (COCO)</div>
          <tf-select id="ml-studio-recog-dataset" label="Dataset" placeholder="wybierz zarejestrowany dataset COCO"></tf-select>
          <div id="ml-studio-recog-classes" class="ml-studio-data-origin-text" style="margin-top:8px"></div>
        </section>
        <section class="ml-studio-data-card">
          <div class="ml-studio-data-head">${sprite('services')} Węzeł treningu (mesh)
            <span class="ml-studio-data-hint">trening lokalnie albo na zdalnym węźle (Node B); dataset COCO musi być widoczny na wybranym węźle</span>
          </div>
          <tf-select id="ml-studio-recog-node" label="Węzeł"></tf-select>
        </section>
        <div id="ml-studio-train-form"></div>
        <div class="ml-studio-ft-actions">
          <tf-button variant="primary" icon="play" id="ml-studio-recog-run">Uruchom trening</tf-button>
        </div>
      </div>
      <div id="ml-studio-recog-live"></div>
    </div>
  `;

  // Wznowienie widoku LIVE dla joba wybranego w panelu „Joby": chowamy formularz
  // startu i od razu podłączamy polling. Klasyfikator raportuje przez generyczny
  // status (macro-F1), detekcja przez status detekcji (mAP@50).
  if (focusRunId) {
    const setup = byId('ml-studio-recog-setup');
    if (setup) setup.hidden = true;
    const isClassifier = String(focusKind || '').toLowerCase().includes('klas')
      || String(focusKind || '').toLowerCase() === 'classifier';
    const liveOpts = isClassifier
      ? { selectTab, metricLabel: 'macro-F1', statusRequest: 'mlStudioGenericTrainStatusRequest' }
      : { selectTab };
    startRecogLive(byId('ml-studio-recog-live'), focusRunId, liveOpts);
    return;
  }

  // Segmented control celu: detekcja zawsze aktywna, klasyfikator aktywny gdy
  // schemat ma nadający się atrybut, OCR na razie disabled (faza 2).
  function renderTargetSeg() {
    const host = byId('ml-studio-train-target');
    if (!host) return;
    const hasClassifier = trainTargets.some((t) => t.task === 'classifier');
    const avail = { detection: true, classifier: hasClassifier, ocr: false };
    host.innerHTML = ['detection', 'classifier', 'ocr'].map((task) => {
      const on = cfg.target === task;
      const dis = !avail[task];
      const suffix = task === 'ocr' ? ' (wkrótce)' : '';
      return `<button type="button" role="tab" data-target="${task}"
        class="ml-studio-target-seg-btn${on ? ' selected' : ''}"
        aria-selected="${on}"${dis ? ' disabled' : ''}>${escapeHtml(TRAIN_TARGET_LABELS[task])}${suffix}</button>`;
    }).join('');
    host.querySelectorAll('.ml-studio-target-seg-btn').forEach((btn) => {
      btn.addEventListener('click', () => {
        if (btn.hasAttribute('disabled')) return;
        const task = btn.getAttribute('data-target');
        if (task === cfg.target) return;
        cfg.target = task;
        renderTargetSeg();
        renderTargetForm();
      });
    });
  }

  // Dynamiczny formularz zależny od celu. detection → dotychczasowe karty RF-DETR;
  // classifier → atrybut + klasa źródłowa + podgląd etykiet + warianty timm + HP.
  function renderTargetForm() {
    const host = byId('ml-studio-train-form');
    if (!host) return;
    if (cfg.target === 'classifier') {
      host.innerHTML = classifierFormHtml();
      bindClassifierForm();
    } else {
      host.innerHTML = detectionFormHtml();
      bindDetectionForm();
    }
  }

  function detectionFormHtml() {
    const variantCards = RECOG_VARIANTS.map((v) => `
      <button type="button" class="ml-studio-ft-axis-card${cfg.variant === v.id ? ' selected' : ''}"
              data-variant="${escapeAttr(v.id)}" aria-pressed="${cfg.variant === v.id}">
        <div class="ml-studio-ft-axis-name">${escapeHtml(v.name)}</div>
        <p class="ml-studio-ft-axis-desc">${escapeHtml(v.desc)}</p>
      </button>`).join('');
    const hpInputs = RECOG_HP.map((h) => `
      <div class="ml-studio-ft-hp-field">
        <tf-input type="number" label="${escapeAttr(h.label)}" id="ml-studio-recog-hp-${escapeAttr(h.key)}"
                  value="${escapeAttr(String(cfg.hyperparams[h.key]))}" min="${escapeAttr(String(h.min))}" step="${escapeAttr(h.step)}"></tf-input>
      </div>`).join('');
    return `
      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('image')} Wariant modelu RF-DETR</div>
        <div class="ml-studio-ft-axis-grid" id="ml-studio-recog-variants">${variantCards}</div>
      </section>
      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('tune')} Hiperparametry</div>
        <div class="ml-studio-ft-hp-grid">${hpInputs}</div>
        <p class="ml-studio-ft-hp-hint">Rozdzielczość zostanie zaokrąglona do wielokrotności 32 (nano/small/medium) lub 56 (base/large — backbone DINOv2).</p>
      </section>`;
  }

  function bindDetectionForm() {
    byId('ml-studio-recog-variants')?.querySelectorAll('.ml-studio-ft-axis-card').forEach((card) => {
      card.addEventListener('click', () => {
        cfg.variant = card.getAttribute('data-variant');
        panel.querySelectorAll('#ml-studio-recog-variants .ml-studio-ft-axis-card').forEach((c) => {
          const on = c === card;
          c.classList.toggle('selected', on);
          c.setAttribute('aria-pressed', String(on));
        });
      });
    });
    for (const h of RECOG_HP) {
      byId('ml-studio-recog-hp-' + h.key)?.addEventListener('input', (e) => {
        const v = Number(e.target.value);
        if (Number.isFinite(v)) cfg.hyperparams[h.key] = v;
      });
    }
  }

  function classifierFormHtml() {
    const variantCards = CLF_VARIANTS.map((v) => `
      <button type="button" class="ml-studio-ft-axis-card${cfg.clfVariant === v.id ? ' selected' : ''}"
              data-variant="${escapeAttr(v.id)}" aria-pressed="${cfg.clfVariant === v.id}">
        <div class="ml-studio-ft-axis-name">${escapeHtml(v.name)}</div>
        <p class="ml-studio-ft-axis-desc">${escapeHtml(v.desc)}</p>
      </button>`).join('');
    const hpInputs = CLF_HP.map((h) => `
      <div class="ml-studio-ft-hp-field">
        <tf-input type="number" label="${escapeAttr(h.label)}" id="ml-studio-clf-hp-${escapeAttr(h.key)}"
                  value="${escapeAttr(String(cfg.clfHyperparams[h.key]))}" min="${escapeAttr(String(h.min))}" step="${escapeAttr(h.step)}"></tf-input>
      </div>`).join('');
    return `
      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('model')} Atrybut do klasyfikacji</div>
        <tf-select id="ml-studio-train-attr" label="Atrybut"></tf-select>
        <tf-select id="ml-studio-train-source-class" label="Klasa źródłowa cropów" style="margin-top:8px"></tf-select>
        <div id="ml-studio-train-labels" class="ml-studio-data-origin-text" style="margin-top:8px"></div>
      </section>
      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('image')} Wariant klasyfikatora</div>
        <div class="ml-studio-ft-axis-grid" id="ml-studio-clf-variants">${variantCards}</div>
      </section>
      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('tune')} Hiperparametry</div>
        <div class="ml-studio-ft-hp-grid">${hpInputs}</div>
        <div class="ml-studio-schema-toggle-row" style="margin-top:10px">
          <tf-toggle id="ml-studio-clf-freeze"${cfg.clfHyperparams.freezeBackbone ? ' checked' : ''}></tf-toggle>
          <span><strong>Zamroź backbone</strong> — trenuje tylko głowicę klasyfikatora (szybciej, mniej danych)</span>
        </div>
      </section>`;
  }

  function clfTargets() {
    return trainTargets.filter((t) => t.task === 'classifier');
  }

  function bindClassifierForm() {
    const attrs = clfTargets();
    if (!attrs.length) return;
    // Utrzymaj wybór atrybutu w granicach dostępnych celów.
    let current = attrs.find((t) => t.attribute === cfg.attribute) || attrs[0];
    cfg.attribute = current.attribute;

    const attrSel = byId('ml-studio-train-attr');
    const attrOpts = attrs.map((t) => ({ value: t.attribute, label: t.attribute }));
    if (attrSel?.setOptions) attrSel.setOptions(attrOpts, cfg.attribute);
    else if (attrSel) attrSel.innerHTML = attrOpts.map((o) => `<option value="${escapeAttr(o.value)}"${o.value === cfg.attribute ? ' selected' : ''}>${escapeHtml(o.label)}</option>`).join('');

    const syncSourceAndLabels = () => {
      current = attrs.find((t) => t.attribute === cfg.attribute) || attrs[0];
      const srcSel = byId('ml-studio-train-source-class');
      // Opcja "Wszystkie klasy" (wartość "") na górze i domyślnie wybrana — serwis
      // traktuje pusty sourceClass jako dowolną kategorię i trenuje na cropach ze
      // wszystkich klas mających dany atrybut.
      const srcOpts = [{ value: '', label: 'Wszystkie klasy' }, ...(current.sourceClasses || []).map((c) => ({ value: c, label: c }))];
      if (!srcOpts.some((o) => o.value === cfg.sourceClass)) cfg.sourceClass = '';
      if (srcSel?.setOptions) srcSel.setOptions(srcOpts, cfg.sourceClass);
      else if (srcSel) srcSel.innerHTML = srcOpts.map((o) => `<option value="${escapeAttr(o.value)}"${o.value === cfg.sourceClass ? ' selected' : ''}>${escapeHtml(o.label)}</option>`).join('');
      renderClfLabels(current);
    };

    attrSel?.addEventListener('change', (e) => {
      cfg.attribute = e.detail?.value || attrSel.value || '';
      syncSourceAndLabels();
    });
    byId('ml-studio-train-source-class')?.addEventListener('change', (e) => {
      cfg.sourceClass = e.detail?.value || byId('ml-studio-train-source-class').value || '';
    });
    syncSourceAndLabels();

    byId('ml-studio-clf-variants')?.querySelectorAll('.ml-studio-ft-axis-card').forEach((card) => {
      card.addEventListener('click', () => {
        cfg.clfVariant = card.getAttribute('data-variant');
        panel.querySelectorAll('#ml-studio-clf-variants .ml-studio-ft-axis-card').forEach((c) => {
          const on = c === card;
          c.classList.toggle('selected', on);
          c.setAttribute('aria-pressed', String(on));
        });
      });
    });
    for (const h of CLF_HP) {
      byId('ml-studio-clf-hp-' + h.key)?.addEventListener('input', (e) => {
        const v = Number(e.target.value);
        if (Number.isFinite(v)) cfg.clfHyperparams[h.key] = v;
      });
    }
    byId('ml-studio-clf-freeze')?.addEventListener('change', (e) => {
      cfg.clfHyperparams.freezeBackbone = !!e.detail?.checked;
    });
  }

  // Podgląd etykiet klasyfikatora: liczba klas + wartości; jeśli profil datasetu
  // zawiera liczności per wartość — dokładamy je w nawiasie.
  function renderClfLabels(target) {
    const box = byId('ml-studio-train-labels');
    if (!box || !target) return;
    const values = target.values || [];
    const counts = attrCountsFromProfile(cfg.datasetId, target.attribute);
    const parts = values.map((v) => {
      const n = counts ? counts[v] : null;
      return n != null ? `${escapeHtml(v)} (${n})` : escapeHtml(v);
    });
    box.innerHTML = values.length
      ? `${sprite('info')} ${values.length} klas: ${parts.join(' / ')}`
      : '';
  }

  // Liczności wartości atrybutu z profilu datasetu (gdy backend je udostępnia w
  // profileJson jako attributes[nazwa] = { wartość: liczba }). Brak → null.
  function attrCountsFromProfile(dsId, attribute) {
    const d = datasetList.find((x) => (x.datasetId ?? x.dataset_id) === dsId);
    if (!d) return null;
    let prof = d.profileJson ?? d.profile_json;
    try { prof = typeof prof === 'string' ? JSON.parse(prof) : prof; } catch (_) { return null; }
    const c = prof && prof.attributes && prof.attributes[attribute];
    return c && typeof c === 'object' ? c : null;
  }

  // Lista datasetów COCO do selecta.
  (async () => {
    try {
      const resp = await ApiBinary.one('mlStudioDatasetsListRequest', { projectId: pid });
      const list = (resp.datasets || []).filter((d) => (d.kind || '') === 'coco_path' || (d.kind || '') === 'coco');
      datasetList = list;
      const sel = byId('ml-studio-recog-dataset');
      if (sel) {
        sel.innerHTML = list.map((d) => `<option value="${escapeAttr(d.datasetId ?? d.dataset_id)}">${escapeHtml(d.name)} (${d.rowCount ?? d.row_count ?? 0} obr.)</option>`).join('');
        if (list.length) {
          cfg.datasetId = cfg.datasetId || (list[0].datasetId ?? list[0].dataset_id);
          sel.value = cfg.datasetId;
          showRecogClasses(list, cfg.datasetId);
        }
      }
      sel?.addEventListener('change', (e) => {
        cfg.datasetId = e.detail?.value || sel.value;
        showRecogClasses(list, cfg.datasetId);
        if (cfg.target === 'classifier') renderClfLabels(clfTargets().find((t) => t.attribute === cfg.attribute));
      });
    } catch (_) { /* brak datasetów — select pusty */ }
  })();

  function showRecogClasses(list, dsId) {
    const d = list.find((x) => (x.datasetId ?? x.dataset_id) === dsId);
    const box = byId('ml-studio-recog-classes');
    if (!box || !d) return;
    let prof = d.profileJson ?? d.profile_json;
    try { prof = typeof prof === 'string' ? JSON.parse(prof) : prof; } catch (_) { prof = null; }
    const classes = (prof && prof.classes) || [];
    const splits = (prof && prof.splits) || [];
    box.innerHTML = classes.length
      ? `${sprite('info')} ${classes.length} klas: ${classes.map(escapeHtml).join(', ')} · splity: ${splits.map(escapeHtml).join('/')}`
      : '';
  }

  // Lista węzłów mesh do wyboru miejsca treningu (lokalnie / Node B).
  (async () => {
    const nodeSel = byId('ml-studio-recog-node');
    if (!nodeSel) return;
    const opts = [{ value: '', label: 'Lokalnie (ten węzeł)' }];
    try {
      const resp = await ApiBinary.one('meshNodeListRequest');
      for (const n of (resp.nodes || [])) {
        const id = String(n.nodeId ?? n.node_id ?? '');
        if (id) opts.push({ value: id, label: `${n.hostname || id.slice(0, 12)} (zdalny)` });
      }
    } catch (_) { /* brak mesh — tylko lokalnie */ }
    if (typeof nodeSel.setOptions === 'function') nodeSel.setOptions(opts, '');
    else nodeSel.innerHTML = opts.map((o) => `<option value="${escapeAttr(o.value)}">${escapeHtml(o.label)}</option>`).join('');
    nodeSel.addEventListener('change', (e) => { cfg.targetNodeId = e.detail?.value || nodeSel.value || ''; });
  })();

  // Wyprowadź cele ze schematu projektu + kategorii COCO datasetu.
  (async () => {
    let schema = {};
    let cocoCategories = [];
    try {
      const resp = await ApiBinary.one('mlStudioSchemaGetRequest', { projectId: pid });
      schema = JSON.parse(resp.schemaJson ?? resp.schema_json ?? '{}');
    } catch (_) { schema = {}; }
    try {
      const dsResp = await ApiBinary.one('mlStudioDatasetsListRequest', { projectId: pid });
      const first = (dsResp.datasets || []).find((d) => (d.kind || '') === 'coco_path');
      if (first) {
        const imgResp = await ApiBinary.one('mlStudioRecogImagesListRequest', { datasetId: first.datasetId ?? first.dataset_id });
        cocoCategories = JSON.parse(imgResp.categoriesJson ?? imgResp.categories_json ?? '[]');
      }
    } catch (_) { cocoCategories = []; }
    trainTargets = deriveTrainTargets(schema, cocoCategories);
    // Jeśli zapamiętany cel nie ma już pokrycia, wróć do detekcji.
    if (cfg.target === 'classifier' && !trainTargets.some((t) => t.task === 'classifier')) cfg.target = 'detection';
    renderTargetSeg();
    renderTargetForm();
  })();

  // Wstępny render (do wczytania schematu — tylko detekcja).
  renderTargetSeg();
  renderTargetForm();

  byId('ml-studio-recog-run')?.addEventListener('click', async () => {
    if (!cfg.datasetId) { toast('Wybierz zarejestrowany dataset COCO.', 'error'); return; }
    const btn = byId('ml-studio-recog-run');
    btn?.setAttribute('disabled', '');
    try {
      let runId;
      let liveOpts = { selectTab };
      if (cfg.target === 'classifier') {
        const target = clfTargets().find((t) => t.attribute === cfg.attribute);
        if (!target) throw new Error('Wybierz atrybut do klasyfikacji.');
        const resp = await ApiBinary.one('mlStudioClassifierTrainStartRequest', {
          projectId: pid,
          datasetId: cfg.datasetId,
          attribute: cfg.attribute,
          sourceClass: cfg.sourceClass,
          variant: cfg.clfVariant,
          values: target.values || [],
          hyperparams: {
            epochs: cfg.clfHyperparams.epochs,
            batchSize: cfg.clfHyperparams.batchSize,
            learningRate: cfg.clfHyperparams.learningRate,
            imageSize: cfg.clfHyperparams.imageSize,
            freezeBackbone: cfg.clfHyperparams.freezeBackbone,
          },
          targetNodeId: cfg.targetNodeId || '',
        });
        runId = resp.runId ?? resp.run_id;
        liveOpts = { selectTab, metricLabel: 'macro-F1', statusRequest: 'mlStudioGenericTrainStatusRequest' };
      } else {
        const resp = await ApiBinary.one('mlStudioRecogTrainStartRequest', {
          projectId: pid,
          datasetId: cfg.datasetId,
          variant: cfg.variant,
          targetNodeId: cfg.targetNodeId || '',
          hyperparams: {
            epochs: cfg.hyperparams.epochs,
            batchSize: cfg.hyperparams.batchSize,
            gradAccum: cfg.hyperparams.gradAccum,
            learningRate: cfg.hyperparams.learningRate,
            resolution: cfg.hyperparams.resolution,
            earlyStopping: cfg.earlyStopping,
          },
        });
        runId = resp.runId ?? resp.run_id;
      }
      if (!runId) throw new Error('Backend nie zwrócił runId.');
      const setup = byId('ml-studio-recog-setup');
      if (setup) setup.hidden = true;
      startRecogLive(byId('ml-studio-recog-live'), runId, liveOpts);
    } catch (err) {
      btn?.removeAttribute('disabled');
      toast(`Start treningu: ${err.message}`, 'error');
    }
  });
}

// Widok LIVE treningu detekcji + polling (reużywa ftPollTimer/stopFtPolling).
// Wczytuje plik obrazu i zmniejsza do maxDim (dłuższy bok) na canvasie, zwraca
// JPEG base64. Detekcja działa w niskiej rozdzielczości, więc nie ma sensu słać
// pełnego zdjęcia z aparatu (i tak przekroczyłoby limit ramki WS).
function downscaleImageToB64(file, maxDim) {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onload = () => {
      URL.revokeObjectURL(url);
      const scale = Math.min(1, maxDim / Math.max(img.width, img.height || 1));
      const w = Math.max(1, Math.round(img.width * scale));
      const h = Math.max(1, Math.round(img.height * scale));
      const canvas = document.createElement('canvas');
      canvas.width = w;
      canvas.height = h;
      canvas.getContext('2d').drawImage(img, 0, 0, w, h);
      const dataUrl = canvas.toDataURL('image/jpeg', 0.85);
      resolve({ b64: dataUrl.split(',')[1] || '', mime: 'image/jpeg' });
    };
    img.onerror = () => { URL.revokeObjectURL(url); reject(new Error('nie można wczytać obrazu')); };
    img.src = url;
  });
}

// Formatuje liczbę bajtów do czytelnej jednostki (B/KB/MB/GB).
function fmtBytes(n) {
  const b = Number(n) || 0;
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
  if (b < 1024 * 1024 * 1024) return `${(b / (1024 * 1024)).toFixed(1)} MB`;
  return `${(b / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

// Prędkość transferu (bajty/s) → czytelne /s.
function fmtRate(bps) {
  const r = Number(bps) || 0;
  return r > 0 ? `${fmtBytes(r)}/s` : '—';
}

// Czas trwania w sekundach → mm:ss (poniżej godziny) lub hh:mm:ss (powyżej).
// Używane do „czasu trwania" (elapsed_s) w widoku LIVE.
function fmtDuration(seconds) {
  const s = Math.max(0, Math.floor(Number(seconds) || 0));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const pad = (n) => String(n).padStart(2, '0');
  return h > 0 ? `${h}:${pad(m)}:${pad(sec)}` : `${pad(m)}:${pad(sec)}`;
}

// Szacowany czas do końca w sekundach → przyjazny opis („~12 min", „~45 s",
// „~1 h 5 min"). Zwraca '—' dla braku/wartości niedodatnich.
function fmtEta(seconds) {
  const s = Number(seconds);
  if (!Number.isFinite(s) || s <= 0) return '—';
  if (s < 90) return `~${Math.round(s)} s`;
  const totalMin = Math.round(s / 60);
  if (totalMin < 60) return `~${totalMin} min`;
  const h = Math.floor(totalMin / 60);
  const m = totalMin % 60;
  return m > 0 ? `~${h} h ${m} min` : `~${h} h`;
}

// Pamięć GPU w MB → gigabajty z polskim przecinkiem („6,9 GB"). Poniżej 1 GB
// pokazuje pełne MB, żeby drobne joby nie wyświetlały „0,0 GB".
function fmtGb(mb) {
  const m = Number(mb);
  if (!Number.isFinite(m) || m <= 0) return '—';
  if (m < 1024) return `${Math.round(m)} MB`;
  return `${(m / 1024).toLocaleString('pl-PL', { minimumFractionDigits: 1, maximumFractionDigits: 1 })} GB`;
}

// Etykiety etapów treningu (pole `stage` z backendu). Nieznany slug pokazujemy
// dosłownie, żeby nie ukrywać realnego stanu przekazanego przez Core.
const STAGE_LABEL = {
  init: 'inicjalizacja',
  loading: 'wczytywanie danych',
  warmup: 'rozgrzewka',
  training: 'trening',
  validating: 'walidacja',
  saving: 'zapis modelu',
  exporting: 'eksport',
  finalizing: 'finalizacja',
};

function stageLabel(stage) {
  const s = String(stage || '').toLowerCase();
  if (!s) return '';
  return STAGE_LABEL[s] || stage;
}

function startRecogLive(host, runId, { selectTab, metricLabel = 'mAP@50', statusRequest = 'mlStudioRecogTrainStatusRequest' } = {}) {
  if (!host) return;
  stopFtPolling();
  // Klasyfikator raportuje przez generyczny status (curve:[{epoch,metricName,value}])
  // i inną metrykę główną (macro-F1); detekcja zostaje przy mAP@50 + train loss.
  const isGeneric = statusRequest !== 'mlStudioRecogTrainStatusRequest';
  const headTitle = isGeneric ? 'Trening klasyfikatora na żywo' : 'Trening detekcji na żywo';
  host.innerHTML = `
    <section class="ml-studio-data-card ml-studio-ft-live">
      <div class="ml-studio-data-head">${sprite('cpu')} ${escapeHtml(headTitle)}
        <span class="ml-studio-ft-status" id="ml-studio-recog-badge"><tf-badge tone="warning" value="trening trwa"></tf-badge></span>
      </div>
      <div class="ml-studio-ft-progress">
        <div class="ml-studio-ft-progress-meta" id="ml-studio-recog-meta">epoka 0</div>
        <tf-progress-bar id="ml-studio-recog-bar" value="0" tone="accent"></tf-progress-bar>
        <div class="ml-studio-live-info" id="ml-studio-recog-info" hidden></div>
      </div>
      <div class="ml-studio-ft-kpi-grid" id="ml-studio-recog-kpi"></div>
      <div class="ml-studio-ft-chart-wrap">
        <div class="ml-studio-ft-chart-head">
          <span class="ml-studio-ft-chart-title">Krzywa: train loss + ${escapeHtml(metricLabel)}</span>
          <span class="ml-studio-ft-chart-legend">
            <span class="lg"><span class="sw train"></span>train loss</span>
            <span class="lg"><span class="sw eval"></span>${escapeHtml(metricLabel)}</span>
          </span>
        </div>
        <div id="ml-studio-recog-chart"></div>
      </div>
      <div class="ml-studio-ft-done" id="ml-studio-recog-done" hidden></div>
    </section>
  `;

  // Stan do samodzielnego liczenia ETA, gdy backend nie poda `eta_s`: mierzymy
  // realny czas między zmianami epoki (delta czasu / delta epok) i wygładzamy,
  // po czym mnożymy przez liczbę pozostałych epok. `liveStartedAt` służy jako
  // zapasowe źródło „czasu trwania", gdy brak `elapsed_s`.
  const liveStartedAt = Date.now();
  const etaTracker = { lastEpoch: -1, lastTime: 0, secPerEpoch: 0 };

  const renderStatus = (st) => {
    const status = String(st.status || 'running');
    // Faza transferu datasetu przez mesh (trening na zdalnym węźle B): pasek
    // postępu z prędkością B/s. Błąd dopiero przy stallu (po stronie Core).
    if (status === 'syncing') {
      const syncPhase = String(st.syncPhase ?? st.sync_phase ?? 'syncing');
      const sent = Number(st.syncBytesSent ?? st.sync_bytes_sent ?? 0);
      const tot = Number(st.syncBytesTotal ?? st.sync_bytes_total ?? 0);
      const rate = Number(st.syncRateBps ?? st.sync_rate_bps ?? 0);
      const pct = tot > 0 ? Math.max(0, Math.min(100, Math.round((sent / tot) * 100))) : 0;
      const phaseLabel = syncPhase === 'zipping' ? 'pakowanie datasetu'
        : syncPhase === 'starting' ? 'uruchamianie treningu na węźle'
        : 'transfer datasetu przez mesh';
      const meta = byId('ml-studio-recog-meta');
      const bar = byId('ml-studio-recog-bar');
      if (meta) {
        meta.innerHTML = syncPhase === 'syncing'
          ? `${phaseLabel} · ${fmtBytes(sent)} / ${fmtBytes(tot)} · ${pct}% · ${fmtRate(rate)}`
          : `<tf-spinner size="sm"></tf-spinner> ${phaseLabel}`;
      }
      if (bar) bar.setAttribute('value', String(pct));
      const kpi = byId('ml-studio-recog-kpi');
      if (kpi) {
        kpi.innerHTML = `
          <div class="ml-studio-ft-kpi"><div class="lbl">wysłano</div><div class="val">${fmtBytes(sent)}</div></div>
          <div class="ml-studio-ft-kpi"><div class="lbl">rozmiar</div><div class="val">${fmtBytes(tot)}</div></div>
          <div class="ml-studio-ft-kpi"><div class="lbl">prędkość</div><div class="val">${fmtRate(rate)}</div></div>
        `;
      }
      const chart = byId('ml-studio-recog-chart');
      if (chart) chart.innerHTML = `<div class="ml-studio-ft-sync-note">${sprite('cloud')} Dataset przenoszony na węzeł treningowy przez mesh. Trening ruszy po zmaterializowaniu danych.</div>`;
      const badge = byId('ml-studio-recog-badge');
      if (badge) badge.innerHTML = '<tf-badge tone="info" value="transfer danych"></tf-badge>';
      return;
    }
    const epoch = Number(st.epoch ?? 0);
    const total = Number(st.totalEpochs ?? st.total_epochs ?? 0);
    // Znormalizuj krzywą do wspólnego kształtu {epoch, loss, metric}. Detekcja ma
    // pola per-punkt (trainLoss/map50); generyczny status ma [{epoch,metricName,value}]
    // — punkty z metricName zawierającym „loss" idą na oś strat, reszta na metrykę.
    const rawCurve = Array.isArray(st.curve) ? st.curve : [];
    let curve;
    let loss;
    let metricVal;
    if (isGeneric) {
      const byEpoch = new Map();
      for (const c of rawCurve) {
        const e = Number(c.epoch ?? 0);
        if (!byEpoch.has(e)) byEpoch.set(e, { epoch: e, trainLoss: undefined, map50: undefined });
        const slot = byEpoch.get(e);
        const name = String(c.metricName ?? c.metric_name ?? '').toLowerCase();
        const val = Number(c.value);
        if (name.includes('loss')) slot.trainLoss = val;
        else slot.map50 = val;
      }
      curve = [...byEpoch.values()].sort((a, b) => a.epoch - b.epoch);
      const last = curve[curve.length - 1];
      loss = last?.trainLoss;
      metricVal = last?.map50;
    } else {
      curve = rawCurve;
      loss = st.trainLoss ?? st.train_loss;
      metricVal = st.map50;
    }
    const meta = byId('ml-studio-recog-meta');
    const bar = byId('ml-studio-recog-bar');
    if (total > 0) {
      const pct = Math.max(0, Math.min(100, Math.round((epoch / total) * 100)));
      if (meta) meta.textContent = `epoka ${epoch} / ${total} · ${pct}%`;
      if (bar) bar.setAttribute('value', String(pct));
    } else if (meta) {
      meta.innerHTML = `<tf-spinner size="sm"></tf-spinner> trwa — epoka ${epoch}`;
    }

    // Nowe pola statusu (backend doda; toleruj brak): etap, czas trwania, ETA i
    // pamięć GPU joba. ETA bierzemy z `eta_s`, a gdy go nie ma — liczymy sami z
    // tempa zmian epoki mierzonego między pollami.
    const stage = stageLabel(st.stage);
    const elapsedS = Number(st.elapsedS ?? st.elapsed_s);
    const gpuMemMb = Number(st.gpuMemMb ?? st.gpu_mem_mb);
    let etaS = Number(st.etaS ?? st.eta_s);
    if (epoch !== etaTracker.lastEpoch) {
      const now = Date.now();
      if (etaTracker.lastEpoch >= 0 && epoch > etaTracker.lastEpoch) {
        const perEpoch = ((now - etaTracker.lastTime) / 1000) / (epoch - etaTracker.lastEpoch);
        etaTracker.secPerEpoch = etaTracker.secPerEpoch > 0
          ? etaTracker.secPerEpoch * 0.6 + perEpoch * 0.4
          : perEpoch;
      }
      etaTracker.lastEpoch = epoch;
      etaTracker.lastTime = now;
    }
    if ((!Number.isFinite(etaS) || etaS <= 0) && etaTracker.secPerEpoch > 0 && total > 0) {
      etaS = etaTracker.secPerEpoch * Math.max(0, total - epoch);
    }
    const elapsedShown = Number.isFinite(elapsedS) && elapsedS > 0
      ? elapsedS
      : Math.floor((Date.now() - liveStartedAt) / 1000);
    const info = byId('ml-studio-recog-info');
    if (info) {
      const items = [];
      if (stage) items.push({ ico: 'zap', lbl: 'etap', val: stage });
      if (total > 0) items.push({ ico: 'clock', lbl: 'epoka', val: `${epoch} / ${total}` });
      items.push({ ico: 'clock', lbl: 'czas', val: fmtDuration(elapsedShown) });
      items.push({ ico: 'clock', lbl: 'ETA', val: fmtEta(etaS) });
      if (Number.isFinite(gpuMemMb) && gpuMemMb > 0) {
        items.push({ ico: 'cpu', lbl: 'VRAM', val: fmtGb(gpuMemMb) });
      }
      info.hidden = false;
      info.innerHTML = items.map((it) => `
        <span class="ml-studio-live-info-item">${sprite(it.ico)}
          <span class="ml-studio-live-info-lbl">${escapeHtml(it.lbl)}</span>
          <span class="ml-studio-live-info-val">${escapeHtml(it.val)}</span>
        </span>`).join('');
    }
    const kpi = byId('ml-studio-recog-kpi');
    if (kpi) {
      kpi.innerHTML = `
        <div class="ml-studio-ft-kpi"><div class="lbl">train loss</div><div class="val">${loss != null ? Number(loss).toFixed(4) : '—'}</div></div>
        <div class="ml-studio-ft-kpi"><div class="lbl">${escapeHtml(metricLabel)}</div><div class="val">${metricVal != null ? Number(metricVal).toFixed(4) : '—'}</div></div>
        <div class="ml-studio-ft-kpi"><div class="lbl">epoka</div><div class="val">${epoch}${total > 0 ? ' / ' + total : ''}</div></div>
      `;
    }
    const chart = byId('ml-studio-recog-chart');
    if (chart) chart.innerHTML = renderRecogChart(curve);
    const badge = byId('ml-studio-recog-badge');
    if (badge) {
      if (status === 'succeeded') badge.innerHTML = '<tf-badge tone="success" value="zakończony"></tf-badge>';
      else if (status === 'failed') badge.innerHTML = '<tf-badge tone="danger" value="błąd"></tf-badge>';
      else badge.innerHTML = '<tf-badge tone="warning" value="trening trwa"></tf-badge>';
    }
    if (status === 'succeeded') {
      stopFtPolling();
      const done = byId('ml-studio-recog-done');
      if (done) {
        done.hidden = false;
        done.innerHTML = `<div class="ml-studio-ft-done-msg">${sprite('check')} Trening zakończony — model w zakładce „Modele".</div>
          <tf-button variant="outline" icon="layers" id="ml-studio-recog-goto-models">Przejdź do Modele</tf-button>`;
        byId('ml-studio-recog-goto-models')?.addEventListener('click', () => selectTab && selectTab('Modele'));
      }
      toast(isGeneric ? 'Trening klasyfikatora zakończony.' : 'Trening detekcji zakończony.', 'success');
    } else if (status === 'failed') {
      stopFtPolling();
      toast(`Trening nieudany: ${st.error || 'nieznany błąd'}`, 'error');
      const done = byId('ml-studio-recog-done');
      if (done) { done.hidden = false; done.innerHTML = `<div class="ml-studio-ft-done-msg error">${sprite('alert')} ${escapeHtml(st.error || 'Trening zakończył się błędem.')}</div>`; }
    }
  };

  const poll = async () => {
    try {
      const st = await ApiBinary.one(statusRequest, { runId });
      renderStatus(st);
    } catch (err) {
      stopFtPolling();
      toast(`Polling statusu: ${err.message}`, 'error');
    }
  };
  poll();
  ftPollTimer = setInterval(poll, 2500);
}

// Krzywa detekcji: oś X = epoka, lewa seria = train loss, prawa = mAP@50.
// Obie serie skalowane niezależnie do własnego min/max (różne jednostki).
function renderRecogChart(curve) {
  const points = (curve || [])
    .map((c) => ({ epoch: Number(c.epoch ?? 0), loss: c.trainLoss ?? c.train_loss, map: c.map50 }))
    .filter((c) => Number.isFinite(c.epoch));
  if (points.length < 2) {
    return '<div class="ml-studio-ft-chart-empty">Krzywa pojawi się po pierwszych epokach treningu.</div>';
  }
  const W = 600, H = 220, padX = 8, padY = 12;
  const epochs = points.map((p) => p.epoch);
  const minE = Math.min(...epochs), maxE = Math.max(...epochs);
  const spanE = maxE - minE || 1;
  const x = (e) => padX + ((e - minE) / spanE) * (W - 2 * padX);
  const scale = (vals) => {
    const fin = vals.filter(Number.isFinite);
    const lo = fin.length ? Math.min(...fin) : 0;
    const hi = fin.length ? Math.max(...fin) : 1;
    const span = hi - lo || 1;
    return (v) => padY + (1 - (v - lo) / span) * (H - 2 * padY);
  };
  const yLoss = scale(points.map((p) => p.loss));
  const yMap = scale(points.map((p) => p.map));
  const pathFor = (sel, yf) => {
    const segs = []; let started = false;
    for (const p of points) {
      const v = sel(p);
      if (!Number.isFinite(v)) continue;
      segs.push(`${started ? 'L' : 'M'}${x(p.epoch).toFixed(1)},${yf(v).toFixed(1)}`);
      started = true;
    }
    return segs.join(' ');
  };
  const lossPath = pathFor((p) => p.loss, yLoss);
  const mapPath = pathFor((p) => p.map, yMap);
  const grid = [40, 100, 160].map((gy) => `<line x1="0" y1="${gy}" x2="${W}" y2="${gy}" class="grid"/>`).join('');
  return `
    <svg class="ml-studio-ft-loss-svg" viewBox="0 0 ${W} ${H}" preserveAspectRatio="none" role="img" aria-label="Krzywa treningu detekcji">
      <g>${grid}</g>
      ${lossPath ? `<path class="line train" d="${lossPath}"/>` : ''}
      ${mapPath ? `<path class="line eval" d="${mapPath}"/>` : ''}
    </svg>
  `;
}

// =============================================================================
// Widok "Joby" — przegląd uruchomionych treningów w całym ML Studio.
// Woła mlStudioJobsOverviewRequest → { jobs:[...], gpu:{...} }, renderuje nagłówek
// GPU (VRAM + util) i tabelę jobów, auto-odświeżanie co ~3,5 s. Klik w job otwiera
// projekt i wznawia jego widok LIVE (Router → showDetail z runId).
// =============================================================================

// Etykieta typu joba (pole `kind`): tolerujemy warianty PL/EN z backendu.
function jobKindLabel(kind) {
  const k = String(kind || '').toLowerCase();
  if (k.includes('klas') || k === 'classifier') return 'klasyfikator';
  if (k.includes('detek') || k === 'detection') return 'detekcja';
  return kind || '—';
}

async function showJobsOverview() {
  const host = byId('ml-studio-jobs-view');
  if (!host) return;
  stopJobsPolling();

  host.innerHTML = `
    <div class="ml-studio-detail-top">
      <tf-button variant="ghost" icon="chevron-left" id="ml-studio-jobs-back">Projekty</tf-button>
    </div>
    <div class="page-header">
      <div>
        <h1>${sprite('cpu')} Joby</h1>
        <div class="sub">Uruchomione treningi w ML Studio — obciążenie GPU i postęp na żywo</div>
      </div>
      <div class="actions">
        <tf-button variant="ghost" icon="refresh" id="ml-studio-jobs-refresh">Odśwież</tf-button>
      </div>
    </div>
    <div id="ml-studio-jobs-gpu" class="ml-studio-jobs-gpu"></div>
    <div id="ml-studio-jobs-list" class="ml-studio-jobs-list">
      <div class="ml-studio-loading"><tf-spinner></tf-spinner></div>
    </div>
  `;

  byId('ml-studio-jobs-back')?.addEventListener('click', () => {
    stopJobsPolling();
    Router.navigate('ml-studio');
  });

  const renderGpu = (gpu) => {
    const box = byId('ml-studio-jobs-gpu');
    if (!box) return;
    if (!gpu || !(gpu.name || gpu.memTotalMb || gpu.mem_total_mb)) {
      box.innerHTML = '';
      return;
    }
    const name = gpu.name || 'GPU';
    const used = Number(gpu.memUsedMb ?? gpu.mem_used_mb ?? 0);
    const totalMem = Number(gpu.memTotalMb ?? gpu.mem_total_mb ?? 0);
    const util = Number(gpu.utilPct ?? gpu.util_pct ?? 0);
    const memPct = totalMem > 0 ? Math.max(0, Math.min(100, Math.round((used / totalMem) * 100))) : 0;
    box.innerHTML = `
      <section class="ml-studio-gpu-card">
        <div class="ml-studio-gpu-head">${sprite('cpu')} <span class="ml-studio-gpu-name">${escapeHtml(name)}</span>
          <tf-badge tone="accent" value="util ${Number.isFinite(util) ? Math.round(util) : 0}%"></tf-badge>
        </div>
        <div class="ml-studio-gpu-metric">
          <div class="ml-studio-gpu-metric-row">
            <span class="ml-studio-gpu-metric-lbl">VRAM</span>
            <span class="ml-studio-gpu-metric-val">${fmtGb(used)} / ${fmtGb(totalMem)}</span>
          </div>
          <tf-progress-bar value="${memPct}" tone="accent"></tf-progress-bar>
        </div>
        <div class="ml-studio-gpu-metric">
          <div class="ml-studio-gpu-metric-row">
            <span class="ml-studio-gpu-metric-lbl">Wykorzystanie</span>
            <span class="ml-studio-gpu-metric-val">${Number.isFinite(util) ? Math.round(util) : 0}%</span>
          </div>
          <tf-progress-bar value="${Number.isFinite(util) ? Math.max(0, Math.min(100, Math.round(util))) : 0}" tone="success"></tf-progress-bar>
        </div>
      </section>
    `;
  };

  const renderJobs = (jobs) => {
    const listBox = byId('ml-studio-jobs-list');
    if (!listBox) return;
    if (!jobs.length) {
      listBox.innerHTML = '';
      const empty = document.createElement('tf-empty-state');
      empty.setAttribute('icon', 'cpu');
      empty.setAttribute('title', 'Brak uruchomionych jobów');
      empty.setAttribute('message', 'Gdy uruchomisz trening w dowolnym projekcie, pojawi się tu jego postęp na żywo.');
      listBox.appendChild(empty);
      return;
    }

    listBox.innerHTML = '';
    const table = document.createElement('tf-table');
    table.setAttribute('variant', 'lined');
    table.innerHTML = `
      <tf-column key="project" label="Projekt" renderer="html"></tf-column>
      <tf-column key="kind" label="Typ"></tf-column>
      <tf-column key="variant" label="Wariant"></tf-column>
      <tf-column key="status" label="Status" renderer="html"></tf-column>
      <tf-column key="progress" label="Postęp" renderer="html"></tf-column>
      <tf-column key="eta" label="ETA"></tf-column>
      <tf-column key="vram" label="VRAM"></tf-column>
    `;
    table.rows = jobs.map((j) => {
      const runId = String(j.runId ?? j.run_id ?? '');
      const pid = String(j.projectId ?? j.project_id ?? '');
      const kind = j.kind ?? '';
      const epoch = Number(j.epoch ?? 0);
      const total = Number(j.totalEpochs ?? j.total_epochs ?? 0);
      const etaS = Number(j.etaS ?? j.eta_s);
      const gpuMemMb = Number(j.gpuMemMb ?? j.gpu_mem_mb);
      const b = runBadge(j.status);
      const stage = stageLabel(j.stage);
      const pct = total > 0 ? Math.max(0, Math.min(100, Math.round((epoch / total) * 100))) : 0;
      const progressHtml = total > 0
        ? `<div class="ml-studio-job-progress"><tf-progress-bar value="${pct}" tone="accent"></tf-progress-bar>
             <span class="ml-studio-job-progress-txt">${epoch} / ${total} · ${pct}%</span></div>`
        : `<span class="ml-studio-job-progress-txt">${stage || '—'}</span>`;
      return {
        runId,
        projectId: pid,
        kind: jobKindLabel(kind),
        project: `<div class="ml-studio-job-project"><span class="ml-studio-job-name">${escapeHtml(j.projectName ?? j.project_name ?? '(projekt)')}</span>${stage ? `<span class="ml-studio-job-stage">${escapeHtml(stage)}</span>` : ''}</div>`,
        variant: escapeHtml(String(j.variant ?? '—')) || '—',
        status: `<tf-badge tone="${b.tone}" value="${escapeAttr(b.label)}"></tf-badge>`,
        progress: progressHtml,
        eta: fmtEta(etaS),
        vram: fmtGb(gpuMemMb),
      };
    });
    table.addEventListener('row-click', (e) => {
      const row = e.detail?.row;
      if (!row || !row.projectId) return;
      stopJobsPolling();
      Router.navigate('ml-studio', { projectId: row.projectId, runId: row.runId, kind: row.kind });
    });
    listBox.appendChild(table);
  };

  const poll = async () => {
    try {
      const resp = await ApiBinary.one('mlStudioJobsOverviewRequest', {});
      const jobs = Array.isArray(resp.jobs) ? resp.jobs : [];
      renderGpu(resp.gpu || null);
      renderJobs(jobs);
    } catch (err) {
      stopJobsPolling();
      const listBox = byId('ml-studio-jobs-list');
      if (listBox) {
        listBox.innerHTML = '';
        const empty = document.createElement('tf-empty-state');
        empty.setAttribute('icon', 'alert');
        empty.setAttribute('title', 'Nie udało się wczytać jobów');
        empty.setAttribute('message', err.message || 'Błąd protokołu ML Studio.');
        const retry = document.createElement('tf-button');
        retry.setAttribute('variant', 'primary');
        retry.textContent = 'Spróbuj ponownie';
        retry.addEventListener('click', () => showJobsOverview());
        empty.appendChild(retry);
        listBox.appendChild(empty);
      }
    }
  };

  byId('ml-studio-jobs-refresh')?.addEventListener('click', poll);
  await poll();
  jobsPollTimer = setInterval(poll, 3500);
}

// =============================================================================
// Zakładka "Modele" — lista wytrenowanych modeli projektu (wszystkie typy).
// Pusto → tf-empty-state; inaczej tf-table z metrykami z metricsJson.
// =============================================================================

// Wyciąga skrótowe metryki z metricsJson modelu (np. "acc 0.94" / "loss 1.2").
// Zwraca pusty string, gdy JSON nie zawiera znanych pól — wtedy kolumna pokaże "—".
// Formatuje loss tak, by mikroskopijna niezerowa wartość (przeuczenie na małym
// zbiorze daje ~1e-6) nie zaokrąglała się do „0.00" — to myli z brakiem metryki
// albo zepsutym modelem. Bardzo małe wartości pokazujemy w notacji wykładniczej.
function formatLoss(n) {
  if (n !== 0 && Math.abs(n) < 0.005) return n.toExponential(1);
  return n.toFixed(2);
}

function modelMetricsSummary(metricsJson) {
  if (!metricsJson) return '';
  let m = metricsJson;
  if (typeof m === 'string') {
    try { m = JSON.parse(m); } catch (_) { return ''; }
  }
  if (!m || typeof m !== 'object') return '';
  const parts = [];
  const num = (v) => (Number.isFinite(Number(v)) ? Number(v) : null);
  const acc = num(m.accuracy ?? m.acc);
  const f1 = num(m.f1 ?? m.f1_score ?? m.f1Score);
  const loss = num(m.train_loss ?? m.trainLoss ?? m.eval_loss ?? m.evalLoss ?? m.loss);
  if (acc != null) parts.push(`acc ${acc.toFixed(2)}`);
  if (f1 != null) parts.push(`f1 ${f1.toFixed(2)}`);
  if (loss != null) parts.push(`loss ${formatLoss(loss)}`);
  return parts.join(' · ');
}

// Etykieta silnika modelu (kolumna „Silnik" w tabeli Modele). Nieznany silnik
// pokazujemy surowo, żeby nie ukrywać nowych typów backendu.
const FRAMEWORK_LABELS = {
  rfdetr: 'Detekcja RF-DETR',
  'classifier-timm': 'Klasyfikator atrybutu',
  'ocr-paddle': 'OCR',
};
function frameworkLabel(fw) {
  const f = String(fw ?? '');
  return FRAMEWORK_LABELS[f] || (f || '—');
}

function renderModelsTab(panel, p) {
  const pid = projectId(p);
  panel.innerHTML = '<div class="ml-studio-loading"><tf-spinner></tf-spinner></div>';
  ApiBinary.one('mlStudioModelsListRequest', { projectId: pid })
    .then((resp) => {
      const models = Array.isArray(resp.models) ? resp.models : [];
      panel.innerHTML = '';
      if (!models.length) {
        const empty = document.createElement('tf-empty-state');
        empty.setAttribute('icon', 'catalog');
        empty.setAttribute('title', 'Brak modeli');
        empty.setAttribute('message', 'Brak modeli — pojawią się po udanym treningu.');
        panel.appendChild(empty);
        return;
      }
      const card = document.createElement('div');
      card.className = 'ml-studio-section-card';
      card.innerHTML = `
        <div class="ml-studio-section-card-head">
          <div class="title">${sprite('catalog')} Modele <span class="ml-studio-section-sub">— wytrenowane wersje w projekcie</span></div>
        </div>
        <div id="ml-studio-models-table"></div>
      `;
      panel.appendChild(card);

      const table = document.createElement('tf-table');
      table.setAttribute('variant', 'lined');
      table.innerHTML = `
        <tf-column key="model" label="Model"></tf-column>
        <tf-column key="framework" label="Silnik"></tf-column>
        <tf-column key="baseModel" label="Model bazowy"></tf-column>
        <tf-column key="status" label="Status" renderer="html"></tf-column>
        <tf-column key="metrics" label="Metryki"></tf-column>
        <tf-column key="createdAt" label="Utworzony"></tf-column>
      `;
      table.rows = models.map((m) => {
        const b = runBadge(m.status);
        const metrics = modelMetricsSummary(m.metricsJson ?? m.metrics_json);
        const modelId = String(m.modelId ?? m.model_id ?? '');
        const baseModel = String(m.baseModel ?? m.base_model ?? '');
        const modelName = String(m.name ?? (modelId || '—'));
        // Stan z metryk: wdrożony (`inference_model_name`) oraz wyeksportowany
        // do GGUF (`export_status=succeeded` + `gguf_path`).
        let deployed = false;
        let deploying = false;
        let exported = false;
        try {
          const mj = JSON.parse(m.metricsJson ?? m.metrics_json ?? '{}');
          // „Zapytaj" tylko gdy serwis REALNIE serwuje model — status jest
          // rekoncyliowany po żywym serwisie (patrz reconcile_local_inference_status).
          // Sama obecność `inference_model_name` nie wystarcza: serwis mógł paść
          // albo zostać usunięty (status → „failed") i czat by się wywalił.
          const infStatus = String(mj.inference_status ?? '');
          deployed = infStatus === 'deployed';
          deploying = infStatus === 'deploying' || infStatus === 'transferring';
          exported = mj.export_status === 'succeeded' && Boolean(mj.gguf_path);
        } catch (_) { deployed = false; deploying = false; exported = false; }
        const framework = String(m.framework ?? '');
        return {
          model: modelName,
          framework: frameworkLabel(framework),
          baseModel: baseModel || '—',
          status: `<tf-badge tone="${b.tone}" value="${escapeAttr(b.label)}"></tf-badge>`,
          metrics: metrics || '—',
          createdAt: formatRelative(m.createdAt ?? m.created_at),
          // Pola pomocnicze do buildera akcji (nie kolumny — tf-table ich nie renderuje).
          _modelId: modelId,
          _modelName: modelName,
          // Model detekcji (RF-DETR) → akcja „Wykryj"; model FT (adapter, niepuste
          // baseModel) → „Eksportuj GGUF"; klasyfikator timm → eksport ONNX;
          // wdrożony FT → też „Zapytaj".
          _isRecog: framework === 'rfdetr',
          _framework: framework,
          // Modele wizyjne (RF-DETR / klasyfikator timm) można opublikować do
          // rejestru vision_models — pipeline'y kamer użyją ich bez rekompilacji.
          _canPublishVision: Boolean(modelId && (framework === 'rfdetr' || framework === 'classifier-timm')),
          _canExport: Boolean(modelId && framework !== 'rfdetr' && (baseModel.trim().length > 0 || framework === 'classifier-timm')),
          _canChat: Boolean(modelId && deployed),
          _deploying: Boolean(modelId && deploying),
          _canDeploy: Boolean(modelId && exported && !deployed && !deploying && String(m.framework ?? '') !== 'rfdetr'),
        };
      });
      // Per-wierszowy builder akcji tf-table: zwraca realny Element z własnym
      // handlerem klika — działa w shadow DOM (delegacja z light DOM by nie złapała).
      table.rowActions = (row) => {
        if (!row) return null;
        const publishBtn = () => {
          const pub = document.createElement('tf-button');
          pub.setAttribute('size', 'sm');
          pub.setAttribute('variant', 'outline');
          pub.setAttribute('icon', 'eye');
          pub.textContent = 'Publikuj do kamer';
          pub.addEventListener('click', () => openVisionPublishPanel(p, row, () => renderModelsTab(panel, p)));
          return pub;
        };
        if (row._isRecog) {
          const wrap = document.createElement('div');
          wrap.style.cssText = 'display:flex;gap:6px;flex-wrap:wrap';
          const btn = document.createElement('tf-button');
          btn.setAttribute('size', 'sm');
          btn.setAttribute('variant', 'outline');
          btn.setAttribute('icon', 'image');
          btn.textContent = 'Wykryj na zdjęciu';
          btn.addEventListener('click', () => openRecogDetectPanel(p, row._modelId, row._modelName));
          wrap.appendChild(btn);
          wrap.appendChild(publishBtn());
          return wrap;
        }
        if (row._canPublishVision && !row._canChat && !row._canDeploy && !row._deploying && !row._canExport) {
          return publishBtn();
        }
        if (row._canPublishVision && row._canExport && !row._canChat && !row._canDeploy && !row._deploying) {
          const wrap = document.createElement('div');
          wrap.style.cssText = 'display:flex;gap:6px;flex-wrap:wrap';
          wrap.appendChild(publishBtn());
          const exp = document.createElement('tf-button');
          exp.setAttribute('size', 'sm');
          exp.setAttribute('variant', 'outline');
          exp.setAttribute('icon', 'package');
          exp.textContent = 'Eksportuj GGUF';
          exp.addEventListener('click', () => openFtExportPanel(p, row._modelId, row._modelName));
          wrap.appendChild(exp);
          return wrap;
        }
        // Model FT: gdy wdrożony → przycisk „Zapytaj"; w przeciwnym razie eksport.
        if (row._canChat) {
          const wrap = document.createElement('div');
          wrap.style.cssText = 'display:flex;gap:6px;flex-wrap:wrap';
          const ask = document.createElement('tf-button');
          ask.setAttribute('size', 'sm');
          ask.setAttribute('variant', 'primary');
          ask.setAttribute('icon', 'sparkle');
          ask.textContent = 'Zapytaj';
          ask.addEventListener('click', () => openFtChatPanel(row._modelId, row._modelName));
          wrap.appendChild(ask);
          if (row._canExport) {
            const exp = document.createElement('tf-button');
            exp.setAttribute('size', 'sm');
            exp.setAttribute('variant', 'outline');
            exp.setAttribute('icon', 'package');
            exp.textContent = 'Eksportuj GGUF';
            exp.addEventListener('click', () => openFtExportPanel(p, row._modelId, row._modelName));
            wrap.appendChild(exp);
          }
          return wrap;
        }
        // Wyeksportowany, ale niewdrożony → bezpośredni „Wdróż" (bez ponownego eksportu).
        if (row._canDeploy) {
          const wrap = document.createElement('div');
          wrap.style.cssText = 'display:flex;gap:6px;flex-wrap:wrap';
          const dep = document.createElement('tf-button');
          dep.setAttribute('size', 'sm');
          dep.setAttribute('variant', 'primary');
          dep.setAttribute('icon', 'cpu');
          dep.textContent = 'Wdróż';
          dep.addEventListener('click', () => deployFtModel(row._modelId, row._modelName, () => renderModelsTab(panel, p), p));
          wrap.appendChild(dep);
          return wrap;
        }
        // Deploy w toku — nie oferuj równoległego eksportu/wdrożenia; pokaż stan.
        if (row._deploying) {
          const dep = document.createElement('tf-button');
          dep.setAttribute('size', 'sm');
          dep.setAttribute('variant', 'outline');
          dep.setAttribute('icon', 'cpu');
          dep.setAttribute('disabled', '');
          dep.textContent = 'Wdrażanie…';
          return dep;
        }
        if (!row._canExport) return null;
        const btn = document.createElement('tf-button');
        btn.setAttribute('size', 'sm');
        btn.setAttribute('variant', 'outline');
        btn.setAttribute('icon', 'package');
        btn.textContent = 'Eksportuj GGUF';
        btn.addEventListener('click', () => openFtExportPanel(p, row._modelId, row._modelName));
        return btn;
      };
      const tableHost = byId('ml-studio-models-table');
      tableHost?.appendChild(table);
      renderVisionRegistrySection(panel, p);
    })
    .catch((err) => {
      panel.innerHTML = '';
      const empty = document.createElement('tf-empty-state');
      empty.setAttribute('icon', 'alert');
      empty.setAttribute('title', 'Nie udało się wczytać modeli');
      empty.setAttribute('message', err.message || 'Błąd protokołu ML Studio.');
      panel.appendChild(empty);
    });
}

// Sekcja „Modele wizyjne" — rejestr vision_models (dynamiczne ONNX serwowane
// przez silnik onnx-cv w pipeline'ach kamer). Rejestr jest wspólny dla całej
// organizacji; publikacja z tabeli modeli powyżej dodaje tu wiersz.
function renderVisionRegistrySection(panel, p) {
  const card = document.createElement('div');
  card.className = 'ml-studio-section-card';
  card.innerHTML = `
    <div class="ml-studio-section-card-head">
      <div class="title">${sprite('eye')} Modele wizyjne <span class="ml-studio-section-sub">— rejestr modeli kamer (onnx-cv, cała organizacja)</span></div>
      <tf-button size="sm" variant="ghost" icon="share" data-vision-share>Udostępnij</tf-button>
    </div>
    <div data-vision-registry-body></div>
  `;
  panel.appendChild(card);
  card.querySelector('[data-vision-share]')?.addEventListener('click', openVisionShareModal);
  const body = card.querySelector('[data-vision-registry-body]');

  ApiBinary.one('mlStudioVisionModelsListRequest', {})
    .then((resp) => {
      const models = Array.isArray(resp.models) ? resp.models : [];
      if (!models.length) {
        const empty = document.createElement('tf-empty-state');
        empty.setAttribute('icon', 'eye');
        empty.setAttribute('title', 'Brak modeli wizyjnych');
        empty.setAttribute('message', 'Opublikuj wytrenowany model detekcji lub klasyfikacji, aby pipeline\'y kamer mogły go używać.');
        body.appendChild(empty);
        return;
      }
      const table = document.createElement('tf-table');
      table.setAttribute('variant', 'lined');
      table.innerHTML = `
        <tf-column key="name" label="Nazwa"></tf-column>
        <tf-column key="op" label="Operacja"></tf-column>
        <tf-column key="source" label="Źródło"></tf-column>
        <tf-column key="classes" label="Klasy"></tf-column>
        <tf-column key="sha" label="SHA-256"></tf-column>
        <tf-column key="createdAt" label="Dodany"></tf-column>
      `;
      table.rows = models.map((m) => ({
        name: String(m.modelName ?? m.model_name ?? ''),
        op: String(m.op ?? '') === 'detect' ? 'detekcja' : 'klasyfikacja',
        source: String(m.source ?? ''),
        classes: String((m.classes || []).length),
        sha: String(m.sha256 ?? '').slice(0, 12),
        createdAt: formatRelative(m.createdAt ?? m.created_at),
        _modelName: String(m.modelName ?? m.model_name ?? ''),
      }));
      table.rowActions = (row) => {
        if (!row) return null;
        const wrap = document.createElement('div');
        wrap.style.display = 'flex';
        wrap.style.gap = '6px';
        const share = document.createElement('tf-button');
        share.setAttribute('size', 'sm');
        share.setAttribute('variant', 'ghost');
        share.setAttribute('icon', 'share');
        share.textContent = 'Udostępnij';
        share.addEventListener('click', () => openVisionShareModal(row._modelName));
        const del = document.createElement('tf-button');
        del.setAttribute('size', 'sm');
        del.setAttribute('variant', 'danger');
        del.setAttribute('icon', 'trash');
        del.textContent = 'Usuń';
        del.addEventListener('click', async () => {
          if (!window.confirm(`Usunąć model wizyjny „${row._modelName}" z rejestru?`)) return;
          try {
            const resp = await ApiBinary.one('mlStudioVisionModelDeleteRequest', { modelName: row._modelName });
            if (!resp.ok) throw new Error(resp.error || 'usunięcie odrzucone');
            toast(`Model „${row._modelName}" usunięty z rejestru`, 'success');
            renderModelsTab(panel, p);
          } catch (err) {
            toast(`Usuwanie modelu: ${err.message}`, 'error');
          }
        });
        wrap.appendChild(share);
        wrap.appendChild(del);
        return wrap;
      };
      body.appendChild(table);
    })
    .catch((err) => {
      body.innerHTML = `<div class="ml-studio-ft-done-msg error">${sprite('alert')} Rejestr modeli wizyjnych: ${escapeHtml(err.message || String(err))}</div>`;
    });
}

// Udostępnianie modeli wizyjnych innej instancji TentaFlow (bez parowania):
// pokazuje gotowy do wklejenia URL manifestu /models/manifest/<ref>. `bundleRef`
// to nazwa modelu z rejestru (pojedynczy model) albo `vision-all` (całość gdy
// wywołane z przycisku sekcji). Druga instancja wkleja URL w kreatorze deployu
// (zakładka „Własny") razem z kluczem API utworzonym TUTAJ (Dostęp i klucze API
// → zakres model_bundle na ten sam ref).
function openVisionShareModal(bundleRef = 'vision-all') {
  const ref = String(bundleRef || 'vision-all');
  const manifestUrl = `${window.location.origin}/models/manifest/${ref}`;
  const single = ref !== 'vision-all';
  const modal = document.createElement('tf-modal');
  modal.setAttribute('variant', 'modal');
  modal.setAttribute('title', single ? `Udostępnij model „${ref}"` : 'Udostępnij modele wizyjne');
  modal.setAttribute('size', 'md');
  modal.innerHTML = `
    <div slot="body">
      <p class="ml-studio-export-intro">${single
        ? `Inna instancja TentaFlow może zaimportować ten pojedynczy model (bez parowania mesh). Wklej poniższy URL manifestu w kreatorze deployu drugiej instancji (krok „Model" → zakładka „Własny") razem z kluczem API utworzonym na TEJ instancji.`
        : `Inna instancja TentaFlow może pobrać stąd bundle modeli wizyjnych bez parowania mesh. Wklej poniższy URL manifestu w kreatorze deployu drugiej instancji (krok „Model" → zakładka „Własny") razem z kluczem API utworzonym na TEJ instancji.`}</p>
      <div class="form-group">
        <tf-input id="ml-studio-vision-share-url" label="URL manifestu (gotowy do wklejenia)" value="${escapeAttr(manifestUrl)}" readonly></tf-input>
      </div>
      <p class="ml-studio-share-hint">${sprite('info')} Dostęp wymaga klucza API typu „Ogólny" z zakresem <code>model_bundle</code> (<code>${escapeHtml(ref)}</code>) — utwórz go w „Dostęp i klucze API". Klucz jest przesyłany jako nagłówek <code>Authorization: Bearer</code> i każdy dostęp trafia do audytu.</p>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-vision-share-close>Zamknij</tf-button>
      <tf-button variant="primary" icon="copy" id="ml-studio-vision-share-copy">Kopiuj URL</tf-button>
    </div>
  `;
  document.body.appendChild(modal);
  modal.setAttribute('open', '');
  const close = () => { try { modal.remove(); } catch (_) {} };
  modal.querySelector('[data-vision-share-close]')?.addEventListener('click', close);
  modal.addEventListener('close', close);
  modal.querySelector('#ml-studio-vision-share-copy')?.addEventListener('click', () => {
    navigator.clipboard?.writeText(manifestUrl);
    toast('URL manifestu skopiowany', 'success');
  });
}

// Publikacja wytrenowanego modelu do rejestru vision_models. Dla RF-DETR bez
// wyeksportowanego ONNX Core sam uruchamia eksport na serwisie treningowym —
// to może potrwać kilka minut, więc modal pokazuje stan i nie polega na kliku.
function openVisionPublishPanel(p, row, onDone) {
  const modelId = row._modelId;
  if (!modelId) return;
  const op = row._framework === 'rfdetr' ? 'detect' : 'classify';
  const suggested = String(row._modelName || '')
    .toLowerCase()
    .replace(/[^a-z0-9-_]+/g, '-')
    .replace(/^-+|-+$/g, '');
  const modal = document.createElement('tf-modal');
  modal.setAttribute('variant', 'modal');
  modal.setAttribute('title', `Publikuj do kamer — ${row._modelName}`);
  modal.setAttribute('size', 'md');
  modal.innerHTML = `
    <div slot="body">
      <p class="ml-studio-export-intro">Model trafi do rejestru modeli wizyjnych (silnik onnx-cv). Pipeline'y kamer wskazują go przez alias lub bezpośrednio po nazwie — bez rekompilacji. Jeśli ONNX nie był jeszcze wyeksportowany, eksport uruchomi się automatycznie (RF-DETR: kilka minut).</p>
      <tf-input id="ml-studio-vision-name" label="Nazwa w rejestrze (a-z, 0-9, -, _)" value="${escapeAttr(suggested)}"></tf-input>
      <tf-select id="ml-studio-vision-op" label="Operacja" value="${op}" disabled>
        <option value="${op}">${op === 'detect' ? 'detekcja (RF-DETR)' : 'klasyfikacja (softmax)'}</option>
      </tf-select>
      ${op === 'detect' ? '<tf-input type="number" id="ml-studio-vision-threshold" label="Domyślny próg pewności" value="0.5" min="0" max="1" step="0.05"></tf-input>' : ''}
      <tf-input id="ml-studio-vision-alias" label="Alias (opcjonalnie, np. tentavision-detect)"></tf-input>
      <div id="ml-studio-vision-publish-status" style="margin-top:10px"></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-vision-close>Anuluj</tf-button>
      <tf-button variant="primary" icon="eye" id="ml-studio-vision-go">Publikuj</tf-button>
    </div>
  `;
  document.body.appendChild(modal);
  modal.setAttribute('open', '');
  const close = () => { try { modal.remove(); } catch (_) {} };
  modal.querySelector('[data-vision-close]')?.addEventListener('click', close);
  modal.addEventListener('close', close);

  const go = modal.querySelector('#ml-studio-vision-go');
  go?.addEventListener('click', async () => {
    const status = modal.querySelector('#ml-studio-vision-publish-status');
    const modelName = String(modal.querySelector('#ml-studio-vision-name')?.value || '').trim();
    if (!/^[a-z0-9-_]+$/.test(modelName)) {
      if (status) status.innerHTML = `<div class="ml-studio-ft-done-msg error">${sprite('alert')} Nazwa musi pasować do [a-z0-9-_]+.</div>`;
      return;
    }
    const thresholdRaw = modal.querySelector('#ml-studio-vision-threshold')?.value;
    const alias = String(modal.querySelector('#ml-studio-vision-alias')?.value || '').trim();
    go.setAttribute('disabled', '');
    if (status) status.innerHTML = '<tf-spinner></tf-spinner> publikacja (eksport ONNX może potrwać kilka minut)…';
    try {
      const resp = await ApiBinary.action('mlStudioVisionModelPublishRequest', {
        modelId,
        modelName,
        op,
        threshold: op === 'detect' && thresholdRaw !== undefined && thresholdRaw !== '' ? Number(thresholdRaw) : null,
        alias: alias || null,
      }, { timeoutMs: 20 * 60 * 1000 });
      if (!resp.ok) throw new Error(resp.error || 'publikacja odrzucona');
      toast(`Model „${modelName}" opublikowany do rejestru kamer`, 'success');
      close();
      if (typeof onDone === 'function') onDone();
    } catch (err) {
      go.removeAttribute('disabled');
      if (status) status.innerHTML = `<div class="ml-studio-ft-done-msg error">${sprite('alert')} Publikacja nieudana: ${escapeHtml(err.message || String(err))}</div>`;
    }
  });
}

// Detekcja na zdjęciu modelem RF-DETR (R4). Modal: upload małego obrazu +
// próg → mlStudioRecogDetectRequest → lista detekcji + nakładka bboxów na obraz.
function openRecogDetectPanel(p, modelId, modelName) {
  if (!modelId) return;
  const modal = document.createElement('tf-modal');
  modal.setAttribute('variant', 'modal');
  modal.setAttribute('title', `Detekcja — ${modelName}`);
  modal.setAttribute('size', 'lg');
  modal.innerHTML = `
    <div slot="body">
      <p class="ml-studio-export-intro">Wgraj zdjęcie — model wykryje obiekty i zwróci klasy + ramki. Duże zdjęcia są automatycznie zmniejszane (do 1280 px) przed wysłaniem.</p>
      <div style="display:flex;gap:8px;align-items:flex-end;flex-wrap:wrap;margin-bottom:10px">
        <tf-file-input id="ml-studio-detect-file" accept="image/*" label="Zdjęcie do detekcji" style="flex:1;min-width:240px"></tf-file-input>
        <tf-input type="number" id="ml-studio-detect-threshold" label="Próg pewności" value="0.5" min="0" max="1" step="0.05" style="width:140px"></tf-input>
      </div>
      <div id="ml-studio-detect-result"></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-detect-close>Zamknij</tf-button>
    </div>
  `;
  document.body.appendChild(modal);
  // tf-modal pokazuje backdrop/kartę tylko z atrybutem `open` (tf-modal.js:123).
  modal.setAttribute('open', '');
  const close = () => { try { modal.remove(); } catch (_) {} };
  modal.querySelector('[data-detect-close]')?.addEventListener('click', close);
  modal.addEventListener('close', close);

  const fileInput = modal.querySelector('#ml-studio-detect-file');
  fileInput?.addEventListener('change', async (e) => {
    const files = e.detail?.files;
    const file = files && files.length ? files[0] : null;
    if (!file) return;
    const result = modal.querySelector('#ml-studio-detect-result');
    if (result) result.innerHTML = '<tf-spinner></tf-spinner> przygotowanie obrazu…';
    try {
      // Detekcja działa w rozdzielczości ≤640 px, więc zmniejszamy zdjęcie po
      // stronie klienta (≤1280 px JPEG) — szybciej i nie przekracza limitu ramki WS.
      const { b64, mime } = await downscaleImageToB64(file, 1280);
      if (result) result.innerHTML = '<tf-spinner></tf-spinner> detekcja…';
      const threshold = Number(modal.querySelector('#ml-studio-detect-threshold')?.value ?? 0.5);
      const resp = await ApiBinary.one('mlStudioRecogDetectRequest', { modelId, threshold, imageB64: b64 });
      if (resp.error) throw new Error(resp.error);
      let dets = [];
      try { dets = JSON.parse(resp.detectionsJson ?? resp.detections_json ?? '[]'); } catch (_) { dets = []; }
      renderDetections(result, b64, mime, dets, resp.width, resp.height);
    } catch (err) {
      if (result) result.innerHTML = `<div class="ml-studio-ft-done-msg error">${sprite('alert')} Detekcja nieudana: ${escapeHtml(err.message || String(err))}</div>`;
    }
  });
}

// Render detekcji: obraz z nałożonymi ramkami (SVG overlay) + lista klas.
function renderDetections(host, b64, mime, dets, width, height) {
  if (!host) return;
  if (!dets.length) {
    host.innerHTML = '<div class="ml-studio-ft-done-msg">Brak detekcji powyżej progu — obniż próg pewności i spróbuj ponownie.</div>';
    return;
  }
  const W = width || 1000, H = height || 1000;
  const boxes = dets.map((d, i) => {
    const [x1, y1, x2, y2] = d.bbox_xyxy || d.bboxXyxy || [0, 0, 0, 0];
    const hue = (i * 67) % 360;
    return `<rect x="${x1}" y="${y1}" width="${Math.max(0, x2 - x1)}" height="${Math.max(0, y2 - y1)}"
      fill="none" stroke="hsl(${hue} 90% 55%)" stroke-width="${Math.max(2, W / 300)}"/>
      <text x="${x1}" y="${Math.max(12, y1 - 4)}" fill="hsl(${hue} 90% 55%)" font-size="${Math.max(12, W / 50)}" font-weight="700">${escapeHtml(d.class_name ?? d.className ?? '')} ${(Number(d.score) * 100).toFixed(0)}%</text>`;
  }).join('');
  const list = dets.map((d) => `<li><strong>${escapeHtml(d.class_name ?? d.className ?? '')}</strong> — ${(Number(d.score) * 100).toFixed(1)}%</li>`).join('');
  host.innerHTML = `
    <div style="position:relative;max-width:100%;border:1px solid var(--border);border-radius:var(--radius-sm);overflow:hidden">
      <img src="data:${mime || 'image/jpeg'};base64,${b64}" style="display:block;width:100%;height:auto"/>
      <svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="none" style="position:absolute;inset:0;width:100%;height:100%">${boxes}</svg>
    </div>
    <div class="ml-studio-data-head" style="margin-top:10px">${sprite('check')} Wykryto ${dets.length} obiektów</div>
    <ul class="ml-studio-detect-list">${list}</ul>
  `;
}

// Wdrożenie wyeksportowanego modelu FT do inferencji (bez ponownego eksportu).
// Deploy ZDALNY jest DETACHOWANY w Core: odpowiedź „deploying" wraca natychmiast,
// a transfer artefaktu + start serwisu biegnie w tle. Modal NIE polega na timeoucie
// requestu — po „deploying" odpytuje metryki modelu i pokazuje fazę/pasek B/s
// (transfer ma watchdog STALL: 0 B/s przez ~30s = błąd, ale aktywny transfer nigdy
// nie pada). Po „deployed" flip na „Zapytaj"; po „failed" pokaz błąd i wróć do „Wdróż".
// Wybór innego węzła → Core przenosi artefakt przez mesh (np. model MLX z B na Mac C).
async function deployFtModel(modelId, modelName, onDone, project) {
  if (!modelId) return;
  const modal = document.createElement('tf-modal');
  modal.setAttribute('variant', 'modal');
  modal.setAttribute('title', `Wdróż model — ${modelName}`);
  modal.setAttribute('size', 'md');
  modal.innerHTML = `
    <div slot="body">
      <p class="ml-studio-export-intro">Wybierz węzeł docelowy. „Węzeł modelu" wdraża tam, gdzie powstał artefakt. Inny węzeł → Core przeniesie artefakt przez mesh (np. model MLX na Mac mini).</p>
      <tf-select id="ml-studio-deploy-node" label="Węzeł docelowy"></tf-select>
      <div id="ml-studio-deploy-status" style="margin-top:10px"></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-deploy-close>Anuluj</tf-button>
      <tf-button variant="primary" icon="cpu" id="ml-studio-deploy-go">Wdróż</tf-button>
    </div>
  `;
  document.body.appendChild(modal);
  modal.setAttribute('open', '');
  const close = () => { try { modal.remove(); } catch (_) {} };
  modal.querySelector('[data-deploy-close]')?.addEventListener('click', close);
  modal.addEventListener('close', close);

  let targetNodeId = '';
  const sel = modal.querySelector('#ml-studio-deploy-node');
  sel?.setOptions([{ value: '', label: 'Węzeł modelu (domyślnie)' }], '');
  ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' }).then((nodes) => {
    const opts = [{ value: '', label: 'Węzeł modelu (domyślnie)' }];
    (nodes || []).forEach((n) => {
      const id = n.nodeId ?? n.node_id ?? '';
      const host = n.hostname ?? n.host ?? id.slice(0, 12);
      if (id) opts.push({ value: id, label: `${host} (${id.slice(0, 8)})` });
    });
    sel?.setOptions(opts, '');
  }).catch(() => {});
  sel?.addEventListener('change', (e) => { targetNodeId = e.detail?.value || sel.value || ''; });

  // Odpytuje metryki modelu (faza/postęp transferu) i renderuje status w modalu.
  // Zwraca obiekt metryk modelu lub null. NIE polega na timeoucie — czyta realny
  // stan zapisany przez detachowany deploy w tle (transferring→deploying→deployed/failed).
  const pollModelMetrics = async () => {
    const pid = project ? projectId(project) : '';
    if (!pid) return null;
    try {
      const resp = await ApiBinary.one('mlStudioModelsListRequest', { projectId: pid });
      const models = Array.isArray(resp.models) ? resp.models : [];
      const m = models.find((x) => String(x.modelId ?? x.model_id ?? '') === String(modelId));
      if (!m) return null;
      return JSON.parse(m.metricsJson ?? m.metrics_json ?? '{}');
    } catch (_) { return null; }
  };

  const renderDeployPhase = (st, mj) => {
    if (!st) return;
    const phase = String(mj?.inference_status ?? 'deploying');
    if (phase === 'transferring') {
      const sent = Number(mj.inference_transfer_sent ?? 0);
      const tot = Number(mj.inference_transfer_total ?? 0);
      const rate = Number(mj.inference_transfer_rate ?? 0);
      const pct = tot > 0 ? Math.max(0, Math.min(100, Math.round((sent / tot) * 100))) : 0;
      const detail = tot > 0
        ? `${fmtBytes(sent)} / ${fmtBytes(tot)} · ${pct}% · ${fmtRate(rate)}`
        : 'transfer artefaktu na węzeł docelowy…';
      st.innerHTML = `<div class="ml-studio-export-deploy-progress"><tf-spinner size="sm"></tf-spinner><span>Transfer artefaktu — ${detail}</span></div>`;
    } else {
      st.innerHTML = '<div class="ml-studio-export-deploy-progress"><tf-spinner size="sm"></tf-spinner><span>Uruchamianie serwisu na węźle docelowym…</span></div>';
    }
  };

  modal.querySelector('#ml-studio-deploy-go')?.addEventListener('click', async () => {
    const st = modal.querySelector('#ml-studio-deploy-status');
    modal.querySelector('#ml-studio-deploy-go')?.setAttribute('disabled', '');
    if (st) st.innerHTML = '<div class="ml-studio-export-deploy-progress"><tf-spinner size="sm"></tf-spinner><span>Zlecam wdrożenie…</span></div>';
    let resp;
    try {
      resp = await ApiBinary.one('mlStudioFtDeployRequest', { modelId, targetNodeId });
    } catch (err) {
      toast(`Wdrożenie nieudane: ${err.message}`, 'error');
      if (st) st.innerHTML = `<div class="ml-studio-ft-done-msg error">${sprite('alert')} ${escapeHtml(err.message)}</div>`;
      modal.querySelector('#ml-studio-deploy-go')?.removeAttribute('disabled');
      return;
    }
    const status = String(resp?.status || '');
    // „deployed" = deploy lokalny (synchroniczny) zakończony od razu.
    if (status === 'deployed') {
      toast(`Model „${resp?.modelName || modelName}" wdrożony`, 'success');
      close();
      if (typeof onDone === 'function') onDone();
      return;
    }
    // „deploying" = deploy zdalny detachowany — polling fazy/postępu w tle.
    if (status !== 'deploying') {
      const msg = String(resp?.error ?? '') || 'Nieoczekiwany stan wdrożenia';
      toast(msg, 'error');
      if (st) st.innerHTML = `<div class="ml-studio-ft-done-msg error">${sprite('alert')} ${escapeHtml(msg)}</div>`;
      modal.querySelector('#ml-studio-deploy-go')?.removeAttribute('disabled');
      return;
    }
    // Polling do skutku — transfer ma własny watchdog STALL po stronie Core,
    // więc tu NIE narzucamy sztywnego deadline; czekamy aż stan przejdzie w
    // „deployed" albo „failed". Pasek B/s aktualizowany z metryk modelu.
    if (st) renderDeployPhase(st, { inference_status: 'transferring' });
    const tick = async () => {
      if (!modal.isConnected) return;
      const mj = await pollModelMetrics();
      const phase = String(mj?.inference_status ?? 'deploying');
      if (phase === 'deployed') {
        toast(`Model „${resp?.modelName || modelName}" wdrożony — odpytaj go przyciskiem „Zapytaj"`, 'success');
        close();
        if (typeof onDone === 'function') onDone();
        return;
      }
      if (phase === 'failed') {
        const msg = String(mj?.inference_error ?? '') || 'Wdrożenie nieudane';
        toast(msg, 'error');
        if (st) st.innerHTML = `<div class="ml-studio-ft-done-msg error">${sprite('alert')} ${escapeHtml(msg)}</div>`;
        modal.querySelector('#ml-studio-deploy-go')?.removeAttribute('disabled');
        return;
      }
      renderDeployPhase(st, mj || { inference_status: phase });
      setTimeout(tick, 1500);
    };
    setTimeout(tick, 1200);
  });
}

// Czat z wdrożonym modelem FT (test/„użyj"). Modal: prompt → mlStudioFtChatRequest
// → odpowiedź. Gdy model żyje na innym węźle mesh, Core proxuje przez MlChat —
// dla UI to ta sama akcja. Pierwsze zapytanie może chwilę potrwać (ładowanie GGUF).
function openFtChatPanel(modelId, modelName) {
  if (!modelId) return;
  const modal = document.createElement('tf-modal');
  modal.setAttribute('variant', 'modal');
  modal.setAttribute('title', `Zapytaj model — ${modelName}`);
  modal.setAttribute('size', 'lg');
  modal.innerHTML = `
    <div slot="body">
      <p class="ml-studio-export-intro">Wpisz zapytanie — model odpowie przez lokalny silnik inferencji (a gdy żyje na innym węźle mesh, Core proxuje zapytanie do niego). Pierwsza odpowiedź może chwilę potrwać (ładowanie modelu).</p>
      <tf-textarea id="ml-studio-chat-input" rows="3" placeholder="np. Czy nalepka ADR jest poprawnie naklejona?"></tf-textarea>
      <div style="display:flex;gap:8px;align-items:center;margin:10px 0">
        <tf-input type="number" id="ml-studio-chat-maxtok" label="Maks. tokenów" value="256" min="1" max="2048" step="16" style="width:160px"></tf-input>
        <tf-button variant="primary" icon="sparkle" id="ml-studio-chat-send" style="align-self:flex-end">Zapytaj</tf-button>
      </div>
      <div id="ml-studio-chat-answer"></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-chat-close>Zamknij</tf-button>
    </div>
  `;
  document.body.appendChild(modal);
  modal.setAttribute('open', '');
  const close = () => { try { modal.remove(); } catch (_) {} };
  modal.querySelector('[data-chat-close]')?.addEventListener('click', close);
  modal.addEventListener('close', close);

  const send = async () => {
    const input = modal.querySelector('#ml-studio-chat-input');
    const message = String(input?.value ?? '').trim();
    const answerHost = modal.querySelector('#ml-studio-chat-answer');
    if (!message) { toast('Wpisz zapytanie', 'info'); return; }
    const maxTokens = Number(modal.querySelector('#ml-studio-chat-maxtok')?.value ?? 256);
    const btn = modal.querySelector('#ml-studio-chat-send');
    btn?.setAttribute('disabled', '');
    if (answerHost) answerHost.innerHTML = '<div class="ml-studio-export-deploy-progress"><tf-spinner size="sm"></tf-spinner><span>Generowanie odpowiedzi…</span></div>';
    try {
      const resp = await ApiBinary.one('mlStudioFtChatRequest', { modelId, message, maxTokens });
      if (resp?.error) throw new Error(resp.error);
      const answer = String(resp?.answer ?? '').trim();
      if (answerHost) {
        answerHost.innerHTML = answer
          ? `<div class="ml-studio-data-head" style="margin-top:6px">${sprite('sparkle')} Odpowiedź modelu</div><pre class="ml-studio-chat-text" style="white-space:pre-wrap;word-break:break-word;background:var(--surface-2,#1a1f2b);border:1px solid var(--border);border-radius:var(--radius-sm);padding:10px;margin:6px 0 0;font-family:inherit"></pre>`
          : `<div class="ml-studio-ft-done-msg">Model nie zwrócił treści — zwiększ liczbę tokenów lub spróbuj ponownie.</div>`;
        const pre = answerHost.querySelector('.ml-studio-chat-text');
        if (pre) pre.textContent = answer;
      }
    } catch (err) {
      if (answerHost) answerHost.innerHTML = `<div class="ml-studio-ft-done-msg error">${sprite('alert')} Zapytanie nieudane: ${escapeHtml(err.message || String(err))}</div>`;
    } finally {
      btn?.removeAttribute('disabled');
    }
  };
  modal.querySelector('#ml-studio-chat-send')?.addEventListener('click', send);
}

// =============================================================================
// Eksport modelu FT do GGUF (f03). Modal: wybór outtype (f16 / q8_0), start
// async przez mlStudioFtExportRequest, polling co 2s mlStudioFtExportStatusRequest,
// na końcu ścieżka + rozmiar pliku. Interwał czyszczony przy zamknięciu modala
// oraz w unmount/przy zmianie zakładki (stopFtExportPolling).
// =============================================================================
function openFtExportPanel(p, modelId, modelName) {
  if (!modelId) return;
  stopFtExportPolling();

  const modal = document.createElement('tf-modal');
  modal.setAttribute('variant', 'modal');
  modal.setAttribute('size', 'md');
  modal.setAttribute('title', `Eksport do GGUF — ${modelName || modelId}`);

  const body = document.createElement('div');
  body.setAttribute('slot', 'body');
  body.className = 'ml-studio-export-body';

  const footer = document.createElement('div');
  footer.setAttribute('slot', 'footer');
  footer.className = 'ml-studio-export-footer';

  modal.appendChild(body);
  modal.appendChild(footer);

  // Sprzątanie pollingu przy każdym zamknięciu modala (ESC, backdrop, krzyżyk).
  modal.addEventListener('close', () => {
    stopFtExportPolling();
    modal.remove();
  }, { once: true });

  // Zamknięcie z przycisków footera — zdejmuje atrybut i emituje 'close',
  // żeby zadziałał wspólny handler sprzątający (tf-modal nie ma publicznego close()).
  const closeModal = () => {
    modal.removeAttribute('open');
    modal.dispatchEvent(new CustomEvent('close', { bubbles: true }));
  };

  // Widok 1: wybór formatu kwantyzacji + akcja startu eksportu.
  const renderForm = () => {
    body.innerHTML = `
      <p class="ml-studio-export-intro">Wybierz precyzję pliku GGUF. Adapter LoRA zostanie scalony z modelem bazowym i przekonwertowany do jednego pliku <code>.gguf</code> gotowego dla llama.cpp.</p>
      <tf-radio-group id="ml-studio-export-outtype" name="ml-studio-export-outtype" value="q4_k_m">
        <tf-radio value="q4_k_m" label="GGUF Q4_K_M (4-bit)" hint="Najmniejszy plik dobrej jakości — zalecane do deployu."></tf-radio>
        <tf-radio value="q5_k_m" label="GGUF Q5_K_M (5-bit)" hint="Większy niż Q4, lepsza jakość."></tf-radio>
        <tf-radio value="q6_k" label="GGUF Q6_K (6-bit)" hint="Bliski q8_0 jakością, mniejszy plik."></tf-radio>
        <tf-radio value="q8_0" label="GGUF q8_0 (8-bit)" hint="Nieznaczna utrata jakości, większy plik."></tf-radio>
        <tf-radio value="f16" label="GGUF f16 (pełna precyzja)" hint="Największy plik, bez utraty jakości względem wag bazowych."></tf-radio>
        <tf-radio value="mlx" label="MLX (Apple safetensors)" hint="Model do silnika MLX na macOS/Apple Silicon — deploy np. na Mac mini."></tf-radio>
      </tf-radio-group>
    `;
    footer.innerHTML = `
      <tf-button variant="ghost" data-export-close>Anuluj</tf-button>
      <tf-button variant="primary" icon="package" id="ml-studio-export-start">Eksportuj do GGUF</tf-button>
    `;
    footer.querySelector('[data-export-close]')?.addEventListener('click', () => closeModal());
    byId('ml-studio-export-start')?.addEventListener('click', startExport);
  };

  // Widok 2: postęp scalania + konwersji (spinner), polling w tle.
  const renderProgress = () => {
    body.innerHTML = `
      <div class="ml-studio-export-progress">
        <tf-spinner size="md"></tf-spinner>
        <div class="ml-studio-export-progress-text">Eksportowanie — scalanie adaptera i konwersja do GGUF…</div>
      </div>
    `;
    footer.innerHTML = `<tf-button variant="ghost" data-export-close>Zamknij</tf-button>`;
    footer.querySelector('[data-export-close]')?.addEventListener('click', () => closeModal());
  };

  // Widok 3: wynik — ścieżka pliku na węźle + rozmiar + akcja deployu do inferencji.
  const renderResult = (st) => {
    const ggufPath = String(st.ggufPath ?? st.gguf_path ?? '');
    const size = formatFileSize(st.sizeBytes ?? st.size_bytes);
    body.innerHTML = `
      <div class="ml-studio-export-result">
        <div class="ml-studio-export-result-head">
          ${sprite('check')} <span>Eksport zakończony</span>
          <tf-badge tone="success" value="Gotowe"></tf-badge>
        </div>
        <div class="ml-studio-export-result-row">
          <span class="lbl">Plik GGUF</span>
          <span class="ml-studio-mono ml-studio-export-path">${escapeHtml(ggufPath || '—')}</span>
        </div>
        <div class="ml-studio-export-result-row">
          <span class="lbl">Rozmiar</span>
          <span class="val">${size}</span>
        </div>
        <p class="ml-studio-export-note">Plik GGUF zapisany na węźle — gotowy do pobrania/deployu.</p>
        <div class="ml-studio-export-deploy" id="ml-studio-export-deploy"></div>
      </div>
    `;
    footer.innerHTML = `<tf-button variant="primary" data-export-close>Zamknij</tf-button>`;
    footer.querySelector('[data-export-close]')?.addEventListener('click', () => closeModal());
    renderDeployIdle();
  };

  // Sekcja deployu wewnątrz widoku wyniku — stan początkowy z przyciskiem.
  const renderDeployIdle = () => {
    const slot = byId('ml-studio-export-deploy');
    if (!slot) return;
    slot.innerHTML = `
      <tf-button variant="primary" icon="cpu" id="ml-studio-export-deploy-btn">Deploy do inferencji</tf-button>
    `;
    byId('ml-studio-export-deploy-btn')?.addEventListener('click', startDeploy);
  };

  // Stan w trakcie wdrażania — spinner, przycisk zablokowany przez podmianę widoku.
  const renderDeploying = () => {
    const slot = byId('ml-studio-export-deploy');
    if (!slot) return;
    slot.innerHTML = `
      <div class="ml-studio-export-deploy-progress">
        <tf-spinner size="sm"></tf-spinner>
        <span>Wdrażanie modelu do inferencji…</span>
      </div>
    `;
  };

  // Sukces wdrożenia — nazwa modelu (mono) + jak go odpytać.
  const renderDeploySuccess = (name) => {
    const slot = byId('ml-studio-export-deploy');
    if (!slot) return;
    const safe = escapeHtml(name || modelName || modelId);
    slot.innerHTML = `
      <div class="ml-studio-export-deploy-ok">
        ${sprite('check')} <span>Model wdrożony do inferencji jako «<code class="ml-studio-mono">${safe}</code>». Odpytaj go niżej albo przez API <code class="ml-studio-mono">/v1</code>.</span>
      </div>
      <tf-button variant="primary" icon="sparkle" id="ml-studio-deploy-chat-btn" style="margin-top:8px">Zapytaj model</tf-button>
    `;
    byId('ml-studio-deploy-chat-btn')?.addEventListener('click', () => openFtChatPanel(modelId, name || modelName));
  };

  // Błąd wdrożenia — komunikat + ponowienie akcji.
  const renderDeployError = (message) => {
    const slot = byId('ml-studio-export-deploy');
    if (!slot) return;
    slot.innerHTML = `
      <div class="ml-studio-export-deploy-error">
        ${sprite('alert')} <span>Wdrożenie nieudane: ${escapeHtml(message || 'nieznany błąd.')}</span>
      </div>
      <tf-button variant="ghost" icon="cpu" id="ml-studio-export-deploy-btn">Spróbuj ponownie</tf-button>
    `;
    byId('ml-studio-export-deploy-btn')?.addEventListener('click', startDeploy);
  };

  // Wdrożenie wytrenowanego modelu jako serwis inferencji llama.cpp.
  // Deploy lokalny (z tego panelu) jest synchroniczny → status "deployed".
  // Deploy zdalny jest detachowany → status "deploying" (serwis wstaje w tle).
  const startDeploy = async () => {
    const btn = byId('ml-studio-export-deploy-btn');
    if (btn) btn.setAttribute('disabled', '');
    renderDeploying();
    try {
      const resp = await ApiBinary.one('mlStudioFtDeployRequest', { modelId });
      const status = String(resp?.status || '');
      if (status === 'deploying' || status === 'deployed') {
        renderDeploySuccess(resp?.modelName);
      } else {
        const msg = String(resp?.error ?? '') || 'Nieoczekiwany stan wdrożenia.';
        toast(msg, 'error');
        renderDeployError(msg);
      }
    } catch (err) {
      const msg = err.message || 'Nie udało się wdrożyć modelu.';
      toast(msg, 'error');
      renderDeployError(msg);
    }
  };

  const renderError = (message) => {
    body.innerHTML = `
      <div class="ml-studio-export-error">
        ${sprite('alert')} <span>${escapeHtml(message || 'Eksport nie powiódł się.')}</span>
      </div>
    `;
    footer.innerHTML = `<tf-button variant="ghost" data-export-close>Zamknij</tf-button>`;
    footer.querySelector('[data-export-close]')?.addEventListener('click', () => closeModal());
  };

  // Polling statusu co 2s — kończy się na succeeded/failed (czyści interwał).
  const startPolling = () => {
    stopFtExportPolling();
    const poll = async () => {
      let st;
      try {
        st = await ApiBinary.one('mlStudioFtExportStatusRequest', { modelId });
      } catch (err) {
        stopFtExportPolling();
        renderError(err.message || 'Błąd protokołu ML Studio.');
        return;
      }
      const status = String(st.status || 'running');
      if (status === 'succeeded') {
        stopFtExportPolling();
        renderResult(st);
      } else if (status === 'failed') {
        stopFtExportPolling();
        const msg = String(st.error ?? '') || 'Eksport nie powiódł się.';
        toast(msg, 'error');
        renderError(msg);
      }
    };
    ftExportPollTimer = setInterval(poll, 2000);
    poll();
  };

  const startExport = async () => {
    const outtype = byId('ml-studio-export-outtype')?.value || 'q8_0';
    const startBtn = byId('ml-studio-export-start');
    if (startBtn) startBtn.setAttribute('disabled', '');
    try {
      const resp = await ApiBinary.one('mlStudioFtExportRequest', { modelId, outtype });
      if (String(resp?.status || '') === 'running') {
        renderProgress();
        startPolling();
      } else {
        renderError('Nieoczekiwany stan eksportu.');
      }
    } catch (err) {
      if (startBtn) startBtn.removeAttribute('disabled');
      toast(err.message || 'Nie udało się uruchomić eksportu.', 'error');
    }
  };

  document.body.appendChild(modal);
  modal.setAttribute('open', '');
  renderForm();
}

// =============================================================================
// Zakładka "Treningi" — historia jobów treningowych projektu. Klik w wiersz
// pokazuje pod tabelą KPI wybranego runa i krzywą loss z metrics_history
// (mlStudioFtTrainStatusRequest działa dla KAŻDEGO runa, też zakończonego).
// =============================================================================

function renderRunsTab(panel, p) {
  const pid = projectId(p);
  panel.innerHTML = '<div class="ml-studio-loading"><tf-spinner></tf-spinner></div>';
  ApiBinary.one('mlStudioTrainingRunsListRequest', { projectId: pid })
    .then((resp) => {
      const runs = Array.isArray(resp.runs) ? resp.runs : [];
      panel.innerHTML = '';
      if (!runs.length) {
        const empty = document.createElement('tf-empty-state');
        empty.setAttribute('icon', 'brain');
        empty.setAttribute('title', 'Brak treningów');
        empty.setAttribute('message', 'Brak treningów — uruchom job w zakładce Trenuj/Trening.');
        panel.appendChild(empty);
        return;
      }

      const card = document.createElement('div');
      card.className = 'ml-studio-section-card';
      card.innerHTML = `
        <div class="ml-studio-section-card-head">
          <div class="title">${sprite('clock')} Treningi <span class="ml-studio-section-sub">— kliknij wiersz, aby zobaczyć krzywą loss</span></div>
        </div>
        <div id="ml-studio-runs-table"></div>
      `;
      panel.appendChild(card);

      // Sekcja szczegółów wybranego runa — wypełniana po kliknięciu wiersza.
      const detail = document.createElement('div');
      detail.id = 'ml-studio-run-detail';
      detail.hidden = true;
      panel.appendChild(detail);

      const table = document.createElement('tf-table');
      table.setAttribute('variant', 'lined');
      table.innerHTML = `
        <tf-column key="run" label="Run" renderer="html"></tf-column>
        <tf-column key="status" label="Status" renderer="html"></tf-column>
        <tf-column key="started" label="Start"></tf-column>
        <tf-column key="finished" label="Koniec"></tf-column>
      `;
      table.rows = runs.map((r) => {
        const runId = String(r.runId ?? r.run_id ?? '');
        const b = runBadge(r.status);
        const finished = r.finishedAt ?? r.finished_at;
        return {
          runId,
          run: `<span class="ml-studio-mono">${escapeHtml(runId.slice(0, 8) || '—')}</span>`,
          status: `<tf-badge tone="${b.tone}" value="${escapeAttr(b.label)}"></tf-badge>`,
          started: formatRelative(r.startedAt ?? r.started_at),
          finished: finished ? formatRelative(finished) : '—',
        };
      });
      table.addEventListener('row-click', (e) => {
        const runId = e.detail?.row?.runId;
        if (runId) renderRunDetail(detail, runId);
      });
      byId('ml-studio-runs-table')?.appendChild(table);
    })
    .catch((err) => {
      panel.innerHTML = '';
      const empty = document.createElement('tf-empty-state');
      empty.setAttribute('icon', 'alert');
      empty.setAttribute('title', 'Nie udało się wczytać treningów');
      empty.setAttribute('message', err.message || 'Błąd protokołu ML Studio.');
      panel.appendChild(empty);
    });
}

// Szczegóły wybranego runa: status + KPI (train/eval loss, krok) + krzywa loss.
// Reużywa renderLossChart (ten sam helper co live trening FT).
function renderRunDetail(host, runId) {
  host.hidden = false;
  host.innerHTML = '<div class="ml-studio-loading"><tf-spinner></tf-spinner></div>';
  ApiBinary.one('mlStudioFtTrainStatusRequest', { runId })
    .then((st) => {
      const status = String(st.status || 'running');
      const step = Number(st.step ?? 0);
      const totalSteps = Number(st.totalSteps ?? st.total_steps ?? 0);
      const trainLoss = st.trainLoss ?? st.train_loss;
      const evalLoss = st.evalLoss ?? st.eval_loss;
      const curve = Array.isArray(st.lossCurve ?? st.loss_curve) ? (st.lossCurve ?? st.loss_curve) : [];
      const b = runBadge(status);
      host.innerHTML = `
        <div class="ml-studio-section-card">
          <div class="ml-studio-section-card-head">
            <div class="title">${sprite('brain')} Run <span class="ml-studio-mono">${escapeHtml(String(runId).slice(0, 8))}</span> <tf-badge tone="${b.tone}" value="${escapeAttr(b.label)}"></tf-badge></div>
          </div>
          <div class="ml-studio-ft-kpi-grid">
            <div class="ml-studio-ft-kpi"><div class="lbl">train loss</div><div class="val">${trainLoss != null ? Number(trainLoss).toFixed(4) : '—'}</div></div>
            <div class="ml-studio-ft-kpi"><div class="lbl">eval loss</div><div class="val">${evalLoss != null ? Number(evalLoss).toFixed(4) : '—'}</div></div>
            <div class="ml-studio-ft-kpi"><div class="lbl">krok</div><div class="val">${step}${totalSteps > 0 ? ' / ' + totalSteps : ''}</div></div>
          </div>
          <div class="ml-studio-ft-chart-wrap">
            <div class="ml-studio-ft-chart-head">
              <span class="ml-studio-ft-chart-title">Krzywa loss</span>
              <span class="ml-studio-ft-chart-legend">
                <span class="lg"><span class="sw train"></span>train</span>
                <span class="lg"><span class="sw eval"></span>eval</span>
              </span>
            </div>
            ${renderLossChart(curve)}
          </div>
        </div>
      `;
    })
    .catch((err) => {
      host.innerHTML = '';
      const empty = document.createElement('tf-empty-state');
      empty.setAttribute('icon', 'alert');
      empty.setAttribute('title', 'Nie udało się wczytać szczegółów');
      empty.setAttribute('message', err.message || 'Błąd protokołu ML Studio.');
      host.appendChild(empty);
    });
}

// Per-tab state lives on the panel element so re-entering the tab starts clean
// and concurrent projects never cross-talk.
function renderTrainTab(panel, pid) {
  panel.innerHTML = '<div class="ml-studio-loading"><tf-spinner></tf-spinner></div>';
  ApiBinary.one('mlStudioDatasetsListRequest', { projectId: pid })
    .then((resp) => {
      const datasets = Array.isArray(resp.datasets) ? resp.datasets : [];
      renderTrainContent(panel, pid, datasets);
    })
    .catch((err) => {
      panel.innerHTML = '';
      const empty = document.createElement('tf-empty-state');
      empty.setAttribute('icon', 'alert');
      empty.setAttribute('title', 'Nie udało się wczytać zbiorów');
      empty.setAttribute('message', err.message || 'Błąd protokołu ML Studio.');
      panel.appendChild(empty);
    });
}

function renderTrainContent(panel, pid, datasets) {
  if (!datasets.length) {
    panel.innerHTML = '';
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', 'database');
    empty.setAttribute('title', 'Brak danych do treningu');
    empty.setAttribute('message', 'Najpierw wgraj dane w zakładce „Dane" — system odczyta kolumny i klasy, na których uczy się model.');
    panel.appendChild(empty);
    return;
  }

  panel.innerHTML = `
    <div class="ml-studio-train">
      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('star')} Dane i cel
          <span class="ml-studio-data-hint">wybierz zbiór, a potem kolumnę, którą model ma przewidywać</span>
        </div>
        <div class="ml-studio-train-pickers">
          <tf-select id="ml-studio-train-dataset" label="Zbiór danych"></tf-select>
          <tf-select id="ml-studio-train-target" label="Kolumna-cel (co przewidywać)" disabled></tf-select>
          <tf-select id="ml-studio-train-task" label="Typ zadania" disabled>
            <option value="classification">klasyfikacja</option>
            <option value="regression">regresja</option>
          </tf-select>
        </div>
        <div id="ml-studio-train-callout"></div>
      </section>

      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('cpu')} Silnik treningu
          <span class="ml-studio-data-hint">wybierz, czym wytrenować model — szybkim silnikiem natywnym albo automatycznym AutoML</span>
        </div>
        <div class="ml-studio-train-engine-grid" id="ml-studio-train-engine-grid">
          <button type="button" class="ml-studio-train-engine-card selected" data-engine="rust" aria-pressed="true">
            <div class="ml-studio-train-engine-ico">${sprite('cpu')}</div>
            <div class="ml-studio-train-engine-body">
              <div class="ml-studio-train-engine-title">Klasyczny ML (Rust)</div>
              <p class="ml-studio-train-engine-text">Drzewa decyzyjne + regresja (smartcore), uczone natywnie w Rust — szybko, on-device, bez GPU.</p>
              <div class="ml-studio-train-engine-tags">
                <tf-chip status="accent" label="Rust"></tf-chip>
                <tf-chip status="info" label="on-device"></tf-chip>
                <tf-chip status="info" label="bez GPU"></tf-chip>
              </div>
            </div>
          </button>
          <button type="button" class="ml-studio-train-engine-card" data-engine="autogluon" aria-pressed="false" disabled>
            <div class="ml-studio-train-engine-ico ml-studio-train-engine-ico-automl">${sprite('sparkle')}</div>
            <div class="ml-studio-train-engine-body">
              <div class="ml-studio-train-engine-title">AutoML (AutoGluon)</div>
              <p class="ml-studio-train-engine-text">Automatyczny ensemble (LightGBM/CatBoost/XGBoost…) z leaderboardem — najwyższa jakość. Python, NVIDIA/CPU.</p>
              <div class="ml-studio-train-engine-tags">
                <tf-chip status="accent" label="AutoML"></tf-chip>
                <tf-chip status="info" label="Python"></tf-chip>
                <tf-chip status="info" label="GPU/CPU"></tf-chip>
              </div>
              <div class="ml-studio-train-engine-hint" id="ml-studio-train-engine-hint" hidden></div>
            </div>
          </button>
        </div>
        <div class="ml-studio-train-actions">
          <tf-button variant="primary" icon="play" id="ml-studio-train-run" disabled>Trenuj</tf-button>
        </div>
      </section>

      <section class="ml-studio-data-card" id="ml-studio-train-result-card" hidden>
        <div class="ml-studio-data-head">${sprite('star')} Leaderboard
          <span class="ml-studio-data-hint" id="ml-studio-train-result-meta"></span>
        </div>
        <div id="ml-studio-train-best" class="ml-studio-detail-stats"></div>
        <div id="ml-studio-train-leaderboard"></div>
      </section>
    </div>
  `;

  // Local selection state — datasetId / its profile / chosen target column / task
  // / selected training engine ('rust' is always available, 'autogluon' only when
  // the AutoGluon service is running in the mesh).
  const state = { datasetId: '', profile: null, target: '', task: 'classification', engine: 'rust' };

  const datasetSel = byId('ml-studio-train-dataset');
  const targetSel = byId('ml-studio-train-target');
  const taskSel = byId('ml-studio-train-task');
  const runBtn = byId('ml-studio-train-run');

  datasetSel?.setOptions(
    datasets.map((d) => ({
      value: String(d.datasetId ?? d.dataset_id ?? ''),
      label: d.name || '(bez nazwy)',
    })),
    null,
  );
  // Leave the dataset unselected until the user picks one, so the target picker
  // never shows columns from an arbitrary first dataset.
  datasetSel && (datasetSel.value = '');

  const resetTarget = () => {
    state.target = '';
    state.task = 'classification';
    targetSel?.setOptions([], null);
    targetSel?.setAttribute('disabled', '');
    taskSel?.setAttribute('disabled', '');
    runBtn?.setAttribute('disabled', '');
    const callout = byId('ml-studio-train-callout');
    if (callout) callout.innerHTML = '';
  };

  datasetSel?.addEventListener('change', async (e) => {
    const id = e.detail?.value || '';
    state.datasetId = id;
    state.profile = null;
    resetTarget();
    if (!id) return;
    try {
      const resp = await ApiBinary.one('mlStudioDatasetProfileRequest', { datasetId: id });
      state.profile = resp.profile || resp;
      const columns = Array.isArray(state.profile.columns) ? state.profile.columns : [];
      if (!columns.length) {
        toast('Ten zbiór nie ma rozpoznanych kolumn.', 'error');
        return;
      }
      targetSel?.setOptions(
        columns.map((c) => ({
          value: String(c.name ?? ''),
          label: `${c.name ?? ''} · ${COLUMN_TYPE_LABEL[columnTypeSlug(c)] || columnTypeSlug(c)}`,
        })),
        null,
      );
      targetSel.value = '';
      targetSel?.removeAttribute('disabled');
    } catch (err) {
      toast(`Profil zbioru: ${err.message}`, 'error');
    }
  });

  targetSel?.addEventListener('change', (e) => {
    const name = e.detail?.value || '';
    state.target = name;
    const columns = Array.isArray(state.profile?.columns) ? state.profile.columns : [];
    const col = columns.find((c) => String(c.name ?? '') === name);
    if (!col) {
      runBtn?.setAttribute('disabled', '');
      return;
    }
    // Auto-suggest the task from the detected column type; the select stays
    // editable so the user can override (e.g. treat an integer as a class).
    state.task = isRegressionColumn(col) ? 'regression' : 'classification';
    if (taskSel) {
      taskSel.value = state.task;
      taskSel.removeAttribute('disabled');
    }
    renderTrainCallout(col, state.task);
    runBtn?.removeAttribute('disabled');
  });

  taskSel?.addEventListener('change', (e) => {
    state.task = e.detail?.value === 'regression' ? 'regression' : 'classification';
    const columns = Array.isArray(state.profile?.columns) ? state.profile.columns : [];
    const col = columns.find((c) => String(c.name ?? '') === state.target);
    if (col) renderTrainCallout(col, state.task);
  });

  setupEnginePicker(state);

  runBtn?.addEventListener('click', () => runTraining(pid, state, runBtn));
}

// Engine availability: AutoGluon runs as a mesh service (engine_id=autogluon-training,
// category=training). It is selectable only when at least one such service is in the
// 'running' state. The Rust engine is native and always available. The card stays
// disabled (with a hint pointing to Serwisy) until availability resolves; the backend
// still validates `engine`, so a stale/raced state degrades to a toast on Trenuj.
async function setupEnginePicker(state) {
  const grid = byId('ml-studio-train-engine-grid');
  if (!grid) return;
  const autoCard = grid.querySelector('[data-engine="autogluon"]');
  const hint = byId('ml-studio-train-engine-hint');

  const select = (engine) => {
    state.engine = engine;
    grid.querySelectorAll('.ml-studio-train-engine-card').forEach((card) => {
      const on = card.dataset.engine === engine;
      card.classList.toggle('selected', on);
      card.setAttribute('aria-pressed', on ? 'true' : 'false');
    });
  };

  grid.querySelectorAll('.ml-studio-train-engine-card').forEach((card) => {
    card.addEventListener('click', () => {
      if (card.hasAttribute('disabled')) return;
      select(card.dataset.engine);
    });
  });

  // Resolve AutoGluon availability from the live service list. On any error we keep
  // the card disabled with a hint — the user can still train on Rust.
  try {
    const services = await ApiBinary.list('serviceListRequest', { arrayKey: 'services' });
    const running = (Array.isArray(services) ? services : []).some((s) =>
      (s.engine_id || s.engineId || '') === 'autogluon-training'
      && (s.category || '') === 'training'
      && (s.status || '').toLowerCase() === 'running');
    if (running && autoCard) {
      autoCard.removeAttribute('disabled');
      if (hint) hint.hidden = true;
    } else if (autoCard) {
      autoCard.setAttribute('disabled', '');
      showEngineHint(hint, 'Uruchom serwis „AutoGluon (Tabular AutoML)" w');
    }
  } catch (err) {
    if (autoCard) autoCard.setAttribute('disabled', '');
    showEngineHint(hint, 'Nie udało się sprawdzić serwisów — uruchom „AutoGluon (Tabular AutoML)" w');
  }
}

// Renders the "uruchom serwis w Serwisach" hint with a real link into the Serwisy
// module (SPA navigation, not an href the hash-less router would ignore).
function showEngineHint(hint, prefix) {
  if (!hint) return;
  hint.hidden = false;
  hint.innerHTML = `${sprite('info')} ${escapeHtml(prefix)} <button type="button" class="ml-studio-train-engine-hint-link">Serwisach</button>.`;
  hint.querySelector('.ml-studio-train-engine-hint-link')?.addEventListener('click', (e) => {
    e.stopPropagation();
    Router.navigate('services');
  });
}

// The callout is the provenance of "wykryto N klas": for a categorical target it
// lists the detected classes (value + count) read from the dataset profile; for a
// continuous numeric target it explains the regression fallback.
function renderTrainCallout(col, task) {
  const host = byId('ml-studio-train-callout');
  if (!host) return;
  const slug = columnTypeSlug(col);
  const typeLbl = COLUMN_TYPE_LABEL[slug] || slug;
  const classes = Array.isArray(col.classes) ? col.classes : [];

  let body;
  if (task === 'classification' && classes.length) {
    const list = classes
      .map((c) => `<span class="ml-studio-train-class">${escapeHtml(String(c.value ?? ''))} <span class="ml-studio-train-class-n">(${formatNumber(c.count ?? 0)})</span></span>`)
      .join('');
    body = `
      <div class="ml-studio-train-callout-title">${sprite('info')} Wykryto: <strong>KATEGORIA</strong>, ${classes.length} ${plural(classes.length, 'klasa', 'klasy', 'klas')}</div>
      <div class="ml-studio-train-classes">${list}</div>
      <div class="ml-studio-train-callout-note">→ <strong>klasyfikacja</strong>. Liczba klas to policzone unikalne wartości w tej kolumnie — nie ustawienie systemu.</div>
    `;
  } else if (task === 'classification') {
    const uniqueCount = col.uniqueCount ?? col.unique_count ?? 0;
    body = `
      <div class="ml-studio-train-callout-title">${sprite('info')} Wykryto: <strong>${escapeHtml(typeLbl)}</strong>, ${formatNumber(uniqueCount)} unikalnych wartości</div>
      <div class="ml-studio-train-callout-note">→ <strong>klasyfikacja</strong>. Każda unikalna wartość kolumny-celu staje się klasą.</div>
    `;
  } else {
    body = `
      <div class="ml-studio-train-callout-title">${sprite('info')} Wykryto: <strong>${escapeHtml(typeLbl)}</strong> (liczba ciągła)</div>
      <div class="ml-studio-train-callout-note">→ <strong>regresja</strong>. Model przewiduje wartość liczbową, nie klasę.</div>
    `;
  }
  host.innerHTML = `<div class="ml-studio-train-callout">${body}</div>`;
}

async function runTraining(pid, state, runBtn) {
  if (!state.datasetId || !state.target) {
    toast('Wybierz zbiór i kolumnę-cel.', 'error');
    return;
  }
  runBtn.setAttribute('loading', '');
  runBtn.setAttribute('disabled', '');
  if (state.engine === 'autogluon') {
    toast('Trening AutoML może potrwać — uruchamiam serwis…', 'info');
  }
  try {
    const resp = await ApiBinary.one('mlStudioTabularTrainRequest', {
      projectId: pid,
      datasetId: state.datasetId,
      targetColumn: state.target,
      task: state.task,
      engine: state.engine,
    });
    renderLeaderboard(resp, state);
    toast('Trening zakończony — leaderboard gotowy', 'success');
  } catch (err) {
    toast(`Trening: ${err.message}`, 'error');
  } finally {
    runBtn.removeAttribute('loading');
    runBtn.removeAttribute('disabled');
  }
}

function fmtMetric(value) {
  const n = Number(value);
  if (!Number.isFinite(n)) return '—';
  return n.toLocaleString('pl-PL', { minimumFractionDigits: 3, maximumFractionDigits: 3 });
}

function fmtSecs(value) {
  const n = Number(value);
  if (!Number.isFinite(n)) return '—';
  return `${n.toLocaleString('pl-PL', { maximumFractionDigits: 1 })} s`;
}

function renderLeaderboard(resp, state) {
  const card = byId('ml-studio-train-result-card');
  const meta = byId('ml-studio-train-result-meta');
  const bestHost = byId('ml-studio-train-best');
  const host = byId('ml-studio-train-leaderboard');
  if (!card || !host) return;
  card.hidden = false;

  const isRegression = state.task === 'regression';
  const runId = String(resp.runId ?? resp.run_id ?? '');
  const bestModelId = String(resp.bestModelId ?? resp.best_model_id ?? '');
  let rows = Array.isArray(resp.leaderboard) ? resp.leaderboard.slice() : [];

  // Sort best-first: by F1 (classification) or by lowest RMSE (regression).
  rows.sort((a, b) => {
    if (isRegression) {
      return Number(a.rmse ?? a.RMSE ?? Infinity) - Number(b.rmse ?? b.RMSE ?? Infinity);
    }
    return Number(b.f1Macro ?? b.f1_macro ?? 0) - Number(a.f1Macro ?? a.f1_macro ?? 0);
  });

  const modelId = (m) => String(m.modelId ?? m.model_id ?? m.modelName ?? m.model_name ?? '');
  const best = rows.find((m) => bestModelId && modelId(m) === bestModelId) || rows[0];

  if (meta) {
    let text = `${rows.length} ${plural(rows.length, 'model', 'modele', 'modeli')} · cel = ${escapeHtml(state.target)} · ${taskLabel(state.task)}`;
    if (runId) text += ` · run ${runId.slice(0, 8)}`;
    meta.textContent = text;
  }

  if (bestHost) {
    bestHost.innerHTML = '';
    if (best) {
      const name = String(best.modelName ?? best.model_name ?? '—');
      const cards = isRegression
        ? `<tf-stat-card label="Najlepszy model" value="${escapeAttr(name)}" icon="star"></tf-stat-card>
           <tf-stat-card label="RMSE" value="${escapeAttr(fmtMetric(best.rmse ?? best.RMSE))}" icon="bar-chart"></tf-stat-card>
           <tf-stat-card label="Czas treningu" value="${escapeAttr(fmtSecs(best.trainSecs ?? best.train_secs))}" icon="clock"></tf-stat-card>`
        : `<tf-stat-card label="Najlepszy model" value="${escapeAttr(name)}" icon="star"></tf-stat-card>
           <tf-stat-card label="Dokładność" value="${escapeAttr(fmtMetric(best.accuracy))}" icon="check"></tf-stat-card>
           <tf-stat-card label="F1 (macro)" value="${escapeAttr(fmtMetric(best.f1Macro ?? best.f1_macro))}" icon="bar-chart"></tf-stat-card>
           <tf-stat-card label="Czas treningu" value="${escapeAttr(fmtSecs(best.trainSecs ?? best.train_secs))}" icon="clock"></tf-stat-card>`;
      bestHost.innerHTML = cards;
    }
  }

  host.innerHTML = '';
  if (!rows.length) {
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', 'alert');
    empty.setAttribute('title', 'Brak wyników treningu');
    empty.setAttribute('message', 'Silnik nie zwrócił żadnego modelu — sprawdź, czy zbiór ma dość danych dla wybranego celu.');
    host.appendChild(empty);
    return;
  }

  const bestId = best ? modelId(best) : '';
  const table = document.createElement('tf-table');
  table.setAttribute('variant', 'lined');
  table.innerHTML = isRegression
    ? `
      <tf-column key="model" label="Model" renderer="html"></tf-column>
      <tf-column key="framework" label="Silnik"></tf-column>
      <tf-column key="rmse" label="RMSE" renderer="num"></tf-column>
      <tf-column key="trainSecs" label="Czas" renderer="num"></tf-column>
    `
    : `
      <tf-column key="model" label="Model" renderer="html"></tf-column>
      <tf-column key="framework" label="Silnik"></tf-column>
      <tf-column key="accuracy" label="Dokładność" renderer="num"></tf-column>
      <tf-column key="f1Macro" label="F1 (macro)" renderer="num"></tf-column>
      <tf-column key="trainSecs" label="Czas" renderer="num"></tf-column>
    `;
  table.rows = rows.map((m) => {
    const name = String(m.modelName ?? m.model_name ?? '—');
    const isBest = bestId && modelId(m) === bestId;
    const nameHtml = isBest
      ? `<span class="ml-studio-train-best-row">${sprite('star')} <strong>${escapeHtml(name)}</strong> <tf-chip status="accent" label="najlepszy"></tf-chip></span>`
      : `<span>${escapeHtml(name)}</span>`;
    const row = {
      model: nameHtml,
      framework: String(m.framework ?? m.framework_name ?? '—') || '—',
      trainSecs: fmtSecs(m.trainSecs ?? m.train_secs),
    };
    if (isRegression) {
      row.rmse = fmtMetric(m.rmse ?? m.RMSE);
    } else {
      row.accuracy = fmtMetric(m.accuracy);
      row.f1Macro = fmtMetric(m.f1Macro ?? m.f1_macro);
    }
    return row;
  });
  host.appendChild(table);
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
    const name = m.displayName ?? m.display_name ?? uid;
    const role = String(m.role ?? '').toLowerCase();
    const status = String(m.status ?? 'active').toLowerCase();
    const isSelf = selfId && uid === selfId;
    const invitedBy = m.invitedBy ?? m.invited_by;

    const userHtml = `
      <div class="ml-studio-member-cell">
        <span class="ml-studio-member-id">${escapeHtml(name || '—')}${isSelf ? ' <span class="ml-studio-member-self">(Ty)</span>' : ''}</span>
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

// Relative "X temu" label for the card footer (matches the mockup, which shows a
// single "edytowany 12 min temu" line). Falls back to absolute date for old rows.
function formatRelative(value) {
  if (!value) return '—';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const diffSec = Math.round((Date.now() - date.getTime()) / 1000);
  if (diffSec < 0) return 'przed chwilą';
  if (diffSec < 60) return 'przed chwilą';
  const min = Math.floor(diffSec / 60);
  if (min < 60) return `${min} min temu`;
  const hours = Math.floor(min / 60);
  if (hours < 24) return `${hours} ${plural(hours, 'godz.', 'godz.', 'godz.')} temu`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days} ${plural(days, 'dzień', 'dni', 'dni')} temu`;
  return date.toLocaleDateString('pl-PL');
}

function plural(n, one, few, many) {
  const mod10 = n % 10;
  const mod100 = n % 100;
  if (n === 1) return one;
  if (mod10 >= 2 && mod10 <= 4 && (mod100 < 10 || mod100 >= 20)) return few;
  return many;
}

export default MlStudioScreen;
