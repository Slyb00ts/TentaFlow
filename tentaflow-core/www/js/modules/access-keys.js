// =============================================================================
// File: modules/access-keys.js
// Purpose: "Dostęp i klucze API" admin screen. Three tabs:
//   - Klucze: API key list (type user/group/general, scope, sync) + create
//     wizard modal (3 steps, deploy-style step indicator) + rotate/revoke.
//   - Macierz dostępu: addon-style permission matrix (rows = subjects, columns
//     = resources model/flow/alias, tri-state allow/deny/inherit), subtabs
//     Per grupa / Per user / Per klucz / Domyślne. Per klucz also carries the
//     "Wzory wiadomości" columns (schema registry REST): one per TentaBus
//     instance × organisation × read/write, because those grants exist only
//     for general keys and read never implies write or the other way round.
//   - Wg zasobu: same data, transposed (pick a resource → who can access it).
// All writes go through the binary protocol; default-DENY is enforced server
// side, this screen only edits resource_permissions / api-key scopes.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { modelBundleScopeResources } from '/js/modules/catalog/camera-cv-bundles.js';
import {
  BUS_SCHEMA_REGISTRY,
  BUS_SCHEMA_ACTIONS,
  busSchemaScopeId,
  busSchemaScopeNames,
  scopeKey,
} from '/js/modules/access-keys-scopes.js';
import '/js/components/tf-button.js';
import '/js/components/tf-table.js';
import '/js/components/tf-window.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-checkbox.js';
import '/js/components/tf-chip.js';

let host = null;
let activeTab = 'keys';
let matrixSubtab = 'group';
let resourceView = 'model';
let highlightKeyUid = null;
let keys = [];
let groups = [];
let users = [];
let resources = { model: [], flow: [], alias: [], model_bundle: [], ml_studio_export: [] };
// TentaBus instances ({ addonId, title }) and organisations ({ orgId, name })
// a schema-registry grant can be scoped to.
let busInstances = [];
let orgs = [];

const TENTABUS_PACKAGE_ID = 'tentabus';

const NEXT_MODE = { allow: 'deny', deny: 'inherit', inherit: 'allow' };

// `vars` feeds I18n's `{name}` and `{count|a|b|c}` forms; the Polish fallback
// only ever shows when a bundle lacks the key, so it substitutes plain `{name}`.
function t(key, fallback, vars = null) {
  const v = I18n.t(key, vars);
  if (v !== key || fallback == null) return v;
  return vars ? fallback.replace(/\{(\w+)\}/g, (m, k) => (k in vars ? String(vars[k]) : m)) : fallback;
}

const AccessKeysScreen = {
  render() {
    return shellHtml();
  },
  async mount() {
    host = document.getElementById('main');
    activeTab = 'keys';
    bindTabs();
    await loadKeys();
    renderActiveTab();
  },
  unmount() {
    host = null;
    keys = [];
    groups = [];
    users = [];
    resources = { model: [], flow: [], alias: [], model_bundle: [], ml_studio_export: [] };
    busInstances = [];
    orgs = [];
  },
};
export default AccessKeysScreen;

// ---------------------------------------------------------------------------
// Shell + tabs
// ---------------------------------------------------------------------------
function shellHtml() {
  return `
    <div class="tf-screen access-keys-screen">
      <div class="page-header">
        <div>
          <h1><svg class="icon icon-lg"><use href="#i-key"/></svg>${escapeHtml(t('access_keys.title', 'Dostęp i klucze API'))}</h1>
          <div class="sub">${escapeHtml(t('access_keys.subtitle', 'Zewnętrzne /v1 — default-DENY, klucze i uprawnienia synchronizowane przez mesh'))}</div>
        </div>
      </div>
      <div class="tf-tabs-bar" id="ak-tabs">
        <button class="tf-tab-btn" data-tab="keys"><svg class="icon icon-sm"><use href="#i-key"/></svg>${escapeHtml(t('access_keys.tab_keys', 'Klucze'))}</button>
        <button class="tf-tab-btn" data-tab="matrix"><svg class="icon icon-sm"><use href="#i-shield"/></svg>${escapeHtml(t('access_keys.tab_matrix', 'Macierz dostępu'))}</button>
        <button class="tf-tab-btn" data-tab="byresource"><svg class="icon icon-sm"><use href="#i-model"/></svg>${escapeHtml(t('access_keys.tab_by_resource', 'Wg zasobu'))}</button>
      </div>
      <div id="ak-body" class="access-keys-body"></div>
    </div>`;
}

function bindTabs() {
  host.querySelector('#ak-tabs')?.addEventListener('click', (e) => {
    const btn = e.target.closest('.tf-tab-btn');
    if (!btn) return;
    activeTab = btn.dataset.tab;
    renderActiveTab();
  });
}

// Every render may land after an await: by then the admin can have left the
// screen (`unmount` clears `host`) or another screen can own `#main`.
function screenBody() {
  const body = host?.querySelector('#ak-body');
  return body?.isConnected ? body : null;
}

function renderActiveTab() {
  const body = screenBody();
  if (!body) return;
  host.querySelectorAll('#ak-tabs .tf-tab-btn').forEach((b) => {
    b.classList.toggle('active', b.dataset.tab === activeTab);
  });
  if (activeTab === 'keys') renderKeysTab(body);
  else if (activeTab === 'matrix') renderMatrixTab(body);
  else renderByResourceTab(body);
}

// ---------------------------------------------------------------------------
// Data loaders
// ---------------------------------------------------------------------------
async function loadKeys() {
  keys = await ApiBinary.list('apiKeyListRequest').catch(() => []);
  if (!Array.isArray(keys)) keys = [];
}

