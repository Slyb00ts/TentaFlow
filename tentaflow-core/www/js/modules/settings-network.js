// =============================================================================
// Plik: modules/settings-network.js
// Opis: Zakladka "Mesh i Siec" w ekranie Ustawienia. Pozwala wybrac tryb bind
//       QUIC (auto / custom na wybrany interfejs), przegladac wykryte interfejsy
//       hosta, wlaczac globalne filtry (Docker / link-local / loopback / CGNAT /
//       prefer-same-subnet) oraz ustawic iroh relay URL. Komunikuje sie z
//       backendem przez binary protocol (ApiBinary): `networkInterfacesListRequest`
//       + `networkConfigGetRequest` przy ladowaniu, `networkConfigUpdateRequest`
//       przy zapisie. Zmiana bind_mode/bind_ipv4/relay_url wymaga restartu mesh
//       (backend zwraca flage `restartRequired`).
// =============================================================================

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';

// Stan modulu — odswiezany przy kazdym wejsciu w zakladke.
let interfaces = [];
let config = defaultConfig();
let filterKind = 'all';      // 'all' | 'lan' | 'vpn' | 'virtual' | 'down'
let searchQuery = '';
let pendingRestart = false;

function defaultConfig() {
  return {
    bindMode: 'auto',
    bindIpv4: '',
    hideDocker: true,
    hideLinkLocal: true,
    hideLoopback: true,
    hideCgnat: false,
    preferSameSubnet: true,
    irohRelayUrl: '',
  };
}

// Kategoryzacja `kind` z backendu na semantyczne role GUI (pill + filtr).
function kindToRole(kind) {
  switch (kind) {
    case 'ethernet': return 'lan';
    case 'wifi': return 'wifi';
    case 'tunnel': return 'vpn';
    case 'docker':
    case 'virtual': return 'virt';
    case 'loopback': return 'loop';
    default: return 'lan';
  }
}

function roleLabel(role) {
  switch (role) {
    case 'lan': return I18n.t('settings.mesh.kind_ethernet');
    case 'wifi': return I18n.t('settings.mesh.kind_wifi');
    case 'vpn': return I18n.t('settings.mesh.kind_vpn');
    case 'virt': return I18n.t('settings.mesh.kind_virtual');
    case 'loop': return I18n.t('settings.mesh.kind_loopback');
    default: return role;
  }
}

function kindIcon(kind) {
  // Uzywamy istniejacych ikon z global sprite — svg `<use href="#i-...">`.
  switch (kind) {
    case 'wifi': return 'network-svg';
    case 'tunnel': return 'shield';
    case 'docker': return 'registry';
    case 'virtual': return 'chip';
    case 'loopback': return 'refresh';
    case 'ethernet':
    default: return 'network';
  }
}

// ==========================================================================
// Public API (wywolywane z settings.js)
// ==========================================================================

/**
 * Laduje stan z backendu i zwraca HTML zakladki (string). Caller wstawia do
 * `innerHTML` a potem wola `bindMeshTab(host, onChange)`.
 */
export async function renderMeshTab() {
  await loadAll();
  return renderAll();
}

/**
 * Podpina event listenery (input/change/click) do juz zrenderowanego DOM.
 * `host` to element zawierajacy zakladke; `rerender` jest wolane gdy trzeba
 * ponownie wyrysowac (bez refetchu).
 */
