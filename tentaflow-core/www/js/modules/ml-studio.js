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
  recognition: ['Schemat', 'Dane', 'Anotacje', 'Treningi', 'Modele'],
  ft_llm: ['Model bazowy', 'Dane', 'Trening', 'Ewaluacja', 'Modele'],
  ft_vision_audio: ['Model bazowy', 'Dane', 'Trening', 'Ewaluacja', 'Modele'],
  tabular_anomaly: ['Dane', 'Trenuj', 'Cechy', 'Anomalie', 'Modele'],
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
    if (params && params.projectId && params.share) {
      await showShare(params.projectId);
      return;
    }
    if (params && params.projectId) {
      await showDetail(params.projectId);
      return;
    }
    if (params && params.create) {
      await showCreateWizard();
      return;
    }
    byId('ml-studio-refresh')?.addEventListener('click', loadAll);
    byId('ml-studio-new')?.addEventListener('click', () => Router.navigate('ml-studio', { create: '1' }));

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
      ? `<div class="ml-studio-wiz-summary-note">${sprite('info')} Model bazowy${state.type === 'distillation' ? ' / nauczyciela' : ''} wybierzesz już w projekcie — w zakładce „${state.type === 'distillation' ? 'Nauczyciel' : 'Model bazowy'}”.</div>`
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
  // "Przegląd" jest zawsze pierwszą zakładką (stan projektu na jednym ekranie),
  // a "Zasoby" zawsze ostatnią (§11.3 — zasoby mesh przydzielone projektowi).
  // Żaden wpis TYPE_TABS nie zawiera "Przegląd", więc bez duplikatów.
  const tabs = ['Przegląd', ...(TYPE_TABS[slug] || ['Dane', 'Treningi', 'Modele']), 'Zasoby'];

  host.innerHTML = `
    <div class="ml-studio-detail-top">
      <tf-button variant="ghost" icon="chevron-left" id="ml-studio-back">Projekty</tf-button>
    </div>

    <tf-detail-header
      title="${escapeAttr(p.name || '(bez nazwy)')}"
      subtitle="${escapeAttr(typeLabel(slug))}"
      icon="${escapeAttr(typeIcon(slug))}">
      <span slot="badges"><tf-badge tone="${statusTone(p.status)}" value="${escapeAttr(statusLabel(p.status))}"></tf-badge></span>
      ${isOwnerProject(p) ? `<span slot="actions"><tf-button variant="outline" icon="share" id="ml-studio-manage-access">Zarządzaj dostępem</tf-button></span>` : ''}
    </tf-detail-header>

    <p class="ml-studio-detail-desc">${escapeHtml(p.description || 'Bez opisu.')}</p>

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
    if (label === 'Model bazowy' && slug === 'ft_llm') {
      renderFtModelTab(panel, p);
      return;
    }
    if (label === 'Schemat' && slug === 'recognition') {
      renderRecogTrainTab(panel, p, { selectTab });
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
    if (label === 'Dane') {
      renderDataTab(panel, projectId(p));
      return;
    }
    if (label === 'Trening' && slug === 'ft_llm') {
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
  renderPanel('ml-tab-0');
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

  const kpi = (icon, label, value, delta) => `
    <div class="ml-studio-kpi">
      <div class="label">${sprite(icon)}${escapeHtml(label)}</div>
      <div class="value">${escapeHtml(String(value))}</div>
      <div class="delta">${escapeHtml(delta)}</div>
    </div>`;

  const kpiGrid = `
    <div class="ml-studio-kpi-grid">
      ${kpi('image', 'Datasety', datasetCount, 'z zakładki Dane')}
      ${kpi('catalog', 'Modele', modelCount, 'wytrenowane wersje w projekcie')}
      ${kpi('brain', 'Treningi', trainingCount, 'uruchomione joby treningowe')}
      ${kpi('users', 'Członkowie', members.length, 'właściciel + osoby z dostępem')}
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
          <tf-column key="status" label="Status" renderer="html"></tf-column>
          <tf-column key="time" label="Czas"></tf-column>
        `;
        table.rows = runs.slice(0, 5).map((r) => {
          const runId = String(r.runId ?? r.run_id ?? '');
          const b = runBadge(r.status);
          return {
            job: `<span class="ml-studio-mono">${escapeHtml(runId.slice(0, 8) || '—')}</span>`,
            status: `<tf-badge tone="${b.tone}" value="${escapeAttr(b.label)}"></tf-badge>`,
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
        const table = document.createElement('tf-table');
        table.setAttribute('variant', 'lined');
        table.innerHTML = `
          <tf-column key="model" label="Model"></tf-column>
          <tf-column key="framework" label="Framework"></tf-column>
          <tf-column key="status" label="Status" renderer="html"></tf-column>
          <tf-column key="createdAt" label="Utworzony"></tf-column>
        `;
        table.rows = models.map((m) => {
          const b = runBadge(m.status);
          return {
            model: String(m.name ?? m.modelId ?? m.model_id ?? '—'),
            framework: String(m.framework ?? '—') || '—',
            status: `<tf-badge tone="${b.tone}" value="${escapeAttr(b.label)}"></tf-badge>`,
            createdAt: formatRelative(m.createdAt ?? m.created_at),
          };
        });
        host.appendChild(table);
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

  // Szybkie skróty do pozostałych zakładek (bez "Przegląd"). Ikona per skrót.
  const shortcutsHost = byId('ml-studio-ov-shortcuts');
  if (shortcutsHost) {
    const slug = p.projectType ?? p.project_type ?? '';
    const shortcutIcon = (label) => {
      if (label === 'Dane') return 'image';
      if (label === 'Zasoby') return 'host';
      if (label === 'Treningi' || label === 'Trening' || label === 'Trenuj') return 'brain';
      if (label === 'Modele') return 'catalog';
      return typeIcon(slug);
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
        <div class="sc-title">${escapeHtml(label)}</div>`;
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
    id: 'Qwen/Qwen3.5-0.8B',
    name: 'Qwen3.5-0.8B',
    sub: 'Mały, szybki — zalecany start dla większości fine-tuningów',
    params: '0.8 B',
    context: '32k',
    license: 'Apache-2.0',
    source: 'serwis',
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
  { id: 'qlora', name: 'QLoRA', desc: '4-bit baza + adaptery LoRA. Najniższa VRAM.', vram: '~8 GB', tone: 'low', lora: true },
  { id: 'lora', name: 'LoRA', desc: 'Baza 16-bit + adaptery. Lepsza jakość niż QLoRA.', vram: '~16 GB', tone: 'mid', lora: true },
  { id: 'dora', name: 'DoRA', desc: 'LoRA z dekompozycją wagi — wyższa wierność.', vram: '~18 GB', tone: 'high', lora: true },
  { id: 'full', name: 'Full', desc: 'Pełny fine-tune wszystkich wag. Najlepsza jakość.', vram: '~24 GB', tone: 'max', lora: false },
];

