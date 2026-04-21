// =============================================================================
// Plik: modules/addons/permissions.js
// Opis: Tab Permissions dla detail addona (admin). Trzy podzakladki:
//       Per grupa (wiersze = grupy, kolumny = uprawnienia),
//       Per user (override dla konkretnego uzytkownika),
//       Default (fallback). Uklad 1:1 z mockupem addons-permissions-20260420.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast, createEchoGuard } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

let currentAddonId = null;
let currentAddonName = '';
let currentContainer = null;
let activeSubtab = 'per_group';

let groups = [];                    // [{groupId, groupName, visible, userCount}]
let permissions = [];               // [{permissionId, displayName, description, risk, sortOrder}]
let usersCatalog = [];              // [{userId, username}]
let groupCells = new Map();         // `${groupId}::${pid}` -> grantMode
let userCells = new Map();          // `${userId}::${pid}` -> grantMode
let defaults = new Map();           // pid -> grantMode
let overriddenUserIds = new Set();  // users with at least one explicit grant
let lastChangeBy = '';
let lastChangeAtEpoch = 0;
let unsubscribePush = null;
const echoGuard = createEchoGuard(1500);

const NEXT_MODE_FULL = { allow: 'deny', deny: 'inherit', inherit: 'allow' };
const NEXT_MODE_BINARY = { allow: 'deny', deny: 'allow' };

export const PermissionsTab = {
  async mount(container, addonId, addonName) {
    currentAddonId = addonId;
    currentAddonName = addonName || addonId;
    currentContainer = container;
    activeSubtab = 'per_group';
    await loadAll();
    try {
      const client = await ApiBinary.client();
      unsubscribePush = client.addUnsolicitedListener(({ body }) => {
        if (body?.variant !== 'AddonPermissionChangedEvent') return;
        if ((body.addonId || body.addon_id) !== currentAddonId) return;
        const st = body.subjectType ?? body.subject_type ?? '';
        const sid = body.subjectId ?? body.subject_id ?? '';
        const pid = body.permissionId ?? body.permission_id ?? '';
        const key = `${st}:${sid}:${pid}`;
        // Skip the echo of our own mutation — we already patched the cell optimistically.
        if (echoGuard.isOwnEcho(key)) return;
        refreshMatrixFromRemote().catch(() => {});
      });
    } catch (_) { /* ignore */ }
  },

  unmount() {
    if (unsubscribePush) { unsubscribePush(); unsubscribePush = null; }
    currentAddonId = null;
    currentAddonName = '';
    currentContainer = null;
    groups = [];
    permissions = [];
    usersCatalog = [];
    groupCells = new Map();
    userCells = new Map();
    defaults = new Map();
    overriddenUserIds = new Set();
    lastChangeBy = '';
    lastChangeAtEpoch = 0;
  },
};

async function loadAll() {
  if (!currentContainer) return;
  currentContainer.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('common.loading'))}</div>`;
  try {
    const [catalog, visibility, matrix] = await Promise.all([
      ApiBinary.one('addonPermissionCatalogRequest', { addonId: currentAddonId }),
      ApiBinary.one('addonVisibilityListRequest', { addonId: currentAddonId }),
      ApiBinary.one('addonPermissionMatrixRequest', { addonId: currentAddonId }),
    ]);

    permissions = (catalog.entries || []).map((p) => ({
      permissionId: p.permissionId ?? p.permission_id,
      displayName: p.displayName ?? p.display_name,
      description: p.description,
      risk: p.risk || 'low',
      sortOrder: p.sortOrder ?? p.sort_order ?? 0,
    })).sort((a, b) => a.sortOrder - b.sortOrder);

    groups = (visibility.rows || []).map((r) => ({
      groupId: Number(r.groupId ?? r.group_id),
      groupName: r.groupName ?? r.group_name ?? `#${r.groupId ?? r.group_id}`,
      visible: Boolean(r.visible),
      userCount: Number(r.userCount ?? r.user_count ?? 0),
    }));

    groupCells = new Map();
    userCells = new Map();
    overriddenUserIds = new Set();
    for (const r of (matrix.rows || [])) {
      const st = r.subjectType ?? r.subject_type;
      const sid = Number(r.subjectId ?? r.subject_id);
      const pid = r.permissionId ?? r.permission_id;
      const gm = r.grantMode ?? r.grant_mode;
      if (st === 'group') {
        groupCells.set(`${sid}::${pid}`, gm);
      } else if (st === 'user') {
        userCells.set(`${sid}::${pid}`, gm);
        overriddenUserIds.add(sid);
      }
    }

    defaults = new Map();
    for (const d of (matrix.defaults || [])) {
      const pid = d.permissionId ?? d.permission_id;
      defaults.set(pid, d.grantMode ?? d.grant_mode);
    }

    lastChangeBy = matrix.lastChangeBy ?? matrix.last_change_by ?? '';
    lastChangeAtEpoch = Number(matrix.lastChangeAtEpoch ?? matrix.last_change_at_epoch ?? 0);

    renderShell();
  } catch (err) {
    currentContainer.innerHTML = `<div class="addons-empty" style="color:var(--danger);">${escapeHtml(err.message)}</div>`;
  }
}