export function bindMeshTab(host, rerender) {
  if (!host) return;

  // Tryb bind — karty radio.
  host.querySelectorAll('[data-bind-mode]').forEach((card) => {
    card.addEventListener('click', () => {
      const mode = card.dataset.bindMode;
      if (!mode || config.bindMode === mode) return;
      config.bindMode = mode;
      if (mode === 'auto') config.bindIpv4 = '';
      rerender();
    });
  });

  // Search — filtr lokalny (debounce z komponentu).
  host.querySelector('#mesh-search')?.addEventListener('search', (e) => {
    searchQuery = (e.detail?.value || '').toLowerCase();
    rerender();
  });

  // Chips rodzaju (all/lan/vpn/virtual/down).
  host.querySelectorAll('[data-filter-kind]').forEach((chip) => {
    chip.addEventListener('click', () => {
      filterKind = chip.dataset.filterKind;
      rerender();
    });
  });

  // Radio bind (w wierszach tabeli). Klik ustawia config.bindIpv4 i wymusza
  // bindMode='custom'.
  host.querySelectorAll('[data-bind-ipv4]').forEach((btn) => {
    btn.addEventListener('click', () => {
      if (btn.classList.contains('disabled')) return;
      const ip = btn.dataset.bindIpv4;
      if (!ip) return;
      config.bindIpv4 = ip;
      config.bindMode = 'custom';
      rerender();
    });
  });

  // Globalne toggle filtrow.
  host.querySelectorAll('[data-filter-key]').forEach((tgl) => {
    tgl.addEventListener('change', (e) => {
      const key = tgl.dataset.filterKey;
      if (!key) return;
      config[key] = !!e.detail?.checked;
      rerender();
    });
  });

  // Relay URL input.
  host.querySelector('#relay-url')?.addEventListener('input', (e) => {
    // tf-input emituje natywny `input`; czytamy przez `.value` komponentu.
    const target = e.currentTarget;
    config.irohRelayUrl = (target.value || '').trim();
  });

  host.querySelector('#relay-reset')?.addEventListener('click', () => {
    config.irohRelayUrl = '';
    rerender();
  });

  // Save.
  host.querySelector('#mesh-save')?.addEventListener('click', async () => {
    if (config.bindMode === 'custom' && !config.bindIpv4) {
      toast(I18n.t('settings.mesh.save_custom_no_ip'), 'error');
      return;
    }
    try {
      const resp = await ApiBinary.action('networkConfigUpdateRequest', {
        bindMode: config.bindMode,
        bindIpv4: config.bindIpv4 || '',
        hideDocker: !!config.hideDocker,
        hideLinkLocal: !!config.hideLinkLocal,
        hideLoopback: !!config.hideLoopback,
        hideCgnat: !!config.hideCgnat,
        preferSameSubnet: !!config.preferSameSubnet,
        irohRelayUrl: config.irohRelayUrl || '',
      });
      pendingRestart = !!(resp?.restartRequired ?? resp?.restart_required);
      toast(I18n.t('settings.mesh.save_success'), 'success');
      rerender();
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message || err}`, 'error');
    }
  });

  host.querySelector('#mesh-defaults')?.addEventListener('click', () => {
    config = defaultConfig();
    rerender();
  });

  // Refresh listy interfejsow.
  host.querySelector('#mesh-refresh')?.addEventListener('click', async () => {
    try {
      const resp = await ApiBinary.one('networkInterfacesListRequest');
      interfaces = Array.isArray(resp?.interfaces) ? resp.interfaces : [];
      rerender();
    } catch (err) {
      toast(`${I18n.t('common.error')}: ${err.message || err}`, 'error');
    }
  });
}

// ==========================================================================
// Ladowanie danych
// ==========================================================================

async function loadAll() {
  try {
    const [ifacesResp, cfgResp] = await Promise.all([
      ApiBinary.one('networkInterfacesListRequest').catch(() => ({ interfaces: [] })),
      ApiBinary.one('networkConfigGetRequest').catch(() => ({ config: defaultConfig() })),
    ]);
    interfaces = Array.isArray(ifacesResp?.interfaces) ? ifacesResp.interfaces : [];
    const raw = cfgResp?.config || cfgResp || {};
    config = {
      bindMode: String(raw.bindMode ?? raw.bind_mode ?? 'auto'),
      bindIpv4: String(raw.bindIpv4 ?? raw.bind_ipv4 ?? ''),
      hideDocker: !!(raw.hideDocker ?? raw.hide_docker),
      hideLinkLocal: !!(raw.hideLinkLocal ?? raw.hide_link_local),
      hideLoopback: !!(raw.hideLoopback ?? raw.hide_loopback),
      hideCgnat: !!(raw.hideCgnat ?? raw.hide_cgnat),
      preferSameSubnet: !!(raw.preferSameSubnet ?? raw.prefer_same_subnet),
      irohRelayUrl: String(raw.irohRelayUrl ?? raw.iroh_relay_url ?? ''),
    };
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message || err}`, 'error');
    interfaces = [];
    config = defaultConfig();
  }
}

// ==========================================================================
// Render glowny
// ==========================================================================

