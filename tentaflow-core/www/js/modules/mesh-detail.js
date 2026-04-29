// =============================================================================
// Plik: modules/mesh-detail.js
// Opis: Drill-down view pojedynczego noda mesh. Layout zgodny z Mesh & Network
//       settings: tf-screen shell, breadcrumb, detail-header, section cards.
//       Profilowanie jest jedyna sciezka — multi-source przez ProfilingLaunchModal,
//       z banerem aktywnej sesji i routingiem do globalnej listy sesji /
//       szczegolow raportu.
// =============================================================================

import {
  escapeHtml,
  escapeAttr,
  toast,
  formatMb,
  formatBytes,
} from '/js/utils.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { patchInner } from '/js/lib/patch.js';
import { createRefresher } from '/js/lib/refresh.js';
import { isOnline as isOnlineHelper } from '/js/modules/mesh-helpers.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import { ProfilingLaunchModal } from '/js/modules/profiling-launch.js';
import { ProfilingActiveBanner } from '/js/modules/profiling-active-banner.js';

let currentNodeId = null;
let nodeData = null;
let refresher = null;
let wasDisconnected = false;
let lastFetchAt = null;
let backHandler = null;
let containerHandler = null;
let profileHandler = null;
// Banner aktywnej sesji profilingu — montowany pod headerem detail view.
// Sam polluje backend (`profilingActiveInfo`) co 1s, sam renderuje countdown
// i przycisk Stop. `bannerNodeId` chroni przed wspoldzieleniem instancji
// miedzy nodami przy nawigacji bez full cleanup.
let activeBanner = null;
let bannerNodeId = null;

// Inline SVG przez <use href="#i-..."> — sprite definiuje symbole raz w
// index.html. Nie parsujemy zadnego SVG przy kazdym renderDetail.
const ico = (id, extraClass = '') =>
  `<svg viewBox="0 0 24 24"${extraClass ? ` class="${extraClass}"` : ''} fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><use href="#${id}"/></svg>`;

const MeshDetailScreen = {
  title: 'Node',
  async show(nodeId) {
    if (!nodeId) return;
    currentNodeId = nodeId;
    nodeData = null;
    wasDisconnected = false;
    lastFetchAt = null;

    const content = document.getElementById('main');
    if (!content) return;
    content.innerHTML = renderSkeleton();
    bindBack(content);

    await loadNode();
    renderDetail();

    refresher = createRefresher({
      run: async () => {
        if (!currentNodeId) { MeshDetailScreen.cleanup(); return; }
        if (!document.querySelector('.nd-shell')) { MeshDetailScreen.cleanup(); return; }
        await loadNode();
        if (currentNodeId && document.querySelector('.nd-shell')) renderDetail();
      },
      intervalMs: 2000,
      hiddenIntervalMs: 5000,
    });
    refresher.start();
  },
  cleanup() {
    if (refresher) { refresher.dispose(); refresher = null; }
    if (activeBanner) {
      try { activeBanner.unmount(); } catch (_e) { /* ignore */ }
      activeBanner = null;
      bannerNodeId = null;
    }
    const root = document.getElementById('main');
    if (root) {
      if (backHandler) root.removeEventListener('click', backHandler);
      if (containerHandler) root.removeEventListener('click', containerHandler);
      if (profileHandler) root.removeEventListener('click', profileHandler);
    }
    backHandler = null;
    containerHandler = null;
    profileHandler = null;
    currentNodeId = null;
    nodeData = null;
  },
};

// ---- Data ----------------------------------------------------------------

async function loadNode() {
  if (!currentNodeId) return;
  try {
    const resp = await ApiBinary.one('meshNodeDetailRequest', { nodeId: currentNodeId });
    const wasDc = wasDisconnected;
    nodeData = resp.node;
    lastFetchAt = Date.now();
    wasDisconnected = false;
    if (wasDc) toast(I18n.t('mesh.reconnected'), 'success');
  } catch (err) {
    const age = lastFetchAt ? (Date.now() - lastFetchAt) / 1000 : Infinity;
    if (age > 30) wasDisconnected = true;
  }
}

// ---- Helpers -------------------------------------------------------------

function bindBack(root) {
  // Idempotentne — listener jest dodawany raz na cykl zycia ekranu.
  // Trzymamy referencje do handlera w module zeby cleanup() mogl go usunac
  // (root #main jest wspoldzielony miedzy ekranami — bez removeEventListener
  // listenery z poprzednich wizyt by sie nakladaly).
  if (backHandler) return;
  backHandler = (e) => {
    const back = e.target.closest('#btn-back-mesh, .nd-back-crumb');
    if (back) {
      MeshDetailScreen.cleanup();
      import('/js/router.js').then(({ Router }) => Router.navigate('mesh'));
    }
  };
  root.addEventListener('click', backHandler);
}

function gaugeLevel(pct) {
  if (pct == null) return '';
  if (pct > 80) return 'hot';
  if (pct >= 50) return 'warm';
  return '';
}

function isOnline(n) {
  return isOnlineHelper(n);
}

function shortId(id) {
  if (!id) return '';
  return id.length > 12 ? id.slice(0, 12) : id;
}

function freshnessClass() {
  const age = lastFetchAt ? (Date.now() - lastFetchAt) / 1000 : 0;
  if (age < 5) return '';
  if (age <= 30) return ' stale';
  return ' disconnected';
}

