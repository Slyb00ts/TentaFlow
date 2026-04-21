// =============================================================================
// Plik: modules/addons/visibility.js
// Opis: Tab Visibility (admin) — 1:1 z mockupem addons-permissions-20260420.
//       Zawiera: info alert o roznicy visibility vs permissions, liste grup
//       uzytkownikow z toggle per grupa (group-row), sekcje "Opcje dodatkowe"
//       z toggle Admin only i Pokaz w katalogu. Subskrybuje
//       AddonPermissionChangedEvent aby odswiezyc stan gdy inny admin zmieni.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast, createEchoGuard } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

let currentAddonId = null;
let unsubscribePush = null;
const echoGuard = createEchoGuard(1500);

export const VisibilityTab = {
  async mount(container, addonId, { adminOnlyInitial = false } = {}) {
    currentAddonId = addonId;
    renderShell(container, adminOnlyInitial);
    attachExtraHandlers(container);
    await loadAndRenderGroups(container);
    try {
      const client = await ApiBinary.client();
      unsubscribePush = client.addUnsolicitedListener(({ body }) => {
        if (body?.variant !== 'AddonPermissionChangedEvent') return;
        if ((body.addonId || body.addon_id) !== currentAddonId) return;
        const st = body.subjectType ?? body.subject_type ?? '';
        const sid = body.subjectId ?? body.subject_id ?? '';
        const pid = body.permissionId ?? body.permission_id ?? '';
        if (echoGuard.isOwnEcho(`${st}:${sid}:${pid}`)) return;
        loadAndRenderGroups(container).catch(() => {});
      });
    } catch (_) { /* brak push — ignoruj */ }
  },

  unmount() {
    if (unsubscribePush) { unsubscribePush(); unsubscribePush = null; }
    currentAddonId = null;
  },
};

function renderShell(container, adminOnlyInitial) {
  container.innerHTML = `
    <div class="alert info">
      <svg class="icon"><use href="#i-info"/></svg>
      <div>${I18n.t('addon_visibility.help_text')}</div>
    </div>

    <div class="section-card">
      <h3><svg class="icon icon-sm"><use href="#i-users"/></svg>${escapeHtml(I18n.t('addon_visibility.section_groups_title'))}</h3>
      <div class="section-sub">${escapeHtml(I18n.t('addon_visibility.section_groups_subtitle'))}</div>
      <div id="vis-groups-list">
        <div style="padding:14px;text-align:center;color:var(--text-3);font-size:12px;">
          ${escapeHtml(I18n.t('common.loading'))}
        </div>
      </div>
    </div>

    <div class="section-card">
      <h3><svg class="icon icon-sm"><use href="#i-key"/></svg>${escapeHtml(I18n.t('addon_visibility.section_extra_title'))}</h3>
      <div class="form-row">
        <label>
          ${escapeHtml(I18n.t('addon_visibility.admin_only_label'))}
          <div class="label-desc">${escapeHtml(I18n.t('addon_visibility.admin_only_desc'))}</div>
        </label>
        <tf-toggle id="vis-admin-only" ${adminOnlyInitial ? 'checked' : ''}></tf-toggle>
      </div>
      <div class="form-row">
        <label>
          ${escapeHtml(I18n.t('addon_visibility.show_in_catalog_label'))}
          <div class="label-desc">${escapeHtml(I18n.t('addon_visibility.show_in_catalog_desc'))}</div>
        </label>
        <tf-toggle id="vis-show-in-catalog"></tf-toggle>
      </div>
    </div>
  `;
}