// Remote-triggered refresh: pull only matrix + visibility (no catalog), then
// re-render the currently active subtab body. Preserves scroll position and
// avoids tearing down the subtab bar + description table.
async function refreshMatrixFromRemote() {
  if (!currentContainer || !currentAddonId) return;
  try {
    const [visibility, matrix] = await Promise.all([
      ApiBinary.one('addonVisibilityListRequest', { addonId: currentAddonId }),
      ApiBinary.one('addonPermissionMatrixRequest', { addonId: currentAddonId }),
    ]);
    groups = (visibility.rows || []).map((r) => ({
      groupId: Number(r.groupId ?? r.group_id),
      groupName: r.groupName ?? r.group_name ?? `#${r.groupId ?? r.group_id}`,
      visible: Boolean(r.visible),
      userCount: Number(r.userCount ?? r.user_count ?? 0),
    }));
    groupCells = new Map();
    userCells = new Map();
    overriddenUserIds = new Set();
    for (const r of (matrix.rows || [])) {
      const st = r.subjectType ?? r.subject_type;
      const sid = Number(r.subjectId ?? r.subject_id);
      const pid = r.permissionId ?? r.permission_id;
      const gm = r.grantMode ?? r.grant_mode;
      if (st === 'group') {
        groupCells.set(`${sid}::${pid}`, gm);
      } else if (st === 'user') {
        userCells.set(`${sid}::${pid}`, gm);
        overriddenUserIds.add(sid);
      }
    }
    defaults = new Map();
    for (const d of (matrix.defaults || [])) {
      const pid = d.permissionId ?? d.permission_id;
      defaults.set(pid, d.grantMode ?? d.grant_mode);
    }
    lastChangeBy = matrix.lastChangeBy ?? matrix.last_change_by ?? '';
    lastChangeAtEpoch = Number(matrix.lastChangeAtEpoch ?? matrix.last_change_at_epoch ?? 0);
    const body = currentContainer.querySelector('#perm-subtab-body');
    if (body) await switchSubtab(activeSubtab);
  } catch (_) { /* ignore */ }
}

async function ensureUsersCatalog() {
  if (usersCatalog.length > 0) return;
  try {
    const rows = await ApiBinary.list('usersListRequest', { arrayKey: 'users' });
    usersCatalog = rows.map((u) => ({
      userId: Number(u.userId ?? u.user_id ?? u.id),
      username: u.username ?? u.name ?? `#${u.userId ?? u.user_id ?? u.id}`,
    }));
  } catch (_) {
    usersCatalog = [];
  }
}

