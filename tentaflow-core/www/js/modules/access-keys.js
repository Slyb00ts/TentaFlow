// =============================================================================
// File: modules/access-keys.js
// Purpose: "Dostęp i klucze API" admin screen. Three tabs:
//   - Klucze: API key list (type user/group/general, scope, sync) + create
//     wizard modal (3 steps, deploy-style step indicator) + rotate/revoke.
//   - Macierz dostępu: addon-style permission matrix (rows = subjects, columns
//     = resources model/flow/alias, tri-state allow/deny/inherit), subtabs
//     Per grupa / Per user / Per klucz / Domyślne.
//   - Wg zasobu: same data, transposed (pick a resource → who can access it).
// All writes go through the binary protocol; default-DENY is enforced server
// side, this screen only edits resource_permissions / api-key scopes.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
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
let resources = { model: [], flow: [], alias: [] };

const NEXT_MODE = { allow: 'deny', deny: 'inherit', inherit: 'allow' };

function t(key, fallback) {
  const v = I18n.t(key);
  return v === key && fallback != null ? fallback : v;
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
    resources = { model: [], flow: [], alias: [] };
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

function renderActiveTab() {
  host.querySelectorAll('#ak-tabs .tf-tab-btn').forEach((b) => {
    b.classList.toggle('active', b.dataset.tab === activeTab);
  });
  const body = host.querySelector('#ak-body');
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
  const [g, u, models, flows, aliases] = await Promise.all([
    ApiBinary.action('iamListGroupsRequest').then((r) => r?.groups ?? []).catch(() => []),
    ApiBinary.action('iamListUsersRequest').then((r) => r?.users ?? []).catch(() => []),
    ApiBinary.list('modelListRequest').catch(() => []),
    ApiBinary.list('flowListRequest').catch(() => []),
    ApiBinary.list('modelAliasListRequest', { arrayKey: 'aliases' }).catch(() => []),
  ]);
  groups = Array.isArray(g) ? g : [];
  users = Array.isArray(u) ? u : [];
  if (keys.length === 0) await loadKeys();
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
  };
}

function allResourceColumns() {
  return [
    ...resources.model.map((r) => ({ ...r, type: 'model' })),
    ...resources.flow.map((r) => ({ ...r, type: 'flow' })),
    ...resources.alias.map((r) => ({ ...r, type: 'alias' })),
  ];
}

// ---------------------------------------------------------------------------
// Tab: Klucze
// ---------------------------------------------------------------------------
function typeChip(keyType) {
  const map = {
    user: ['user', t('access_keys.type_user', 'User')],
    group: ['users', t('access_keys.type_group', 'Grupa')],
    general: ['key', t('access_keys.type_general', 'Ogólny')],
  };
  const [icon, label] = map[keyType] || ['key', keyType];
  return `<span class="tf-chip ak-type-${escapeAttr(keyType)}"><svg class="icon icon-xs"><use href="#i-${icon}"/></svg>${escapeHtml(label)}</span>`;
}

