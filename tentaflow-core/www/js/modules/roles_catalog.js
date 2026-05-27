// ============ File: roles_catalog.js — Katalog ról biznesowych (admin) ============
//
// Administracyjny katalog rol opisujacych KIM ktos jest w organizacji
// (handlowiec / PM techniczny / decydent klienta). Read-only dla zwyklych
// uzytkownikow, full CRUD dla adminow. Komunikacja: binary protocol
// (`MessageBody::RoleCatalogBody` przez ApiBinary). Mockup: O2 (designs/crm-v1).
// Cala warstwa prezentacji oparta o komponenty tf-* (tf-table, tf-tabs,
// tf-input, tf-select, tf-textarea, tf-toggle, tf-chip, tf-button, tf-window).

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';

// =============================================================================
// Stale UI
// =============================================================================

const STRINGS = {
  title: 'Katalog ról',
  crumbManagement: 'Struktura organizacyjna',
  crumbCatalog: 'Katalog ról',
  subtitle: 'Definicje ról biznesowych — kto kim jest. Co rola może zrobić konfigurujesz w Uprawnieniach.',

  tabTree: 'Drzewo',
  tabList: 'Lista',
  tabCatalog: 'Katalog ról',
  tabVisibility: 'Widoczność danych',
  tabHistory: 'Historia zmian',

  actionNew: '+ Nowa rola',
  actionSave: 'Zapisz',
  actionCreate: 'Utwórz',
  actionCancel: 'Anuluj',
  actionDelete: 'Dezaktywuj rolę',
  actionClose: 'Zamknij',

  searchPh: 'Szukaj roli…',
  readOnlyBanner: 'Widok tylko do odczytu — edycja zarezerwowana dla administratorów.',

  kpiAll: 'Wszystkich ról',
  kpiSales: 'Sales',
  kpiTechnical: 'Technical',
  kpiManagement: 'Management',
  kpiExternal: 'External',

  kpiDeltaAll: 'Wszystkie aktywne role',
  kpiDeltaSales: 'Handlowiec L1/L2/Lead',
  kpiDeltaTechnical: 'PM/Arch/Konsult.',
  kpiDeltaManagement: 'Dyrektorzy/Prezes',
  kpiDeltaExternal: 'Decydent/Sponsor/User',

  filterAll: 'Wszystkie',
  filterSales: 'sales',
  filterTechnical: 'technical',
  filterManagement: 'management',
  filterExternal: 'external',
  filterOther: 'other',

  colName: 'Nazwa',
  colKind: 'Kind',
  colManager: 'is_manager',
  colScope: 'Default visibility scope',
  colActions: '',

  managerYes: 'tak',
  managerDash: '—',

  emptyList: 'Brak ról w katalogu.',
  emptyMatch: 'Brak ról pasujących do filtra.',
  loading: 'Wczytywanie…',
  loadFailed: 'Nie udało się załadować katalogu ról.',

  modalCreateTitle: 'Nowa rola',
  modalEditTitle: 'Edycja roli',
  modalViewTitle: 'Podgląd roli',

  sectionNamePerLocale: 'Nazwa (per język)',
  sectionDescriptionPerLocale: 'Opis (per język)',
  sectionPlatformTraits: 'Platformowe traity (strukturalne)',
  labelName: 'Nazwa',
  labelDescription: 'Opis',
  labelSlug: 'Slug (stała, używana w kodzie)',
  labelKind: 'Kind',
  labelIcon: 'Ikona',
  labelColor: 'Kolor (hex lub --css-var)',
  labelIsManager: 'is_manager',
  hintIsManager: 'Rola menedżerska. Używana w O1 do layoutu drzewa (manager nad podwładnymi).',
  labelScope: 'Default visibility scope',
  hintScope: 'Tylko sugestia dla addonów liczących permissions. Każdy addon i tak może override w P2.',
  placeholderSlug: 'np. handlowiec_l1',
  placeholderColor: '#a78bfa lub --accent-2',

  scopeAssigned: 'assigned — widzi tylko zasoby do których jest przypisany',
  scopeOwn: 'own — widzi tylko własne (gdzie jest owner)',
  scopeSection: 'section — widzi swoją sekcję',
  scopeDepartment: 'department — widzi swój dział (transitive)',
  scopeAll: 'all — widzi wszystko',

  iconNone: '(brak)',

  errSlugRequired: 'Slug jest wymagany.',
  errSlugFormat: 'Slug musi pasować do wzorca [a-z][a-z0-9_]*, max 50 znaków.',
  errKindRequired: 'Kind jest wymagany.',
  errNameRequired: 'Nazwa jest wymagana dla każdego aktywnego języka.',
  errDescriptionPartial: 'Opis musi być uzupełniony dla wszystkich aktywnych języków albo żaden.',
  errColorFormat: 'Kolor musi mieć format #rrggbb lub --nazwa-css-var.',
  errIconUnknown: 'Nieznana ikona — wybierz z listy.',

  saveOk: 'Zapisano.',
  createOk: 'Rola utworzona.',
  deactivateOk: 'Rola dezaktywowana.',
  confirmDeactivate: 'Czy na pewno chcesz dezaktywować rolę „{name}"? Operacja jest miękkim usunięciem.',
  loadDetailFailed: 'Nie udało się pobrać szczegółów roli.',

  infoBannerTitle: 'Co rola może zrobić w danym addonie — konfigurujesz osobno',
  infoBannerText: 'Akcje typu „edytuj deal", „zatwierdź budżet", „refakturuj koszt", „loguj czas" są regułami w P2 Permissions i referują role z tego katalogu. Tutaj rola to tylko kto to jest, w P2 co może.',
  infoBannerCta: 'Otwórz P2 Permissions',

  warnBannerTitle: 'Brakuje flag typu can_edit_deal / can_approve_budget?',
  warnBannerText: 'Tutaj rola jest opisem kim ktoś jest. To co rola realnie może zrobić w konkretnym addonie konfigurujesz w P2 Permissions jako reguły (np. „role=pm_technical może edit deal gdy assigned_to_self"). Zapewnia to czystość: rola jest stała, reguły mogą się zmieniać per addon bez ruszania katalogu.',

  footerNotice: 'Zmiany dotkną <strong>{n}</strong> osoby aktualnie pełniące tę rolę. Recalculation uprawnień ~5s.',
  footerNoticeUnknown: 'Zmiany przeliczą uprawnienia dla wszystkich osób z tą rolą (~5s).',

  kindLabels: {
    sales: 'sales',
    technical: 'technical',
    management: 'management',
    external: 'external',
    other: 'other',
  },
};

