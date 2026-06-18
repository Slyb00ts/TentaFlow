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
// robotId -> card element currently in the DOM. Lets renderList() do a keyed
// diff (append new, remove gone, update-in-place existing) instead of rebuilding
// innerHTML each poll — rebuilding tears down the live <tf-video-stream> (MSE)
// every REFRESH_INTERVAL_MS and the video never stabilizes.
let cardEls = new Map();

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
    cardEls = new Map();
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
  // The error empty-state replaces the grid contents; drop stale card refs so a
  // later successful poll rebuilds every card from scratch.
  cardEls = new Map();
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

// Keyed reconcile by robot id. Cards (and their <tf-video-stream>) are created
// once and kept across polls; only mutable fields update in place. A card is
// recreated only when its robot appears, disappears, or its camera_id changes.
function renderList() {
  const host = byId('robots-list');
  if (!host) return;

  if (!robots.length) {
    host.innerHTML = '';
    cardEls = new Map();
    const empty = document.createElement('tf-empty-state');
    empty.setAttribute('icon', 'cpu');
    empty.setAttribute('title', 'Brak robotów w sieci mesh');
    empty.setAttribute('message', 'Sparuj robota lub dołącz węzeł, który nim steruje — pojawi się tu po wykryciu przez mesh.');
    host.appendChild(empty);
    return;
  }

  // The previous poll may have left a non-card child (spinner/empty-state) as
  // the sole content; clear it so card append order stays clean.
  if (cardEls.size === 0 && host.firstChild) {
    host.innerHTML = '';
  }

  const present = new Set();
  for (const r of robots) {
    const id = robotId(r);
    if (!id) continue;
    present.add(id);

    let el = cardEls.get(id);
    // Recreate the card when the camera tile identity changes (camera_id flip,
    // or camera appearing/disappearing) — otherwise the stream-id would be stale.
    if (el && el.dataset.cameraId !== String(cameraId(r) ?? '')) {
      el.remove();
      cardEls.delete(id);
      el = null;
    }

    if (!el) {
      el = buildCard(r);
      cardEls.set(id, el);
      host.appendChild(el);
    }
    // Apply mutable state on both new and existing cards (a freshly built card
    // still needs its offline-disable applied for a robot that appears offline).
    updateCard(el, r);
  }

  // Remove cards for robots no longer in the list.
  for (const [id, el] of cardEls) {
    if (!present.has(id)) {
      el.remove();
      cardEls.delete(id);
    }
  }
}

// Creates a card element from robotCard() markup and binds its action handlers
// once. Listeners live on the persistent element, so update polls never rebind.
function buildCard(r) {
  const tmp = document.createElement('div');
  tmp.innerHTML = robotCard(r);
  const el = tmp.firstElementChild;

  el.querySelectorAll('[data-control]').forEach((btn) => {
    btn.addEventListener('click', () => {
      handleControl(btn.dataset.robot, btn.dataset.control, btn);
    });
  });
  el.querySelectorAll('[data-share-camera]').forEach((btn) => {
    btn.addEventListener('click', () => {
      handleShareCamera(btn.dataset.robot, btn.dataset.shareCamera, btn);
    });
  });
  return el;
}