function renderShell() {
  if (!currentContainer) return;
  if (permissions.length === 0) {
    currentContainer.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('addon_permissions.catalog_empty'))}</div>`;
    return;
  }

  currentContainer.innerHTML = `
    <div class="subtabs" role="tablist">
      <div class="subtab${activeSubtab === 'per_group' ? ' active' : ''}" data-view="per_group" role="tab">${escapeHtml(I18n.t('addon_permissions.subtab.per_group'))}</div>
      <div class="subtab${activeSubtab === 'per_user' ? ' active' : ''}" data-view="per_user" role="tab">${escapeHtml(I18n.t('addon_permissions.subtab.per_user'))}</div>
      <div class="subtab${activeSubtab === 'default' ? ' active' : ''}" data-view="default" role="tab">${escapeHtml(I18n.t('addon_permissions.subtab.default'))}</div>
    </div>

    <div class="alert info">
      <svg class="icon" width="18" height="18"><use href="#i-info"/></svg>
      <div><strong>${escapeHtml(I18n.t('addon_permissions.resolution_label'))}:</strong> ${escapeHtml(I18n.t('addon_permissions.resolution_order'))}</div>
    </div>

    <div id="perm-subtab-body"></div>

    <div class="section-card">
      <h3><svg class="icon icon-sm"><use href="#i-info"/></svg>${escapeHtml(I18n.t('addon_permissions.descriptions_title'))}</h3>
      <table class="perm-matrix" style="background:var(--bg-input);">
        <thead>
          <tr>
            <th style="min-width:200px;">${escapeHtml(I18n.t('addon_permissions.col_permission_id'))}</th>
            <th>${escapeHtml(I18n.t('addon_permissions.col_description'))}</th>
            <th style="width:140px;">${escapeHtml(I18n.t('addon_permissions.col_risk'))}</th>
          </tr>
        </thead>
        <tbody>
          ${permissions.map((p) => renderDescriptionRow(p)).join('')}
        </tbody>
      </table>
    </div>
  `;

  currentContainer.querySelectorAll('.subtabs .subtab').forEach((el) => {
    el.addEventListener('click', () => switchSubtab(el.dataset.view));
  });

  switchSubtab(activeSubtab);
}

async function switchSubtab(id) {
  activeSubtab = id;
  currentContainer?.querySelectorAll('.subtabs .subtab').forEach((el) => {
    el.classList.toggle('active', el.dataset.view === id);
  });
  const body = currentContainer?.querySelector('#perm-subtab-body');
  if (!body) return;

  if (id === 'per_group') {
    renderPerGroup(body);
  } else if (id === 'per_user') {
    body.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('common.loading'))}</div>`;
    await ensureUsersCatalog();
    renderPerUser(body);
  } else if (id === 'default') {
    renderDefault(body);
  }
}

// --- Per group view (mockup 1:1) -------------------------------------------
function renderPerGroup(body) {
  const title = I18n.t('addon_permissions.matrix_title').replace('{name}', currentAddonName);
  body.innerHTML = `
    <div class="section-card">
      <h3><svg class="icon icon-sm"><use href="#i-shield"/></svg>${escapeHtml(title)}</h3>
      <div class="section-sub">${escapeHtml(I18n.t('addon_permissions.matrix_subtitle'))}</div>

      <div style="overflow:auto;">
        <table class="perm-matrix">
          <thead>
            <tr>
              <th style="min-width:220px;">${escapeHtml(I18n.t('addon_permissions.user_group_header'))}</th>
              ${permissions.map((p) => `<th class="func" title="${escapeAttr(p.description || '')}">${formatPermissionHeader(p)}</th>`).join('')}
            </tr>
          </thead>
          <tbody>
            ${groups.length === 0 ? '' : groups.map((g) => renderGroupRow(g)).join('')}
            ${renderDefaultRow()}
          </tbody>
        </table>
      </div>

      ${renderLegend()}
    </div>
  `;
  attachGroupHandlers(body);
  attachDefaultRowHandlers(body);
}

function renderGroupRow(g) {
  const locked = !g.visible;
  const meta = I18n.t('addon_permissions.group_meta')
    .replace('{n}', String(g.userCount))
    .replace('{visible}', g.visible
      ? I18n.t('addon_permissions.visible_yes')
      : I18n.t('addon_permissions.visible_no'));
  const cells = permissions.map((p) => {
    const v = locked ? 'inherit' : (groupCells.get(`${g.groupId}::${p.permissionId}`) || 'inherit');
    return `<td class="func">${renderCellBtn(v, { subjectType: 'group', subjectId: g.groupId, permissionId: p.permissionId, locked, binary: false })}</td>`;
  }).join('');
  return `
    <tr${locked ? ' class="row-locked"' : ''}>
      <td>
        <div class="group-name">${escapeHtml(g.groupName)}</div>
        <div class="group-meta">${escapeHtml(meta)}</div>
      </td>
      ${cells}
    </tr>
  `;
}