function ifaceRoleClass(iface) {
  const t = String(iface.interface_type || '').toLowerCase();
  if (t === 'thunderbolt') return 'tb';
  if (t === 'wifi' || t === 'wireless') return 'wifi';
  if (t === 'vpn' || t === 'wireguard' || t === 'tailscale') return 'vpn';
  if (t === 'virtual' || t === 'docker' || t === 'bridge') return 'virt';
  if (t === 'loopback') return 'loop';
  return 'lan';
}

function ifaceRoleLabel(iface) {
  const cls = ifaceRoleClass(iface);
  switch (cls) {
    case 'tb': return 'TB';
    case 'wifi': return 'Wi-Fi';
    case 'vpn': return 'VPN';
    case 'virt': return 'Virtual';
    case 'loop': return 'Loopback';
    default: return 'LAN';
  }
}

function ifaceIconSvg(iface) {
  const cls = ifaceRoleClass(iface);
  const map = {
    tb: 'i-iface-tb',
    wifi: 'i-iface-wifi',
    vpn: 'i-iface-vpn',
    virt: 'i-iface-virt',
    loop: 'i-iface-loop',
  };
  return ico(map[cls] || 'i-iface-lan');
}

function speedLabel(mbps) {
  if (!mbps) return '—';
  if (mbps >= 1000) return `${Math.round(mbps / 1000)} G`;
  return `${mbps} M`;
}

function connectionTransportLabel(value) {
  if (value === 'p2p') return I18n.t('mesh.connection_p2p');
  if (value === 'relay') return I18n.t('mesh.connection_relay');
  if (value === 'custom') return I18n.t('mesh.connection_custom');
  return I18n.t('mesh.connection_unknown');
}

function connectionScopeLabel(value) {
  if (value === 'lan') return I18n.t('mesh.connection_lan');
  if (value === 'wan') return I18n.t('mesh.connection_wan');
  return value || '';
}

// ---- Skeleton ------------------------------------------------------------

function renderSkeleton() {
  return `
    <div class="nd-shell">
      <div class="nd-head">
        <div class="nd-head__crumbs">
          <div class="nd-breadcrumb">
            <span class="crumb nd-back-crumb">Mesh</span>
            <span class="sep">${ico('i-chevron-right')}</span>
            <span class="crumb current">…</span>
          </div>
        </div>
        <div class="nd-head__header">
          <div class="nd-detail-header">
            <div class="big-ico">${ico('i-host')}</div>
            <div class="d-meta">
              <div class="d-name"><span class="skeleton" style="display:inline-block;width:200px;height:24px;"></span></div>
            </div>
          </div>
        </div>
      </div>
    </div>
  `;
}

// ---- Render --------------------------------------------------------------

function renderDetail() {
  const content = document.getElementById('main');
  if (!content) return;

  if (!nodeData) {
    content.innerHTML = `
      <div class="nd-shell">
        <div class="nd-head">
          <div class="nd-head__crumbs">
            <div class="nd-breadcrumb">
              <span class="crumb nd-back-crumb">${escapeHtml(I18n.t('mesh.back_to_mesh'))}</span>
            </div>
          </div>
        </div>
        <div class="nd-body">
          <div class="nd-empty">${escapeHtml(I18n.t('mesh.load_error') || 'Nie udalo sie zaladowac danych noda')}</div>
        </div>
      </div>
    `;
    bindBack(content);
    return;
  }

  const n = nodeData;
  const hostname = n.hostname || shortId(n.node_id) || I18n.t('mesh.unknown_host');
  const online = isOnline(n);

  const html = `
    <div class="nd-shell${freshnessClass()}">
      ${renderHead(n, hostname, online)}
      <div class="nd-active-banner-host" data-banner-host></div>
      <div class="nd-body">
        ${renderSystemInfo(n)}
        ${renderResources(n)}
        ${renderGpus(n)}
        ${renderProfilingWrap(n)}
        ${renderNetwork(n)}
        ${renderModels(n)}
        ${renderContainers(n)}
      </div>
    </div>
  `;
  // Diff zamiast full innerHTML — zachowuje fokus, animacje meterów
  // i scroll w zagniezdzonych kontenerach. Custom elements (`tf-*`) nie
  // sa rekurencyjnie morphowane (patrz lib/patch.js).
  patchInner(content, html);
  bindContainerActions(content);
  bindProfileActions(content, n);
  ensureActiveBanner(content, n);
}