// Whitelist ikon zsynchronizowana z `services::role_catalog::ALLOWED_ICONS`.
const ALLOWED_ICONS = [
  'i-briefcase', 'i-shield', 'i-users', 'i-user', 'i-deal', 'i-activity',
  'i-calendar', 'i-receipt', 'i-folder', 'i-grid', 'i-puzzle', 'i-network',
  'i-star', 'i-info', 'i-key', 'i-clipboard', 'i-cube', 'i-headset',
  'i-code', 'i-bug', 'i-building', 'i-chart', 'i-crown', 'i-user-check',
  'i-user-cog',
];

const KIND_VALUES = ['sales', 'technical', 'management', 'external', 'other'];
const SCOPE_VALUES = ['assigned', 'own', 'section', 'department', 'all'];

const SLUG_REGEX = /^[a-z][a-z0-9_]*$/;
const COLOR_REGEX = /^(#[0-9a-fA-F]{6}|--[a-z][a-z0-9-]*)$/;

// =============================================================================
// Stan modulu
// =============================================================================

const state = {
  me: null,
  isAdmin: false,
  roles: [],
  locales: [],
  activeFilter: 'all',
  searchQuery: '',
  currentLocale: 'pl',
  editorMode: null,
  editingDetail: null,
  formData: null,
  formErrors: {},
  currentEditorLocale: 'pl',
};

let searchDebounceTimer = null;

// =============================================================================
// Screen lifecycle
// =============================================================================

const RolesCatalogScreen = {
  title: STRINGS.title,
  render() {
    return `<div id="roles-catalog-root"></div>`;
  },
  async mount() {
    try {
      state.me = await ApiBinary.one('authMeRequest');
    } catch {
      state.me = null;
    }
    state.isAdmin = !!(state.me && (state.me.role === 'admin' || state.me.isAdmin === true));
    state.currentLocale = (state.me && typeof state.me.locale === 'string' && state.me.locale)
      ? state.me.locale
      : 'pl';

    const root = byId('roles-catalog-root');
    if (!root) return;
    root.innerHTML = `<div class="rc-loading">${escapeHtml(STRINGS.loading)}</div>`;

    try {
      await Promise.all([loadRoles(), loadLocales()]);
    } catch (err) {
      root.innerHTML = `<div class="rc-error">${escapeHtml(STRINGS.loadFailed)}: ${escapeHtml(err.message || '')}</div>`;
      return;
    }
    state.currentEditorLocale = pickDefaultLocale();
    renderScreen();
  },
  unmount() {
    state.me = null;
    state.isAdmin = false;
    state.roles = [];
    state.locales = [];
    state.activeFilter = 'all';
    state.searchQuery = '';
    state.editorMode = null;
    state.editingDetail = null;
    state.formData = null;
    state.formErrors = {};
    if (searchDebounceTimer) {
      clearTimeout(searchDebounceTimer);
      searchDebounceTimer = null;
    }
  },
};

export default RolesCatalogScreen;

// =============================================================================
// Dane — binary protocol
// =============================================================================

async function loadRoles() {
  const filter = {};
  if (state.activeFilter !== 'all') filter.kind = state.activeFilter;
  if (state.searchQuery.trim()) filter.search = state.searchQuery.trim();
  filter.isActive = true;
  const resp = await ApiBinary.action('roleCatalogListRequest', filter);
  state.roles = Array.isArray(resp?.roles) ? resp.roles : [];
}

async function loadLocales() {
  const resp = await ApiBinary.one('roleCatalogListLocalesRequest');
  state.locales = Array.isArray(resp?.locales) ? resp.locales : [];
}

async function fetchRoleDetail(id) {
  const resp = await ApiBinary.action('roleCatalogGetRequest', { id });
  return resp?.role || null;
}

function pickDefaultLocale() {
  if (!state.locales.length) return 'pl';
  const def = state.locales.find((l) => l.isDefault);
  if (def) return def.code;
  if (state.locales.some((l) => l.code === state.currentLocale)) return state.currentLocale;
  return state.locales[0].code;
}

// =============================================================================
// Render — glowny ekran
// =============================================================================

function renderScreen() {
  const root = byId('roles-catalog-root');
  if (!root) return;
  const chev = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="9 18 15 12 9 6"/></svg>';

  const newRoleBtn = state.isAdmin
    ? `<tf-button variant="primary" id="rc-new">${escapeHtml(STRINGS.actionNew)}</tf-button>`
    : '';

  const readOnlyBanner = state.isAdmin
    ? ''
    : `<div class="rc-readonly-banner">${escapeHtml(STRINGS.readOnlyBanner)}</div>`;

  // Pill-tabs ekranow O-tool. Tylko `roles` jest aktywne; reszta to placeholder
  // dla przyszlych ekranow (O1 drzewo, lista, widocznosc, historia).
  const orgTabs = `
    <tf-tabs variant="solid" value="roles" id="rc-org-tabs">
      <tf-tab id="tree" disabled>${escapeHtml(STRINGS.tabTree)}</tf-tab>
      <tf-tab id="list" disabled>${escapeHtml(STRINGS.tabList)}</tf-tab>
      <tf-tab id="roles">${escapeHtml(STRINGS.tabCatalog)}</tf-tab>
      <tf-tab id="visibility" disabled>${escapeHtml(STRINGS.tabVisibility)}</tf-tab>
      <tf-tab id="history" disabled>${escapeHtml(STRINGS.tabHistory)}</tf-tab>
    </tf-tabs>
  `;

  root.innerHTML = `
    <div class="rc-screen">
      <div class="rc-topbar">
        <div class="rc-crumb">
          <strong>${escapeHtml(STRINGS.crumbManagement)}</strong>
          <span class="sep">${chev}</span>
          <span>${escapeHtml(STRINGS.crumbCatalog)}</span>
        </div>
        ${orgTabs}
        <div class="rc-topbar-actions">
          <tf-searchbox id="rc-search" placeholder="${escapeAttr(STRINGS.searchPh)}" debounce="200" value="${escapeAttr(state.searchQuery)}"></tf-searchbox>
          ${newRoleBtn}
        </div>
      </div>

      ${readOnlyBanner}

      <div class="rc-kpi-grid" id="rc-kpi-grid">${renderKpis()}</div>

      <div class="rc-filter-bar" id="rc-filter-group">${renderFilterChips()}</div>

      <div class="rc-table-host" id="rc-table-host"></div>

      <div class="rc-info-banner">
        <svg class="icon" viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/></svg>
        <div class="rc-info-banner-body">
          <div class="rc-info-banner-title">${escapeHtml(STRINGS.infoBannerTitle)}</div>
          <div class="rc-info-banner-text">${escapeHtml(STRINGS.infoBannerText)}</div>
        </div>
        <tf-button variant="ghost" size="sm" disabled>${escapeHtml(STRINGS.infoBannerCta)}</tf-button>
      </div>
    </div>
  `;

  mountTable();
  wireScreen(root);
}

function renderKpis() {
  const total = state.roles.length;
  const byKind = countByKind(state.roles);
  const tile = (kindClass, label, value, delta) => `
    <div class="rc-kpi-tile rc-kpi-${kindClass}">
      <div class="rc-kpi-label">${escapeHtml(label)}</div>
      <div class="rc-kpi-value">${escapeHtml(String(value))}</div>
      <div class="rc-kpi-delta">${escapeHtml(delta)}</div>
      <div class="rc-kpi-bar"></div>
    </div>
  `;
  return [
    tile('all', STRINGS.kpiAll, total, STRINGS.kpiDeltaAll),
    tile('sales', STRINGS.kpiSales, byKind.sales || 0, STRINGS.kpiDeltaSales),
    tile('technical', STRINGS.kpiTechnical, byKind.technical || 0, STRINGS.kpiDeltaTechnical),
    tile('management', STRINGS.kpiManagement, byKind.management || 0, STRINGS.kpiDeltaManagement),
    tile('external', STRINGS.kpiExternal, byKind.external || 0, STRINGS.kpiDeltaExternal),
  ].join('');
}

function countByKind(roles) {
  const out = {};
  for (const r of roles) {
    out[r.kind] = (out[r.kind] || 0) + 1;
  }
  return out;
}

function renderFilterChips() {
  const total = state.roles.length;
  const byKind = countByKind(state.roles);
  const chip = (filterId, label, count) => {
    const activeAttr = state.activeFilter === filterId ? 'active' : '';
    return `<tf-chip class="rc-filter-chip" clickable ${activeAttr} data-filter="${escapeAttr(filterId)}">${escapeHtml(label)}<span class="rc-chip-count">${escapeHtml(String(count))}</span></tf-chip>`;
  };
  return [
    chip('all', STRINGS.filterAll, total),
    chip('sales', STRINGS.filterSales, byKind.sales || 0),
    chip('technical', STRINGS.filterTechnical, byKind.technical || 0),
    chip('management', STRINGS.filterManagement, byKind.management || 0),
    chip('external', STRINGS.filterExternal, byKind.external || 0),
    chip('other', STRINGS.filterOther, byKind.other || 0),
  ].join('');
}

// =============================================================================
// Tabela — tf-table z renderem `html` per komorka.
// =============================================================================

function mountTable() {
  const host = byId('rc-table-host');
  if (!host) return;
  const visible = filteredRoles();

  if (visible.length === 0) {
    const msg = state.roles.length === 0 ? STRINGS.emptyList : STRINGS.emptyMatch;
    host.innerHTML = `<div class="rc-empty">${escapeHtml(msg)}</div>`;
    return;
  }

  const editColHeader = state.isAdmin
    ? `<tf-column key="actions" label="" renderer="html"></tf-column>`
    : '';

  host.innerHTML = `
    <tf-table id="rc-table" sortable>
      <tf-column key="icon" label="" renderer="html"></tf-column>
      <tf-column key="name" label="${escapeAttr(STRINGS.colName)}" renderer="html" sortable></tf-column>
      <tf-column key="kind" label="${escapeAttr(STRINGS.colKind)}" renderer="chip"></tf-column>
      <tf-column key="manager" label="${escapeAttr(STRINGS.colManager)}" renderer="html"></tf-column>
      <tf-column key="scope" label="${escapeAttr(STRINGS.colScope)}" renderer="html"></tf-column>
      ${editColHeader}
    </tf-table>
  `;

  const tbl = byId('rc-table');
  tbl.rows = visible.map(roleToRow);

  tbl.addEventListener('row-click', (e) => {
    const row = e.detail?.row;
    if (!row || !row._id) return;
    openRoleDetail(row._id);
  });
}

function filteredRoles() {
  // Filtr po `kind` jest realizowany serwerowo; aplikujemy dodatkowo lokalnie
  // aby przelaczenie chip bez round-tripu pokazalo wynik.
  let list = state.roles;
  if (state.activeFilter !== 'all') {
    list = list.filter((r) => r.kind === state.activeFilter);
  }
  return list;
}

function roleToRow(role) {
  const name = pickTranslation(role.nameTranslations, state.currentLocale) || role.slug;
  const iconRef = role.icon || 'i-briefcase';
  const colorStyle = role.colorHint
    ? `style="background:${escapeAttr(role.colorHint)};"`
    : '';
  const iconHtml = `
    <div class="rc-avatar rc-avatar-${escapeAttr(role.kind || 'other')}" ${colorStyle}>
      <svg class="icon" viewBox="0 0 24 24"><use href="#${escapeAttr(iconRef)}"/></svg>
    </div>
  `;
  const nameHtml = `
    <div class="rc-name-cell">
      <div class="rc-name">${escapeHtml(name)}</div>
      <div class="rc-slug">${escapeHtml(role.slug)}</div>
    </div>
  `;
  const kindChip = { status: kindChipStatus(role.kind), label: role.kind };
  const managerHtml = role.isManager
    ? `<span class="tf-chip ok"><span class="tf-chip-dot"></span>${escapeHtml(STRINGS.managerYes)}</span>`
    : `<span class="rc-muted">${escapeHtml(STRINGS.managerDash)}</span>`;
  const scopeHtml = role.defaultVisibilityScope
    ? `<span class="tf-chip info">${escapeHtml(role.defaultVisibilityScope)}</span>`
    : `<span class="rc-muted">${escapeHtml(STRINGS.managerDash)}</span>`;
  const actionsHtml = state.isAdmin
    ? `<tf-button variant="ghost" size="sm" icon="edit" data-action="edit" data-role-id="${escapeAttr(role.id)}"></tf-button>`
    : '';

  return {
    _id: role.id,
    icon: iconHtml,
    name: nameHtml,
    kind: kindChip,
    manager: managerHtml,
    scope: scopeHtml,
    actions: actionsHtml,
  };
}

function kindChipStatus(kind) {
  switch (kind) {
    case 'sales': return 'accent';
    case 'technical': return 'warn';
    case 'management': return 'info';
    case 'external': return 'ok';
    default: return 'info';
  }
}

function pickTranslation(translations, preferredLocale) {
  if (!Array.isArray(translations) || translations.length === 0) return '';
  const exact = translations.find((p) => Array.isArray(p) && p[0] === preferredLocale);
  if (exact) return exact[1] || '';
  const def = state.locales.find((l) => l.isDefault);
  if (def) {
    const m = translations.find((p) => Array.isArray(p) && p[0] === def.code);
    if (m) return m[1] || '';
  }
  const first = translations[0];
  return Array.isArray(first) ? (first[1] || '') : '';
}

// =============================================================================
// Wiring zdarzen na ekranie
// =============================================================================

function wireScreen(root) {
  byId('rc-new')?.addEventListener('click', () => openEditor('create', null));

  const sb = byId('rc-search');
  if (sb) {
    sb.addEventListener('search', async (e) => {
      const v = e.detail?.value ?? '';
      state.searchQuery = String(v);
      try {
        await loadRoles();
        rerenderKpisAndChips();
        mountTable();
      } catch (err) {
        toast(err.message || STRINGS.loadFailed, 'error');
      }
    });
  }

  root.querySelector('#rc-filter-group')?.addEventListener('click', async (e) => {
    const chip = e.target.closest('[data-filter]');
    if (!chip) return;
    const f = chip.dataset.filter;
    if (f === state.activeFilter) return;
    state.activeFilter = f;
    try {
      await loadRoles();
      rerenderKpisAndChips();
      mountTable();
    } catch (err) {
      toast(err.message || STRINGS.loadFailed, 'error');
    }
  });

  // Edit button bezposrednio w komorce akcji — interceptuje przed row-click.
  root.querySelector('#rc-table-host')?.addEventListener('click', (e) => {
    const editBtn = e.target.closest('[data-action="edit"][data-role-id]');
    if (!editBtn) return;
    e.stopPropagation();
    const id = editBtn.dataset.roleId;
    openRoleDetail(id);
  });
}

async function openRoleDetail(id) {
  try {
    const detail = await fetchRoleDetail(id);
    if (!detail) {
      toast(STRINGS.loadDetailFailed, 'error');
      return;
    }
    openEditor(state.isAdmin ? 'edit' : 'view', detail);
  } catch (err) {
    toast(err.message || STRINGS.loadDetailFailed, 'error');
  }
}

function rerenderKpisAndChips() {
  const kpiHost = byId('rc-kpi-grid');
  if (kpiHost) kpiHost.innerHTML = renderKpis();
  const chipsHost = byId('rc-filter-group');
  if (chipsHost) chipsHost.innerHTML = renderFilterChips();
}

// =============================================================================
// Editor — modal tf-window z formularzem 2-kolumnowym
// =============================================================================

function openEditor(mode, detail) {
  state.editorMode = mode;
  state.editingDetail = detail;
  state.formData = buildFormData(mode, detail);
  state.formErrors = {};
  state.currentEditorLocale = pickDefaultLocale();

  const win = document.createElement('tf-window');
  const title = mode === 'create'
    ? STRINGS.modalCreateTitle
    : (mode === 'view' ? STRINGS.modalViewTitle : STRINGS.modalEditTitle);
  win.setAttribute('title', title);
  win.setAttribute('icon', 'edit');
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '820');
  win.setAttribute('draggable', '');

  const body = document.createElement('div');
  body.slot = 'body';
  body.className = 'rc-editor';
  body.innerHTML = renderEditorBody();
  win.appendChild(body);

  const foot = document.createElement('div');
  foot.slot = 'footer';
  foot.className = 'rc-editor-footer';
  foot.innerHTML = renderEditorFooter();
  win.appendChild(foot);

  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  document.body.append(backdrop, win);

  const cleanup = () => {
    if (win.isConnected) win.close(true);
    if (backdrop.isConnected) backdrop.remove();
    state.editorMode = null;
    state.editingDetail = null;
    state.formData = null;
    state.formErrors = {};
  };

  win.addEventListener('close-request', () => {
    if (backdrop.isConnected) backdrop.remove();
  });

  wireEditor(win, body, foot, cleanup);
}