// Cel treningu (oś 1).
const FT_OBJECTIVES = [
  { id: 'sft', name: 'SFT', desc: 'Supervised fine-tuning (pary wejście→wyjście).' },
  { id: 'dpo', name: 'DPO', desc: 'Direct Preference Optimization (odpowiedź lepsza/gorsza).' },
  { id: 'kd', name: 'KD', desc: 'Knowledge Distillation (student uczy się od nauczyciela).' },
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

function getFtConfig(pid) {
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
// Zakładka „Model bazowy" — f00 (wybór modelu) + f01 (metoda i hiperparametry).
// Auto-zapis do ftConfig[pid] przy każdej zmianie, plus jawny przycisk „Zapisz".
// =============================================================================
function renderFtModelTab(panel, p) {
  const pid = projectId(p);
  const cfg = getFtConfig(pid);

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
        <tf-input id="ml-studio-ft-custom-repo" placeholder="np. Qwen/Qwen3.5-0.8B-Instruct"
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
        <div class="ml-studio-ft-axis-label">Oś 1 — Cel</div>
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
  // Brak konfiguracji → kierujemy do zakładki Model bazowy.
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
  { key: 'resolution', label: 'rozdzielczość', def: 560, step: '28', min: 196 },
];

const recogCfg = {};
function defaultRecogCfg() {
  const hyperparams = {};
  for (const h of RECOG_HP) hyperparams[h.key] = h.def;
  return { datasetId: '', variant: 'base', targetNodeId: '', earlyStopping: true, hyperparams };
}
function getRecogCfg(pid) {
  if (!recogCfg[pid]) recogCfg[pid] = defaultRecogCfg();
  return recogCfg[pid];
}

// Zakładka "Dane" dla recognition: rejestracja datasetu COCO przez ścieżkę.
function renderRecogDataTab(panel, p) {
  const pid = projectId(p);
  panel.innerHTML = `
    <div class="ml-studio-data">
      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('database')} Dataset COCO (katalog na serwerze)
          <span class="ml-studio-data-hint">splity train/valid/test z _annotations.coco.json + obrazy</span>
        </div>
        <p class="ml-studio-data-origin-text" style="margin:0 0 10px">Zbiory detekcji to dziesiątki/setki MB obrazów — podajesz ŚCIEŻKĘ do katalogu COCO na węźle (nie wgrywasz bajtów). Klasy i liczba obrazów są czytane z plików COCO.</p>
        <div style="display:flex;gap:8px;align-items:flex-end;flex-wrap:wrap">
          <tf-input id="ml-studio-recog-path" label="Ścieżka katalogu COCO" placeholder="/home/.../dataset_aug" style="flex:1;min-width:260px"></tf-input>
          <tf-input id="ml-studio-recog-name" label="Nazwa (opcjonalnie)" placeholder="np. Acme ADR" style="min-width:180px"></tf-input>
          <tf-button variant="primary" icon="plus" id="ml-studio-recog-register">Zarejestruj dataset</tf-button>
        </div>
      </section>
      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('cloud')} Zbuduj dataset z plików
          <span class="ml-studio-data-hint">obrazy + wideo → katalog COCO train/ z pustymi anotacjami</span>
        </div>
        <p class="ml-studio-data-origin-text" style="margin:0 0 10px">Wgraj wiele plików (jpg/png/heic, mp4/mov). Obrazy są kopiowane, HEIC dekodowane, a z wideo wycinane klatki. Powstaje dataset COCO gotowy do auto-etykietowania, ręcznej anotacji i treningu.</p>
        <tf-file-input id="ml-studio-recog-build-files" accept=".jpg,.jpeg,.png,.heic,.mp4,.mov" multiple label="Przeciągnij pliki lub kliknij, aby wgrać"></tf-file-input>
        <tf-input id="ml-studio-recog-build-srcdir" label="lub folder na serwerze (ścieżka)" placeholder="np. /mnt/dane/adr" style="margin-top:10px"></tf-input>
        <div style="display:flex;gap:8px;align-items:flex-end;flex-wrap:wrap;margin-top:10px">
          <tf-input id="ml-studio-recog-build-name" label="Nazwa datasetu" placeholder="np. ADR z terenu" style="flex:1;min-width:200px"></tf-input>
          <tf-input id="ml-studio-recog-build-fps" type="number" label="Klatki/s z wideo" value="5" min="1" max="60" style="min-width:140px"></tf-input>
          <tf-button variant="primary" icon="plus" id="ml-studio-recog-build">Zbuduj dataset</tf-button>
        </div>
        <div id="ml-studio-recog-build-progress" class="ml-studio-data-hint" style="margin-top:8px"></div>
      </section>
      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('database')} Zarejestrowane zbiory</div>
        <div id="ml-studio-datasets"></div>
      </section>
    </div>
  `;

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
    } catch (err) {
      toast(`Rejestracja datasetu: ${err.message}`, 'error');
    } finally {
      btn?.removeAttribute('disabled');
    }
  });
  loadDatasets(pid);
}