// Idempotentnie montuje ProfilingActiveBanner do `[data-banner-host]`. Banner
// sam polluje `profilingActiveInfo` co 1s i sam dba o show/hide oraz
// countdown. Wymontowujemy gdy zmienia sie node id (nawigacja A->B bez
// pelnego cleanup).
function ensureActiveBanner(root, n) {
  const host = root.querySelector('[data-banner-host]');
  if (!host) return;
  if (activeBanner && bannerNodeId !== n.node_id) {
    try { activeBanner.unmount(); } catch (_e) { /* ignore */ }
    activeBanner = null;
    bannerNodeId = null;
  }
  if (!activeBanner) {
    const nodeId = n.node_id;
    activeBanner = new ProfilingActiveBanner({
      nodeId,
      // Po zakonczeniu sesji (timeout / external stop) — toast z akcjami:
      // "View report" otwiera profile-report; "View all sessions" otwiera
      // globalny ekran profilingu zfiltrowany na ten nod. User nie zostaje
      // bez sladu po zniknieciu bannera.
      onSessionEnded: (sessionId) => {
        const t = document.createElement('div');
        t.style.cssText = 'position:fixed;bottom:24px;left:50%;transform:translateX(-50%);background:var(--tf-bg-2,#0a0d24);color:var(--tf-text,#e8ebf5);border:1px solid var(--tf-border,#1f2548);border-radius:8px;padding:10px 14px;font-size:13px;z-index:9999;display:flex;gap:10px;align-items:center;box-shadow:0 12px 32px rgba(0,0,0,0.5);';
        t.innerHTML = `<span>Profiling session finished</span>`;
        const reportBtn = document.createElement('tf-button');
        reportBtn.setAttribute('size', 'sm');
        reportBtn.setAttribute('variant', 'primary');
        reportBtn.textContent = 'View report';
        reportBtn.addEventListener('click', () => {
          if (window.Router) window.Router.navigate('profile-report', { nodeId, sessionId });
          t.remove();
        });
        const listBtn = document.createElement('tf-button');
        listBtn.setAttribute('size', 'sm');
        listBtn.setAttribute('variant', 'ghost');
        listBtn.textContent = 'All sessions';
        listBtn.addEventListener('click', () => {
          if (window.Router) window.Router.navigate('profiling-sessions', { nodeId, nodeName: bannerNodeId ? null : null });
          t.remove();
        });
        t.appendChild(reportBtn);
        t.appendChild(listBtn);
        document.body.appendChild(t);
        setTimeout(() => t.remove(), 12000);
      },
    });
    activeBanner.mount(host);
    bannerNodeId = nodeId;
  } else if (activeBanner.root && activeBanner.root.parentNode !== host) {
    // Po patchInner host moze byc nowym elementem DOM — przepnij banner.
    host.appendChild(activeBanner.root);
  }
}

// Po sukcesie ProfilingLaunchModal banner sam zalapie sesje przy nastepnym
// 1s polleru, ale chcemy pokazac REC natychmiast — wymuszamy explicit poll.
function pokeActiveBanner() {
  if (activeBanner && typeof activeBanner._poll === 'function') {
    activeBanner._poll();
  }
}

function renderHead(n, hostname, online) {
  const conn = n.connection || {};
  const transport = conn.transport ? connectionTransportLabel(conn.transport) : null;
  const scope = conn.scope ? connectionScopeLabel(conn.scope) : '';
  const addr = conn.address || '';

  const subParts = [];
  if (n.os_info) {
    subParts.push(`<span>${ico('i-os')}${escapeHtml(n.os_info)}</span>`);
  }
  if (n.docker_version) {
    subParts.push(`<span>${ico('i-docker')}Docker ${escapeHtml(n.docker_version)}</span>`);
  }
  if (transport) {
    const txt = [transport, scope].filter(Boolean).join(' · ');
    subParts.push(`<span class="nd-conn-badge">${ico('i-arrow')}${escapeHtml(txt)}${addr ? ` · <span class="addr">${escapeHtml(addr)}</span>` : ''}</span>`);
  }

  const badges = [];
  badges.push(online
    ? `<span class="nd-stat-pill ok dot live">${escapeHtml(I18n.t('mesh.online'))}</span>`
    : `<span class="nd-stat-pill off dot">${escapeHtml(I18n.t('mesh.offline'))}</span>`);
  if (n.is_local) badges.push(`<span class="nd-stat-pill info">${escapeHtml(I18n.t('mesh.local'))}</span>`);
  // `nsys_available` wraz z `nsys_version` to capability flag z heartbeatu —
  // backend dalej go raportuje jako wskaznik dostepnosci NVIDIA Nsight Systems
  // dla collectora `nvidia.nsys.gpu`. Pokazujemy go tylko jako informacyjny pill.
  if (n.nsys_available === true) {
    const ver = n.nsys_version ? ` ${escapeHtml(n.nsys_version)}` : '';
    badges.push(`<span class="nd-stat-pill info">nsys${ver}</span>`);
  }

  const idChip = n.node_id ? `<span class="id-mono">${escapeHtml(shortId(n.node_id))}</span>` : '';

  return `
    <header class="nd-head">
      <nav class="nd-head__crumbs">
        <div class="nd-breadcrumb">
          <span class="crumb nd-back-crumb">${escapeHtml(I18n.t('mesh.title') || 'Mesh')}</span>
          <span class="sep">${ico('i-chevron-right')}</span>
          <span class="crumb current">${escapeHtml(hostname)}</span>
        </div>
      </nav>
      <div class="nd-head__header">
        <div class="nd-detail-header">
          <div class="big-ico">
            ${ico('i-host')}
            <span class="live-dot${online ? '' : ' off'}"></span>
          </div>
          <div class="d-meta">
            <div class="d-name">
              ${escapeHtml(hostname)}
              ${idChip}
            </div>
            ${subParts.length ? `<div class="d-sub">${subParts.join('')}</div>` : ''}
            <div class="d-badges">${badges.join('')}</div>
          </div>
          <div class="d-actions">
            ${profileTopbarHtml(n)}
            <tf-button variant="ghost" size="sm" id="btn-back-mesh">
              ← ${escapeHtml(I18n.t('mesh.back_to_mesh'))}
            </tf-button>
          </div>
        </div>
      </div>
    </header>
  `;
}