function buildFormData(mode, detail) {
  const nameMap = {};
  const descMap = {};
  for (const loc of state.locales) {
    nameMap[loc.code] = '';
    descMap[loc.code] = '';
  }
  if (mode === 'create') {
    return {
      id: null,
      slug: '',
      kind: 'sales',
      nameByLocale: nameMap,
      descriptionByLocale: descMap,
      icon: '',
      colorHint: '',
      isManager: false,
      defaultVisibilityScope: 'assigned',
    };
  }
  for (const [code, val] of detail.nameTranslations || []) nameMap[code] = val;
  for (const [code, val] of detail.descriptionTranslations || []) descMap[code] = val;
  return {
    id: detail.id,
    slug: detail.slug,
    kind: detail.kind,
    nameByLocale: nameMap,
    descriptionByLocale: descMap,
    icon: detail.icon || '',
    colorHint: detail.colorHint || '',
    isManager: !!detail.isManager,
    defaultVisibilityScope: detail.defaultVisibilityScope || 'assigned',
  };
}

function renderEditorBody() {
  const readOnly = state.editorMode === 'view';
  const disabledAttr = readOnly ? 'disabled' : '';
  const slugDisabled = state.editorMode !== 'create' || readOnly;
  const fd = state.formData;
  const err = state.formErrors;
  const currentLoc = state.currentEditorLocale;
  const nameValue = fd.nameByLocale[currentLoc] ?? '';
  const descValue = fd.descriptionByLocale[currentLoc] ?? '';

  // Tabki jezykow — value = currentEditorLocale, kazda tabka ma marker
  // wypelnienia jako prefix w label.
  const localeNameTabs = state.locales.map((loc) => {
    const filled = (fd.nameByLocale[loc.code] || '').trim();
    const marker = filled ? '●' : '○';
    return `<tf-tab id="name-${escapeAttr(loc.code)}">${marker} ${escapeHtml(loc.displayName || loc.code)}</tf-tab>`;
  }).join('');

  const localeDescTabs = state.locales.map((loc) => {
    const filled = (fd.descriptionByLocale[loc.code] || '').trim();
    const marker = filled ? '●' : '○';
    return `<tf-tab id="desc-${escapeAttr(loc.code)}">${marker} ${escapeHtml(loc.displayName || loc.code)}</tf-tab>`;
  }).join('');

  const kindOptions = KIND_VALUES.map((k) =>
    `<option value="${escapeAttr(k)}" ${fd.kind === k ? 'selected' : ''}>${escapeHtml(STRINGS.kindLabels[k])}</option>`,
  ).join('');

  const scopeOptionLabels = {
    assigned: STRINGS.scopeAssigned,
    own: STRINGS.scopeOwn,
    section: STRINGS.scopeSection,
    department: STRINGS.scopeDepartment,
    all: STRINGS.scopeAll,
  };
  const scopeOptions = SCOPE_VALUES.map((s) =>
    `<option value="${escapeAttr(s)}" ${fd.defaultVisibilityScope === s ? 'selected' : ''}>${escapeHtml(scopeOptionLabels[s])}</option>`,
  ).join('');

  const iconOptions = [`<option value="">${escapeHtml(STRINGS.iconNone)}</option>`]
    .concat(ALLOWED_ICONS.map((ic) =>
      `<option value="${escapeAttr(ic)}" ${fd.icon === ic ? 'selected' : ''}>${escapeHtml(ic.replace(/^i-/, ''))}</option>`,
    )).join('');

  const errBlock = (key) => err[key]
    ? `<div class="rc-field-error">${escapeHtml(err[key])}</div>`
    : '';

  const colorPreviewStyle = fd.colorHint && COLOR_REGEX.test(fd.colorHint)
    ? (fd.colorHint.startsWith('#') ? `background:${fd.colorHint};` : `background:var(${fd.colorHint});`)
    : 'background:transparent;border:1px dashed var(--border);';

  return `
    <div class="rc-editor-grid">
      <div class="rc-editor-col">

        <div class="rc-field">
          <label class="rc-section-label">${escapeHtml(STRINGS.sectionNamePerLocale)}</label>
          <tf-tabs variant="underline" value="name-${escapeAttr(currentLoc)}" id="rc-name-tabs">
            ${localeNameTabs}
          </tf-tabs>
          <tf-input
            id="rc-input-name"
            label=""
            value="${escapeAttr(nameValue)}"
            placeholder=""
            ${disabledAttr}
            data-field="name"></tf-input>
          ${errBlock('name')}
        </div>

        <div class="rc-field">
          <tf-input
            label="${escapeAttr(STRINGS.labelSlug)}"
            value="${escapeAttr(fd.slug)}"
            placeholder="${escapeAttr(STRINGS.placeholderSlug)}"
            ${slugDisabled ? 'disabled' : ''}
            data-field="slug"></tf-input>
          ${errBlock('slug')}
        </div>

        <div class="rc-field">
          <label class="rc-section-label">${escapeHtml(STRINGS.labelKind)}</label>
          <tf-select value="${escapeAttr(fd.kind)}" ${disabledAttr} data-field="kind">
            ${kindOptions}
          </tf-select>
          ${errBlock('kind')}
        </div>

        <div class="rc-field">
          <label class="rc-section-label">${escapeHtml(STRINGS.sectionDescriptionPerLocale)}</label>
          <tf-tabs variant="underline" value="desc-${escapeAttr(currentLoc)}" id="rc-desc-tabs">
            ${localeDescTabs}
          </tf-tabs>
          <tf-textarea
            id="rc-input-desc"
            label=""
            value="${escapeAttr(descValue)}"
            rows="3"
            ${disabledAttr}
            data-field="description"></tf-textarea>
          ${errBlock('description')}
        </div>

        <div class="rc-field">
          <label class="rc-section-label">${escapeHtml(STRINGS.labelIcon)}</label>
          <tf-select value="${escapeAttr(fd.icon)}" ${disabledAttr} data-field="icon">
            ${iconOptions}
          </tf-select>
          ${errBlock('icon')}
        </div>

        <div class="rc-field">
          <label class="rc-section-label">${escapeHtml(STRINGS.labelColor)}</label>
          <div class="rc-color-row">
            <div class="rc-color-swatch" id="rc-color-preview" style="${colorPreviewStyle}"></div>
            <tf-input
              label=""
              value="${escapeAttr(fd.colorHint)}"
              placeholder="${escapeAttr(STRINGS.placeholderColor)}"
              ${disabledAttr}
              data-field="colorHint"></tf-input>
          </div>
          ${errBlock('colorHint')}
        </div>

      </div>

      <div class="rc-editor-col">

        <div class="rc-field">
          <label class="rc-section-label">${escapeHtml(STRINGS.sectionPlatformTraits)}</label>
          <div class="rc-toggle-row">
            <tf-toggle data-field="isManager" ${fd.isManager ? 'checked' : ''} ${disabledAttr}></tf-toggle>
            <div class="rc-toggle-text">
              <div class="rc-toggle-label">${escapeHtml(STRINGS.labelIsManager)}</div>
              <div class="rc-toggle-hint">${escapeHtml(STRINGS.hintIsManager)}</div>
            </div>
          </div>
        </div>

        <div class="rc-field">
          <label class="rc-section-label">${escapeHtml(STRINGS.labelScope)}</label>
          <tf-select value="${escapeAttr(fd.defaultVisibilityScope)}" ${disabledAttr} data-field="defaultVisibilityScope">
            ${scopeOptions}
          </tf-select>
          <div class="rc-toggle-hint">${escapeHtml(STRINGS.hintScope)}</div>
        </div>

        <div class="rc-warn-banner">
          <svg class="icon" viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/></svg>
          <div>
            <div class="rc-warn-banner-title">${escapeHtml(STRINGS.warnBannerTitle)}</div>
            <div class="rc-warn-banner-text">${escapeHtml(STRINGS.warnBannerText)}</div>
          </div>
        </div>

      </div>
    </div>

    <div class="rc-form-error" data-form-error hidden></div>
  `;
}

