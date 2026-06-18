// ===== File: robots.js — Robots core app (mesh robot list, live camera, control) =====
// Talks to Core over the binary protocol via MessageBody::RobotsBody:
//   RobotsListRequest / RobotControlRequest / RobotCameraShareRequest.
// Renders one card per robot advertised in the mesh (org-scoped): status badge,
// owner node, battery / RTT, capability chips, an optional live camera tile and
// the closed-allowlist control buttons (Hello, Sit, Stand Up, Recovery Stand,
// Stop, E-stop). The list auto-refreshes so mesh discovery state stays visible.
// tf-* components only; control results surface through tf-toast.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import '/js/components/tf-button.js';
import '/js/components/tf-badge.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-spinner.js';
import '/js/components/tf-video-stream.js';

// Mesh discovery refreshes the registry periodically; re-poll so status, battery
// and online/offline transitions appear without a manual reload.
const REFRESH_INTERVAL_MS = 4000;

// Closed control allowlist mirrored from RobotActionWire (message_body.rs:1166).
// Non-move actions ignore vx/vy/vyaw; the owner clamps again to its safety cap.
const CONTROL_ACTIONS = [
  { kind: 'hello', label: 'Przywitaj', icon: 'sparkle', variant: 'outline' },
  { kind: 'sit', label: 'Usiądź', icon: 'pause', variant: 'outline' },
  { kind: 'stand_up', label: 'Wstań', icon: 'arrow', variant: 'outline' },
  { kind: 'recovery_stand', label: 'Podnieś się', icon: 'rotate', variant: 'outline' },
];

let robots = [];
let refreshTimer = null;
let inFlightRefresh = false;