function renderSystemInfo(n) {
  const ageSec = lastFetchAt ? Math.max(0, Math.round((Date.now() - lastFetchAt) / 1000)) : 0;
  const stats = [];
  if (n.cpu_model || n.cpu_count) {
    stats.push({
      label: 'CPU',
      icon: ico('i-cpu'),
      value: n.cpu_model || `${n.cpu_count} ${I18n.t('mesh.cpu_cores')}`,
    });
  }
  if (n.cpu_count) {
    stats.push({
      label: I18n.t('mesh.cores') || 'Cores',
      icon: ico('i-grid-rows'),
      value: `${n.cpu_count} ${I18n.t('mesh.cpu_cores')}`,
    });
  }
  if (n.ram_total_mb) {
    stats.push({
      label: I18n.t('mesh.memory') || 'RAM',
      icon: ico('i-ram'),
      value: formatMb(n.ram_total_mb),
    });
  }
  if (n.os_info) {
    stats.push({
      label: 'OS',
      icon: ico('i-os'),
      value: n.os_info,
      mono: true,
    });
  }
  if (n.docker_version) {
    stats.push({
      label: 'Docker',
      icon: ico('i-docker'),
      value: n.docker_version,
      mono: true,
    });
  }
  const gpuSummary = Array.isArray(n.gpus) && n.gpus.length > 0
    ? n.gpus.map(g => g.name).filter(Boolean).join(', ')
    : null;
  if (gpuSummary) {
    stats.push({
      label: 'GPU',
      icon: ico('i-gpu'),
      value: gpuSummary,
    });
  }

  if (stats.length === 0) return '';

  const cells = stats.map(s => `
    <div class="nd-stat">
      <div class="l">${s.icon}${escapeHtml(s.label)}</div>
      <div class="v${s.mono ? ' mono' : ''}" title="${escapeAttr(s.value)}">${escapeHtml(s.value)}</div>
    </div>
  `).join('');

  return `
    <div class="nd-section">
      <h3>
        ${ico('i-clock-glance')}
        ${escapeHtml(I18n.t('mesh.info') || 'System info')}
        <span class="h-actions">${escapeHtml(I18n.t('mesh.last_fetch') || 'Ostatnia aktualizacja')}: <strong style="color:var(--text-2);">${ageSec} s</strong></span>
      </h3>
      <div class="nd-stat-grid">${cells}</div>
    </div>
  `;
}

function renderResources(n) {
  const cpuPct = n.cpu_usage ?? n.cpu_usage_percent;
  const cpuTemp = n.cpu_temperature_c;
  const ramUsed = n.ram_used_mb;
  const ramTotal = n.ram_total_mb;
  const swapUsed = n.swap_used_mb;
  const swapTotal = n.swap_total_mb;

  if (cpuPct == null && ramUsed == null) return '';

  const cpuFill = cpuPct != null ? Math.min(100, Math.max(0, Math.round(cpuPct))) : 0;
  const cpuLevel = gaugeLevel(cpuPct);
  const cpuBig = cpuPct != null ? `${Math.round(cpuPct)}%` : '—';

  const ramPct = (ramUsed != null && ramTotal) ? Math.round((ramUsed / ramTotal) * 100) : null;
  const ramLevel = gaugeLevel(ramPct);
  const ramBig = ramUsed != null ? formatMb(ramUsed) : '—';

  const swapPct = (swapUsed != null && swapTotal && swapTotal > 0)
    ? Math.round((swapUsed / swapTotal) * 100) : null;

  const cpuMini = [];
  if (cpuTemp != null) cpuMini.push({ l: I18n.t('mesh.temperature') || 'Temp', v: `${Math.round(cpuTemp)}°C` });
  if (n.cpu_count) cpuMini.push({ l: I18n.t('mesh.cores') || 'Cores', v: `${n.cpu_count}` });
  if (n.load_avg_1) cpuMini.push({ l: 'Load', v: Number(n.load_avg_1).toFixed(2) });

  const cpuCard = cpuPct != null ? `
    <div class="nd-rcard">
      <div class="rc-head">
        <div class="title">
          ${ico('i-cpu')}
          ${escapeHtml(I18n.t('mesh.cpu'))}
        </div>
        <div class="big">${escapeHtml(cpuBig)}</div>
      </div>
      <div class="nd-meter">
        <div class="nd-meter-bar ${cpuLevel}"><div class="fill" style="width:${cpuFill}%;"></div></div>
        <div class="nd-meter-foot"><span>${100 - cpuFill}% idle</span>${cpuTemp != null ? `<span>${Math.round(cpuTemp)}°C</span>` : ''}</div>
      </div>
      ${cpuMini.length ? `<div class="mini-stats">${cpuMini.map(m => `<div class="mini-stat"><div class="ml">${escapeHtml(m.l)}</div><div class="mv">${escapeHtml(m.v)}</div></div>`).join('')}</div>` : ''}
    </div>
  ` : '';

  const ramCard = ramUsed != null ? `
    <div class="nd-rcard">
      <div class="rc-head">
        <div class="title">
          ${ico('i-ram')}
          ${escapeHtml(I18n.t('mesh.ram') || 'RAM')}
        </div>
        <div class="big">${escapeHtml(ramBig)}</div>
      </div>
      <div class="nd-meter">
        <div class="nd-meter-head"><span class="t">RAM</span><span class="v">${formatMb(ramUsed)} / ${formatMb(ramTotal || 0)}${ramPct != null ? ` · ${ramPct}%` : ''}</span></div>
        <div class="nd-meter-bar ${ramLevel}"><div class="fill" style="width:${ramPct || 0}%;"></div></div>
      </div>
      ${swapTotal && swapTotal > 0 ? `
        <div class="nd-meter">
          <div class="nd-meter-head"><span class="t">${escapeHtml(I18n.t('mesh.swap') || 'Swap')}</span><span class="v">${formatMb(swapUsed || 0)} / ${formatMb(swapTotal)}${swapPct != null ? ` · ${swapPct}%` : ''}</span></div>
          <div class="nd-meter-bar ${gaugeLevel(swapPct)}"><div class="fill" style="width:${swapPct || 0}%;"></div></div>
        </div>
      ` : ''}
    </div>
  ` : '';

  return `
    <div class="nd-section">
      <h3>
        ${ico('i-trend')}
        ${escapeHtml(I18n.t('mesh.resources') || 'Resource usage')}
        <span class="h-actions"><span class="nd-stat-pill ok dot live">live</span></span>
      </h3>
      <div class="nd-resource-grid">${cpuCard}${ramCard}</div>
    </div>
  `;
}