function renderDefaultRow() {
  const cells = permissions.map((p) => {
    const v = defaults.get(p.permissionId) || 'deny';
    return `<td class="func">${renderCellBtn(v, { permissionId: p.permissionId, isDefault: true, binary: true })}</td>`;
  }).join('');
  return `
    <tr class="row-default">
      <td>
        <div class="group-name">${escapeHtml(I18n.t('addon_permissions.default_row_name'))}</div>
        <div class="group-meta">${escapeHtml(I18n.t('addon_permissions.default_row_meta'))}</div>
      </td>
      ${cells}
    </tr>
  `;
}

// --- Per user view ---------------------------------------------------------
function renderPerUser(body) {
  const overriddenUsers = usersCatalog.filter((u) => overriddenUserIds.has(u.userId));
  const searchOptions = usersCatalog
    .filter((u) => !overriddenUserIds.has(u.userId))
    .map((u) => `<option value="${u.userId}">${escapeHtml(u.username)}</option>`)
    .join('');

  const title = I18n.t('addon_permissions.matrix_title').replace('{name}', currentAddonName);

  body.innerHTML = `
    <div class="section-card">
      <h3><svg class="icon icon-sm"><use href="#i-user"/></svg>${escapeHtml(title)}</h3>
      <div class="section-sub">${escapeHtml(I18n.t('addon_permissions.per_user_subtitle'))}</div>

      <div style="display:flex;gap:8px;align-items:center;margin-bottom:12px;">
        <tf-select id="perm-user-add" style="min-width:280px;">
          <option value="">${escapeHtml(I18n.t('addon_permissions.per_user_search_placeholder'))}</option>
          ${searchOptions}
        </tf-select>
      </div>

      ${overriddenUsers.length === 0 ? `
        <div class="addons-empty">${escapeHtml(I18n.t('addon_permissions.per_user_empty'))}</div>
      ` : `
        <div style="overflow:auto;">
          <table class="perm-matrix">
            <thead>
              <tr>
                <th style="min-width:220px;">${escapeHtml(I18n.t('addon_permissions.user_header'))}</th>
                ${permissions.map((p) => `<th class="func" title="${escapeAttr(p.description || '')}">${formatPermissionHeader(p)}</th>`).join('')}
              </tr>
            </thead>
            <tbody>
              ${overriddenUsers.map((u) => renderUserRow(u)).join('')}
            </tbody>
          </table>
        </div>
        ${renderLegend()}
      `}
    </div>
  `;

  body.querySelector('#perm-user-add')?.addEventListener('change', (e) => {
    const val = e.detail?.value || '';
    if (!val) return;
    const uid = Number(val);
    if (Number.isFinite(uid)) {
      overriddenUserIds.add(uid);
      renderPerUser(body);
    }
  });

  attachUserHandlers(body);
}

function renderUserRow(u) {
  const cells = permissions.map((p) => {
    const v = userCells.get(`${u.userId}::${p.permissionId}`) || 'inherit';
    return `<td class="func">${renderCellBtn(v, { subjectType: 'user', subjectId: u.userId, permissionId: p.permissionId, binary: false })}</td>`;
  }).join('');
  return `
    <tr>
      <td>
        <div class="group-name">${escapeHtml(u.username)}</div>
        <div class="group-meta">#${u.userId}</div>
      </td>
      ${cells}
    </tr>
  `;
}