function renderAll() {
  return `
    <div class="settings-mesh-tab">
      ${renderBanner()}
      ${renderBindModeSection()}
      ${renderInterfacesSection()}
      ${renderAdvertisePreviewSection()}
      ${renderFiltersSection()}
      ${renderRelaySection()}
      ${renderSaveFooter()}
      ${renderRestartBanner()}
    </div>
  `;
}

// --- Banner ostrzegawczy dla multi-NIC + auto bind -----------------------

function renderBanner() {
  // Pokazuj ostrzezenie tylko gdy >=2 aktywne interfejsy fizyczne i bind=auto.
  const physical = interfaces.filter((i) => i.isUp && (i.kind === 'ethernet' || i.kind === 'wifi'));
  if (config.bindMode !== 'auto' || physical.length < 2) return '';
  const names = physical.map((i) => i.name).slice(0, 3).join(', ');
  return `
    <div class="net-banner">
      <svg viewBox="0 0 24 24"><path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"/><path d="M12 9v4M12 17h.01"/></svg>
      <div>
        <strong>${escapeHtml(I18n.t('settings.mesh.banner_multi_nic_title'))}</strong>
        ${escapeHtml(I18n.t('settings.mesh.banner_multi_nic', { names }))}
      </div>
    </div>
  `;
}

// --- Tryb bind (karty radio) ---------------------------------------------

function renderBindModeSection() {
  const auto = config.bindMode === 'auto';
  return `
    <div class="tf-section-card">
      <h3>
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="4" y="8" width="16" height="12" rx="2"/><path d="M8 8V5a4 4 0 0 1 8 0v3"/></svg>
        ${escapeHtml(I18n.t('settings.mesh.bind_mode_title'))}
      </h3>
      <div class="section-sub">${escapeHtml(I18n.t('settings.mesh.bind_mode_sub'))}</div>
      <div class="bind-mode-grid">
        <div class="bind-mode-card ${auto ? 'active' : ''}" data-bind-mode="auto" role="button" tabindex="0">
          <div class="bind-radio ${auto ? 'on' : ''}"></div>
          <div>
            <div class="t">${escapeHtml(I18n.t('settings.mesh.bind_mode_auto'))} <span class="status-pill off">${escapeHtml(I18n.t('settings.mesh.bind_mode_auto_badge'))}</span></div>
            <div class="d">${I18n.t('settings.mesh.bind_mode_auto_desc')}</div>
          </div>
        </div>
        <div class="bind-mode-card ${!auto ? 'active' : ''}" data-bind-mode="custom" role="button" tabindex="0">
          <div class="bind-radio ${!auto ? 'on' : ''}"></div>
          <div>
            <div class="t">${escapeHtml(I18n.t('settings.mesh.bind_mode_custom'))} <span class="status-pill local">${escapeHtml(I18n.t('settings.mesh.bind_mode_custom_badge'))}</span></div>
            <div class="d">${I18n.t('settings.mesh.bind_mode_custom_desc')}</div>
          </div>
        </div>
      </div>
    </div>
  `;
}

// --- Interfejsy (tabela) -------------------------------------------------

function interfaceMatchesFilter(iface) {
  const role = kindToRole(iface.kind);
  if (filterKind === 'lan' && !(role === 'lan' || role === 'wifi')) return false;
  if (filterKind === 'vpn' && role !== 'vpn') return false;
  if (filterKind === 'virtual' && role !== 'virt') return false;
  if (filterKind === 'down' && iface.isUp) return false;
  return true;
}

function interfaceMatchesSearch(iface) {
  if (!searchQuery) return true;
  const haystack = [
    iface.name || '',
    iface.description || '',
    ...(iface.ipv4Addrs || []),
  ].join(' ').toLowerCase();
  return haystack.includes(searchQuery);
}