function renderGpus(n) {
  const gpus = Array.isArray(n.gpus) ? n.gpus : [];
  if (gpus.length === 0) return '';

  const usedTotal = gpus.reduce((s, g) => s + (g.vram_used_mb || 0), 0);
  const totalTotal = gpus.reduce((s, g) => s + (g.vram_total_mb || 0), 0);
  const summary = totalTotal > 0
    ? `VRAM: <strong style="color:var(--text);">${formatMb(usedTotal)} / ${formatMb(totalTotal)}</strong>`
    : '';

  const cards = gpus.map((g, idx) => {
    const usage = g.usage_percent ?? 0;
    const usagePct = Math.min(100, Math.max(0, Math.round(usage)));
    const vramPct = g.vram_total_mb ? Math.round((g.vram_used_mb / g.vram_total_mb) * 100) : 0;
    const power = (g.power_draw_w != null && g.power_limit_w)
      ? `${Math.round(g.power_draw_w)} / ${Math.round(g.power_limit_w)} W`
      : (g.power_draw_w != null ? `${Math.round(g.power_draw_w)} W` : '—');
    const temp = g.temperature_c != null ? `${Math.round(g.temperature_c)}°C` : '—';
    const fan = g.fan_speed_percent != null ? `${Math.round(g.fan_speed_percent)}%` : '—';
    const profileBtn = gpuProfileButtonHtml(n, g, idx);
    const vendor = [g.driver_version ? `driver ${g.driver_version}` : null, g.cuda_version ? `CUDA ${g.cuda_version}` : null]
      .filter(Boolean).join(' · ');

    return `
      <div class="nd-gpu" data-key="gpu-${idx}">
        <div class="gc-head">
          <div class="gc-title-wrap">
            <span class="gc-idx">${idx}</span>
            <span class="gc-name">${escapeHtml(g.name || '—')}</span>
            ${vendor ? `<div class="gc-vendor">${escapeHtml(vendor)}</div>` : ''}
          </div>
          <div class="gc-actions">${profileBtn}</div>
        </div>
        <div class="gc-meters">
          <div class="nd-meter">
            <div class="nd-meter-head"><span class="t">${escapeHtml(I18n.t('mesh.usage') || 'Usage')}</span><span class="v">${usagePct}%</span></div>
            <div class="nd-meter-bar ${gaugeLevel(usagePct)}"><div class="fill" style="width:${usagePct}%;"></div></div>
          </div>
          <div class="nd-meter">
            <div class="nd-meter-head"><span class="t">VRAM</span><span class="v">${formatMb(g.vram_used_mb || 0)} / ${formatMb(g.vram_total_mb || 0)} · ${vramPct}%</span></div>
            <div class="nd-meter-bar ${gaugeLevel(vramPct)}"><div class="fill" style="width:${vramPct}%;"></div></div>
          </div>
        </div>
        <div class="gc-foot">
          <div class="stat"><div class="l">${escapeHtml(I18n.t('mesh.gpu_temp') || 'Temp')}</div><div class="v">${escapeHtml(temp)}</div></div>
          <div class="stat"><div class="l">${escapeHtml(I18n.t('mesh.gpu_power') || 'Power')}</div><div class="v">${escapeHtml(power)}</div></div>
          <div class="stat"><div class="l">Fan</div><div class="v">${escapeHtml(fan)}</div></div>
        </div>
      </div>
    `;
  }).join('');

  // Auto-dense gdy >= 6 GPU (typowe rigi 8x). Mniejsze karty bez 'driver/CUDA'
  // line, ciasniejsze odstepy.
  const dense = gpus.length >= 6;
  return `
    <div class="nd-section">
      <h3>
        ${ico('i-gpu')}
        GPU
        <span class="count-pill">${gpus.length}</span>
        ${summary ? `<span class="h-actions">${summary}</span>` : ''}
      </h3>
      <div class="nd-gpu-grid${dense ? ' dense' : ''}">${cards}</div>
    </div>
  `;
}