async function loadSubjectsAndResources() {
  const [g, u, models, flows, aliases, visionModels, mlProjects, apps, orgList] = await Promise.all([
    ApiBinary.action('iamListGroupsRequest').then((r) => r?.groups ?? []).catch(() => []),
    ApiBinary.action('iamListUsersRequest').then((r) => r?.users ?? []).catch(() => []),
    ApiBinary.list('modelListRequest').catch(() => []),
    ApiBinary.list('flowListRequest').catch(() => []),
    ApiBinary.list('modelAliasListRequest', { arrayKey: 'aliases' }).catch(() => []),
    ApiBinary.one('mlStudioVisionModelsListRequest', {}).then((r) => r?.models ?? []).catch(() => []),
    ApiBinary.one('mlStudioProjectsListRequest').then((r) => r?.projects ?? []).catch(() => []),
    ApiBinary.list('appsListRequest', { arrayKey: 'apps' }).catch(() => []),
    ApiBinary.action('iamListOrganizationsRequest').then((r) => r?.orgs ?? []).catch(() => []),
  ]);
  busInstances = (Array.isArray(apps) ? apps : [])
    .filter((a) => (a.packageId ?? a.package_id) === TENTABUS_PACKAGE_ID)
    .map((a) => {
      const addonId = String(a.addonId ?? a.addon_id ?? '');
      const title = (a.titleKey && I18n.t(a.titleKey) !== a.titleKey && I18n.t(a.titleKey)) || a.title || '';
      return { addonId, title: String(title) };
    })
    .filter((a) => a.addonId && a.title);
  orgs = (Array.isArray(orgList) ? orgList : [])
    .map((o) => ({ orgId: String(o.orgId ?? o.org_id ?? ''), name: String(o.name ?? '') }))
    .filter((o) => o.orgId && o.name);
  groups = Array.isArray(g) ? g : [];
  users = Array.isArray(u) ? u : [];
  if (keys.length === 0) await loadKeys();
  // model_bundle = fixed camera-CV bundles + published vision-registry models
  // — the same refs the /models endpoints can serve.
  const bundleStatic = modelBundleScopeResources(t);
  const bundleRegistry = (Array.isArray(visionModels) ? visionModels : [])
    .map((m) => String(m.modelName ?? m.model_name ?? ''))
    .filter((name) => name && !bundleStatic.some((b) => b.id === name))
    .map((name) => ({ id: name, name }));
  resources = {
    model: (models || []).map((m) => ({ id: String(m.id || m.name || ''), name: String(m.name || m.id || '') })),
    // Only flows published as a model are callable via /v1, and the resource
    // id must be the published model name (what the catalog + authorizer key
    // on) — NOT the flow UUID. A grant on the UUID would never match a request
    // that comes in under the published name.
    flow: (flows || [])
      .filter((f) => f.publishedModelName)
      .map((f) => ({ id: String(f.publishedModelName), name: `${f.name || f.publishedModelName} (${f.publishedModelName})` })),
    alias: (aliases || []).map((a) => ({ id: String(a.alias || a.id || ''), name: String(a.alias || a.id || '') })),
    model_bundle: [...bundleStatic, ...bundleRegistry],
    // ml_studio_export scopes a general key to ONE project's export archive
    // download — the resource id is the ML Studio project id (v4 UUID).
    ml_studio_export: (Array.isArray(mlProjects) ? mlProjects : [])
      .map((p) => ({ id: String(p.projectId ?? p.project_id ?? ''), name: String(p.name || p.projectId || p.project_id || '') }))
      .filter((p) => p.id),
  };
}

function allResourceColumns() {
  return [
    ...resources.model.map((r) => ({ ...r, type: 'model' })),
    ...resources.flow.map((r) => ({ ...r, type: 'flow' })),
    ...resources.alias.map((r) => ({ ...r, type: 'alias' })),
    ...resources.model_bundle.map((r) => ({ ...r, type: 'model_bundle' })),
    ...resources.ml_studio_export.map((r) => ({ ...r, type: 'ml_studio_export' })),
  ];
}

// ---------------------------------------------------------------------------
// Tab: Klucze
// ---------------------------------------------------------------------------
function typeChip(keyType) {
  const map = {
    user: ['user', t('access_keys.chip_user', 'User')],
    group: ['users', t('access_keys.chip_group', 'Grupa')],
    general: ['key', t('access_keys.chip_general', 'Ogólny')],
  };
  const [icon, label] = map[keyType] || ['key', keyType];
  return `<span class="tf-chip ak-type-${escapeAttr(keyType)}"><svg class="icon icon-xs"><use href="#i-${icon}"/></svg>${escapeHtml(label)}</span>`;
}

function renderKeysTab(body) {
  if (!body?.isConnected) return;
  const rows = keys.length === 0
    ? `<tr><td colspan="6"><div class="empty-big" style="padding:24px;">${escapeHtml(t('access_keys.empty', 'Brak kluczy API'))}</div></td></tr>`
    : keys.map((k) => {
        const subject = k.subjectLabel || k.subjectId || (k.keyType === 'general' ? '—' : '');
        const scope = k.keyType === 'general'
          ? escapeHtml(t('access_keys.resources_count', '{count} zasobów', { count: Number(k.scopeCount || 0) }))
          : escapeHtml(t('access_keys.inherits', 'dziedziczy'));
        const status = k.isActive === false
          ? `<span class="tf-chip danger">${escapeHtml(t('access_keys.revoked', 'zrewokowany'))}</span>`
          : `<span class="tf-chip success">${escapeHtml(t('access_keys.active', 'aktywny'))}</span>`;
        const last = k.lastUsedAtEpoch ? new Date(Number(k.lastUsedAtEpoch) * 1000).toLocaleString() : '—';
        const scopeBtn = k.keyType === 'general'
          ? `<tf-button size="sm" variant="ghost" icon="shield" data-scope="${escapeAttr(k.keyId)}">${escapeHtml(t('access_keys.scope', 'Zakres'))}</tf-button>`
          : '';
        return `<tr>
          <td><div class="strong">${escapeHtml(k.name)}</div><div class="cell-sub mono">${escapeHtml(k.keyId)}</div></td>
          <td>${typeChip(k.keyType)}</td>
          <td>${escapeHtml(subject)}<div class="cell-sub">${scope}</div></td>
          <td>${status}</td>
          <td>${escapeHtml(last)}</td>
          <td style="text-align:right;white-space:nowrap">
            ${scopeBtn}
            <tf-button size="sm" variant="ghost" icon="refresh" data-rotate="${escapeAttr(k.keyId)}">${escapeHtml(t('access_keys.rotate', 'Rotuj'))}</tf-button>
            <tf-button size="sm" variant="danger" icon="trash" data-revoke="${escapeAttr(k.keyId)}" aria-label="${escapeAttr(t('access_keys.revoke_label', 'Zrewokuj klucz {name}', { name: k.name }))}" title="${escapeAttr(t('access_keys.revoke_label', 'Zrewokuj klucz {name}', { name: k.name }))}"></tf-button>
          </td>
        </tr>`;
      }).join('');

  body.innerHTML = `
    <div class="tf-section-card">
      <div class="ak-toolbar">
        <div class="ak-info">${escapeHtml(t('access_keys.deny_hint', 'Default-DENY: klucz widzi i wywołuje tylko jawnie nadane zasoby. Synchronizowane na wszystkie nody.'))}</div>
        <tf-button variant="ghost" size="sm" icon="refresh" id="ak-refresh">${escapeHtml(t('access_keys.refresh', 'Odśwież'))}</tf-button>
        <tf-button variant="primary" size="sm" icon="plus" id="ak-create">${escapeHtml(t('access_keys.new_key', 'Nowy klucz'))}</tf-button>
      </div>
      <table class="tf-table">
        <thead><tr>
          <th>${escapeHtml(t('access_keys.col_name', 'Nazwa / prefiks'))}</th>
          <th>${escapeHtml(t('access_keys.col_type', 'Typ'))}</th>
          <th>${escapeHtml(t('access_keys.col_subject', 'Podmiot / zakres'))}</th>
          <th>${escapeHtml(t('access_keys.col_status', 'Status'))}</th>
          <th>${escapeHtml(t('access_keys.col_last_used', 'Ostatnie użycie'))}</th>
          <th></th>
        </tr></thead>
        <tbody>${rows}</tbody>
      </table>
    </div>`;

  body.querySelector('#ak-refresh')?.addEventListener('click', async () => { await loadKeys(); renderKeysTab(body); });
  body.querySelector('#ak-create')?.addEventListener('click', () => openCreateWizard(body));
  body.querySelectorAll('[data-revoke]').forEach((b) => b.addEventListener('click', () => revokeKey(b.getAttribute('data-revoke'), body)));
  body.querySelectorAll('[data-rotate]').forEach((b) => b.addEventListener('click', () => rotateKey(b.getAttribute('data-rotate'))));
  body.querySelectorAll('[data-scope]').forEach((b) => b.addEventListener('click', () => openScopeEditor(b.getAttribute('data-scope'))));
}