function renderInterfacesSection() {
  const chipDef = [
    { id: 'all', label: I18n.t('settings.mesh.filter_all') },
    { id: 'lan', label: I18n.t('settings.mesh.filter_lan') },
    { id: 'vpn', label: I18n.t('settings.mesh.filter_vpn') },
    { id: 'virtual', label: I18n.t('settings.mesh.filter_virtual') },
    { id: 'down', label: I18n.t('settings.mesh.filter_down') },
  ];

  const chips = chipDef.map((c) => `
    <tf-chip clickable ${filterKind === c.id ? 'active' : ''} data-filter-kind="${escapeAttr(c.id)}">${escapeHtml(c.label)}</tf-chip>
  `).join('');

  const visible = interfaces.filter(interfaceMatchesFilter).filter(interfaceMatchesSearch);

  const rows = visible.length === 0
    ? `<tr><td colspan="7"><div class="empty-big" style="padding:24px;">${escapeHtml(I18n.t('settings.mesh.interfaces_empty'))}</div></td></tr>`
    : visible.map(renderInterfaceRow).join('');

  return `
    <div class="tf-section-card flush">
      <h3>
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="11" width="18" height="8" rx="1"/><path d="M7 11V7h10v4"/></svg>
        ${escapeHtml(I18n.t('settings.mesh.interfaces_title'))}
      </h3>
      <div class="section-sub">${escapeHtml(I18n.t('settings.mesh.interfaces_sub'))}</div>
      <div style="padding: 0 20px 14px;">
        <div class="users-toolbar">
          <tf-searchbox id="mesh-search" placeholder="${escapeAttr(I18n.t('settings.mesh.search_placeholder'))}" value="${escapeAttr(searchQuery)}"></tf-searchbox>
          <div class="tf-filter-group">${chips}</div>
          <div style="margin-left:auto;">
            <tf-button variant="ghost" size="sm" icon="refresh" id="mesh-refresh">${escapeHtml(I18n.t('settings.mesh.rescan'))}</tf-button>
          </div>
        </div>
      </div>
      <table class="tf-accounts-table">
        <thead>
          <tr>
            <th style="width: 60px; padding-left: 20px;">${escapeHtml(I18n.t('settings.mesh.col_bind'))}</th>
            <th>${escapeHtml(I18n.t('settings.mesh.col_interface'))}</th>
            <th>${escapeHtml(I18n.t('settings.mesh.col_ip'))}</th>
            <th>${escapeHtml(I18n.t('settings.mesh.col_kind'))}</th>
            <th>${escapeHtml(I18n.t('settings.mesh.col_status'))}</th>
            <th style="width: 100px;">${escapeHtml(I18n.t('settings.mesh.col_advertise'))}</th>
            <th class="actions-col" style="padding-right: 20px;">${escapeHtml(I18n.t('common.actions'))}</th>
          </tr>
        </thead>
        <tbody>${rows}</tbody>
      </table>
    </div>
  `;
}

function renderInterfaceRow(iface) {
  const role = kindToRole(iface.kind);
  const ipv4 = iface.ipv4Addrs || [];
  const primary = ipv4[0] || '';
  const rest = ipv4.slice(1);
  const isLoop = iface.kind === 'loopback';
  const selected = config.bindMode === 'custom' && primary && config.bindIpv4 === primary;

  const ipCell = primary
    ? `<div class="ip-mono">${escapeHtml(primary)}</div>${rest.map((a) => `<div class="ip-mono-sub">${escapeHtml(a)}</div>`).join('')}`
    : `<div class="ip-mono-sub">—</div>`;

  const statusPill = iface.isUp
    ? `<span class="status-pill ok">UP</span>`
    : `<span class="status-pill off">DOWN</span>`;

  const advertiseOn = iface.isUp && !isLoop && !(config.hideDocker && role === 'virt' && (iface.name || '').startsWith('docker'));
  const advertiseAttr = advertiseOn ? 'checked' : '';
  const advertiseDisabled = isLoop ? 'disabled' : '';

  const rowClass = [];
  if (selected) rowClass.push('selected');
  if (isLoop) rowClass.push('disabled');

  const bindBtnDisabled = isLoop || !primary || !iface.isUp;

  return `
    <tr class="${rowClass.join(' ')}">
      <td style="padding-left: 20px;">
        <div class="bind-radio ${selected ? 'on' : ''} ${bindBtnDisabled ? 'disabled' : ''}"
             data-bind-ipv4="${escapeAttr(primary)}"
             role="button" tabindex="${bindBtnDisabled ? '-1' : '0'}"
             aria-label="${escapeAttr(I18n.t('settings.mesh.bind_to', { name: iface.name || '' }))}"
             title="${escapeAttr(I18n.t('settings.mesh.bind_to', { name: iface.name || '' }))}"></div>
      </td>
      <td>
        <div class="tf-account-cell">
          <div class="tf-account-avatar">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><use href="#i-${kindIcon(iface.kind)}"/></svg>
          </div>
          <div>
            <div class="tf-account-name">${escapeHtml(iface.name || '—')}${selected ? ` <span class="group-tag">${escapeHtml(I18n.t('settings.mesh.bind_target'))}</span>` : ''}</div>
            <div class="tf-account-sub">${escapeHtml(iface.description || '')} · MTU ${Number(iface.mtu) || 0}</div>
          </div>
        </div>
      </td>
      <td>${ipCell}</td>
      <td><span class="role-pill role-${role}">${escapeHtml(roleLabel(role))}</span></td>
      <td>${statusPill}</td>
      <td><tf-toggle ${advertiseAttr} ${advertiseDisabled}></tf-toggle></td>
      <td class="actions-col" style="padding-right: 20px;">
        <div class="row-actions">
          <tf-button variant="ghost" size="sm" icon="info" title="${escapeAttr(I18n.t('common.details'))}"></tf-button>
        </div>
      </td>
    </tr>
  `;
}