// Zakładka "Anotacje" — edytor bboxów COCO: galeria obrazów + płótno z rysowaniem,
// przesuwaniem, resize (uchwyty), zmianą klasy, usuwaniem; zapis do COCO.
function renderRecogAnnotateTab(panel, p) {
  const pid = projectId(p);
  // Stan edytora.
  const S = {
    datasetId: '', images: [], categories: [], curIdx: -1,
    origW: 0, origH: 0, boxes: [], sel: -1, dirty: false,
    drag: null, // {mode:'new'|'move'|'resize', handle, startX, startY, orig}
  };

  panel.innerHTML = `
    <div class="ml-studio-annot">
      <section class="ml-studio-data-card">
        <div class="ml-studio-data-head">${sprite('image')} Edytor anotacji (COCO)
          <span class="ml-studio-data-hint">rysuj/przesuwaj/zmieniaj rozmiar ramek, ustaw klasę, zapisz</span>
        </div>
        <tf-select id="ml-studio-annot-dataset" label="Dataset COCO"></tf-select>
        <div class="ml-studio-annot-autolabel" style="display:flex;gap:8px;align-items:flex-end;flex-wrap:wrap;margin-top:10px">
          <tf-input id="ml-studio-annot-threshold" type="number" label="Próg" value="0.5" min="0.5" max="1" step="0.05" style="width:110px"></tf-input>
          <tf-select id="ml-studio-annot-mode" label="Tryb">
            <option value="only_empty">Tylko puste</option>
            <option value="overwrite">Nadpisz</option>
          </tf-select>
          <tf-button id="ml-studio-annot-autolabel-btn" variant="secondary">Auto-etykietuj dataset (RF-DETR)</tf-button>
          <span id="ml-studio-annot-autolabel-prog" class="ml-studio-data-hint"></span>
        </div>
      </section>
      <div class="ml-studio-annot-body">
        <section class="ml-studio-data-card" style="max-height:70vh;overflow:auto">
          <div class="ml-studio-data-head">Obrazy <span id="ml-studio-annot-count" class="ml-studio-data-hint"></span></div>
          <div id="ml-studio-annot-gallery"></div>
        </section>
        <section class="ml-studio-data-card">
          <div id="ml-studio-annot-toolbar" style="display:flex;gap:8px;align-items:center;flex-wrap:wrap;margin-bottom:8px"></div>
          <div id="ml-studio-annot-stage" style="position:relative;max-width:100%;background:var(--bg-3);border:1px solid var(--border);border-radius:var(--radius-sm);min-height:240px"></div>
          <div id="ml-studio-annot-hint" class="ml-studio-data-origin-text" style="margin-top:8px"></div>
        </section>
      </div>
    </div>
  `;

  // Lista datasetów coco_path do selecta.
  (async () => {
    const sel = byId('ml-studio-annot-dataset');
    try {
      const resp = await ApiBinary.one('mlStudioDatasetsListRequest', { projectId: pid });
      const list = (resp.datasets || []).filter((d) => (d.kind || '') === 'coco_path');
      const opts = list.map((d) => ({ value: d.datasetId ?? d.dataset_id, label: `${d.name} (${d.rowCount ?? d.row_count ?? 0} obr.)` }));
      if (sel?.setOptions) sel.setOptions(opts, opts.length ? opts[0].value : null);
      else if (sel) sel.innerHTML = opts.map((o) => `<option value="${escapeAttr(o.value)}">${escapeHtml(o.label)}</option>`).join('');
      if (opts.length) { S.datasetId = opts[0].value; await loadImages(); }
      sel?.addEventListener('change', async (e) => { S.datasetId = e.detail?.value || sel.value; await loadImages(); });
    } catch (err) { byId('ml-studio-annot-gallery').innerHTML = `<div class="ml-studio-ft-done-msg error">${escapeHtml(err.message)}</div>`; }
  })();

  // Auto-etykietowanie całego datasetu wbudowanym detektorem RF-DETR (ADR). Po
  // sukcesie odświeża listę obrazów i bieżący obraz, by ramki były widoczne od razu.
  (() => {
    const btn = byId('ml-studio-annot-autolabel-btn');
    if (!btn) return;
    btn.addEventListener('click', async () => {
      if (!S.datasetId) { toast('Wybierz dataset COCO.', 'error'); return; }
      const thrEl = byId('ml-studio-annot-threshold');
      const modeEl = byId('ml-studio-annot-mode');
      const threshold = Number(thrEl?.value ?? 0.5);
      const mode = String(modeEl?.value ?? 'only_empty');
      // Floor of 0.5: the RF-DETR detector hard-drops anything below it, so a lower
      // threshold cannot surface extra boxes (server enforces the same range).
      if (!(threshold >= 0.5 && threshold <= 1)) { toast('Próg musi być w zakresie 0,5–1.', 'error'); return; }
      const prog = byId('ml-studio-annot-autolabel-prog');
      btn.setAttribute('disabled', '');
      try {
        const resp = await ApiBinary.one('mlStudioRecogAutolabelRequest', { datasetId: S.datasetId, threshold, mode });
        if (resp.status === 'failed' || resp.error) throw new Error(resp.error || 'start nieudany');
        const jobId = resp.jobId ?? resp.job_id;
        if (prog) prog.textContent = 'Auto-etykietowanie…';
        await pollRecogAutolabel(jobId, prog, async () => {
          await loadImages();
          if (S.curIdx >= 0) await selectImage(S.curIdx);
        });
      } catch (err) {
        if (prog) prog.textContent = '';
        toast(`Auto-etykietowanie: ${err.message}`, 'error');
      } finally {
        btn.removeAttribute('disabled');
      }
    });
  })();

  async function loadImages() {
    const gal = byId('ml-studio-annot-gallery');
    gal.innerHTML = '<tf-spinner></tf-spinner>';
    try {
      const resp = await ApiBinary.one('mlStudioRecogImagesListRequest', { datasetId: S.datasetId });
      S.images = JSON.parse(resp.imagesJson ?? resp.images_json ?? '[]');
      S.categories = JSON.parse(resp.categoriesJson ?? resp.categories_json ?? '[]');
      byId('ml-studio-annot-count').textContent = `(${S.images.length})`;
      renderGallery();
      if (S.images.length) selectImage(0);
      else { byId('ml-studio-annot-stage').innerHTML = '<div class="ml-studio-ft-chart-empty">Brak obrazów w datasecie.</div>'; }
    } catch (err) { gal.innerHTML = `<div class="ml-studio-ft-done-msg error">${escapeHtml(err.message)}</div>`; }
  }

  function renderGallery() {
    const gal = byId('ml-studio-annot-gallery');
    gal.innerHTML = S.images.map((im, i) => `
      <div class="ml-studio-annot-thumb${i === S.curIdx ? ' active' : ''}" data-idx="${i}"
           style="padding:6px 8px;border-radius:6px;cursor:pointer;display:flex;justify-content:space-between;gap:6px;${i === S.curIdx ? 'background:var(--accent-glow)' : ''}">
        <span style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${escapeHtml(im.file_name)}</span>
        <span class="ml-studio-data-hint">${im.ann_count}</span>
      </div>`).join('');
    gal.querySelectorAll('.ml-studio-annot-thumb').forEach((el) => {
      el.addEventListener('click', () => maybeLeave(() => selectImage(Number(el.getAttribute('data-idx')))));
    });
  }

  function maybeLeave(fn) {
    if (S.dirty && !confirm('Masz niezapisane zmiany. Porzucić je?')) return;
    fn();
  }

  async function selectImage(idx) {
    if (idx < 0 || idx >= S.images.length) return;
    S.curIdx = idx; S.sel = -1; S.dirty = false;
    renderGallery();
    const stage = byId('ml-studio-annot-stage');
    stage.innerHTML = '<tf-spinner></tf-spinner>';
    try {
      const im = S.images[idx];
      const resp = await ApiBinary.one('mlStudioRecogImageRequest', { datasetId: S.datasetId, imageId: im.image_id });
      if (resp.error) throw new Error(resp.error);
      S.origW = resp.origWidth ?? resp.orig_width ?? im.width;
      S.origH = resp.origHeight ?? resp.orig_height ?? im.height;
      const anns = JSON.parse(resp.annotationsJson ?? resp.annotations_json ?? '[]');
      S.boxes = anns.map((a) => ({ category_id: a.category_id, x: a.bbox[0], y: a.bbox[1], w: a.bbox[2], h: a.bbox[3] }));
      renderStage(`data:${resp.mime || 'image/jpeg'};base64,${resp.imageB64 ?? resp.image_b64}`);
      renderToolbar();
    } catch (err) { stage.innerHTML = `<div class="ml-studio-ft-done-msg error">${escapeHtml(err.message)}</div>`; }
  }

  function catName(id) { const c = S.categories.find((c) => c.id === id); return c ? c.name : String(id); }
  function catColor(id) { return `hsl(${(id * 67) % 360} 85% 55%)`; }
  function defaultCat() { return S.categories.length ? S.categories[S.categories.length > 1 && S.categories[0].id === 0 ? 1 : 0].id : 0; }

  function renderToolbar() {
    const tb = byId('ml-studio-annot-toolbar');
    const catOpts = S.categories.map((c) => `<option value="${c.id}">${escapeHtml(c.name)}</option>`).join('');
    tb.innerHTML = `
      <button class="btn btn-secondary" id="annot-prev">◀</button>
      <span class="ml-studio-data-hint" id="annot-pos">${S.curIdx + 1}/${S.images.length}</span>
      <button class="btn btn-secondary" id="annot-next">▶</button>
      <span style="margin-left:8px">Klasa zazn.:</span>
      <select id="annot-cat" class="ml-studio-annot-cat">${catOpts}</select>
      <button class="btn btn-secondary" id="annot-del">Usuń ramkę</button>
      <button class="btn btn-primary" id="annot-save" style="margin-left:auto">Zapisz anotacje</button>`;
    byId('annot-prev').onclick = () => maybeLeave(() => selectImage(S.curIdx - 1));
    byId('annot-next').onclick = () => maybeLeave(() => selectImage(S.curIdx + 1));
    byId('annot-del').onclick = () => { if (S.sel >= 0) { S.boxes.splice(S.sel, 1); S.sel = -1; S.dirty = true; drawBoxes(); } };
    byId('annot-save').onclick = saveAnns;
    byId('annot-cat').onchange = (e) => { if (S.sel >= 0) { S.boxes[S.sel].category_id = Number(e.target.value); S.dirty = true; drawBoxes(); } };
    byId('ml-studio-annot-hint').textContent = 'Rysuj ramkę: przeciągnij na pustym. Zaznacz: klik. Przesuń: przeciągnij wnętrze. Skaluj: narożniki. Usuń: klawisz Delete.';
  }

  function renderStage(src) {
    const stage = byId('ml-studio-annot-stage');
    stage.innerHTML = `
      <img id="annot-img" src="${src}" style="display:block;width:100%;height:auto;user-select:none;-webkit-user-drag:none"/>
      <svg id="annot-svg" viewBox="0 0 ${S.origW} ${S.origH}" preserveAspectRatio="none"
           style="position:absolute;inset:0;width:100%;height:100%;cursor:crosshair"></svg>`;
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
      const sw = Math.max(2, S.origW / 400);
      // Interior stays transparent so the image underneath is always visible, but a
      // transparent fill (NOT `none`) still captures pointer events, so the whole box
      // is clickable to re-select/move it. Selection is shown by a thicker outline.
      html += `<rect data-box="${i}" x="${b.x}" y="${b.y}" width="${Math.max(0, b.w)}" height="${Math.max(0, b.h)}"
        fill="transparent" stroke="${col}" stroke-width="${seld ? sw * 1.9 : sw}" style="pointer-events:all;cursor:move"/>`;
      html += `<text x="${b.x}" y="${Math.max(hs, b.y - 4)}" fill="${col}" font-size="${Math.max(11, S.origW / 55)}" font-weight="700" style="pointer-events:none">${escapeHtml(catName(b.category_id))}</text>`;
      if (seld) {
        const corners = [[b.x, b.y, 'nw'], [b.x + b.w, b.y, 'ne'], [b.x, b.y + b.h, 'sw'], [b.x + b.w, b.y + b.h, 'se']];
        for (const [cx, cy, h] of corners) {
          html += `<rect class="annot-handle" data-box="${i}" data-handle="${h}" x="${cx - hs / 2}" y="${cy - hs / 2}" width="${hs}" height="${hs}" fill="#fff" stroke="${col}" stroke-width="1" style="cursor:nwse-resize"/>`;
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
      S.drag = { mode: 'move', startX: p0.x, startY: p0.y, orig: { ...S.boxes[S.sel] } };
      syncCatSelect();
    } else {
      // Nowa ramka.
      const b = { category_id: Number(byId('annot-cat')?.value ?? defaultCat()), x: p0.x, y: p0.y, w: 0, h: 0 };
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

  function onUp() {
    if (!S.drag) return;
    const b = S.boxes[S.sel];
    if (S.drag.mode === 'new' && b && (b.w < 3 || b.h < 3)) { S.boxes.splice(S.sel, 1); S.sel = -1; } // za mała = anuluj
    S.drag = null; syncCatSelect(); drawBoxes();
  }

  function syncCatSelect() { const s = byId('annot-cat'); if (s && S.sel >= 0) s.value = String(S.boxes[S.sel].category_id); }
  function clamp(v, lo, hi) { return Math.max(lo, Math.min(hi, v)); }

  async function saveAnns() {
    const btn = byId('annot-save'); btn?.setAttribute('disabled', '');
    try {
      const anns = S.boxes.filter((b) => b.w >= 3 && b.h >= 3).map((b) => ({
        category_id: b.category_id, bbox: [Math.round(b.x), Math.round(b.y), Math.round(b.w), Math.round(b.h)],
      }));
      const resp = await ApiBinary.one('mlStudioRecogSaveAnnotationsRequest', {
        datasetId: S.datasetId, imageId: S.images[S.curIdx].image_id, annotationsJson: JSON.stringify(anns),
      });
      if (!resp.ok) throw new Error(resp.error || 'zapis nieudany');
      S.dirty = false;
      S.images[S.curIdx].ann_count = anns.length; renderGallery();
      toast('Anotacje zapisane.', 'success');
    } catch (err) { toast(`Zapis anotacji: ${err.message}`, 'error'); }
    finally { btn?.removeAttribute('disabled'); }
  }

  // Klawisz Delete usuwa zaznaczoną ramkę (gdy zakładka aktywna).
  const keyHandler = (e) => {
    if ((e.key === 'Delete' || e.key === 'Backspace') && S.sel >= 0 && byId('annot-svg')) {
      S.boxes.splice(S.sel, 1); S.sel = -1; S.dirty = true; drawBoxes(); e.preventDefault();
    }
  };
  document.addEventListener('keydown', keyHandler);
}

// Zakładka "Schemat" dla recognition: wybór datasetu + wariantu + hiperparametry
// + start treningu. Po starcie przechodzi w widok LIVE (startRecogLive).
function renderRecogTrainTab(panel, p, { selectTab }) {
  const pid = projectId(p);
  const cfg = getRecogCfg(pid);
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

  panel.innerHTML = `
    <div class="ml-studio-ft">
      <div id="ml-studio-recog-setup">
        <section class="ml-studio-data-card">
          <div class="ml-studio-data-head">${sprite('database')} Zbiór treningowy (COCO)</div>
          <tf-select id="ml-studio-recog-dataset" label="Dataset" placeholder="wybierz zarejestrowany dataset COCO"></tf-select>
          <div id="ml-studio-recog-classes" class="ml-studio-data-origin-text" style="margin-top:8px"></div>
        </section>
        <section class="ml-studio-data-card">
          <div class="ml-studio-data-head">${sprite('image')} Wariant modelu RF-DETR</div>
          <div class="ml-studio-ft-axis-grid" id="ml-studio-recog-variants">${variantCards}</div>
        </section>
        <section class="ml-studio-data-card">
          <div class="ml-studio-data-head">${sprite('services')} Węzeł treningu (mesh)
            <span class="ml-studio-data-hint">trening lokalnie albo na zdalnym węźle (Node B); dataset COCO musi być widoczny na wybranym węźle</span>
          </div>
          <tf-select id="ml-studio-recog-node" label="Węzeł"></tf-select>
        </section>
        <section class="ml-studio-data-card">
          <div class="ml-studio-data-head">${sprite('tune')} Hiperparametry</div>
          <div class="ml-studio-ft-hp-grid">${hpInputs}</div>
        </section>
        <div class="ml-studio-ft-actions">
          <tf-button variant="primary" icon="play" id="ml-studio-recog-run">Uruchom trening</tf-button>
        </div>
      </div>
      <div id="ml-studio-recog-live"></div>
    </div>
  `;

  // Lista datasetów COCO do selecta.
  (async () => {
    try {
      const resp = await ApiBinary.one('mlStudioDatasetsListRequest', { projectId: pid });
      const list = (resp.datasets || []).filter((d) => (d.kind || '') === 'coco_path' || (d.kind || '') === 'coco');
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

  byId('ml-studio-recog-run')?.addEventListener('click', async () => {
    if (!cfg.datasetId) { toast('Wybierz zarejestrowany dataset COCO.', 'error'); return; }
    const btn = byId('ml-studio-recog-run');
    btn?.setAttribute('disabled', '');
    try {
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
      const runId = resp.runId ?? resp.run_id;
      if (!runId) throw new Error('Backend nie zwrócił runId.');
      const setup = byId('ml-studio-recog-setup');
      if (setup) setup.hidden = true;
      startRecogLive(byId('ml-studio-recog-live'), runId, { selectTab });
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

function startRecogLive(host, runId, { selectTab }) {
  if (!host) return;
  stopFtPolling();
  host.innerHTML = `
    <section class="ml-studio-data-card ml-studio-ft-live">
      <div class="ml-studio-data-head">${sprite('cpu')} Trening detekcji na żywo
        <span class="ml-studio-ft-status" id="ml-studio-recog-badge"><tf-badge tone="warning" value="trening trwa"></tf-badge></span>
      </div>
      <div class="ml-studio-ft-progress">
        <div class="ml-studio-ft-progress-meta" id="ml-studio-recog-meta">epoka 0</div>
        <tf-progress-bar id="ml-studio-recog-bar" value="0" tone="accent"></tf-progress-bar>
      </div>
      <div class="ml-studio-ft-kpi-grid" id="ml-studio-recog-kpi"></div>
      <div class="ml-studio-ft-chart-wrap">
        <div class="ml-studio-ft-chart-head">
          <span class="ml-studio-ft-chart-title">Krzywa: train loss + mAP@50</span>
          <span class="ml-studio-ft-chart-legend">
            <span class="lg"><span class="sw train"></span>train loss</span>
            <span class="lg"><span class="sw eval"></span>mAP@50</span>
          </span>
        </div>
        <div id="ml-studio-recog-chart"></div>
      </div>
      <div class="ml-studio-ft-done" id="ml-studio-recog-done" hidden></div>
    </section>
  `;

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
    const loss = st.trainLoss ?? st.train_loss;
    const map50 = st.map50;
    const curve = Array.isArray(st.curve) ? st.curve : [];
    const meta = byId('ml-studio-recog-meta');
    const bar = byId('ml-studio-recog-bar');
    if (total > 0) {
      const pct = Math.max(0, Math.min(100, Math.round((epoch / total) * 100)));
      if (meta) meta.textContent = `epoka ${epoch} / ${total} · ${pct}%`;
      if (bar) bar.setAttribute('value', String(pct));
    } else if (meta) {
      meta.innerHTML = `<tf-spinner size="sm"></tf-spinner> trwa — epoka ${epoch}`;
    }
    const kpi = byId('ml-studio-recog-kpi');
    if (kpi) {
      kpi.innerHTML = `
        <div class="ml-studio-ft-kpi"><div class="lbl">train loss</div><div class="val">${loss != null ? Number(loss).toFixed(4) : '—'}</div></div>
        <div class="ml-studio-ft-kpi"><div class="lbl">mAP@50</div><div class="val">${map50 != null ? Number(map50).toFixed(4) : '—'}</div></div>
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
      toast('Trening detekcji zakończony.', 'success');
    } else if (status === 'failed') {
      stopFtPolling();
      toast(`Trening nieudany: ${st.error || 'nieznany błąd'}`, 'error');
      const done = byId('ml-studio-recog-done');
      if (done) { done.hidden = false; done.innerHTML = `<div class="ml-studio-ft-done-msg error">${sprite('alert')} ${escapeHtml(st.error || 'Trening zakończył się błędem.')}</div>`; }
    }
  };

  const poll = async () => {
    try {
      const st = await ApiBinary.one('mlStudioRecogTrainStatusRequest', { runId });
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
// Zakładka "Modele" — lista wytrenowanych modeli projektu (wszystkie typy).
// Pusto → tf-empty-state; inaczej tf-table z metrykami z metricsJson.
// =============================================================================

// Wyciąga skrótowe metryki z metricsJson modelu (np. "acc 0.94" / "loss 1.2").
// Zwraca pusty string, gdy JSON nie zawiera znanych pól — wtedy kolumna pokaże "—".
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
  if (loss != null) parts.push(`loss ${loss.toFixed(2)}`);
  return parts.join(' · ');
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
        let exported = false;
        try {
          const mj = JSON.parse(m.metricsJson ?? m.metrics_json ?? '{}');
          deployed = Boolean(mj.inference_model_name);
          exported = mj.export_status === 'succeeded' && Boolean(mj.gguf_path);
        } catch (_) { deployed = false; exported = false; }
        return {
          model: modelName,
          framework: String(m.framework ?? '—') || '—',
          baseModel: baseModel || '—',
          status: `<tf-badge tone="${b.tone}" value="${escapeAttr(b.label)}"></tf-badge>`,
          metrics: metrics || '—',
          createdAt: formatRelative(m.createdAt ?? m.created_at),
          // Pola pomocnicze do buildera akcji (nie kolumny — tf-table ich nie renderuje).
          _modelId: modelId,
          _modelName: modelName,
          // Model detekcji (RF-DETR) → akcja „Wykryj"; model FT (adapter, niepuste
          // baseModel) → „Eksportuj GGUF"; wdrożony FT → też „Zapytaj".
          _isRecog: String(m.framework ?? '') === 'rfdetr',
          _canExport: Boolean(modelId && baseModel.trim().length > 0 && String(m.framework ?? '') !== 'rfdetr'),
          _canChat: Boolean(modelId && deployed),
          _canDeploy: Boolean(modelId && exported && !deployed && String(m.framework ?? '') !== 'rfdetr'),
        };
      });
      // Per-wierszowy builder akcji tf-table: zwraca realny Element z własnym
      // handlerem klika — działa w shadow DOM (delegacja z light DOM by nie złapała).
      table.rowActions = (row) => {
        if (!row) return null;
        if (row._isRecog) {
          const btn = document.createElement('tf-button');
          btn.setAttribute('size', 'sm');
          btn.setAttribute('variant', 'outline');
          btn.setAttribute('icon', 'image');
          btn.textContent = 'Wykryj na zdjęciu';
          btn.addEventListener('click', () => openRecogDetectPanel(p, row._modelId, row._modelName));
          return btn;
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