async function revokeKey(keyId, body) {
  if (!window.confirm(t('access_keys.revoke_confirm', 'Zrewokować ten klucz? Przestanie działać natychmiast.'))) return;
  try {
    await ApiBinary.action('apiKeyRevokeRequest', { keyId });
    toast(t('access_keys.revoked_ok', 'Klucz zrewokowany'), 'success');
    await loadKeys();
    renderKeysTab(body);
  } catch (e) { toast(e.message || 'error', 'error'); }
}

async function rotateKey(keyUid) {
  if (!window.confirm(t('access_keys.rotate_confirm', 'Wygenerować nowy token? Stary przestanie działać.'))) return;
  try {
    const resp = await ApiBinary.action('apiKeyRotateRequest', { keyUid });
    showToken(resp.token);
  } catch (e) { toast(e.message || 'error', 'error'); }
}

// ---------------------------------------------------------------------------
// Create wizard (modal, deploy-style step indicator)
// ---------------------------------------------------------------------------
function stepIndicator(step) {
  let h = '<div class="wizard-step-indicator">';
  for (let i = 1; i <= 3; i++) {
    const cls = i === step ? 'active' : (i < step ? 'done' : '');
    h += `<div class="wizard-step-dot ${cls}"><span>${i}</span></div>`;
    if (i < 3) h += '<div class="wizard-step-line"></div>';
  }
  return h + '</div>';
}