// --- Default subtab --------------------------------------------------------
function renderDefault(body) {
  body.innerHTML = `
    <div class="section-card">
      <h3><svg class="icon icon-sm"><use href="#i-shield"/></svg>${escapeHtml(I18n.t('addon_permissions.default.title'))}</h3>
      <div class="section-sub">${escapeHtml(I18n.t('addon_permissions.default.description'))}</div>

      <div style="overflow:auto;">
        <table class="perm-matrix">
          <thead>
            <tr>
              <th style="min-width:220px;">${escapeHtml(I18n.t('addon_permissions.col_permission_id'))}</th>
              <th class="func">${escapeHtml(I18n.t('addon_permissions.column_default'))}</th>
            </tr>
          </thead>
          <tbody>
            ${permissions.map((p) => `
              <tr>
                <td>
                  <div class="group-name">${escapeHtml(p.displayName || p.permissionId)}</div>
                  <div class="group-meta">${escapeHtml(p.permissionId)}</div>
                </td>
                <td class="func">${renderCellBtn(defaults.get(p.permissionId) || 'deny', { permissionId: p.permissionId, isDefault: true, binary: true })}</td>
              </tr>
            `).join('')}
          </tbody>
        </table>
      </div>
    </div>
  `;
  attachDefaultRowHandlers(body);
}

// --- Shared rendering helpers ----------------------------------------------
function formatPermissionHeader(p) {
  const id = p.permissionId || '';
  const parts = id.split('.').pop() || id;
  const words = parts.split(/[_\s-]/).filter(Boolean);
  if (words.length >= 2) {
    return `${escapeHtml(words[0])}<br>${escapeHtml(words.slice(1).join(' '))}`;
  }
  return escapeHtml(parts);
}

function renderCellBtn(mode, opts) {
  const m = (mode === 'allow' || mode === 'deny' || mode === 'inherit') ? mode : 'inherit';
  const inner = m === 'allow'
    ? `<svg class="icon"><use href="#i-check"/></svg>`
    : m === 'deny'
      ? `<svg class="icon"><use href="#i-x"/></svg>`
      : '—';
  const dataAttrs = [];
  if (opts.subjectType) {
    dataAttrs.push(`data-subject-type="${escapeAttr(opts.subjectType)}"`);
    dataAttrs.push(`data-subject-id="${escapeAttr(String(opts.subjectId))}"`);
  }
  if (opts.isDefault) {
    dataAttrs.push(`data-default="1"`);
  }
  dataAttrs.push(`data-perm="${escapeAttr(opts.permissionId)}"`);
  dataAttrs.push(`data-mode="${escapeAttr(m)}"`);
  dataAttrs.push(`data-binary="${opts.binary ? '1' : '0'}"`);
  const disabled = opts.locked ? 'disabled' : '';
  return `<button type="button" class="perm-btn ${m}" ${dataAttrs.join(' ')} ${disabled}>${inner}</button>`;
}

function renderLegend() {
  const when = lastChangeAtEpoch > 0
    ? new Date(lastChangeAtEpoch * 1000).toLocaleString()
    : '—';
  const who = lastChangeBy || '—';
  const lastText = I18n.t('addon_permissions.last_change')
    .replace('{user}', who)
    .replace('{when}', when);
  return `
    <div class="legend">
      <div class="li"><span class="dot allow"></span>${escapeHtml(I18n.t('addon_permissions.legend_allow_desc'))}</div>
      <div class="li"><span class="dot deny"></span>${escapeHtml(I18n.t('addon_permissions.legend_deny_desc'))}</div>
      <div class="li"><span class="dot inherit"></span>${escapeHtml(I18n.t('addon_permissions.legend_inherit_desc'))}</div>
      <div class="li" style="margin-left:auto;">${escapeHtml(lastText)}</div>
    </div>
  `;
}

function renderDescriptionRow(p) {
  const status = riskChipStatus(p.risk);
  return `
    <tr>
      <td><div class="perm-id-mono">${escapeHtml(p.permissionId)}</div></td>
      <td>${escapeHtml(p.description || '')}</td>
      <td><tf-chip status="${escapeAttr(status)}">${escapeHtml(I18n.t('addon_permissions.risk.' + (p.risk || 'low')))}</tf-chip></td>
    </tr>
  `;
}