// Sekcja zachecajaca do otwarcia globalnego ekranu Profiling Sessions,
// gdy node ma jakiekolwiek dostepne kolektory. Pelna lista sesji zyje w
// `profiling-sessions-screen.js` (z filtrem per-node), tu pokazujemy tylko
// shortcut + capability badge.
function renderProfilingWrap(n) {
  if (!hasProfilingCapability(n)) return '';
  return `
    <div class="nd-section">
      <h3>
        ${ico('i-trend')}
        ${escapeHtml(I18n.t('mesh.profiling_section') || 'Profiling')}
      </h3>
      <div class="nd-profiling-hint">
        <p>${escapeHtml(I18n.t('mesh.profiling_hint') || 'Capture multi-source profiling sessions for this node and review reports in the dedicated screen.')}</p>
        <div class="nd-row-actions">
          <tf-button size="sm" variant="primary" data-action="profile-node-open">
            ${escapeHtml(I18n.t('mesh.profile_start') || 'Start profiling…')}
          </tf-button>
          <tf-button size="sm" variant="ghost" data-action="profile-view-sessions">
            ${escapeHtml(I18n.t('mesh.profile_view_sessions') || 'View sessions')}
          </tf-button>
        </div>
      </div>
    </div>
  `;
}

function renderNetwork(n) {
  const ifaces = Array.isArray(n.network_interfaces) ? n.network_interfaces : [];
  if (ifaces.length === 0) return '';

  const rows = ifaces.map((i, idx) => {
    const up = i.link_up;
    const speed = speedLabel(i.speed_mbps);
    const ipv4 = i.ipv4_address || (up ? '—' : (I18n.t('mesh.network_no_link') || '—'));
    const ipv6 = i.ipv6_address || '';
    const rx = i.rx_bytes_per_sec || 0;
    const tx = i.tx_bytes_per_sec || 0;
    const role = ifaceRoleClass(i);
    const roleLabel = ifaceRoleLabel(i);

    const badges = [];
    if (i.interface_type === 'thunderbolt') badges.push('<span class="nd-net-badge tb">TB</span>');
    if (i.rdma_available) badges.push('<span class="nd-net-badge rdma">RDMA</span>');
    if (i.roce_available) badges.push('<span class="nd-net-badge roce">RoCE</span>');
    if (i.numa_node != null && i.numa_node >= 0) badges.push(`<span class="nd-net-badge numa">NUMA${i.numa_node}</span>`);

    const statusPill = up
      ? '<span class="nd-stat-pill ok dot">UP</span>'
      : '<span class="nd-stat-pill off">DOWN</span>';

    return `
      <tr data-key="iface-${escapeAttr(i.name || idx)}">
        <td>
          <div class="nd-acell">
            <div class="nd-aicon">${ifaceIconSvg(i)}</div>
            <div>
              <div class="nd-aname">${escapeHtml(i.name || '—')}</div>
              ${i.description ? `<div class="nd-asub">${escapeHtml(i.description)}</div>` : ''}
            </div>
          </div>
        </td>
        <td>
          <div class="nd-mono">${escapeHtml(ipv4)}</div>
          ${ipv6 ? `<div class="nd-mono-sub">${escapeHtml(ipv6)}</div>` : ''}
        </td>
        <td><span class="nd-rolepill ${role}">${escapeHtml(roleLabel)}</span></td>
        <td><span class="nd-mono">${escapeHtml(speed)}</span></td>
        <td><div class="nd-bw"><span class="down">↓ ${formatBytes(rx)}/s</span><span class="up">↑ ${formatBytes(tx)}/s</span></div></td>
        <td>${badges.length ? `<span class="nd-net-badges">${badges.join('')}</span>` : '—'}</td>
        <td>${statusPill}</td>
      </tr>
    `;
  }).join('');

  const mobileCards = ifaces.map((i, idx) => {
    const up = i.link_up;
    const speed = speedLabel(i.speed_mbps);
    const ipv4 = i.ipv4_address || '—';
    const rx = i.rx_bytes_per_sec || 0;
    const tx = i.tx_bytes_per_sec || 0;
    const statusPill = up ? '<span class="nd-stat-pill ok dot">UP</span>' : '<span class="nd-stat-pill off">DOWN</span>';
    return `
      <div class="nd-net-card-m" data-key="iface-card-${escapeAttr(i.name || idx)}">
        <div class="nh">
          <div class="nd-aicon">${ifaceIconSvg(i)}</div>
          <div class="nh-meta">
            <div class="nd-aname">${escapeHtml(i.name || '—')}</div>
            <div class="nd-asub">${escapeHtml(ifaceRoleLabel(i))}${i.description ? ' · ' + escapeHtml(i.description) : ''}</div>
          </div>
          ${statusPill}
        </div>
        <div class="nb">
          <div><div class="k">IPv4</div><div class="v">${escapeHtml(ipv4)}</div></div>
          <div><div class="k">Speed</div><div class="v">${escapeHtml(speed)}</div></div>
          <div><div class="k">↓</div><div class="v">${formatBytes(rx)}/s</div></div>
          <div><div class="k">↑</div><div class="v">${formatBytes(tx)}/s</div></div>
        </div>
      </div>
    `;
  }).join('');

  return `
    <div class="nd-section flush">
      <h3>
        ${ico('i-iface-lan')}
        ${escapeHtml(I18n.t('mesh.network_section') || 'Network')}
        <span class="count-pill">${ifaces.length}</span>
      </h3>
      <div class="nd-table-wrap">
        <table class="nd-table">
          <thead>
            <tr>
              <th>${escapeHtml(I18n.t('mesh.iface') || 'Interfejs')}</th>
              <th>${escapeHtml(I18n.t('mesh.ip') || 'IP')}</th>
              <th>${escapeHtml(I18n.t('mesh.role') || 'Rodzaj')}</th>
              <th>Speed</th>
              <th>Bandwidth</th>
              <th>Capabilities</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>${rows}</tbody>
        </table>
      </div>
      <div class="nd-net-cards">${mobileCards}</div>
    </div>
  `;
}