function openCreateWizard(body) {
  // `busScopes`: schema-registry grants chosen in step 2, one entry per
  // (instance, organisation, action) — read and write are separate grants.
  const state = { step: 1, keyType: 'user', name: '', subjectId: '', scope: new Set(), busScopes: [] };
  const selectedText = () => t('access_keys.selected_count', '{count} zaznaczone', { count: state.scope.size + state.busScopes.length });
  let busSection = null;
  const win = document.createElement('tf-window');
  win.setAttribute('title', t('access_keys.new_key_title', 'Nowy klucz API'));
  win.setAttribute('width', '720');
  document.body.appendChild(win);

  const close = () => win.remove();

  const renderStep = async () => {
    if (state.step === 1) {
      win.innerHTML = `
        <div class="ak-wizard">
          ${stepIndicator(1)}
          <h4 class="wizard-step-title">${escapeHtml(t('access_keys.wiz_pick_type', 'Wybierz typ klucza'))}</h4>
          <div class="ak-type-grid">
            ${typeCard('user', 'user', t('access_keys.type_user', 'Klucz użytkownika'), t('access_keys.type_user_desc', 'Dziedziczy uprawnienia usera i jego grup.'), state.keyType)}
            ${typeCard('group', 'users', t('access_keys.type_group', 'Klucz grupy'), t('access_keys.type_group_desc', 'Dziedziczy uprawnienia grupy.'), state.keyType)}
            ${typeCard('general', 'key', t('access_keys.type_general', 'Klucz ogólny'), t('access_keys.type_general_desc', 'Własna jawna allowlista (default-DENY).'), state.keyType)}
          </div>
          <div class="ak-form-row">
            <label>${escapeHtml(t('access_keys.name_label', 'Nazwa klucza'))}</label>
            <tf-input id="ak-name" value="${escapeAttr(state.name)}" placeholder="${escapeAttr(t('access_keys.name_placeholder', 'np. CI Pipeline'))}"></tf-input>
          </div>
        </div>
        <div slot="footer" class="ak-wizard-footer">
          <tf-button variant="ghost" id="ak-cancel">${escapeHtml(t('common.cancel', 'Anuluj'))}</tf-button>
          <span style="flex:1"></span>
          <tf-button variant="primary" id="ak-next">${escapeHtml(t('common.next', 'Dalej'))}</tf-button>
        </div>`;
      win.querySelectorAll('.ak-type-card').forEach((c) => c.addEventListener('click', () => {
        state.keyType = c.dataset.type;
        win.querySelectorAll('.ak-type-card').forEach((x) => x.classList.toggle('active', x === c));
      }));
      win.querySelector('#ak-cancel').addEventListener('click', close);
      win.querySelector('#ak-next').addEventListener('click', async () => {
        state.name = win.querySelector('#ak-name')?.value?.trim() || '';
        if (!state.name) { toast(t('access_keys.name_required', 'Podaj nazwę'), 'error'); return; }
        state.step = 2; await renderStep();
      });
    } else if (state.step === 2) {
      if (state.keyType === 'general') {
        await loadSubjectsAndResources();
        const cols = allResourceColumns();
        const rowsHtml = cols.map((r) => `
          <label class="ak-pick-row ${state.scope.has(`${r.type}:${r.id}`) ? 'checked' : ''}" data-key="${escapeAttr(`${r.type}:${r.id}`)}">
            <tf-checkbox ${state.scope.has(`${r.type}:${r.id}`) ? 'checked' : ''}></tf-checkbox>
            <span class="ak-pick-name">${escapeHtml(r.name)}</span>
            <span class="ak-pick-meta">${escapeHtml(r.type)}</span>
          </label>`).join('') || `<div class="empty-big">${escapeHtml(t('access_keys.no_resources', 'Brak zasobów'))}</div>`;
        win.innerHTML = `
          <div class="ak-wizard">
            ${stepIndicator(2)}
            <h4 class="wizard-step-title">${escapeHtml(t('access_keys.wiz_pick_scope', 'Zaznacz dostępne zasoby'))}</h4>
            <div class="ak-deny-note">${escapeHtml(t('access_keys.deny_note', 'Default-DENY: bez zaznaczenia każde /v1 zwróci 403.'))}</div>
            <div class="ak-pick-list">${rowsHtml}</div>
            ${busSchemaSectionHtml()}
          </div>
          <div slot="footer" class="ak-wizard-footer">
            <tf-button variant="ghost" id="ak-back">${escapeHtml(t('common.back', 'Wstecz'))}</tf-button>
            <span style="flex:1"></span>
            <span class="ak-pick-count" id="ak-count">${escapeHtml(selectedText())}</span>
            <tf-button variant="primary" id="ak-create-btn">${escapeHtml(t('access_keys.create_btn', 'Utwórz'))}</tf-button>
          </div>`;
        const refreshCount = () => {
          const count = win.querySelector('#ak-count');
          if (count) count.textContent = selectedText();
        };
        win.querySelectorAll('.ak-pick-row').forEach((row) => row.addEventListener('click', (e) => {
          e.preventDefault();
          const key = row.dataset.key;
          if (state.scope.has(key)) state.scope.delete(key); else state.scope.add(key);
          row.classList.toggle('checked', state.scope.has(key));
          const cb = row.querySelector('tf-checkbox');
          if (cb) cb.toggleAttribute('checked', state.scope.has(key));
          refreshCount();
        }));
        busSection = bindBusSchemaSection(win, state.busScopes, refreshCount);
      } else {
        await loadSubjectsAndResources();
        const opts = (state.keyType === 'user' ? users : groups)
          .map((s) => `<option value="${escapeAttr(s.id)}">${escapeHtml(s.username || s.name || s.id)}</option>`).join('');
        win.innerHTML = `
          <div class="ak-wizard">
            ${stepIndicator(2)}
            <h4 class="wizard-step-title">${escapeHtml(state.keyType === 'user' ? t('access_keys.wiz_pick_user', 'Wybierz użytkownika') : t('access_keys.wiz_pick_group', 'Wybierz grupę'))}</h4>
            <div class="ak-deny-note info">${escapeHtml(t('access_keys.inherit_note', 'Klucz ściśle dziedziczy efektywne uprawnienia podmiotu.'))}</div>
            <div class="ak-deny-note info" id="ak-bus-user-note">${escapeHtml(t('access_keys.bus_schema_user_key_note', 'Wzorów wiadomości nie da się dodać do tego klucza. Do nich potrzebny jest klucz ogólny.'))}</div>
            <div class="ak-form-row">
              <label>${escapeHtml(state.keyType === 'user' ? t('access_keys.col_user', 'Użytkownik') : t('access_keys.col_group', 'Grupa'))}</label>
              <tf-select id="ak-subject"><option value="">—</option>${opts}</tf-select>
            </div>
          </div>
          <div slot="footer" class="ak-wizard-footer">
            <tf-button variant="ghost" id="ak-back">${escapeHtml(t('common.back', 'Wstecz'))}</tf-button>
            <span style="flex:1"></span>
            <tf-button variant="primary" id="ak-create-btn">${escapeHtml(t('access_keys.create_btn', 'Utwórz'))}</tf-button>
          </div>`;
      }
      win.querySelector('#ak-back').addEventListener('click', async () => { state.step = 1; await renderStep(); });
      win.querySelector('#ak-create-btn').addEventListener('click', async () => {
        if (state.keyType !== 'general') {
          state.subjectId = win.querySelector('#ak-subject')?.value || '';
          if (!state.subjectId) { toast(t('access_keys.subject_required', 'Wybierz podmiot'), 'error'); return; }
        } else if (busSection) {
          // A complete pick the admin forgot to add is taken as meant; a
          // half-made one stops the creation instead of silently vanishing.
          const pending = busSection.commitPending();
          if (!pending.ok) { toast(pending.message, 'error'); return; }
        }
        await submitCreate(state, win, body, renderStep);
      });
    }
  };

  const submitCreate = async (s, w, body, renderStepFn) => {
    const scopeResources = [...s.scope].map((k) => {
      const [resourceType, ...rest] = k.split(':');
      return { resourceType, resourceId: rest.join(':') };
    });
    if (s.keyType === 'general') {
      s.busScopes.forEach((b) => scopeResources.push({
        resourceType: BUS_SCHEMA_REGISTRY,
        resourceId: busSchemaScopeId(b.instanceId, b.orgId),
        action: b.action,
      }));
    }
    try {
      const resp = await ApiBinary.action('apiKeyCreateRequest', {
        name: s.name,
        keyType: s.keyType,
        subjectId: s.keyType === 'general' ? null : s.subjectId,
        scopeResources,
      });
      s.step = 3;
      w.innerHTML = `
        <div class="ak-wizard">
          ${stepIndicator(3)}
          <h4 class="wizard-step-title">${escapeHtml(t('access_keys.wiz_token', 'Skopiuj token teraz'))}</h4>
          <div class="ak-token-box">
            <code class="ak-token" id="ak-token">${escapeHtml(resp.token)}</code>
            <tf-button size="sm" variant="ghost" icon="copy" id="ak-copy">${escapeHtml(t('access_keys.copy', 'Kopiuj'))}</tf-button>
          </div>
          <div class="ak-deny-note warn">${escapeHtml(t('access_keys.token_once', 'Token pokazujemy raz. W bazie trzymany jest tylko HMAC.'))}</div>
        </div>
        <div slot="footer" class="ak-wizard-footer">
          <span style="flex:1"></span>
          <tf-button variant="primary" id="ak-done">${escapeHtml(t('access_keys.done', 'Zakończ'))}</tf-button>
        </div>`;
      w.querySelector('#ak-copy').addEventListener('click', () => {
        navigator.clipboard?.writeText(resp.token);
        toast(t('access_keys.copied', 'Skopiowano'), 'success');
      });
      w.querySelector('#ak-done').addEventListener('click', async () => { w.remove(); await loadKeys(); renderKeysTab(body); });
    } catch (e) { toast(e.message || 'error', 'error'); }
  };

  renderStep();
}

// ---------------------------------------------------------------------------
// "Wzory wiadomości" — schema registry REST grants (general keys only)
// ---------------------------------------------------------------------------
function busSchemaUnknownNames() {
  return {
    instance: t('access_keys.bus_schema_unknown_instance', 'Usunięty TentaBus'),
    org: t('access_keys.bus_schema_unknown_org', 'Usunięta organizacja'),
  };
}

function busSchemaActionLabel(action) {
  return action === 'write'
    ? t('access_keys.bus_schema_write', 'Zapisywanie')
    : t('access_keys.bus_schema_read', 'Czytanie');
}

function busSchemaNames(scopeIds) {
  return busSchemaScopeNames(scopeIds, busInstances, orgs, busSchemaUnknownNames());
}

function busSchemaScopeLabel(scopeId) {
  const names = busSchemaNames([scopeId]).get(scopeId);
  return `${names.instance} · ${names.org}`;
}

function busSchemaChipsHtml(busScopes) {
  if (busScopes.length === 0) {
    return `<div class="ak-bus-empty">${escapeHtml(t('access_keys.bus_schema_empty', 'Ten klucz nie ma jeszcze dostępu do wzorów wiadomości.'))}</div>`;
  }
  return busScopes.map((b, i) => {
    const label = `${busSchemaScopeLabel(busSchemaScopeId(b.instanceId, b.orgId))} · ${busSchemaActionLabel(b.action)}`;
    return `<tf-chip status="info" variant="outline" removable data-bus-index="${i}" label="${escapeAttr(label)}"></tf-chip>`;
  }).join('');
}