function renderEditorFooter() {
  if (state.editorMode === 'view') {
    return `
      <div class="rc-footer-left"></div>
      <div class="rc-footer-right">
        <tf-button variant="ghost" data-action="cancel">${escapeHtml(STRINGS.actionClose)}</tf-button>
      </div>
    `;
  }
  const noticeHtml = state.editorMode === 'edit'
    ? `<div class="rc-footer-notice">${STRINGS.footerNoticeUnknown}</div>`
    : '';
  const deleteBtn = state.editorMode === 'edit'
    ? `<tf-button variant="danger" icon="trash" data-action="deactivate">${escapeHtml(STRINGS.actionDelete)}</tf-button>`
    : '';
  const saveLabel = state.editorMode === 'create' ? STRINGS.actionCreate : STRINGS.actionSave;
  return `
    <div class="rc-footer-left">${noticeHtml}</div>
    <div class="rc-footer-right">
      ${deleteBtn}
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(STRINGS.actionCancel)}</tf-button>
      <tf-button variant="primary" data-action="save">${escapeHtml(saveLabel)}</tf-button>
    </div>
  `;
}

function wireEditor(win, body, foot, cleanup) {
  // Tabki jezyka — zmiana wartosci tf-tabs przelacza widoczna locale w inputach
  // bez przerysowania calego body (nie tracimy focusu, animacja indykatora dziala).
  body.addEventListener('change', (e) => {
    const target = e.target;
    if (target.id === 'rc-name-tabs') {
      const code = String(target.value || '').replace(/^name-/, '');
      if (!code) return;
      syncCurrentInputsToState(body);
      state.currentEditorLocale = code;
      const nameInput = body.querySelector('#rc-input-name');
      if (nameInput) nameInput.value = state.formData.nameByLocale[code] ?? '';
      // Synchronizuj druga tabke (desc) zeby obie pokazywaly ten sam jezyk.
      const descTabs = body.querySelector('#rc-desc-tabs');
      if (descTabs && descTabs.value !== `desc-${code}`) {
        descTabs.setAttribute('value', `desc-${code}`);
      }
      const descInput = body.querySelector('#rc-input-desc');
      if (descInput) descInput.value = state.formData.descriptionByLocale[code] ?? '';
      return;
    }
    if (target.id === 'rc-desc-tabs') {
      const code = String(target.value || '').replace(/^desc-/, '');
      if (!code) return;
      syncCurrentInputsToState(body);
      state.currentEditorLocale = code;
      const descInput = body.querySelector('#rc-input-desc');
      if (descInput) descInput.value = state.formData.descriptionByLocale[code] ?? '';
      const nameTabs = body.querySelector('#rc-name-tabs');
      if (nameTabs && nameTabs.value !== `name-${code}`) {
        nameTabs.setAttribute('value', `name-${code}`);
      }
      const nameInput = body.querySelector('#rc-input-name');
      if (nameInput) nameInput.value = state.formData.nameByLocale[code] ?? '';
      return;
    }

    const field = target.dataset?.field;
    if (!field) return;
    if (field === 'kind') state.formData.kind = target.value;
    else if (field === 'icon') state.formData.icon = target.value || '';
    else if (field === 'defaultVisibilityScope') state.formData.defaultVisibilityScope = target.value;
    else if (field === 'isManager') {
      const checked = e.detail?.checked ?? target.hasAttribute('checked');
      state.formData.isManager = !!checked;
    }
  });

  body.addEventListener('input', (e) => {
    const t = e.target;
    const field = t.dataset?.field;
    if (!field) return;
    const v = e.detail?.value ?? t.value ?? '';
    if (field === 'name') {
      const code = state.currentEditorLocale;
      state.formData.nameByLocale[code] = v;
      updateLocaleTabMarker(body, 'name', code);
    } else if (field === 'description') {
      const code = state.currentEditorLocale;
      state.formData.descriptionByLocale[code] = v;
      updateLocaleTabMarker(body, 'desc', code);
    } else if (field === 'slug') {
      state.formData.slug = v;
    } else if (field === 'colorHint') {
      state.formData.colorHint = v;
      updateColorPreview(body, v);
    }
  });

  // Slug sanitize na change (po blur z tf-input).
  body.addEventListener('change', (e) => {
    const t = e.target;
    if (t.dataset?.field !== 'slug') return;
    if (state.editorMode !== 'create') return;
    const raw = e.detail?.value ?? t.value ?? '';
    const sanitized = String(raw)
      .toLowerCase()
      .trim()
      .replace(/[^a-z0-9_]+/g, '_')
      .replace(/^_+|_+$/g, '');
    if (sanitized !== raw) {
      state.formData.slug = sanitized;
      t.value = sanitized;
    } else {
      state.formData.slug = sanitized;
    }
  });

  foot.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    const action = btn.dataset.action;
    if (action === 'cancel') {
      cleanup();
      return;
    }
    if (action === 'deactivate') {
      syncCurrentInputsToState(body);
      const fd = state.formData;
      const name = pickTranslation(toPairs(fd.nameByLocale), state.currentLocale) || fd.slug;
      if (!confirm(STRINGS.confirmDeactivate.replace('{name}', name))) return;
      try {
        await ApiBinary.action('roleCatalogDeactivateRequest', { id: fd.id });
        toast(STRINGS.deactivateOk, 'success');
        cleanup();
        await loadRoles();
        renderScreen();
      } catch (err) {
        showFormError(body, err.message || err.toString());
      }
      return;
    }
    if (action === 'save') {
      syncCurrentInputsToState(body);
      const errors = validateForm();
      if (Object.keys(errors).length > 0) {
        state.formErrors = errors;
        body.innerHTML = renderEditorBody();
        return;
      }
      try {
        if (state.editorMode === 'create') {
          await submitCreate();
          toast(STRINGS.createOk, 'success');
        } else {
          await submitUpdate();
          toast(STRINGS.saveOk, 'success');
        }
        cleanup();
        await loadRoles();
        renderScreen();
      } catch (err) {
        showFormError(body, err.message || err.toString());
      }
    }
  });

  win.addEventListener('action', (e) => {
    if (e.detail?.action === 'close') {
      cleanup();
    }
  });
}