// --- Podglad listy wysylanych adresow ------------------------------------

function isIpv4LinkLocal(ip) { return ip.startsWith('169.254.'); }
function isIpv4Loopback(ip) { return ip.startsWith('127.'); }
function isIpv4Cgnat(ip) {
  // 100.64.0.0/10 — druga oktet 64..127.
  if (!ip.startsWith('100.')) return false;
  const second = parseInt(ip.split('.')[1] || '0', 10);
  return second >= 64 && second <= 127;
}

function computeAdvertise() {
  const rows = [];
  for (const iface of interfaces) {
    const role = kindToRole(iface.kind);
    for (const ip of iface.ipv4Addrs || []) {
      let included = true;
      let reason = '';
      if (!iface.isUp) { included = false; reason = 'down'; }
      else if (config.hideLoopback && (iface.kind === 'loopback' || isIpv4Loopback(ip))) { included = false; reason = 'loopback'; }
      else if (config.hideLinkLocal && isIpv4LinkLocal(ip)) { included = false; reason = 'link-local'; }
      else if (config.hideDocker && iface.kind === 'docker') { included = false; reason = 'docker'; }
      else if (config.hideCgnat && isIpv4Cgnat(ip) && iface.kind !== 'tunnel') { included = false; reason = 'cgnat'; }
      else if (config.bindMode === 'custom' && config.bindIpv4 && ip !== config.bindIpv4) {
        // W custom mode tylko wybrany adres idzie do peerow.
        included = false; reason = 'not-bound';
      }
      rows.push({ iface: iface.name, role, roleLabel: roleLabel(role), ip, included, reason });
    }
  }
  return rows;
}

function reasonLabel(reason) {
  switch (reason) {
    case 'down': return I18n.t('settings.mesh.reason_down');
    case 'loopback': return I18n.t('settings.mesh.reason_loopback');
    case 'link-local': return I18n.t('settings.mesh.reason_link_local');
    case 'docker': return I18n.t('settings.mesh.reason_docker');
    case 'cgnat': return I18n.t('settings.mesh.reason_cgnat');
    case 'not-bound': return I18n.t('settings.mesh.reason_not_bound');
    default: return reason;
  }
}

function renderAdvertisePreviewSection() {
  const rows = computeAdvertise();
  const active = rows.filter((r) => r.included).length;
  const list = rows.length === 0
    ? `<div class="adv-row filtered"><span class="label">—</span><span class="addr">${escapeHtml(I18n.t('settings.mesh.advertise_empty'))}</span></div>`
    : rows.map((r) => `
      <div class="adv-row ${r.included ? 'included' : 'filtered'}">
        <span class="label">${escapeHtml(r.iface || '')} · ${escapeHtml(r.roleLabel)}</span>
        <span class="addr">${escapeHtml(r.ip)}</span>
        <span class="status-pill ${r.included ? 'ok' : 'off'}" style="margin-left:auto;">
          ${r.included ? escapeHtml(I18n.t('settings.mesh.advertise_sent')) : escapeHtml(I18n.t('settings.mesh.advertise_filter', { reason: reasonLabel(r.reason) }))}
        </span>
      </div>
    `).join('');

  return `
    <div class="tf-section-card">
      <h3>
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M2 12a10 10 0 0 1 20 0"/><path d="M5 12a7 7 0 0 1 14 0"/><path d="M8 12a4 4 0 0 1 8 0"/></svg>
        ${escapeHtml(I18n.t('settings.mesh.advertise_preview_title'))}
        <span class="status-pill ok" style="margin-left: 4px;">${escapeHtml(I18n.t('settings.mesh.advertise_active', { count: active }))}</span>
      </h3>
      <div class="section-sub">${escapeHtml(I18n.t('settings.mesh.advertise_preview_sub'))}</div>
      <div class="adv-list">${list}</div>
    </div>
  `;
}