function busSchemaSectionHtml() {
  const title = `<h5 class="ak-section-title">${escapeHtml(t('access_keys.bus_schema_title', 'Wzory wiadomości'))}</h5>`;
  const hint = `<div class="ak-deny-note info">
    <div>${escapeHtml(t('access_keys.bus_schema_hint', 'Pozwól innemu programowi czytać albo dodawać wzory wiadomości w TentaBusie.'))}</div>
    <div>${escapeHtml(t('access_keys.bus_schema_records_note', 'Do wysyłania wiadomości na topiki potrzebny jest osobny klucz użytkownika.'))}</div>
  </div>`;
  if (busInstances.length === 0 || orgs.length === 0) {
    const missing = busInstances.length === 0
      ? t('access_keys.bus_schema_no_instances', 'Nie ma jeszcze żadnego TentaBusa.')
      : t('access_keys.bus_schema_no_orgs', 'Nie ma jeszcze żadnej organizacji.');
    return `<div class="ak-bus-section">${title}<div class="ak-bus-empty">${escapeHtml(missing)}</div></div>`;
  }
  const instanceOpts = busInstances
    .map((i) => `<option value="${escapeAttr(i.addonId)}">${escapeHtml(i.title)}</option>`).join('');
  const orgOpts = orgs
    .map((o) => `<option value="${escapeAttr(o.orgId)}">${escapeHtml(o.name)}</option>`).join('');
  const action = (id, labelKey, labelFallback, descKey, descFallback) => `
    <div class="ak-bus-action">
      <tf-checkbox id="${id}" label="${escapeAttr(t(labelKey, labelFallback))}"></tf-checkbox>
      <div class="ak-bus-action-desc">${escapeHtml(t(descKey, descFallback))}</div>
    </div>`;
  // The chosen grants sit above the picker, so a new chip appears where the
  // admin is already looking instead of below the fold.
  return `<div class="ak-bus-section">
    ${title}
    ${hint}
    <div class="ak-bus-chips" id="ak-bus-chips">${busSchemaChipsHtml([])}</div>
    <div class="tf-toolbar ak-bus-pickers">
      <tf-select id="ak-bus-instance" aria-label="${escapeAttr(t('access_keys.bus_schema_instance', 'TentaBus'))}">
        <option value="">${escapeHtml(t('access_keys.bus_schema_pick_instance', 'Wybierz TentaBus'))}</option>${instanceOpts}
      </tf-select>
      <tf-select id="ak-bus-org" aria-label="${escapeAttr(t('access_keys.bus_schema_org', 'Organizacja'))}">
        <option value="">${escapeHtml(t('access_keys.bus_schema_pick_org', 'Wybierz organizację'))}</option>${orgOpts}
      </tf-select>
    </div>
    <div class="ak-bus-actions" role="group" aria-label="${escapeAttr(t('access_keys.bus_schema_actions', 'Co program może robić'))}">
      ${action('ak-bus-read', 'access_keys.bus_schema_read', 'Czytanie', 'access_keys.bus_schema_read_desc', 'Program widzi wzory.')}
      ${action('ak-bus-write', 'access_keys.bus_schema_write', 'Zapisywanie', 'access_keys.bus_schema_write_desc', 'Program może dodawać nowe wzory i wersje, ale bez Czytania ich nie zobaczy.')}
    </div>
    <tf-button size="sm" variant="secondary" icon="plus" id="ak-bus-add">${escapeHtml(t('access_keys.bus_schema_add', 'Dodaj'))}</tf-button>
  </div>`;
}

/**
 * Wires the section rendered by `busSchemaSectionHtml`. `busScopes` is the
 * wizard's own array, mutated in place so going Back and Forward keeps it.
 * Returns `{ commitPending }`: on Create, a complete pick still sitting in the
 * pickers is added, a half-made one is refused with the reason.
 */
function bindBusSchemaSection(root, busScopes, onChange) {
  const chips = root.querySelector('#ak-bus-chips');
  if (!chips) return { commitPending: () => ({ ok: true }) };
  const instanceSel = root.querySelector('#ak-bus-instance');
  const orgSel = root.querySelector('#ak-bus-org');
  const readBox = root.querySelector('#ak-bus-read');
  const writeBox = root.querySelector('#ak-bus-write');
  const redraw = () => {
    chips.innerHTML = busSchemaChipsHtml(busScopes);
    chips.querySelectorAll('tf-chip[data-bus-index]').forEach((chip) => chip.addEventListener('remove', () => {
      busScopes.splice(Number(chip.dataset.busIndex), 1);
      redraw();
      onChange();
    }));
  };
  const pick = () => ({
    instanceId: instanceSel?.value || '',
    orgId: orgSel?.value || '',
    actions: BUS_SCHEMA_ACTIONS.filter((a) => (a === 'read' ? readBox : writeBox)?.checked),
  });
  // `null` when the pick is complete, otherwise the message saying what is missing.
  const incomplete = ({ instanceId, orgId, actions }) => {
    if (!instanceId || !orgId) return t('access_keys.bus_schema_pick_both', 'Wybierz TentaBus i organizację.');
    if (actions.length === 0) return t('access_keys.bus_schema_pick_action', 'Zaznacz czytanie, zapisywanie albo oba.');
    return null;
  };
  const add = ({ instanceId, orgId, actions }) => {
    actions.forEach((action) => {
      if (!busScopes.some((b) => b.instanceId === instanceId && b.orgId === orgId && b.action === action)) {
        busScopes.push({ instanceId, orgId, action });
      }
    });
    if (instanceSel) instanceSel.value = '';
    if (orgSel) orgSel.value = '';
    readBox?.removeAttribute('checked');
    writeBox?.removeAttribute('checked');
    redraw();
    onChange();
    chips.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
  };
  root.querySelector('#ak-bus-add')?.addEventListener('click', () => {
    const current = pick();
    const problem = incomplete(current);
    if (problem) { toast(problem, 'error'); return; }
    add(current);
  });
  redraw();
  return {
    commitPending() {
      const current = pick();
      const untouched = !current.instanceId && !current.orgId && current.actions.length === 0;
      if (untouched) return { ok: true };
      const problem = incomplete(current);
      if (problem) {
        return { ok: false, message: t('access_keys.bus_schema_pending_incomplete', 'Wybór wzorów wiadomości nie jest skończony: {reason} Albo wyczyść ten wybór.', { reason: problem }) };
      }
      add(current);
      return { ok: true };
    },
  };
}

/**
 * Matrix columns for the Per klucz subtab: every known instance ×
 * organisation, plus every scope a key already holds for an instance or
 * organisation no longer listed (so it stays visible and revocable). Each
 * scope contributes a read and a write column next to each other; the header
 * names the scope once above the pair.
 */
function busSchemaColumns(levelMaps) {
  const ids = [];
  const addId = (id) => { if (!ids.includes(id)) ids.push(id); };
  busInstances.forEach((i) => orgs.forEach((o) => addId(busSchemaScopeId(i.addonId, o.orgId))));
  levelMaps.forEach((lv) => Object.values(lv.__bus || {}).forEach(({ id }) => addId(id)));
  const names = busSchemaNames(ids);
  return ids.flatMap((id) => {
    const scope = `${names.get(id).instance} · ${names.get(id).org}`;
    return BUS_SCHEMA_ACTIONS.map((action) => ({
      type: BUS_SCHEMA_REGISTRY,
      id,
      action,
      scope,
      name: busSchemaActionLabel(action),
      title: `${scope} · ${busSchemaActionLabel(action)}`,
    }));
  });
}