function renderModels(n) {
  const models = Array.isArray(n.models) ? n.models : [];
  if (models.length === 0) return '';

  const rows = models.map((m, idx) => {
    const loaded = !!m.loaded;
    const statusPill = loaded
      ? `<span class="nd-stat-pill ok dot">${escapeHtml(I18n.t('mesh.loaded') || 'loaded')}</span>`
      : `<span class="nd-stat-pill off">${escapeHtml(I18n.t('mesh.unloaded') || 'unloaded')}</span>`;
    return `
      <div class="nd-model" data-key="model-${escapeAttr(m.alias || idx)}">
        <span class="kind">${escapeHtml(m.kind || '—')}</span>
        <span class="alias" title="${escapeAttr(m.alias || '')}">${escapeHtml(m.alias || '—')}</span>
        <span class="backend">${escapeHtml(m.backend || '—')}</span>
        <span class="size">${m.size_mb ? formatMb(m.size_mb) : '—'}</span>
        ${statusPill}
      </div>
    `;
  }).join('');

  return `
    <div class="nd-section">
      <h3>
        ${ico('i-models')}
        ${escapeHtml(I18n.t('mesh.models_section') || 'Models')}
        <span class="count-pill">${models.length}</span>
      </h3>
      <div class="nd-models">${rows}</div>
    </div>
  `;
}

function renderContainers(n) {
  const containers = Array.isArray(n.containers) ? n.containers : [];
  if (containers.length === 0) return '';

  const rows = containers.map(c => {
    const statusLower = String(c.status || '').toLowerCase();
    const running = statusLower.includes('up') || statusLower.includes('running');
    const exited = statusLower.includes('exited');
    let statusPill;
    if (running) statusPill = `<span class="nd-stat-pill ok dot live">${escapeHtml(c.status || 'Running')}</span>`;
    else if (exited) statusPill = `<span class="nd-stat-pill off">${escapeHtml(c.status || 'Exited')}</span>`;
    else statusPill = `<span class="nd-stat-pill warn">${escapeHtml(c.status || '—')}</span>`;

    const cpuPct = c.cpu_percent != null ? `${c.cpu_percent.toFixed(1)}%` : '—';
    const mem = c.memory_limit_mb
      ? `${formatMb(c.memory_mb || 0)} / ${formatMb(c.memory_limit_mb)}`
      : formatMb(c.memory_mb || 0);

    const actions = running
      ? `<tf-button variant="ghost" size="sm" data-container-action="stop" data-container-name="${escapeAttr(c.name)}">${escapeHtml(I18n.t('mesh.stop'))}</tf-button>
         <tf-button variant="ghost" size="sm" data-container-action="restart" data-container-name="${escapeAttr(c.name)}">${escapeHtml(I18n.t('mesh.restart'))}</tf-button>`
      : `<tf-button variant="primary" size="sm" data-container-action="start" data-container-name="${escapeAttr(c.name)}">${escapeHtml(I18n.t('mesh.start'))}</tf-button>`;

    return `
      <tr data-key="ct-${escapeAttr(c.name || '')}">
        <td>
          <div class="nd-cname">
            <span class="nd-aicon">${ico('i-docker')}</span>
            ${escapeHtml(c.name || '—')}
          </div>
        </td>
        <td><span class="nd-cimg">${escapeHtml(c.image || '—')}</span></td>
        <td>${statusPill}</td>
        <td><span class="nd-mono">${escapeHtml(cpuPct)}</span></td>
        <td><span class="nd-mono">${escapeHtml(mem)}</span></td>
        <td class="actions-col"><div class="nd-row-actions">${actions}</div></td>
      </tr>
    `;
  }).join('');

  return `
    <div class="nd-section flush">
      <h3>
        ${ico('i-docker')}
        ${escapeHtml(I18n.t('mesh.containers'))}
        <span class="count-pill">${containers.length}</span>
      </h3>
      <div class="nd-table-wrap">
        <table class="nd-table">
          <thead>
            <tr>
              <th>${escapeHtml(I18n.t('mesh.container_name'))}</th>
              <th>${escapeHtml(I18n.t('mesh.container_image'))}</th>
              <th>${escapeHtml(I18n.t('mesh.container_status'))}</th>
              <th>CPU</th>
              <th>RAM</th>
              <th class="actions-col">${escapeHtml(I18n.t('mesh.container_actions'))}</th>
            </tr>
          </thead>
          <tbody>${rows}</tbody>
        </table>
      </div>
    </div>
  `;
}