// --- Filtry globalne -----------------------------------------------------

function renderFiltersSection() {
  const filters = [
    { key: 'hideDocker', tKey: 'filter_docker', sub: '172.17.0.0/16 — 172.31.0.0/16' },
    { key: 'hideLinkLocal', tKey: 'filter_link_local', sub: 'fe80::/10, 169.254.0.0/16' },
    { key: 'hideLoopback', tKey: 'filter_loopback', sub: '127.0.0.0/8, ::1' },
    { key: 'hideCgnat', tKey: 'filter_cgnat', sub: '100.64.0.0/10' },
    { key: 'preferSameSubnet', tKey: 'filter_prefer_subnet', sub: 'reorder direct_addrs' },
  ];

  const rows = filters.map((f) => `
    <div class="net-filter-row">
      <div>
        <div class="t">${escapeHtml(I18n.t(`settings.mesh.${f.tKey}`))}</div>
        <div class="d">${escapeHtml(f.sub)}</div>
      </div>
      <tf-toggle ${config[f.key] ? 'checked' : ''} data-filter-key="${escapeAttr(f.key)}"></tf-toggle>
    </div>
  `).join('');

  return `
    <div class="tf-section-card">
      <h3>
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18M6 12h12M10 18h4"/></svg>
        ${escapeHtml(I18n.t('settings.mesh.filters_title'))}
      </h3>
      <div class="section-sub">${escapeHtml(I18n.t('settings.mesh.filters_sub'))}</div>
      ${rows}
    </div>
  `;
}

// --- Relay URL -----------------------------------------------------------

function renderRelaySection() {
  return `
    <div class="tf-section-card">
      <h3>
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="4"/><path d="M2 12h4M18 12h4M12 2v4M12 18v4"/></svg>
        ${escapeHtml(I18n.t('settings.mesh.relay_title'))}
      </h3>
      <div class="section-sub">${escapeHtml(I18n.t('settings.mesh.relay_sub'))}</div>
      <div class="field" style="max-width:560px;">
        <tf-input
          id="relay-url"
          label="${escapeAttr(I18n.t('settings.mesh.relay_label'))}"
          value="${escapeAttr(config.irohRelayUrl || '')}"
          placeholder="${escapeAttr(I18n.t('settings.mesh.relay_placeholder'))}"
          hint="${escapeAttr(I18n.t('settings.mesh.relay_hint'))}"
        ></tf-input>
      </div>
      <div class="section-footer">
        <tf-button variant="ghost" id="relay-reset">${escapeHtml(I18n.t('settings.mesh.relay_reset'))}</tf-button>
      </div>
    </div>
  `;
}

// --- Save footer + restart banner ----------------------------------------

function renderSaveFooter() {
  return `
    <div class="tf-section-card" style="display:flex; justify-content:flex-end; gap:10px;">
      <tf-button variant="ghost" id="mesh-defaults">${escapeHtml(I18n.t('settings.mesh.save_reset_defaults'))}</tf-button>
      <tf-button variant="primary" icon="check" id="mesh-save">${escapeHtml(I18n.t('settings.mesh.save'))}</tf-button>
    </div>
  `;
}

function renderRestartBanner() {
  if (!pendingRestart) return '';
  return `
    <div class="net-banner" style="background: rgba(96, 165, 250, 0.08); border-color: rgba(96, 165, 250, 0.22);">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="stroke: var(--info);"><path d="M21 12a9 9 0 1 1-3-6.7L21 8"/><path d="M21 3v5h-5"/></svg>
      <div>
        <strong>${escapeHtml(I18n.t('settings.mesh.restart_required_title'))}</strong>
        ${escapeHtml(I18n.t('settings.mesh.restart_required'))}
      </div>
    </div>
  `;
}