const RobotsScreen = {
  get title() { return 'Roboty'; },

  render() {
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('cpu')} Roboty</h1>
          <div class="sub" id="robots-sub">Roboty wykryte w sieci mesh — status, podgląd i sterowanie</div>
        </div>
        <div class="actions">
          <tf-button variant="ghost" icon="refresh" id="robots-refresh">Odśwież</tf-button>
        </div>
      </div>

      <div id="robots-list" class="robots-grid"></div>
    `;
  },

  async mount() {
    byId('robots-refresh')?.addEventListener('click', () => loadRobots({ showSpinner: true }));
    await loadRobots({ showSpinner: true });
    // Periodic re-poll. Skips overlapping requests (inFlightRefresh) so a slow
    // mesh round-trip can't stack timers.
    refreshTimer = window.setInterval(() => loadRobots({ showSpinner: false }), REFRESH_INTERVAL_MS);
  },

  unmount() {
    if (refreshTimer != null) {
      window.clearInterval(refreshTimer);
      refreshTimer = null;
    }
    robots = [];
    inFlightRefresh = false;
  },
};

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

// RobotEntry fields decode from serde snake_case; read camelCase too so the UI
// is resilient to a wasm decoder that normalizes keys (mirrors ml-studio.js).
function field(robot, camel, snake) {
  return robot[camel] ?? robot[snake];
}

function robotId(r) { return field(r, 'robotId', 'robot_id') ?? ''; }
function ownerNodeId(r) { return field(r, 'ownerNodeId', 'owner_node_id') ?? ''; }
function isLocal(r) { return !!field(r, 'isLocal', 'is_local'); }
function batteryPercent(r) { return field(r, 'batteryPercent', 'battery_percent'); }
function rttMs(r) { return field(r, 'rttMs', 'rtt_ms'); }
function cameraId(r) { return field(r, 'cameraId', 'camera_id'); }
function capabilities(r) {
  const caps = r.capabilities;
  return Array.isArray(caps) ? caps : [];
}

// Maps the free-form status string onto a tf-badge tone. tf-badge accepts
// accent/danger/success/warning/info (tf-badge.js).
function statusTone(status) {
  const s = String(status || '').toLowerCase();
  if (s === 'online' || s === 'ready' || s === 'ok') return 'success';
  if (s === 'connecting' || s === 'pairing' || s === 'busy') return 'warning';
  if (s === 'offline' || s === 'error' || s === 'lost') return 'danger';
  return 'info';
}

function statusLabel(status) {
  const s = String(status || '').toLowerCase();
  const map = {
    online: 'online',
    offline: 'offline',
    connecting: 'łączenie',
    pairing: 'parowanie',
    busy: 'zajęty',
    error: 'błąd',
    lost: 'utracony',
  };
  return map[s] || (status || '—');
}

async function loadRobots({ showSpinner }) {
  if (inFlightRefresh) return;
  inFlightRefresh = true;
  const list = byId('robots-list');
  if (showSpinner && list) {
    list.innerHTML = '<div class="robots-loading"><tf-spinner></tf-spinner></div>';
  }
  try {
    robots = await ApiBinary.list('robotsListRequest', { arrayKey: 'robots' });
    if (!Array.isArray(robots)) robots = [];
    renderList();
    const sub = byId('robots-sub');
    if (sub) {
      sub.textContent = robots.length
        ? `${robots.length} ${plural(robots.length, 'robot', 'roboty', 'robotów')} w sieci mesh`
        : 'Roboty wykryte w sieci mesh — status, podgląd i sterowanie';
    }
  } catch (err) {
    // A background poll failure must not wipe a working list; only surface the
    // error path on an explicit (spinner) load.
    if (showSpinner) {
      robots = [];
      renderError(list, err);
      toast(`Roboty: ${err.message}`, 'error');
    }
  } finally {
    inFlightRefresh = false;
  }
}

function renderError(list, err) {
  if (!list) return;
  list.innerHTML = '';
  const empty = document.createElement('tf-empty-state');
  empty.setAttribute('icon', 'alert');
  empty.setAttribute('title', 'Nie udało się wczytać robotów');
  empty.setAttribute('message', err.message || 'Błąd protokołu Roboty.');
  const retry = document.createElement('tf-button');
  retry.setAttribute('variant', 'primary');
  retry.textContent = 'Spróbuj ponownie';
  retry.addEventListener('click', () => loadRobots({ showSpinner: true }));
  empty.appendChild(retry);
  list.appendChild(empty);
}

function renderList() {
  const host = byId('robots-list');
  if (!host) return;

  if (!robots.length) {
    host.innerHTML = '';
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', 'cpu');
    empty.setAttribute('title', 'Brak robotów w sieci mesh');
    empty.setAttribute('message', 'Sparuj robota lub dołącz węzeł, który nim steruje — pojawi się tu po wykryciu przez mesh.');
    host.appendChild(empty);
    return;
  }

  host.innerHTML = robots.map((r) => robotCard(r)).join('');

  // Wire control + camera-share buttons after the markup is in the DOM.
  host.querySelectorAll('[data-control]').forEach((btn) => {
    btn.addEventListener('click', () => {
      handleControl(btn.dataset.robot, btn.dataset.control, btn);
    });
  });
  host.querySelectorAll('[data-share-camera]').forEach((btn) => {
    btn.addEventListener('click', () => {
      handleShareCamera(btn.dataset.robot, btn.dataset.shareCamera, btn);
    });
  });
}

function robotCard(r) {
  const id = robotId(r);
  const status = r.status || '';
  const kind = field(r, 'kind', 'kind') || 'robot';
  const owner = isLocal(r) ? 'ten węzeł' : shortNode(ownerNodeId(r));
  const battery = batteryPercent(r);
  const rtt = rttMs(r);
  const caps = capabilities(r);
  const cam = cameraId(r);

  const metrics = [];
  if (battery != null) {
    metrics.push(metricRow('zap', 'Bateria', `${Math.round(Number(battery))}%`));
  }
  if (rtt != null) {
    metrics.push(metricRow('bolt', 'RTT', `${Math.round(Number(rtt))} ms`));
  }
  metrics.push(metricRow('host', 'Węzeł', owner));

  const capsHtml = caps.length
    ? `<div class="robots-caps">${caps
        .map((c) => `<tf-chip variant="tag" tone="muted" size="sm" label="${escapeAttr(c)}"></tf-chip>`)
        .join('')}</div>`
    : '';

  const cameraHtml = cam
    ? `
      <div class="robots-camera">
        <tf-video-stream stream-id="camera:${escapeAttr(cam)}" label="${escapeAttr(id)}" height-px="240"></tf-video-stream>
        <tf-button variant="outline" size="sm" icon="image" full-width
          data-robot="${escapeAttr(id)}" data-share-camera="${escapeAttr(cam)}">
          Dodaj kamerę do TentaVision
        </tf-button>
      </div>`
    : '';

  return `
    <article class="robots-card">
      <div class="robots-card-top">
        <div class="robots-card-ico">${sprite('cpu')}</div>
        <div class="robots-card-id">
          <div class="robots-card-name">${escapeHtml(id || '(bez identyfikatora)')}</div>
          <div class="robots-card-kind">${escapeHtml(kind)}</div>
        </div>
        <tf-badge tone="${statusTone(status)}" value="${escapeAttr(statusLabel(status))}"></tf-badge>
      </div>

      <div class="robots-metrics">${metrics.join('')}</div>
      ${capsHtml}
      ${cameraHtml}

      <div class="robots-controls">
        ${CONTROL_ACTIONS.map((a) => `
          <tf-button variant="${a.variant}" size="sm" icon="${a.icon}"
            data-robot="${escapeAttr(id)}" data-control="${escapeAttr(a.kind)}">
            ${escapeHtml(a.label)}
          </tf-button>
        `).join('')}
        <tf-button variant="danger-solid" size="sm" icon="stop"
          data-robot="${escapeAttr(id)}" data-control="estop">
          STOP awaryjny
        </tf-button>
      </div>
    </article>
  `;
}

function metricRow(icon, label, value) {
  return `
    <div class="robots-metric">
      <span class="robots-metric-ico">${sprite(icon)}</span>
      <span class="robots-metric-label">${escapeHtml(label)}</span>
      <span class="robots-metric-value">${escapeHtml(value)}</span>
    </div>`;
}

// Owner node ids are endpoint-id hex; show a short prefix for the card.
function shortNode(nodeId) {
  const s = String(nodeId || '');
  return s.length > 12 ? `${s.slice(0, 12)}…` : (s || '—');
}

async function handleControl(id, kind, btn) {
  if (!id || !kind) return;
  btn.setAttribute('loading', '');
  try {
    const resp = await ApiBinary.action('robotControlRequest', {
      robotId: id,
      kind,
      vx: 0,
      vy: 0,
      vyaw: 0,
    });
    // A robot-level refusal is still a successful call carrying `rejected`;
    // `error` is an execution failure (RobotControlResponse, message_body.rs).
    const rejected = resp.rejected;
    const error = resp.error;
    if (resp.ok) {
      toast(`Robot ${shortNode(id)}: ${actionLabel(kind)} ✓`, 'success');
    } else if (rejected) {
      toast(`Robot ${shortNode(id)}: odrzucono — ${rejected}`, 'error');
    } else {
      toast(`Robot ${shortNode(id)}: błąd — ${error || 'nieznany'}`, 'error');
    }
  } catch (err) {
    toast(`Robot ${shortNode(id)}: ${err.message}`, 'error');
  } finally {
    btn.removeAttribute('loading');
  }
}

async function handleShareCamera(id, cam, btn) {
  if (!id || !cam) return;
  btn.setAttribute('loading', '');
  try {
    const resp = await ApiBinary.action('robotCameraShareRequest', {
      robotId: id,
      cameraId: cam,
    });
    if (resp.ok) {
      // For a remote robot there is no local grant; `note` explains the path.
      toast(resp.note || 'Kamera dodana do TentaVision ✓', 'success');
    } else {
      toast(`Udostępnienie kamery: błąd — ${resp.error || 'nieznany'}`, 'error');
    }
  } catch (err) {
    toast(`Udostępnienie kamery: ${err.message}`, 'error');
  } finally {
    btn.removeAttribute('loading');
  }
}

function actionLabel(kind) {
  const a = CONTROL_ACTIONS.find((x) => x.kind === kind);
  if (a) return a.label;
  if (kind === 'estop') return 'STOP awaryjny';
  return kind;
}

function plural(n, one, few, many) {
  const mod10 = n % 10;
  const mod100 = n % 100;
  if (n === 1) return one;
  if (mod10 >= 2 && mod10 <= 4 && (mod100 < 10 || mod100 >= 20)) return few;
  return many;
}

export default RobotsScreen;