function renderKeysTab(body) {
  const rows = keys.length === 0
    ? `<tr><td colspan="6"><div class="empty-big" style="padding:24px;">${escapeHtml(t('access_keys.empty', 'Brak kluczy API'))}</div></td></tr>`
    : keys.map((k) => {
        const subject = k.subjectLabel || k.subjectId || (k.keyType === 'general' ? '—' : '');
        const scope = k.keyType === 'general'
          ? `${k.scopeCount || 0} ${escapeHtml(t('access_keys.resources', 'zasobów'))}`
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
            <tf-button size="sm" variant="danger" icon="trash" data-revoke="${escapeAttr(k.keyId)}"></tf-button>
          </td>
        </tr>`;
      }).join('');

  body.innerHTML = `
    <div class="tf-section-card">
      <div class="ak-toolbar">
        <div class="ak-info">${escapeHtml(t('access_keys.deny_hint', 'Default-DENY: klucz widzi i wywołuje tylko jawnie nadane zasoby. Synchronizowane na wszystkie węzły.'))}</div>
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
  const state = { step: 1, keyType: 'user', name: '', subjectId: '', scope: new Set() };
  const win = document.createElement('tf-window');
  win.setAttribute('title', t('access_keys.new_key', 'Nowy klucz API'));
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
            <tf-input id="ak-name" value="${escapeAttr(state.name)}" placeholder="np. CI Pipeline"></tf-input>
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
          </div>
          <div slot="footer" class="ak-wizard-footer">
            <tf-button variant="ghost" id="ak-back">${escapeHtml(t('common.back', 'Wstecz'))}</tf-button>
            <span style="flex:1"></span>
            <span class="ak-pick-count" id="ak-count">${state.scope.size} ${escapeHtml(t('access_keys.selected', 'zaznaczone'))}</span>
            <tf-button variant="primary" id="ak-create-btn">${escapeHtml(t('access_keys.create_btn', 'Utwórz'))}</tf-button>
          </div>`;
        win.querySelectorAll('.ak-pick-row').forEach((row) => row.addEventListener('click', (e) => {
          e.preventDefault();
          const key = row.dataset.key;
          if (state.scope.has(key)) state.scope.delete(key); else state.scope.add(key);
          row.classList.toggle('checked', state.scope.has(key));
          const cb = row.querySelector('tf-checkbox');
          if (cb) cb.toggleAttribute('checked', state.scope.has(key));
          win.querySelector('#ak-count').textContent = `${state.scope.size} ${t('access_keys.selected', 'zaznaczone')}`;
        }));
      } else {
        await loadSubjectsAndResources();
        const opts = (state.keyType === 'user' ? users : groups)
          .map((s) => `<option value="${escapeAttr(s.id)}">${escapeHtml(s.username || s.name || s.id)}</option>`).join('');
        win.innerHTML = `
          <div class="ak-wizard">
            ${stepIndicator(2)}
            <h4 class="wizard-step-title">${escapeHtml(state.keyType === 'user' ? t('access_keys.wiz_pick_user', 'Wybierz użytkownika') : t('access_keys.wiz_pick_group', 'Wybierz grupę'))}</h4>
            <div class="ak-deny-note info">${escapeHtml(t('access_keys.inherit_note', 'Klucz ściśle dziedziczy efektywne uprawnienia podmiotu.'))}</div>
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
  if (activeTab !== 'matrix') return; // user switched tabs during the async load
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
  if (matrixSubtab === 'api_key') return keys.filter((k) => k.keyType === 'general').map((k) => ({ subjectType: 'api_key', subjectId: k.keyId, label: k.name, meta: 'klucz ogólny' }));
  return [];
}

async function loadSubjectLevels(subjectType, subjectId) {
  // api_key scopes use the dedicated handler; user/group use the IAM perms list.
  const map = {};
  try {
    if (subjectType === 'api_key') {
      const resp = await ApiBinary.action('apiKeyScopeListRequest', { keyUid: subjectId });
      (resp?.entries || []).forEach((e) => { map[`${e.resourceType}:${e.resourceId}`] = e.accessLevel; });
    } else {
      const resp = await ApiBinary.action('iamListPermsForSubjectRequest', { subjectType, subjectId });
      (resp?.entries || []).forEach((e) => { map[`${e.resourceType}:${e.resourceId}`] = e.accessLevel; });
    }
  } catch (_) { /* empty */ }
  return map;
}

// Two-row header: top row groups columns under Modele / Flow / Aliasy
// (spanning), bottom row carries the resource names — 1:1 with mockup 03.
function matrixHead(firstLabel) {
  const groups = [
    { label: t('access_keys.models', 'Modele'), icon: 'model', items: resources.model.map((r) => ({ ...r, type: 'model' })) },
    { label: 'Flow', icon: 'flow', items: resources.flow.map((r) => ({ ...r, type: 'flow' })) },
    { label: t('access_keys.aliases', 'Aliasy'), icon: 'link', items: resources.alias.map((r) => ({ ...r, type: 'alias' })) },
  ].filter((g) => g.items.length > 0);
  const top = groups.map((g) => `<th class="grp" colspan="${g.items.length}"><svg class="icon"><use href="#i-${g.icon}"/></svg> ${escapeHtml(g.label)}</th>`).join('');
  const names = groups.flatMap((g) => g.items).map((c) => `<th class="func" title="${escapeAttr(c.type + ':' + c.id)}">${escapeHtml(c.name)}</th>`).join('');
  return `<tr><th rowspan="2" style="min-width:220px;vertical-align:bottom;">${escapeHtml(firstLabel)}</th>${top}</tr><tr>${names}</tr>`;
}

async function renderMatrixGrid(grid) {
  const cols = allResourceColumns();
  if (matrixSubtab === 'default') {
    grid.innerHTML = `<table class="perm-matrix"><thead>${matrixHead(t('access_keys.defaults', 'Domyślne'))}</thead>
      <tbody><tr class="row-default"><td><div class="group-name">${escapeHtml(t('access_keys.default_v1', 'Domyślne (/v1)'))}</div><div class="group-meta">${escapeHtml(t('access_keys.default_meta', 'fallback = DENY'))}</div></td>
      ${cols.map(() => `<td class="func"><button class="perm-btn deny" disabled><svg class="icon"><use href="#i-x"/></svg></button></td>`).join('')}</tr></tbody></table>`;
    return;
  }
  const subjects = await subjectRows();
  if (subjects.length === 0) { grid.innerHTML = `<div class="empty-big">${escapeHtml(t('access_keys.no_subjects', 'Brak podmiotów'))}</div>`; return; }
  const levels = await Promise.all(subjects.map((s) => loadSubjectLevels(s.subjectType, s.subjectId)));
  const head = matrixHead(t('access_keys.subject', 'Podmiot'));
  const rowsHtml = subjects.map((s, i) => {
    const lv = levels[i];
    const cells = cols.map((c) => {
      const mode = lv[`${c.type}:${c.id}`] || 'inherit';
      return `<td class="func">${cellBtn(mode, s, c)}</td>`;
    }).join('');
    return `<tr><td><div class="group-name">${escapeHtml(s.label)}</div><div class="group-meta">${escapeHtml(s.meta)}</div></td>${cells}</tr>`;
  }).join('');
  grid.innerHTML = `<table class="perm-matrix"><thead>${head}</thead><tbody>${rowsHtml}</tbody></table>`;
  grid.querySelectorAll('.perm-btn[data-subject-id]').forEach((btn) => btn.addEventListener('click', () => cycleCell(btn, grid)));
  if (highlightKeyUid) {
    const target = grid.querySelector(`.perm-btn[data-subject-id="${CSS.escape(highlightKeyUid)}"]`)?.closest('tr');
    if (target) { target.classList.add('ak-row-hl'); target.scrollIntoView({ block: 'center', behavior: 'smooth' }); }
    highlightKeyUid = null;
  }
}

function cellBtn(mode, subject, col) {
  const m = ['allow', 'deny', 'inherit'].includes(mode) ? mode : 'inherit';
  const inner = m === 'allow' ? '<svg class="icon"><use href="#i-check"/></svg>' : m === 'deny' ? '<svg class="icon"><use href="#i-x"/></svg>' : '—';
  return `<button class="perm-btn ${m}" data-subject-type="${escapeAttr(subject.subjectType)}" data-subject-id="${escapeAttr(subject.subjectId)}" data-rtype="${escapeAttr(col.type)}" data-rid="${escapeAttr(col.id)}" data-mode="${m}">${inner}</button>`;
}

async function cycleCell(btn, grid) {
  const cur = btn.dataset.mode;
  const next = NEXT_MODE[cur] || 'allow';
  const subjectType = btn.dataset.subjectType;
  const subjectId = btn.dataset.subjectId;
  const resourceType = btn.dataset.rtype;
  const resourceId = btn.dataset.rid;
  // optimistic
  btn.classList.remove('allow', 'deny', 'inherit'); btn.classList.add(next); btn.dataset.mode = next;
  btn.innerHTML = next === 'allow' ? '<svg class="icon"><use href="#i-check"/></svg>' : next === 'deny' ? '<svg class="icon"><use href="#i-x"/></svg>' : '—';
  try {
    if (subjectType === 'api_key') {
      if (next === 'inherit') await ApiBinary.action('apiKeyScopeClearRequest', { keyUid: subjectId, resourceType, resourceId });
      else await ApiBinary.action('apiKeyScopeSetRequest', { keyUid: subjectId, resourceType, resourceId, accessLevel: next });
    } else if (next === 'inherit') {
      await ApiBinary.action('iamClearPermissionRequest', { resourceType, resourceId, subjectType, subjectId });
    } else {
      await ApiBinary.action('iamSetPermissionRequest', { resourceType, resourceId, subjectType, subjectId, accessLevel: next });
    }
    toast(t('access_keys.saved', 'Zapisano'), 'success');
  } catch (e) {
    btn.classList.remove('allow', 'deny', 'inherit'); btn.classList.add(cur); btn.dataset.mode = cur;
    btn.innerHTML = cur === 'allow' ? '<svg class="icon"><use href="#i-check"/></svg>' : cur === 'deny' ? '<svg class="icon"><use href="#i-x"/></svg>' : '—';
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
async function renderByResourceTab(body) {
  body.innerHTML = `<div class="empty-big" style="padding:24px;">${escapeHtml(t('common.loading', 'Ładowanie...'))}</div>`;
  await loadSubjectsAndResources();
  if (activeTab !== 'byresource') return; // user switched tabs during the async load
  const cols = resources[resourceView] || [];
  const opts = cols.map((r) => `<option value="${escapeAttr(r.id)}">${escapeHtml(r.name)}</option>`).join('');
  body.innerHTML = `
    <div class="tf-section-card">
      <div class="subtabs" id="ak-rsub">
        <div class="subtab ${resourceView === 'model' ? 'active' : ''}" data-rv="model">${escapeHtml(t('access_keys.models', 'Modele'))}</div>
        <div class="subtab ${resourceView === 'flow' ? 'active' : ''}" data-rv="flow">Flow</div>
        <div class="subtab ${resourceView === 'alias' ? 'active' : ''}" data-rv="alias">${escapeHtml(t('access_keys.aliases', 'Aliasy'))}</div>
      </div>
      <div class="ak-form-row"><label>${escapeHtml(t('access_keys.resource', 'Zasób'))}</label><tf-select id="ak-resource"><option value="">—</option>${opts}</tf-select></div>
      <div id="ak-rmatrix"></div>
      ${legend()}
    </div>`;
  body.querySelector('#ak-rsub').addEventListener('click', (e) => { const s = e.target.closest('.subtab'); if (!s) return; resourceView = s.dataset.rv; renderByResourceTab(body); });
  body.querySelector('#ak-resource').addEventListener('change', (e) => renderResourceMatrix(body.querySelector('#ak-rmatrix'), resourceView, e.detail?.value || e.target.value));
}

async function renderResourceMatrix(grid, rtype, rid) {
  if (!rid) { grid.innerHTML = `<div class="empty-big">${escapeHtml(t('access_keys.pick_resource', 'Wybierz zasób'))}</div>`; return; }
  const subjects = [
    ...groups.map((g) => ({ subjectType: 'group', subjectId: g.id, label: g.name, meta: 'grupa' })),
    ...users.map((u) => ({ subjectType: 'user', subjectId: u.id, label: u.username || u.id, meta: 'user' })),
    ...keys.filter((k) => k.keyType === 'general').map((k) => ({ subjectType: 'api_key', subjectId: k.keyId, label: k.name, meta: 'klucz' })),
  ];
  const levels = await Promise.all(subjects.map((s) => loadSubjectLevels(s.subjectType, s.subjectId)));
  const col = { type: rtype, id: rid, name: rid };
  const rows = subjects.map((s, i) => {
    const mode = levels[i][`${rtype}:${rid}`] || 'inherit';
    return `<tr><td><div class="group-name">${escapeHtml(s.label)}</div><div class="group-meta">${escapeHtml(s.meta)}</div></td><td class="func">${cellBtn(mode, s, col)}</td></tr>`;
  }).join('');
  grid.innerHTML = `<table class="perm-matrix"><thead><tr><th style="min-width:220px;">${escapeHtml(t('access_keys.subject', 'Podmiot'))}</th><th class="func">${escapeHtml(rid)}</th></tr></thead>
    <tbody>${rows}<tr class="row-default"><td><div class="group-name">${escapeHtml(t('access_keys.default_v1', 'Domyślne (/v1)'))}</div></td><td class="func"><button class="perm-btn deny" disabled><svg class="icon"><use href="#i-x"/></svg></button></td></tr></tbody></table>`;
  grid.querySelectorAll('.perm-btn[data-subject-id]').forEach((btn) => btn.addEventListener('click', () => cycleCell(btn, grid)));
}