function riskChipStatus(risk) {
  switch (risk) {
    case 'medium': return 'warn';
    case 'high': return 'err';
    case 'critical': return 'err';
    case 'low':
    default: return 'info';
  }
}

// --- Click handlers (cycle state and persist) ------------------------------
function attachGroupHandlers(root) {
  root.querySelectorAll('.perm-btn[data-subject-type="group"]').forEach((btn) => {
    btn.addEventListener('click', async () => {
      if (btn.disabled) return;
      const pid = btn.dataset.perm;
      const sid = Number(btn.dataset.subjectId);
      const current = btn.dataset.mode;
      const next = NEXT_MODE_FULL[current] || 'allow';
      const prev = current;
      groupCells.set(`${sid}::${pid}`, next);
      updateBtn(btn, next);
      echoGuard.markLocal(`group:${sid}:${pid}`);
      try {
        await ApiBinary.action('addonPermissionSetRequest', {
          addonId: currentAddonId,
          subjectType: 'group',
          subjectId: sid,
          permissionId: pid,
          grantMode: next,
        });
        bumpLastChange();
        toast(I18n.t('addon_permissions.saved'), 'success');
      } catch (err) {
        groupCells.set(`${sid}::${pid}`, prev);
        updateBtn(btn, prev);
        toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
      }
    });
  });
}

function attachUserHandlers(root) {
  root.querySelectorAll('.perm-btn[data-subject-type="user"]').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const pid = btn.dataset.perm;
      const sid = Number(btn.dataset.subjectId);
      const current = btn.dataset.mode;
      const next = NEXT_MODE_FULL[current] || 'allow';
      const prev = current;
      userCells.set(`${sid}::${pid}`, next);
      if (next !== 'inherit') overriddenUserIds.add(sid);
      updateBtn(btn, next);
      echoGuard.markLocal(`user:${sid}:${pid}`);
      try {
        await ApiBinary.action('addonPermissionSetRequest', {
          addonId: currentAddonId,
          subjectType: 'user',
          subjectId: sid,
          permissionId: pid,
          grantMode: next,
        });
        bumpLastChange();
        toast(I18n.t('addon_permissions.saved'), 'success');
      } catch (err) {
        userCells.set(`${sid}::${pid}`, prev);
        updateBtn(btn, prev);
        toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
      }
    });
  });
}

function attachDefaultRowHandlers(root) {
  root.querySelectorAll('.perm-btn[data-default="1"]').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const pid = btn.dataset.perm;
      const current = btn.dataset.mode;
      const next = NEXT_MODE_BINARY[current] || 'allow';
      const prev = current;
      defaults.set(pid, next);
      updateBtn(btn, next);
      echoGuard.markLocal(`:${'' }:${pid}`);
      try {
        await ApiBinary.action('addonPermissionDefaultSetRequest', {
          addonId: currentAddonId,
          permissionId: pid,
          grantMode: next,
        });
        bumpLastChange();
        toast(I18n.t('addon_permissions.saved'), 'success');
      } catch (err) {
        defaults.set(pid, prev);
        updateBtn(btn, prev);
        toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
      }
    });
  });
}

// Patches the "last change" line in the legend in place, without rebuilding
// the matrix. Called after a successful local mutation.
function bumpLastChange() {
  lastChangeAtEpoch = Math.floor(Date.now() / 1000);
  if (!currentContainer) return;
  const when = new Date(lastChangeAtEpoch * 1000).toLocaleString();
  const who = lastChangeBy || '—';
  const text = I18n.t('addon_permissions.last_change')
    .replace('{user}', who)
    .replace('{when}', when);
  currentContainer.querySelectorAll('.legend .li').forEach((li) => {
    if (li.style.marginLeft === 'auto') li.textContent = text;
  });
}

function updateBtn(btn, mode) {
  btn.classList.remove('allow', 'deny', 'inherit');
  btn.classList.add(mode);
  btn.dataset.mode = mode;
  btn.innerHTML = mode === 'allow'
    ? '<svg class="icon"><use href="#i-check"/></svg>'
    : mode === 'deny'
      ? '<svg class="icon"><use href="#i-x"/></svg>'
      : '—';
}