// Updates only the mutable fields of an existing card. Never touches the
// <tf-video-stream> node, so the live MSE stream keeps playing across polls.
function updateCard(el, r) {
  const status = r.status || '';
  const badge = el.querySelector('[data-field="status"]');
  if (badge) {
    badge.setAttribute('tone', statusTone(status));
    badge.setAttribute('value', statusLabel(status));
  }

  setMetric(el, 'battery', batteryPercent(r), (v) => `${Math.round(Number(v))}%`);
  setMetric(el, 'rtt', rttMs(r), (v) => `${Math.round(Number(v))} ms`);

  const ownerEl = el.querySelector('[data-field="owner"]');
  if (ownerEl) {
    ownerEl.textContent = isLocal(r) ? 'ten węzeł' : shortNode(ownerNodeId(r));
  }

  // Capability chips: only rebuild when the set actually changed.
  const capsBox = el.querySelector('[data-field="caps"]');
  if (capsBox) {
    const caps = capabilities(r);
    const key = caps.join('');
    if (capsBox.dataset.capsKey !== key) {
      capsBox.dataset.capsKey = key;
      capsBox.innerHTML = caps
        .map((c) => `<tf-chip variant="tag" tone="muted" size="sm" label="${escapeAttr(c)}"></tf-chip>`)
        .join('');
    }
  }

  // Offline robots can't take commands: disable control + share buttons.
  const offline = !isControllable(status);
  el.querySelectorAll('[data-control], [data-share-camera]').forEach((btn) => {
    if (offline) btn.setAttribute('disabled', '');
    else btn.removeAttribute('disabled');
  });
}

// Updates a metric row in place, inserting/removing the row's value text. Rows
// that are optional (battery/RTT) keep a stable slot rendered by robotCard().
function setMetric(el, fieldName, raw, format) {
  const row = el.querySelector(`[data-metric="${fieldName}"]`);
  if (!row) return;
  if (raw == null) {
    row.hidden = true;
    return;
  }
  row.hidden = false;
  const valueEl = row.querySelector('.robots-metric-value');
  if (valueEl) valueEl.textContent = format(raw);
}

// A robot is controllable unless its status reads as offline/lost/error.
function isControllable(status) {
  const s = String(status || '').toLowerCase();
  return s !== 'offline' && s !== 'lost' && s !== 'error';
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

  // Battery/RTT rows always render so updateCard() has a stable slot to fill;
  // they're hidden via [hidden] when the value is absent (mirrors null checks).
  const metrics = [
    metricRow('zap', 'Bateria', battery != null ? `${Math.round(Number(battery))}%` : '', 'battery', battery == null),
    metricRow('bolt', 'RTT', rtt != null ? `${Math.round(Number(rtt))} ms` : '', 'rtt', rtt == null),
    metricRow('host', 'Węzeł', '', 'owner', false, owner),
  ];

  const capsHtml = `<div class="robots-caps" data-field="caps" data-caps-key="${escapeAttr(caps.join(''))}">${caps
    .map((c) => `<tf-chip variant="tag" tone="muted" size="sm" label="${escapeAttr(c)}"></tf-chip>`)
    .join('')}</div>`;

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
    <article class="robots-card" data-robot-card="${escapeAttr(id)}" data-camera-id="${escapeAttr(cam ?? '')}">
      <div class="robots-card-top">
        <div class="robots-card-ico">${sprite('cpu')}</div>
        <div class="robots-card-id">
          <div class="robots-card-name">${escapeHtml(id || '(bez identyfikatora)')}</div>
          <div class="robots-card-kind">${escapeHtml(kind)}</div>
        </div>
        <tf-badge data-field="status" tone="${statusTone(status)}" value="${escapeAttr(statusLabel(status))}"></tf-badge>
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

// `metricName` is the stable update key (battery/rtt/owner) used by updateCard().
// The owner row carries its value in a [data-field="owner"] span (updated by id),
// the numeric rows in `.robots-metric-value` (updated by formatted value).
function metricRow(icon, label, value, metricName, hidden, ownerValue) {
  const isOwner = metricName === 'owner';
  const valueSpan = isOwner
    ? `<span class="robots-metric-value" data-field="owner">${escapeHtml(ownerValue ?? '')}</span>`
    : `<span class="robots-metric-value">${escapeHtml(value)}</span>`;
  return `
    <div class="robots-metric" data-metric="${escapeAttr(metricName)}"${hidden ? ' hidden' : ''}>
      <span class="robots-metric-ico">${sprite(icon)}</span>
      <span class="robots-metric-label">${escapeHtml(label)}</span>
      ${valueSpan}
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