function typeCard(type, icon, title, desc, active) {
  return `<div class="ak-type-card ${active === type ? 'active' : ''}" data-type="${escapeAttr(type)}">
    <div class="ak-type-ico ak-type-${escapeAttr(type)}"><svg class="icon icon-lg"><use href="#i-${icon}"/></svg></div>
    <div class="ak-type-name">${escapeHtml(title)}</div>
    <div class="ak-type-desc">${escapeHtml(desc)}</div>
  </div>`;
}

function showToken(token) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', t('access_keys.token_title', 'Nowy token'));
  win.setAttribute('width', '640');
  win.innerHTML = `
    <div class="ak-wizard">
      <div class="ak-token-box"><code class="ak-token">${escapeHtml(token)}</code>
        <tf-button size="sm" variant="ghost" icon="copy" id="ak-copy2">${escapeHtml(t('access_keys.copy', 'Kopiuj'))}</tf-button></div>
      <div class="ak-deny-note warn">${escapeHtml(t('access_keys.token_once', 'Token pokazujemy raz. W bazie trzymany jest tylko HMAC.'))}</div>
    </div>
    <div slot="footer" class="ak-wizard-footer"><span style="flex:1"></span>
      <tf-button variant="primary" id="ak-done2">${escapeHtml(t('access_keys.done', 'Zakończ'))}</tf-button></div>`;
  document.body.appendChild(win);
  win.querySelector('#ak-copy2').addEventListener('click', () => { navigator.clipboard?.writeText(token); toast(t('access_keys.copied', 'Skopiowano'), 'success'); });
  win.querySelector('#ak-done2').addEventListener('click', () => win.remove());
}

// ---------------------------------------------------------------------------
// Tab: Macierz dostępu (addon-style perm-matrix)
// ---------------------------------------------------------------------------
async function renderMatrixTab(body) {
  body.innerHTML = `<div class="empty-big" style="padding:24px;">${escapeHtml(t('common.loading', 'Ładowanie...'))}</div>`;
  await loadSubjectsAndResources();
  if (activeTab !== 'matrix' || !body.isConnected) return; // tab switch or navigation during the load
  body.innerHTML = `
    <div class="tf-section-card">
      <div class="subtabs" id="ak-msub">
        <div class="subtab ${matrixSubtab === 'group' ? 'active' : ''}" data-sub="group">${escapeHtml(t('access_keys.per_group', 'Per grupa'))}</div>
        <div class="subtab ${matrixSubtab === 'user' ? 'active' : ''}" data-sub="user">${escapeHtml(t('access_keys.per_user', 'Per user'))}</div>
        <div class="subtab ${matrixSubtab === 'api_key' ? 'active' : ''}" data-sub="api_key">${escapeHtml(t('access_keys.per_key', 'Per klucz'))}</div>
        <div class="subtab ${matrixSubtab === 'default' ? 'active' : ''}" data-sub="default">${escapeHtml(t('access_keys.defaults', 'Domyślne'))}</div>
      </div>
      <div class="ak-resolution">${escapeHtml(t('access_keys.resolution', 'Kolejność: admin → deny usera → allow usera → deny grupy → allow grupy → Domyślne. Na /v1 Domyślne = DENY.'))}</div>
      <div id="ak-matrix" style="overflow:auto;"></div>
      ${legend()}
    </div>`;
  body.querySelector('#ak-msub').addEventListener('click', (e) => {
    const s = e.target.closest('.subtab'); if (!s) return;
    matrixSubtab = s.dataset.sub; renderMatrixTab(body);
  });
  await renderMatrixGrid(body.querySelector('#ak-matrix'));
}

function legend() {
  return `<div class="legend">
    <div class="li"><span class="dot allow"></span>${escapeHtml(t('access_keys.legend_allow', 'allow — dozwolone'))}</div>
    <div class="li"><span class="dot deny"></span>${escapeHtml(t('access_keys.legend_deny', 'deny — zablokowane'))}</div>
    <div class="li"><span class="dot inherit"></span>${escapeHtml(t('access_keys.legend_inherit', 'dziedzicz'))}</div>
  </div>`;
}

async function subjectRows() {
  if (matrixSubtab === 'group') return groups.map((g) => ({ subjectType: 'group', subjectId: g.id, label: g.name, meta: `${g.memberCount ?? 0}` }));
  if (matrixSubtab === 'user') return users.map((u) => ({ subjectType: 'user', subjectId: u.id, label: u.username || u.displayName || u.id, meta: u.role || '' }));
  if (matrixSubtab === 'api_key') return keys.filter((k) => k.keyType === 'general').map((k) => ({ subjectType: 'api_key', subjectId: k.keyId, label: k.name, meta: t('access_keys.meta_general_key', 'klucz ogólny') }));
  return [];
}

async function loadSubjectLevels(subjectType, subjectId) {
  // api_key scopes use the dedicated handler; user/group use the IAM perms list.
  // Levels are filed by `scopeKey`, so a key's read and write grants on the
  // same schema registry scope stay two cells. `__bus` lists the schema
  // registry grants themselves, for `busSchemaColumns`.
  const map = { __bus: {} };
  const file = (e) => {
    const key = scopeKey(e.resourceType, e.resourceId, e.action);
    map[key] = e.accessLevel;
    if (e.resourceType === BUS_SCHEMA_REGISTRY) map.__bus[key] = { id: e.resourceId, action: e.action };
  };
  try {
    if (subjectType === 'api_key') {
      const resp = await ApiBinary.action('apiKeyScopeListRequest', { keyUid: subjectId });
      (resp?.entries || []).forEach(file);
    } else {
      const resp = await ApiBinary.action('iamListPermsForSubjectRequest', { subjectType, subjectId });
      (resp?.entries || []).forEach(file);
    }
  } catch (_) { /* empty */ }
  return map;
}