function updateLocaleTabMarker(body, prefix, code) {
  // Tab.id ma format `${prefix}-${code}`; aktualizujemy label tabki re-flagujac
  // jej innerHTML. tf-tab czyta `innerHTML` przy budowie, ale `_label` jest
  // ustawiane raz; bezposrednio modyfikujemy `<button>.querySelector('.tf-tab-label')`.
  const tabsHost = body.querySelector(`#rc-${prefix}-tabs`);
  if (!tabsHost) return;
  const tab = tabsHost.querySelector(`tf-tab#${prefix}-${code}`);
  if (!tab) return;
  const label = tab.querySelector('.tf-tab-label');
  if (!label) return;
  const map = prefix === 'name' ? state.formData.nameByLocale : state.formData.descriptionByLocale;
  const filled = (map[code] || '').trim();
  const marker = filled ? '●' : '○';
  const loc = state.locales.find((l) => l.code === code);
  const name = loc ? (loc.displayName || loc.code) : code;
  label.textContent = `${marker} ${name}`;
}

function updateColorPreview(body, raw) {
  const sw = body.querySelector('#rc-color-preview');
  if (!sw) return;
  if (raw && COLOR_REGEX.test(raw)) {
    sw.style.background = raw.startsWith('#') ? raw : `var(${raw})`;
    sw.style.border = '';
  } else {
    sw.style.background = 'transparent';
    sw.style.border = '1px dashed var(--border)';
  }
}