// ---- Profiling capability + actions --------------------------------------

// Lista id-ow kolektorow rozglaszanych w heartbeacie. Pusta = node nie obsluguje
// multi-source profilingu (np. brak natywnego daemona z odpowiednimi flagami).
function profilingCollectors(node) {
  const list = node?.profiling_collectors_available;
  return Array.isArray(list) ? list : [];
}

function hasProfilingCapability(node) {
  return profilingCollectors(node).length > 0;
}

function buildLaunchSources(node) {
  // Heartbeat broadcastuje plaska liste id-ow; modal oczekuje obiektow
  // `{ id, label, status }`. Status default 'available' — backend dolepi
  // status `unavailable`/`needs_sudo` przy probie startu sesji.
  return profilingCollectors(node).map((id) => ({
    id: String(id),
    label: String(id),
    description: '',
    status: 'available',
  }));
}

// HTML do wstrzykniecia w `card-head` GPU. Pokazuje sie gdy node ma
// jakiekolwiek collectory profilowania — start otwiera modal z preselectGpu.
function gpuProfileButtonHtml(node, _gpu, idx) {
  if (!hasProfilingCapability(node)) return '';
  return `
    <tf-button size="sm" variant="ghost" data-action="profile-gpu-card" data-gpu-idx="${idx}" title="${escapeAttr(I18n.t('mesh.profile_gpu_btn') || 'Profile this GPU')}">
      <svg width="12" height="12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><use href="#i-record"/></svg>
      <span>${escapeHtml(I18n.t('mesh.profile_gpu_btn') || 'Profile')}</span>
    </tf-button>
  `;
}

// HTML topbara: Profile button + Sessions shortcut, gdy node cokolwiek
// potrafi zaprofilowac. REC banner (countdown + Stop) zyje w
// ProfilingActiveBanner pod headerem, wiec topbar jest stale niezalezny od
// stanu sesji.
function profileTopbarHtml(node) {
  if (!hasProfilingCapability(node)) return '';
  return `
    <tf-button size="sm" variant="ghost" data-action="profile-node-open" title="${escapeAttr(I18n.t('mesh.profile_node_btn') || 'Profile this node')}">
      <svg width="12" height="12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><use href="#i-record"/></svg>
      <span>${escapeHtml(I18n.t('mesh.profile_node_btn') || 'Profile')}</span>
    </tf-button>
    <tf-button size="sm" variant="ghost" data-action="profile-view-sessions" title="${escapeAttr(I18n.t('mesh.profile_view_sessions') || 'View profiling sessions')}">
      <svg width="12" height="12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><use href="#i-trend"/></svg>
      <span>${escapeHtml(I18n.t('mesh.profile_view_sessions') || 'Sessions')}</span>
    </tf-button>
  `;
}

async function openLaunchModal(node, { gpuIndex = null } = {}) {
  const sources = buildLaunchSources(node);
  if (sources.length === 0) {
    toast(I18n.t('mesh.profile_unavailable') || 'Profiling is not available on this node.', 'error');
    return;
  }
  let preselectGpu = null;
  if (Number.isInteger(gpuIndex)) {
    const gpu = Array.isArray(node.gpus) ? node.gpus[gpuIndex] : null;
    preselectGpu = {
      deviceIndex: gpuIndex,
      vendor: gpu?.vendor || null,
    };
  }
  try {
    const result = await ProfilingLaunchModal.open({
      nodeId: node.node_id,
      availableSources: sources,
      preselectGpu,
    });
    if (result && result.launched) {
      toast(I18n.t('mesh.profile_started') || 'Profiling session started', 'success');
      pokeActiveBanner();
    }
  } catch (err) {
    toast(`${I18n.t('mesh.profile_error') || 'Profiling error'}: ${err.message || err}`, 'error');
  }
}

function bindProfileActions(root, node) {
  if (profileHandler) return;
  profileHandler = async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    const action = btn.dataset.action;
    if (action === 'profile-node-open') {
      await openLaunchModal(node);
      return;
    }
    if (action === 'profile-gpu-card') {
      const idx = parseInt(btn.dataset.gpuIdx, 10);
      await openLaunchModal(node, { gpuIndex: Number.isFinite(idx) ? idx : null });
      return;
    }
    if (action === 'profile-view-sessions') {
      const { Router } = await import('/js/router.js');
      Router.navigate('profiling-sessions', {
        nodeId: node.node_id,
        nodeName: node.hostname || node.node_id,
      });
    }
  };
  root.addEventListener('click', profileHandler);
}

function bindContainerActions(root) {
  // Patrz komentarz w bindBack — handler trzymany w module dla cleanup().
  if (containerHandler) return;
  containerHandler = async (e) => {
    const btn = e.target.closest('[data-container-action]');
    if (!btn) return;
    const action = btn.dataset.containerAction;
    const name = btn.dataset.containerName;
    if (!action || !name || !currentNodeId) return;
    try {
      await ApiBinary.action('meshNodeCommandRequest', {
        nodeId: currentNodeId,
        command: `container_${action}`,
        args: [name],
      });
      toast(`${action}: ${name}`, 'success');
      await loadNode();
      renderDetail();
    } catch (err) {
      toast(`${action} ${name}: ${err.message}`, 'error');
    }
  };
  root.addEventListener('click', containerHandler);
}

export default MeshDetailScreen;