function attachExtraHandlers(container) {
  container.querySelector('#vis-admin-only')?.addEventListener('change', async (e) => {
    const checked = !!e.detail?.checked;
    try {
      await ApiBinary.action('addonAdminOnlySetRequest', {
        addonId: currentAddonId,
        adminOnly: checked,
      });
      toast(I18n.t('addon_visibility.saved'), 'success');
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  });

  container.querySelector('#vis-show-in-catalog')?.addEventListener('change', async (e) => {
    const checked = !!e.detail?.checked;
    try {
      await ApiBinary.action('addonShowInCatalogSetRequest', {
        addonId: currentAddonId,
        showInCatalog: checked,
      });
      toast(I18n.t('addon_visibility.saved'), 'success');
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
    }
  });
}

async function loadAndRenderGroups(container) {
  try {
    const resp = await ApiBinary.one('addonVisibilityListRequest', { addonId: currentAddonId });
    const rows = resp.rows || [];
    const showInCatalog = !!(resp.showInCatalog ?? resp.show_in_catalog ?? true);
    const catalogToggle = container.querySelector('#vis-show-in-catalog');
    if (catalogToggle) {
      if (showInCatalog) catalogToggle.setAttribute('checked', '');
      else catalogToggle.removeAttribute('checked');
    }

    const list = container.querySelector('#vis-groups-list');
    if (!list) return;

    if (rows.length === 0) {
      list.innerHTML = `<div style="padding:14px;text-align:center;color:var(--text-3);font-size:12px;">
        ${escapeHtml(I18n.t('common.no_data'))}
      </div>`;
      return;
    }

    list.innerHTML = rows.map((r) => {
      const gid = r.groupId ?? r.group_id;
      const gname = r.groupName ?? r.group_name ?? `#${gid}`;
      const userCount = Number(r.userCount ?? r.user_count ?? 0);
      const groupDesc = r.groupDescription ?? r.group_description ?? '';
      const visible = !!r.visible;
      const metaParts = [];
      metaParts.push(I18n.t('addon_visibility.user_count', { n: userCount }));
      if (groupDesc) metaParts.push(escapeHtml(groupDesc));
      const metaLine = metaParts.join(' · ');

      const statusLabel = visible
        ? I18n.t('addon_visibility.status_visible')
        : I18n.t('addon_visibility.status_hidden');
      const statusChipStyle = visible
        ? 'background:rgba(96,165,250,0.15);color:var(--info);'
        : 'background:var(--bg-3);color:var(--text-3);';

      return `
        <div class="group-row${visible ? ' selected' : ''}" data-group-id="${escapeAttr(String(gid))}">
          <div class="g-ico"><svg class="icon"><use href="#i-users"/></svg></div>
          <div class="g-info">
            <div class="g-name">${escapeHtml(gname)}</div>
            <div class="g-meta">${metaLine}</div>
          </div>
          <span class="status-pill" style="padding:2px 8px;border-radius:999px;font-size:10px;font-weight:700;text-transform:uppercase;${statusChipStyle}">
            ${escapeHtml(statusLabel)}
          </span>
          <tf-toggle data-vis-toggle ${visible ? 'checked' : ''}></tf-toggle>
        </div>
      `;
    }).join('');

    list.querySelectorAll('.group-row[data-group-id]').forEach((row) => {
      const gid = Number(row.dataset.groupId);
      const tgl = row.querySelector('tf-toggle[data-vis-toggle]');
      tgl?.addEventListener('change', async (e) => {
        const visible = !!e.detail?.checked;
        row.classList.toggle('selected', visible);
        const pill = row.querySelector('.status-pill');
        if (pill) {
          pill.textContent = visible
            ? I18n.t('addon_visibility.status_visible')
            : I18n.t('addon_visibility.status_hidden');
          pill.setAttribute('style', 'padding:2px 8px;border-radius:999px;font-size:10px;font-weight:700;text-transform:uppercase;' + (visible
            ? 'background:rgba(96,165,250,0.15);color:var(--info);'
            : 'background:var(--bg-3);color:var(--text-3);'));
        }
        echoGuard.markLocal(`group:${gid}:`);
        try {
          await ApiBinary.action('addonVisibilitySetRequest', {
            addonId: currentAddonId,
            groupId: gid,
            visible,
          });
          toast(I18n.t('addon_visibility.saved'), 'success');
        } catch (err) {
          row.classList.toggle('selected', !visible);
          if (pill) {
            pill.textContent = (!visible)
              ? I18n.t('addon_visibility.status_visible')
              : I18n.t('addon_visibility.status_hidden');
          }
          if (visible) tgl.removeAttribute('checked'); else tgl.setAttribute('checked', '');
          toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
        }
      });
    });
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}