function syncCurrentInputsToState(body) {
  // Czytamy aktualne wartosci z tf-* (event input/change mogl nie dolecieci
  // gdy uzytkownik bezposrednio nacisnal Save / przelaczyl tabke).
  const nameInput = body.querySelector('#rc-input-name');
  if (nameInput) {
    state.formData.nameByLocale[state.currentEditorLocale] = nameInput.value || '';
  }
  const descInput = body.querySelector('#rc-input-desc');
  if (descInput) {
    state.formData.descriptionByLocale[state.currentEditorLocale] = descInput.value || '';
  }
  const slugInput = body.querySelector('tf-input[data-field="slug"]');
  if (slugInput) state.formData.slug = slugInput.value || '';
  const colorInput = body.querySelector('tf-input[data-field="colorHint"]');
  if (colorInput) state.formData.colorHint = colorInput.value || '';
  const kindSel = body.querySelector('tf-select[data-field="kind"]');
  if (kindSel) state.formData.kind = kindSel.value || 'sales';
  const iconSel = body.querySelector('tf-select[data-field="icon"]');
  if (iconSel) state.formData.icon = iconSel.value || '';
  const scopeSel = body.querySelector('tf-select[data-field="defaultVisibilityScope"]');
  if (scopeSel) state.formData.defaultVisibilityScope = scopeSel.value || 'assigned';
  const togg = body.querySelector('tf-toggle[data-field="isManager"]');
  if (togg) state.formData.isManager = togg.hasAttribute('checked');
}