// Grouped header: the top row names each resource group (Modele / Flow /
// Aliasy ... spanning its columns), the next row the resources. Schema-registry
// columns need one more level — "instance · organisation" above its
// Czytanie / Zapisywanie pair — so when they are present every other resource
// name spans the two lower rows. The first column is pinned (see
// access-keys.css) so the key name stays readable while the grid scrolls.
function matrixHead(firstLabel, busCols = []) {
  const groups = [
    { label: t('access_keys.models', 'Modele'), icon: 'model', items: resources.model.map((r) => ({ ...r, type: 'model' })) },
    { label: 'Flow', icon: 'flow', items: resources.flow.map((r) => ({ ...r, type: 'flow' })) },
    { label: t('access_keys.aliases', 'Aliasy'), icon: 'link', items: resources.alias.map((r) => ({ ...r, type: 'alias' })) },
    { label: t('access_keys.model_bundles', 'Bundle modeli'), icon: 'eye', items: resources.model_bundle.map((r) => ({ ...r, type: 'model_bundle' })) },
    { label: t('access_keys.ml_studio_projects', 'Projekty ML Studio'), icon: 'model', items: resources.ml_studio_export.map((r) => ({ ...r, type: 'ml_studio_export' })) },
  ].filter((g) => g.items.length > 0);
  const hasBus = busCols.length > 0;
  const rows = hasBus ? 3 : 2;
  const top = groups.map((g) => `<th class="grp" colspan="${g.items.length}"><svg class="icon"><use href="#i-${g.icon}"/></svg> ${escapeHtml(g.label)}</th>`).join('')
    + (hasBus ? `<th class="grp ak-bus-grp" colspan="${busCols.length}"><svg class="icon"><use href="#i-flow"/></svg> ${escapeHtml(t('access_keys.bus_schema_title', 'Wzory wiadomości'))}</th>` : '');
  const names = groups.flatMap((g) => g.items)
    .map((c) => `<th class="func"${hasBus ? ' rowspan="2"' : ''} title="${escapeAttr(c.title || c.name)}">${escapeHtml(c.name)}</th>`).join('');
  // Consecutive columns of one scope share the "instance · organisation" cell.
  const scopes = [];
  busCols.forEach((c) => {
    const last = scopes[scopes.length - 1];
    if (last && last.id === c.id) last.span += 1; else scopes.push({ id: c.id, label: c.scope, span: 1 });
  });
  const busScopes = scopes.map((sc) => `<th class="func ak-bus-scope" colspan="${sc.span}" title="${escapeAttr(sc.label)}">${escapeHtml(sc.label)}</th>`).join('');
  const busActions = busCols.map((c) => `<th class="func ak-bus-action-head" title="${escapeAttr(c.title)}">${escapeHtml(c.name)}</th>`).join('');
  return `<tr><th class="ak-subject-col" rowspan="${rows}">${escapeHtml(firstLabel)}</th>${top}</tr>`
    + `<tr>${names}${busScopes}</tr>`
    + (hasBus ? `<tr>${busActions}</tr>` : '');
}

async function renderMatrixGrid(grid) {
  const cols = allResourceColumns();
  if (matrixSubtab === 'default') {
    grid.innerHTML = `<table class="perm-matrix ak-matrix"><thead>${matrixHead(t('access_keys.defaults', 'Domyślne'))}</thead>
      <tbody><tr class="row-default"><td class="ak-subject-col"><div class="group-name">${escapeHtml(t('access_keys.default_v1', 'Domyślne (/v1)'))}</div><div class="group-meta">${escapeHtml(t('access_keys.default_meta', 'fallback = DENY'))}</div></td>
      ${cols.map(() => `<td class="func"><button class="perm-btn deny" disabled><svg class="icon"><use href="#i-x"/></svg></button></td>`).join('')}</tr></tbody></table>`;
    return;
  }
  const subjects = await subjectRows();
  if (subjects.length === 0) { grid.innerHTML = `<div class="empty-big">${escapeHtml(t('access_keys.no_subjects', 'Brak podmiotów'))}</div>`; return; }
  const levels = await Promise.all(subjects.map((s) => loadSubjectLevels(s.subjectType, s.subjectId)));
  // Schema registry grants exist for general keys only — users and groups
  // never reach that REST surface, so their rows get no such columns.
  const busCols = matrixSubtab === 'api_key' ? busSchemaColumns(levels) : [];
  const allCols = [...cols, ...busCols];
  const head = matrixHead(t('access_keys.subject', 'Podmiot'), busCols);
  const rowsHtml = subjects.map((s, i) => {
    const lv = levels[i];
    const cells = allCols.map((c) => {
      const mode = lv[scopeKey(c.type, c.id, c.action)] || 'inherit';
      return `<td class="func">${cellBtn(mode, s, c)}</td>`;
    }).join('');
    return `<tr><td class="ak-subject-col"><div class="group-name">${escapeHtml(s.label)}</div><div class="group-meta">${escapeHtml(s.meta)}</div></td>${cells}</tr>`;
  }).join('');
  if (!grid.isConnected) return;
  grid.innerHTML = `<table class="perm-matrix ak-matrix"><thead>${head}</thead><tbody>${rowsHtml}</tbody></table>`;
  grid.querySelectorAll('.perm-btn[data-subject-id]').forEach((btn) => btn.addEventListener('click', () => cycleCell(btn, grid)));
  if (highlightKeyUid) {
    const target = grid.querySelector(`.perm-btn[data-subject-id="${CSS.escape(highlightKeyUid)}"]`)?.closest('tr');
    if (target) { target.classList.add('ak-row-hl'); target.scrollIntoView({ block: 'center', behavior: 'smooth' }); }
    highlightKeyUid = null;
  }
}

function modeLabel(mode) {
  if (mode === 'allow') return t('access_keys.legend_allow', 'allow — dozwolone');
  if (mode === 'deny') return t('access_keys.legend_deny', 'deny — zablokowane');
  return t('access_keys.legend_inherit', 'dziedzicz');
}

// Accessible name of a matrix cell: who, what, and the current state — the
// cell itself only shows a tick, a cross or a dash.
function cellLabel(subject, col, mode) {
  return t('access_keys.cell_label', '{subject} — {resource}: {state}', {
    subject: subject.label,
    resource: col.title || col.name,
    state: modeLabel(mode),
  });
}

function cellBtn(mode, subject, col) {
  const m = ['allow', 'deny', 'inherit'].includes(mode) ? mode : 'inherit';
  const inner = m === 'allow' ? '<svg class="icon"><use href="#i-check"/></svg>' : m === 'deny' ? '<svg class="icon"><use href="#i-x"/></svg>' : '—';
  const action = col.action ? ` data-action="${escapeAttr(col.action)}"` : '';
  const label = escapeAttr(cellLabel(subject, col, m));
  return `<button class="perm-btn ${m}" type="button" aria-label="${label}" title="${label}" data-subject-type="${escapeAttr(subject.subjectType)}" data-subject-id="${escapeAttr(subject.subjectId)}" data-subject-label="${escapeAttr(subject.label)}" data-col-label="${escapeAttr(col.title || col.name)}" data-rtype="${escapeAttr(col.type)}" data-rid="${escapeAttr(col.id)}"${action} data-mode="${m}">${inner}</button>`;
}

function paintCell(btn, mode) {
  btn.classList.remove('allow', 'deny', 'inherit'); btn.classList.add(mode); btn.dataset.mode = mode;
  btn.innerHTML = mode === 'allow' ? '<svg class="icon"><use href="#i-check"/></svg>' : mode === 'deny' ? '<svg class="icon"><use href="#i-x"/></svg>' : '—';
  const label = cellLabel({ label: btn.dataset.subjectLabel }, { name: btn.dataset.colLabel }, mode);
  btn.setAttribute('aria-label', label);
  btn.title = label;
}