function showFormError(body, message) {
  const el = body.querySelector('[data-form-error]');
  if (!el) {
    toast(message, 'error');
    return;
  }
  el.hidden = false;
  el.textContent = message;
}

// =============================================================================
// Walidacja
// =============================================================================

function validateForm() {
  const fd = state.formData;
  const errors = {};

  if (state.editorMode === 'create') {
    if (!fd.slug || !fd.slug.trim()) {
      errors.slug = STRINGS.errSlugRequired;
    } else if (!SLUG_REGEX.test(fd.slug) || fd.slug.length > 50) {
      errors.slug = STRINGS.errSlugFormat;
    }
  }

  if (!fd.kind || !KIND_VALUES.includes(fd.kind)) {
    errors.kind = STRINGS.errKindRequired;
  }

  const missingName = state.locales.find((loc) => !(fd.nameByLocale[loc.code] || '').trim());
  if (missingName) errors.name = STRINGS.errNameRequired;

  const descFilled = state.locales.filter((loc) => (fd.descriptionByLocale[loc.code] || '').trim());
  if (descFilled.length > 0 && descFilled.length < state.locales.length) {
    errors.description = STRINGS.errDescriptionPartial;
  }

  if (fd.colorHint && !COLOR_REGEX.test(fd.colorHint)) {
    errors.colorHint = STRINGS.errColorFormat;
  }
  if (fd.icon && !ALLOWED_ICONS.includes(fd.icon)) {
    errors.icon = STRINGS.errIconUnknown;
  }
  return errors;
}

function toPairs(map) {
  const out = [];
  for (const loc of state.locales) {
    out.push([loc.code, map[loc.code] || '']);
  }
  return out;
}

function toPairsOnlyFilled(map) {
  const out = [];
  for (const loc of state.locales) {
    const v = (map[loc.code] || '').trim();
    if (v) out.push([loc.code, v]);
  }
  return out;
}

// =============================================================================
// Submit create / update
// =============================================================================

async function submitCreate() {
  const fd = state.formData;
  const descPairs = toPairsOnlyFilled(fd.descriptionByLocale);
  const payload = {
    slug: fd.slug.trim(),
    kind: fd.kind,
    nameTranslations: toPairs(fd.nameByLocale),
    descriptionTranslations: descPairs.length === state.locales.length ? descPairs : [],
    icon: fd.icon || null,
    colorHint: fd.colorHint || null,
    isManager: !!fd.isManager,
    defaultVisibilityScope: fd.defaultVisibilityScope,
  };
  await ApiBinary.action('roleCatalogCreateRequest', payload);
}

async function submitUpdate() {
  const fd = state.formData;
  const orig = state.editingDetail;
  const patch = { id: fd.id };

  if (fd.kind !== orig.kind) patch.kind = fd.kind;
  if (fd.isManager !== orig.isManager) patch.isManager = fd.isManager;
  if (fd.defaultVisibilityScope !== orig.defaultVisibilityScope) {
    patch.defaultVisibilityScope = fd.defaultVisibilityScope;
  }

  const origIcon = orig.icon || null;
  const newIcon = fd.icon || null;
  if (newIcon !== origIcon) patch.icon = newIcon;

  const origColor = orig.colorHint || null;
  const newColor = fd.colorHint || null;
  if (newColor !== origColor) patch.colorHint = newColor;

  const newNamePairs = toPairs(fd.nameByLocale);
  const origNameMap = mapFromPairs(orig.nameTranslations);
  if (pairsDifferent(newNamePairs, origNameMap)) {
    patch.nameTranslations = newNamePairs;
  }

  const descFilled = toPairsOnlyFilled(fd.descriptionByLocale);
  const newDescPairs = descFilled.length === state.locales.length ? descFilled : [];
  const origDescMap = mapFromPairs(orig.descriptionTranslations);
  if (pairsDifferent(newDescPairs, origDescMap)) {
    patch.descriptionTranslations = newDescPairs;
  }

  await ApiBinary.action('roleCatalogUpdateRequest', patch);
}

function mapFromPairs(pairs) {
  const out = {};
  if (!Array.isArray(pairs)) return out;
  for (const p of pairs) {
    if (Array.isArray(p) && p.length === 2) out[p[0]] = p[1];
  }
  return out;
}

function pairsDifferent(newPairs, origMap) {
  if (newPairs.length !== Object.keys(origMap).length) return true;
  for (const [code, val] of newPairs) {
    if (origMap[code] !== val) return true;
  }
  return false;
}