async function cycleCell(btn, grid) {
  const cur = btn.dataset.mode;
  const next = NEXT_MODE[cur] || 'allow';
  const subjectType = btn.dataset.subjectType;
  const subjectId = btn.dataset.subjectId;
  const resourceType = btn.dataset.rtype;
  const resourceId = btn.dataset.rid;
  const action = btn.dataset.action || null;
  paintCell(btn, next); // optimistic
  try {
    if (subjectType === 'api_key') {
      if (next === 'inherit') await ApiBinary.action('apiKeyScopeClearRequest', { keyUid: subjectId, resourceType, resourceId, action });
      else await ApiBinary.action('apiKeyScopeSetRequest', { keyUid: subjectId, resourceType, resourceId, accessLevel: next, action });
    } else if (next === 'inherit') {
      await ApiBinary.action('iamClearPermissionRequest', { resourceType, resourceId, subjectType, subjectId });
    } else {
      await ApiBinary.action('iamSetPermissionRequest', { resourceType, resourceId, subjectType, subjectId, accessLevel: next });
    }
    toast(t('access_keys.saved', 'Zapisano'), 'success');
  } catch (e) {
    paintCell(btn, cur);
    toast(e.message || 'error', 'error');
  }
}

// Scope editor reuses the matrix logic for a single api-key (general): jump to
// the Per-klucz matrix and highlight + scroll to that key's row.
function openScopeEditor(keyUid) {
  activeTab = 'matrix';
  matrixSubtab = 'api_key';
  highlightKeyUid = keyUid;
  renderActiveTab();
}

// ---------------------------------------------------------------------------
// Tab: Wg zasobu (transpose — pick a resource, see subjects)
// ---------------------------------------------------------------------------
// Schema-registry scopes as pickable resources: one entry per known instance ×
// organisation, named, never shown by id.
function busSchemaResourceOptions() {
  const ids = busInstances.flatMap((i) => orgs.map((o) => busSchemaScopeId(i.addonId, o.orgId)));
  const names = busSchemaNames(ids);
  return ids.map((id) => ({ id, name: `${names.get(id).instance} · ${names.get(id).org}` }));
}

async function renderByResourceTab(body) {
  body.innerHTML = `<div class="empty-big" style="padding:24px;">${escapeHtml(t('common.loading', 'Ładowanie...'))}</div>`;
  await loadSubjectsAndResources();
  if (activeTab !== 'byresource' || !body.isConnected) return; // tab switch or navigation during the load
  const cols = resourceView === BUS_SCHEMA_REGISTRY ? busSchemaResourceOptions() : (resources[resourceView] || []);
  const opts = cols.map((r) => `<option value="${escapeAttr(r.id)}">${escapeHtml(r.name)}</option>`).join('');
  const sub = (view, label) => `<div class="subtab ${resourceView === view ? 'active' : ''}" data-rv="${view}">${escapeHtml(label)}</div>`;
  body.innerHTML = `
    <div class="tf-section-card">
      <div class="subtabs" id="ak-rsub">
        ${sub('model', t('access_keys.models', 'Modele'))}
        ${sub('flow', 'Flow')}
        ${sub('alias', t('access_keys.aliases', 'Aliasy'))}
        ${sub('model_bundle', t('access_keys.model_bundles', 'Bundle modeli'))}
        ${sub('ml_studio_export', t('access_keys.ml_studio_projects', 'Projekty ML Studio'))}
        ${sub(BUS_SCHEMA_REGISTRY, t('access_keys.bus_schema_title', 'Wzory wiadomości'))}
      </div>
      <div class="ak-form-row"><label>${escapeHtml(t('access_keys.resource', 'Zasób'))}</label><tf-select id="ak-resource"><option value="">—</option>${opts}</tf-select></div>
      <div id="ak-rmatrix"></div>
      ${legend()}
    </div>`;
  body.querySelector('#ak-rsub').addEventListener('click', (e) => { const s = e.target.closest('.subtab'); if (!s) return; resourceView = s.dataset.rv; renderByResourceTab(body); });
  body.querySelector('#ak-resource').addEventListener('change', (e) => {
    const rid = e.detail?.value || e.target.value;
    const col = cols.find((c) => c.id === rid);
    renderResourceMatrix(body.querySelector('#ak-rmatrix'), resourceView, rid, col?.name || rid);
  });
}

async function renderResourceMatrix(grid, rtype, rid, resourceName) {
  if (!rid) { grid.innerHTML = `<div class="empty-big">${escapeHtml(t('access_keys.pick_resource', 'Wybierz zasób'))}</div>`; return; }
  const generalKeys = keys.filter((k) => k.keyType === 'general')
    .map((k) => ({ subjectType: 'api_key', subjectId: k.keyId, label: k.name, meta: t('access_keys.meta_key', 'klucz') }));
  // Only general keys can hold a schema-registry grant; users and groups
  // never reach that REST surface.
  const subjects = rtype === BUS_SCHEMA_REGISTRY ? generalKeys : [
    ...groups.map((g) => ({ subjectType: 'group', subjectId: g.id, label: g.name, meta: t('access_keys.meta_group', 'grupa') })),
    ...users.map((u) => ({ subjectType: 'user', subjectId: u.id, label: u.username || u.id, meta: t('access_keys.meta_user', 'user') })),
    ...generalKeys,
  ];
  const cols = rtype === BUS_SCHEMA_REGISTRY
    ? BUS_SCHEMA_ACTIONS.map((action) => ({ type: rtype, id: rid, action, name: busSchemaActionLabel(action), title: `${resourceName} · ${busSchemaActionLabel(action)}` }))
    : [{ type: rtype, id: rid, name: resourceName, title: resourceName }];
  const levels = await Promise.all(subjects.map((s) => loadSubjectLevels(s.subjectType, s.subjectId)));
  if (!grid.isConnected) return;
  const rows = subjects.map((s, i) => {
    const cells = cols.map((c) => `<td class="func">${cellBtn(levels[i][scopeKey(c.type, c.id, c.action)] || 'inherit', s, c)}</td>`).join('');
    return `<tr><td class="ak-subject-col"><div class="group-name">${escapeHtml(s.label)}</div><div class="group-meta">${escapeHtml(s.meta)}</div></td>${cells}</tr>`;
  }).join('');
  const heads = cols.map((c) => `<th class="func" title="${escapeAttr(c.title)}">${escapeHtml(c.name)}</th>`).join('');
  const defaults = cols.map(() => '<td class="func"><button class="perm-btn deny" type="button" disabled><svg class="icon"><use href="#i-x"/></svg></button></td>').join('');
  grid.innerHTML = `<table class="perm-matrix ak-matrix"><thead><tr><th class="ak-subject-col">${escapeHtml(t('access_keys.subject', 'Podmiot'))}</th>${heads}</tr></thead>
    <tbody>${rows}<tr class="row-default"><td class="ak-subject-col"><div class="group-name">${escapeHtml(t('access_keys.default_v1', 'Domyślne (/v1)'))}</div></td>${defaults}</tr></tbody></table>`;
  grid.querySelectorAll('.perm-btn[data-subject-id]').forEach((btn) => btn.addEventListener('click', () => cycleCell(btn, grid)));
}
