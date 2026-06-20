// ===== File: robots.js — Robots core app (mesh robot list, live camera, control) =====
// Talks to Core over the binary protocol via MessageBody::RobotsBody:
//   RobotsListRequest / RobotControlRequest / RobotCameraShareRequest.
// Renders one card per robot advertised in the mesh (org-scoped): status badge,
// owner node, battery / RTT, an optional live camera tile and a CAPABILITY-DRIVEN
// control surface generated from each robot's advertised `actions_meta` (label /
// risk / param schema) — no hardcoded action list. High-risk / acrobatic actions
// are gated behind a confirm dialog using the UNION of advertised risk and an
// authoritative client-side known-dangerous set (a hostile descriptor can't
// downgrade a flip to a one-click button). Parametered actions write values into
// FIXED p1..p4 slots BY NAME and refuse to send if a required param is missing
// (no slot shifting). An always-present E-stop is independent of the advertised
// metadata and stays clickable even when the robot reads offline — safety must
// never depend on status metadata. The server remains the real safety gate.
// The list auto-refreshes so mesh discovery state stays visible.
// tf-* components only; control results surface through tf-toast.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { decodeLidarFrame } from '/js/protocol/codec.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import '/js/components/tf-button.js';
import '/js/components/tf-badge.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-modal.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-spinner.js';
import '/js/components/tf-video-stream.js';

// Mesh discovery refreshes the registry periodically; re-poll so status, battery
// and online/offline transitions appear without a manual reload.
const REFRESH_INTERVAL_MS = 4000;

// Real-time LiDAR PUSH: each robot's canonical frame stream is subscribed via the
// generic StreamHub rails (`streamId = "lidar:<robotId>"`), the SAME path camera
// video uses. The server pushes every canonical L1 frame as a StreamFrame whose
// `data` is the raw frame bytes; we decode it with the sdk-spec header decoder.
// This is the live data-path consumer that proves L1→L3a end to end (no 3D render
// here — raw/points are stashed for L4).
//
// Sliding window for the measured frame rate. Wide enough to smooth the ~5 fps
// arrival jitter, short enough to react quickly when streaming stops.
const LIDAR_FPS_WINDOW_MS = 2000;

// A frame older than this reads as "stale" — at 5 fps a healthy stream lands a
// new frame every ~200 ms, so 1.5 s without one means the source went quiet. A
// periodic staleness sweep flips the badge even when frames simply stop arriving.
const LIDAR_STALE_AFTER_MS = 1500;

// Cadence of the freshness sweep that re-renders each active card so the badge
// can flip to "nieaktualne" when frames stop (no push would otherwise re-render).
const LIDAR_STALE_SWEEP_MS = 500;

// After the server ends a lidar stream because the subscriber lagged, wait this
// long before a single re-subscribe so the UI doesn't flap. We do NOT spin: only
// a `subscriber_lagged` end triggers one retry; any other end leaves it stale.
const LIDAR_RESUBSCRIBE_DELAY_MS = 1000;

// Locomotion magnitude (m/s) for the directional pad, matching the go2 driver's
// own move buttons. The owner re-clamps to its safety cap regardless.
const MOVE_SPEED = 0.3;

// Kinds the control surface must NOT render as a button: read-only telemetry, the
// camera tag, and the e-stop family (a dedicated, always-present STOP button owns
// safety so it can't depend on advertised metadata).
const NON_CONTROL_KINDS = new Set(['status', 'camera', 'estop', 'stop', 'reset_estop']);

// Authoritative client-side set of known dangerous kinds. Defense-in-depth: the
// confirm dialog is required if a kind is in this set REGARDLESS of advertised
// risk, so a malformed/hostile descriptor that advertises e.g. `front_flip` as
// low-risk can NEVER bypass confirmation into a one-click flip. The server stays
// the real safety gate; this just makes the UI un-trickable.
const KNOWN_HIGH_RISK_KINDS = new Set([
  'front_flip',
  'front_jump',
  'front_pounce',
  'scrape',
  'dance1',
  'dance2',
]);

// Documented wire slot order per known parametered kind: param NAME → fixed
// p1..p4 slot. Values are placed BY NAME (never compacted by index), so a missing
// param leaves its slot empty rather than shifting a later param into it (which
// the owner would misread as a different axis). REQUIRED_PARAMS lists the params
// that MUST be present for the kind to be sendable; a missing required param
// refuses the send instead of guessing.
const PARAM_SLOTS = {
  euler: { roll: 'p1', pitch: 'p2', yaw: 'p3' },
  pose: { roll: 'p1', pitch: 'p2', yaw: 'p3', height: 'p4' },
  body_height: { height: 'p1' },
  foot_raise_height: { height: 'p1' },
  speed_level: { level: 'p1' },
};

const REQUIRED_PARAMS = {
  euler: ['roll', 'pitch', 'yaw'],
  pose: ['roll', 'pitch', 'yaw', 'height'],
  body_height: ['height'],
  foot_raise_height: ['height'],
  speed_level: ['level'],
};

let robots = [];
let refreshTimer = null;
let inFlightRefresh = false;
// robotId -> card element currently in the DOM. Lets renderList() do a keyed
// diff (append new, remove gone, update-in-place existing) instead of rebuilding
// innerHTML each poll — rebuilding tears down the live <tf-video-stream> (MSE)
// every REFRESH_INTERVAL_MS and the video never stabilizes.
let cardEls = new Map();

// Per-robot live LiDAR state, keyed by robotId. An entry exists ONLY while that
// robot's LiDAR row is on screen AND enabled AND the robot is online — it's added
// by updateLidar() and removed the moment any of those stops holding (toggle off,
// card gone, robot offline) so no subscription stays open on a dead source. Each
// start has a matching close; teardown closes all (no leaked subscriptions).
// Shape: { unsub:fn|null, closed:bool, resubTimer:number|null,
//          frameTimes:number[], lastPointCount, lastFrameAtMs,
//          lastRaw:Uint8Array|null, lastPoints:Float32Array|null }.
let lidarLive = new Map();
// Single shared slow timer that re-renders every active card so the freshness
// badge flips to "nieaktualne" when frames stop arriving. NOT a poll — it carries
// no network traffic. Started lazily when the first robot subscribes, stopped when
// the set empties or on unmount.
let lidarTimer = null;

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
    stopLidarLoop();
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

// Rich capability descriptors driving the control surface. camel/snake tolerant.
// Each entry: {kind, label, risk, acrobatic, readOnly|read_only, params:[{name,min,max}]}.
function actionsMeta(r) {
  const m = field(r, 'actionsMeta', 'actions_meta');
  return Array.isArray(m) ? m : [];
}

// Structured runtime telemetry snapshot (gait / velocity / IMU / battery detail).
// camel/snake tolerant; null when the robot reports none.
function telemetry(r) {
  return field(r, 'telemetry', 'telemetry') || null;
}

// SMALL LiDAR availability snapshot (no point cloud). camel/snake tolerant; null
// when the robot has no LiDAR capability.
function lidar(r) {
  return field(r, 'lidar', 'lidar') || null;
}

function telNum(t, camel, snake) {
  const v = t[camel] ?? t[snake];
  return typeof v === 'number' && Number.isFinite(v) ? v : null;
}

// Radians → degrees for the IMU orientation display.
function radToDeg(v) {
  return v == null ? null : (v * 180) / Math.PI;
}

function actionRisk(a) { return String(a.risk || 'medium').toLowerCase(); }
function actionAcrobatic(a) { return !!a.acrobatic; }
function actionReadOnly(a) { return !!(a.readOnly ?? a.read_only); }
function actionParams(a) { return Array.isArray(a.params) ? a.params : []; }
// Union of the advertised risk and the authoritative known-dangerous set: an
// advertised-low kind that is in KNOWN_HIGH_RISK_KINDS is STILL high-risk. Never
// trust advertised-low to bypass the known set.
function isHighRisk(a) {
  return (
    KNOWN_HIGH_RISK_KINDS.has(a.kind) ||
    actionRisk(a) === 'high' ||
    actionRisk(a) === 'acrobatic' ||
    actionAcrobatic(a)
  );
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
  // later successful poll rebuilds every card from scratch. No cards remain on
  // screen, so no robot can be an active LiDAR source — close every subscription.
  cardEls = new Map();
  stopLidarLoop();
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
    // No robots on screen → no LiDAR sources; close every subscription.
    stopLidarLoop();
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
      // A gone robot can't produce frames — drop its live loop entry too.
      stopRobotLidar(id);
    }
  }
}

// Creates a card element from robotCard() markup, then builds the capability-
// driven control surface and binds the e-stop / share handlers once. Listeners
// live on the persistent element, so update polls never rebind.
function buildCard(r) {
  const tmp = document.createElement('div');
  tmp.innerHTML = robotCard(r);
  const el = tmp.firstElementChild;

  const id = robotId(r);
  // E-stop is rendered as static markup (always present) — bind it once.
  el.querySelectorAll('[data-control="estop"]').forEach((btn) => {
    btn.addEventListener('click', () => handleControl(id, 'estop', btn));
  });
  el.querySelectorAll('[data-share-camera]').forEach((btn) => {
    btn.addEventListener('click', () => {
      handleShareCamera(btn.dataset.robot, btn.dataset.shareCamera, btn);
    });
  });

  // Capability-driven controls live in their own sub-container so they can be
  // rebuilt independently of the video node when the advertised action set
  // changes — the <tf-video-stream> is NEVER touched.
  buildControls(el, r);
  return el;
}

// Renders the capability-driven controls into [data-field="controls"], grouped by
// kind/risk. Rebuilt only when the advertised action signature actually changed,
// so a steady action set survives every poll untouched (and the video node above
// it stays stable). The e-stop button is separate static markup, never rebuilt.
function buildControls(el, r) {
  const host = el.querySelector('[data-field="controls"]');
  if (!host) return;

  const meta = actionsMeta(r);
  const sig = controlsSignature(meta);
  if (host.dataset.controlsSig === sig) return;
  host.dataset.controlsSig = sig;
  host.innerHTML = '';

  const id = robotId(r);
  const controllable = meta.filter((a) => !NON_CONTROL_KINDS.has(a.kind) && !actionReadOnly(a));

  // Locomotion dpad (only when "move" is advertised).
  const move = controllable.find((a) => a.kind === 'move');
  if (move) host.appendChild(buildMoveGroup(id, move));

  // Parametered poses/levels (euler/pose/body_height/foot_raise_height/speed_level
  // and any other action that advertises params).
  const parametered = controllable.filter((a) => a.kind !== 'move' && actionParams(a).length > 0);
  if (parametered.length) {
    host.appendChild(buildGroup('Pozy', parametered.map((a) => buildParameteredControl(id, a))));
  }

  // Simple parameterless actions, split by risk so acrobatics are visually and
  // behaviourally distinct.
  const simple = controllable.filter(
    (a) => a.kind !== 'move' && actionParams(a).length === 0 && !isHighRisk(a),
  );
  if (simple.length) {
    host.appendChild(buildGroup('Akcje', simple.map((a) => buildSimpleButton(id, a))));
  }

  const acrobatic = controllable.filter(
    (a) => a.kind !== 'move' && actionParams(a).length === 0 && isHighRisk(a),
  );
  if (acrobatic.length) {
    host.appendChild(buildGroup('Akrobacje', acrobatic.map((a) => buildSimpleButton(id, a))));
  }
}

// Stable signature of the action set: any change in the set/order/risk/params of
// advertised actions triggers a controls rebuild; rtt/battery churn does not.
function controlsSignature(meta) {
  return meta
    .map((a) => {
      const params = actionParams(a)
        .map((p) => `${p.name}:${p.min}:${p.max}`)
        .join(',');
      return `${a.kind}|${a.label}|${actionRisk(a)}|${actionAcrobatic(a) ? 1 : 0}|${actionReadOnly(a) ? 1 : 0}|${params}`;
    })
    .join(';');
}

// A labelled group wrapper holding one or more control nodes.
function buildGroup(title, nodes) {
  const group = document.createElement('div');
  group.className = 'robots-control-group';
  const heading = document.createElement('div');
  heading.className = 'robots-control-group-title';
  heading.textContent = title;
  group.appendChild(heading);
  const body = document.createElement('div');
  body.className = 'robots-controls-row';
  nodes.forEach((n) => body.appendChild(n));
  group.appendChild(body);
  return group;
}

// Directional pad (forward/back/left/right + rotate) sending "move" with clamped
// vx/vy/vyaw. Reuses the driver's own ±MOVE_SPEED magnitudes.
function buildMoveGroup(id, move) {
  const dirs = [
    { label: 'Przód', icon: 'arrow', vx: MOVE_SPEED, vy: 0, vyaw: 0 },
    { label: 'Tył', icon: 'arrow', vx: -MOVE_SPEED, vy: 0, vyaw: 0 },
    { label: 'Lewo', icon: 'arrow', vx: 0, vy: MOVE_SPEED, vyaw: 0 },
    { label: 'Prawo', icon: 'arrow', vx: 0, vy: -MOVE_SPEED, vyaw: 0 },
    { label: 'Obrót L', icon: 'rotate', vx: 0, vy: 0, vyaw: MOVE_SPEED },
    { label: 'Obrót P', icon: 'rotate', vx: 0, vy: 0, vyaw: -MOVE_SPEED },
  ];
  const nodes = dirs.map((d) => {
    const btn = document.createElement('tf-button');
    btn.setAttribute('variant', 'outline');
    btn.setAttribute('size', 'sm');
    btn.setAttribute('icon', d.icon);
    btn.dataset.control = 'move';
    btn.textContent = d.label;
    btn.addEventListener('click', () =>
      handleControl(id, 'move', btn, { vx: d.vx, vy: d.vy, vyaw: d.vyaw }, move.label || 'Ruch'),
    );
    return btn;
  });
  return buildGroup(move.label || 'Ruch', nodes);
}

// Simple parameterless action button. High-risk / acrobatic actions get the danger
// variant and a confirm dialog before sending; low/medium use the outline variant.
function buildSimpleButton(id, a) {
  const high = isHighRisk(a);
  const btn = document.createElement('tf-button');
  btn.setAttribute('variant', high ? 'danger' : 'outline');
  btn.setAttribute('size', 'sm');
  btn.setAttribute('icon', high ? 'alert' : 'sparkle');
  btn.dataset.control = a.kind;
  btn.textContent = a.label || a.kind;
  btn.addEventListener('click', () => handleControl(id, a.kind, btn, null, a.label, high));
  return btn;
}

// Parametered control: one bounded input per param plus a send button. speed_level
// renders a tf-select (-1/0/1). Values are written into FIXED p1..p4 slots BY NAME
// (slotsForKind), never compacted by index, so a missing param can't shift a later
// one into the wrong axis. If a REQUIRED documented param is missing from the
// advertised meta, the control refuses to send (disabled + inline error).
function buildParameteredControl(id, a) {
  const wrap = document.createElement('div');
  wrap.className = 'robots-param-control';

  const title = document.createElement('div');
  title.className = 'robots-param-title';
  title.textContent = a.label || a.kind;
  wrap.appendChild(title);

  const params = actionParams(a);
  const high = isHighRisk(a);
  const slots = slotsForKind(a.kind, params);

  // For a KNOWN kind, every required documented param must be advertised; missing
  // one means the descriptor is malformed for this kind, so we refuse rather than
  // guess which slot each value belongs to.
  const required = REQUIRED_PARAMS[a.kind];
  const advertisedNames = new Set(params.map((p) => p.name));
  const missingRequired = required
    ? required.filter((name) => !advertisedNames.has(name))
    : [];

  // speed_level is a discrete level, not a continuous range: a select is clearer.
  if (a.kind === 'speed_level') {
    const sel = document.createElement('tf-select');
    sel.setAttribute('label', 'Poziom');
    sel.setOptions(
      [
        { value: '-1', label: 'Wolno' },
        { value: '0', label: 'Normalnie' },
        { value: '1', label: 'Szybko' },
      ],
      '0',
    );
    wrap.appendChild(sel);
    const send = makeSendButton(a.label || a.kind);
    if (missingRequired.length) {
      refuseControl(wrap, send, a.kind, missingRequired);
    } else {
      send.addEventListener('click', () =>
        handleControl(id, a.kind, send, { p1: Number(sel.value) }, a.label, high),
      );
    }
    wrap.appendChild(send);
    return wrap;
  }

  // Build one input per param that has a FIXED slot. For unknown kinds slotsForKind
  // assigns slots by the meta's declared order (no compaction); a param it can't
  // place at all (more than 4 on an unknown kind) is reported as ambiguous below.
  const fields = [];
  let ambiguous = false;
  for (const p of params) {
    const slot = slots.get(p.name);
    if (!slot) {
      ambiguous = true;
      continue;
    }
    const input = document.createElement('tf-input');
    input.setAttribute('type', 'number');
    input.setAttribute('label', paramLabel(p.name));
    if (Number.isFinite(p.min)) input.setAttribute('min', String(p.min));
    if (Number.isFinite(p.max)) input.setAttribute('max', String(p.max));
    input.setAttribute('step', '0.01');
    input.setAttribute('value', '0');
    wrap.appendChild(input);
    fields.push({ input, slot, min: p.min, max: p.max });
  }

  const send = makeSendButton(a.label || a.kind);

  // Refuse on a malformed/ambiguous descriptor — prefer refusing over guessing for
  // anything that moves the robot.
  if (missingRequired.length) {
    refuseControl(wrap, send, a.kind, missingRequired);
  } else if (ambiguous) {
    refuseControl(wrap, send, a.kind, null, 'Niejednoznaczne parametry akcji');
  } else {
    send.addEventListener('click', () => {
      const payload = {};
      for (const f of fields) {
        let v = Number(f.input.value);
        if (!Number.isFinite(v)) v = 0;
        if (Number.isFinite(f.min)) v = Math.max(v, f.min);
        if (Number.isFinite(f.max)) v = Math.min(v, f.max);
        payload[f.slot] = v;
      }
      handleControl(id, a.kind, send, payload, a.label, high);
    });
  }
  wrap.appendChild(send);
  return wrap;
}

// Disables the send button and surfaces an inline error so a malformed descriptor
// can't be sent with shifted/guessed slots.
function refuseControl(wrap, send, kind, missing, customMsg) {
  send.setAttribute('disabled', '');
  const err = document.createElement('div');
  err.className = 'robots-param-error';
  err.textContent =
    customMsg ||
    `Brak wymaganych parametrów (${(missing || []).join(', ')}) — akcji „${kind}" nie można wysłać`;
  wrap.appendChild(err);
}

function makeSendButton(label) {
  const send = document.createElement('tf-button');
  send.setAttribute('variant', 'primary');
  send.setAttribute('size', 'sm');
  send.setAttribute('icon', 'check');
  send.textContent = `Wyślij (${label})`;
  return send;
}

// Maps each advertised param name to its FIXED wire slot. For a KNOWN kind the slot
// comes from the documented PARAM_SLOTS map (by name, never by index) so a missing
// param leaves its slot empty instead of shifting later params. For an unknown kind
// (no documented map) slots are assigned in the meta's declared param order WITHOUT
// compaction gaps — beyond p4 the param is left unmapped so the caller refuses as
// ambiguous rather than dropping it silently. Returns a Map<name, "pN">.
function slotsForKind(kind, params) {
  const known = PARAM_SLOTS[kind];
  if (known) {
    const out = new Map();
    for (const p of params) {
      if (known[p.name]) out.set(p.name, known[p.name]);
    }
    return out;
  }
  const out = new Map();
  const generic = ['p1', 'p2', 'p3', 'p4'];
  params.forEach((p, idx) => {
    if (idx < generic.length) out.set(p.name, generic[idx]);
  });
  return out;
}

function paramLabel(name) {
  const map = {
    roll: 'Roll',
    pitch: 'Pitch',
    yaw: 'Yaw',
    height: 'Wysokość',
    vx: 'vx',
    vy: 'vy',
    vyaw: 'vyaw',
    level: 'Poziom',
  };
  return map[name] || name;
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

  // Refresh the telemetry panel in place (its own container, never the video
  // node above it). Only present fields render; an absent snapshot hides it.
  updateTelemetry(el, r);

  // Refresh the LiDAR row in place (its own container, never the video node).
  updateLidar(el, r);

  // Rebuild the capability-driven controls only when the advertised action set
  // actually changed (signature compare inside buildControls). This never touches
  // the video node above it.
  buildControls(el, r);

  // Offline robots can't take commands: disable every control + share button —
  // EXCEPT the e-stop family. STOP must stay clickable regardless of advertised
  // status so safety never depends on status metadata (the server still validates).
  const offline = !isControllable(status);
  el.querySelectorAll('[data-control], [data-share-camera]').forEach((btn) => {
    const ctrl = btn.dataset.control;
    if (ctrl === 'estop' || ctrl === 'stop' || ctrl === 'reset_estop') {
      btn.removeAttribute('disabled');
      return;
    }
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

// Rebuilds the telemetry panel from the latest snapshot, IN PLACE inside the
// [data-field="telemetry"] container. The container sits below the video node and
// is never the video node itself, so the live <tf-video-stream> is untouched. Only
// fields actually present render; an absent snapshot hides the whole panel.
function updateTelemetry(el, r) {
  const host = el.querySelector('[data-field="telemetry"]');
  if (!host) return;
  const t = telemetry(r);
  if (!t) {
    host.hidden = true;
    host.innerHTML = '';
    return;
  }

  const rows = [];
  const mode = telNum(t, 'mode', 'mode');
  const gait = telNum(t, 'gaitType', 'gait_type');
  if (mode != null) rows.push(telRow('Tryb', String(Math.round(mode))));
  if (gait != null) rows.push(telRow('Chód', String(Math.round(gait))));
  const bh = telNum(t, 'bodyHeight', 'body_height');
  if (bh != null) rows.push(telRow('Wys. ciała', `${bh.toFixed(2)} m`));

  const vx = telNum(t, 'vx', 'vx');
  const vy = telNum(t, 'vy', 'vy');
  const vyaw = telNum(t, 'vyaw', 'vyaw');
  if (vx != null || vy != null || vyaw != null) {
    const parts = [];
    if (vx != null) parts.push(`vx ${vx.toFixed(2)}`);
    if (vy != null) parts.push(`vy ${vy.toFixed(2)}`);
    if (vyaw != null) parts.push(`yaw ${vyaw.toFixed(2)}`);
    rows.push(telRow('Prędkość', parts.join('  ·  ')));
  }

  const imu = t.imu || null;
  if (imu) {
    const roll = radToDeg(telNum(imu, 'roll', 'roll'));
    const pitch = radToDeg(telNum(imu, 'pitch', 'pitch'));
    const yaw = radToDeg(telNum(imu, 'yaw', 'yaw'));
    if (roll != null || pitch != null || yaw != null) {
      const parts = [];
      if (roll != null) parts.push(`R ${roll.toFixed(1)}°`);
      if (pitch != null) parts.push(`P ${pitch.toFixed(1)}°`);
      if (yaw != null) parts.push(`Y ${yaw.toFixed(1)}°`);
      rows.push(telRow('Orientacja', parts.join('  ·  ')));
    }
    const imuTemp = telNum(imu, 'temperature', 'temperature');
    if (imuTemp != null) rows.push(telRow('Temp. IMU', `${imuTemp.toFixed(0)} °C`));
  }

  const foot = Array.isArray(t.footForce ?? t.foot_force) ? t.footForce ?? t.foot_force : [];
  if (foot.length) {
    const vals = foot.map((f) => (Number.isFinite(Number(f)) ? Math.round(Number(f)) : '—'));
    rows.push(telRow('Siły stóp', vals.join('  ·  ')));
  }

  const bat = t.battery || null;
  if (bat) {
    const soc = telNum(bat, 'soc', 'soc');
    const volt = telNum(bat, 'voltage', 'voltage');
    const curr = telNum(bat, 'current', 'current');
    const temp = telNum(bat, 'temperature', 'temperature');
    if (soc != null) rows.push(telRow('Bateria SOC', `${Math.round(soc)} %`));
    if (volt != null) rows.push(telRow('Napięcie', `${volt.toFixed(1)} V`));
    if (curr != null) rows.push(telRow('Prąd', `${curr.toFixed(1)} A`));
    if (temp != null) rows.push(telRow('Temp. baterii', `${temp.toFixed(0)} °C`));
  }

  if (!rows.length) {
    host.hidden = true;
    host.innerHTML = '';
    return;
  }
  host.hidden = false;
  host.innerHTML = `
    <div class="robots-telemetry-title">Telemetria</div>
    <div class="robots-telemetry-rows">${rows.join('')}</div>`;
}

// Renders the LiDAR row IN PLACE inside [data-field="lidar"]: an enable/disable
// toggle (routed to go2.lidar_on/off via the standard control path) plus a LIVE
// status (point count, measured FPS, freshness) fed by the real-time PUSH stream.
// NO 3D canvas/renderer here — this is the data-path surface only. The container
// sits below the telemetry panel and is never the video node, so the live
// <tf-video-stream> is untouched. The toggle + status nodes are built once and
// then updated in place each poll so a click handler is never lost.
//
// This also OWNS the subscription lifecycle: a robot is subscribed to its PUSH
// stream only while its row is visible AND LiDAR is enabled AND the robot is
// online; any of those dropping closes the subscription (and stops the shared
// staleness sweep when none remain), so no stream stays open on a disabled / gone
// / offline source.
function updateLidar(el, r) {
  const host = el.querySelector('[data-field="lidar"]');
  if (!host) return;
  const l = lidar(r);
  const id = robotId(r);
  // Only show the row for a robot that advertises a LiDAR snapshot (capability).
  if (!l) {
    host.hidden = true;
    host.innerHTML = '';
    host.dataset.built = '';
    stopRobotLidar(id);
    return;
  }
  const offline = !isControllable(r.status || '');
  const enabled = !!l.enabled;
  const available = !!l.available;
  // Snapshot point count from the list — shown until the first live frame lands.
  const snapshotPoints = Number(l.pointCount ?? l.point_count ?? 0);
  const resolution = l.resolution;

  // Build the static structure once; subsequent polls only refresh text/state.
  if (host.dataset.built !== '1') {
    host.innerHTML = `
      <div class="robots-lidar-title">LiDAR</div>
      <div class="robots-lidar-row">
        <tf-toggle data-lidar-toggle></tf-toggle>
        <span class="robots-lidar-status" data-lidar-status></span>
        <tf-badge data-lidar-fresh tone="info" value="—" hidden></tf-badge>
      </div>`;
    const toggle = host.querySelector('[data-lidar-toggle]');
    toggle.addEventListener('change', (e) => {
      // tf-toggle emits its boolean either on e.detail or as its `checked` prop.
      const on = e?.detail?.checked ?? e?.detail ?? toggle.checked ?? toggle.hasAttribute('checked');
      handleLidarToggle(id, !!on, toggle);
    });
    host.dataset.built = '1';
  }

  host.hidden = false;
  const toggle = host.querySelector('[data-lidar-toggle]');
  if (toggle) {
    if (enabled) toggle.setAttribute('checked', '');
    else toggle.removeAttribute('checked');
    if (offline) toggle.setAttribute('disabled', '');
    else toggle.removeAttribute('disabled');
  }

  // Open / close the real-time PUSH subscription. We only subscribe when the
  // source can actually produce frames; otherwise the static text (below) suffices.
  if (enabled && !offline) {
    startRobotLidar(id);
  } else {
    stopRobotLidar(id);
  }

  // Render the status. While a live entry has frames, the live numbers win; the
  // snapshot value is only the placeholder shown before the first frame arrives.
  renderLidarStatus(host, { enabled, available, offline, snapshotPoints, resolution });
}

// Writes the LiDAR status text + freshness badge. Called from updateLidar() (4 s
// list cadence) and from the PUSH handler (per live frame) so the live numbers
// refresh at frame rate, not only every 4 s.
function renderLidarStatus(host, { enabled, available, offline, snapshotPoints, resolution }) {
  const status = host.querySelector('[data-lidar-status]');
  const fresh = host.querySelector('[data-lidar-fresh]');
  if (!status) return;

  if (!enabled) {
    status.textContent = 'wyłączony';
    if (fresh) fresh.hidden = true;
    return;
  }
  if (offline) {
    status.textContent = 'robot offline';
    if (fresh) fresh.hidden = true;
    return;
  }

  const res = typeof resolution === 'number' && Number.isFinite(resolution)
    ? `  ·  ${resolution.toFixed(2)} m`
    : '';

  // The live entry exists for an active robot; once a frame has landed we show the
  // live point count + measured FPS, otherwise we fall back to the snapshot count.
  const id = host.closest('[data-robot-card]')?.dataset.robotCard || '';
  const live = lidarLive.get(id);
  const hasLiveFrame = !!(live && live.lastFrameAtMs);

  if (hasLiveFrame) {
    const points = live.lastPointCount;
    const fps = computeLidarFps(live);
    const ageMs = performance.now() - live.lastFrameAtMs;
    const stale = ageMs > LIDAR_STALE_AFTER_MS;
    status.textContent =
      `${points} ${plural(points, 'punkt', 'punkty', 'punktów')}  ·  ${fps.toFixed(1)} kl./s${res}`;
    if (fresh) {
      fresh.hidden = false;
      fresh.setAttribute('tone', stale ? 'warning' : 'success');
      fresh.setAttribute('value', stale ? 'nieaktualne' : 'na żywo');
    }
    return;
  }

  // Active but no live frame yet: show the snapshot count (if any) as a hint while
  // we wait for the first binary frame to arrive.
  if (available && snapshotPoints > 0) {
    status.textContent =
      `${snapshotPoints} ${plural(snapshotPoints, 'punkt', 'punkty', 'punktów')}${res}`;
  } else {
    status.textContent = 'aktywny, oczekiwanie na dane…';
  }
  if (fresh) {
    fresh.hidden = false;
    fresh.setAttribute('tone', 'info');
    fresh.setAttribute('value', 'łączenie…');
  }
}

// Measured frame rate over the sliding window: count frames whose arrival falls
// inside the last LIDAR_FPS_WINDOW_MS and divide by the actual span covered. Using
// the real span (not the nominal window) keeps the number honest right after the
// stream starts when fewer than a full window of samples exist.
function computeLidarFps(live) {
  const now = performance.now();
  const cutoff = now - LIDAR_FPS_WINDOW_MS;
  const times = live.frameTimes;
  while (times.length && times[0] < cutoff) times.shift();
  if (times.length < 2) return 0;
  const spanMs = times[times.length - 1] - times[0];
  if (spanMs <= 0) return 0;
  return ((times.length - 1) * 1000) / spanMs;
}

// Opens the real-time PUSH subscription for a robot (idempotent) and ensures the
// shared staleness sweep is running. Does NOT reset an existing entry, so an
// in-progress FPS window survives a 4 s list refresh. The subscription uses the
// generic StreamHub rails (the SAME call shape tf-video-stream uses); every frame
// arrives as a StreamFrame whose `data` is the raw canonical L1 bytes.
function startRobotLidar(id) {
  if (!id) return;
  if (lidarLive.has(id)) return;
  const live = {
    unsub: null,
    closed: false,
    resubTimer: null,
    // One re-subscribe per live session. A fresh start (this object) resets it;
    // once consumed we never re-arm, so sustained lag can't spin a 1 s retry loop.
    resubUsed: false,
    frameTimes: [],
    lastPointCount: 0,
    lastFrameAtMs: 0,
    lastRaw: null,
    lastPoints: null,
  };
  lidarLive.set(id, live);
  if (lidarTimer == null) {
    lidarTimer = window.setInterval(sweepLidarStaleness, LIDAR_STALE_SWEEP_MS);
  }
  openLidarSubscription(id, live);
}

// Subscribes to `lidar:<id>` and wires the per-frame / end / error handlers.
// Mirrors tf-video-stream's deferred-unsub guard: if the robot is torn down while
// ApiBinary.subscribe is still resolving, the late-resolved unsub is fired at once
// so no subscription leaks.
function openLidarSubscription(id, live) {
  const pending = { disposed: false };
  live.unsub = () => {
    pending.disposed = true;
  };
  ApiBinary.subscribe(
    'streamSubscribeRequest',
    { streamId: `lidar:${id}` },
    {
      onChunk: (body) => onLidarChunk(id, live, body),
      onEnd: (body) => onLidarEnd(id, live, body),
      onError: (err) => onLidarError(id, live, err),
    },
  )
    .then((unsub) => {
      if (pending.disposed || !lidarLive.has(id) || lidarLive.get(id) !== live) {
        try { unsub(); } catch { /* ignore */ }
        return;
      }
      live.unsub = () => {
        try { unsub(); } catch (e) { console.warn('[robots] lidar unsub threw:', e); }
      };
    })
    .catch((err) => {
      console.warn('[robots] lidar subscribe failed:', err?.message ?? err);
    });
}

// A pushed frame: decode the RAW canonical bytes via the sdk-spec header decoder.
// hasFrame:false (malformed/short) is ignored. On a real frame we update the live
// point count, the FPS sliding window and the freshness timestamp, stash raw/points
// for L4 (not rendered here), then re-render just this card.
function onLidarChunk(id, live, body) {
  if (!body || typeof body !== 'object') return;
  // The StreamHub also pushes a StreamSubscribeResponse handshake (mime_type);
  // only StreamFrame carries point-cloud bytes.
  if (body.variant !== 'StreamFrame') return;
  const data = body.data;
  if (!(data instanceof Uint8Array) || data.byteLength === 0) return;
  // The current live entry may have been torn down/replaced while this chunk was
  // in flight — drop it rather than resurrect a dead source.
  if (lidarLive.get(id) !== live) return;
  let f;
  try {
    f = decodeLidarFrame(data);
  } catch (e) {
    console.warn('[robots] lidar decode failed:', e?.message ?? e);
    return;
  }
  if (!f || !(f.hasFrame ?? f.has_frame)) return;
  live.lastPointCount = Number(f.pointCount ?? f.point_count ?? 0);
  live.lastFrameAtMs = performance.now();
  live.frameTimes.push(live.lastFrameAtMs);
  // Bound the window buffer so a long-lived stream can't grow it unbounded.
  const cutoff = live.lastFrameAtMs - LIDAR_FPS_WINDOW_MS;
  while (live.frameTimes.length && live.frameTimes[0] < cutoff) live.frameTimes.shift();
  // Stash the raw frame + packed points for the L4 wgpu renderer (not drawn yet).
  live.lastRaw = f.raw ?? null;
  live.lastPoints = f.points ?? null;
  refreshLidarCard(id);
}

// Stream ended. A `subscriber_lagged` end means the server dropped us because we
// fell behind — re-subscribe ONCE per live session after a short backoff (mirrors
// tf-video-stream). Under sustained lag the server keeps ending the (re)stream;
// `resubUsed` makes the retry genuinely one-shot so we don't schedule a new 1 s
// resubscribe on every lagged end forever. After the single retry is spent the
// badge just goes stale until the user re-enables (a fresh session resets the
// flag). Any other end (source unregistered, our own close) leaves it stale too.
function onLidarEnd(id, live, body) {
  if (lidarLive.get(id) !== live) return;
  const reason = String(body?.reason ?? '');
  // Our own close already dropped the entry; nothing to do.
  if (reason === 'client_request') return;
  if (reason === 'subscriber_lagged') {
    if (live.resubTimer != null) return;
    if (live.resubUsed) return;
    live.resubUsed = true;
    live.resubTimer = window.setTimeout(() => {
      live.resubTimer = null;
      if (lidarLive.get(id) !== live) return;
      openLidarSubscription(id, live);
    }, LIDAR_RESUBSCRIBE_DELAY_MS);
    return;
  }
  // Source unregistered / unknown end: stop pumping, let freshness go stale.
  live.unsub = null;
}

function onLidarError(id, live, err) {
  if (lidarLive.get(id) !== live) return;
  console.warn('[robots] lidar stream error:', err?.message ?? err);
  // Leave the badge to go stale; the user can re-enable. No aggressive retry.
}

// Closes a robot's PUSH subscription, drops its live state and stops the shared
// staleness sweep once no robot remains active. Called on toggle-off, robot
// offline, card removal, panel close and unmount — every start has a matching
// close here, so no subscription is ever leaked.
function stopRobotLidar(id) {
  if (id) {
    const live = lidarLive.get(id);
    if (live) {
      closeLidarSubscription(id, live);
      lidarLive.delete(id);
    }
  }
  if (lidarLive.size === 0) stopLidarLoop();
}

// Tears down a single live entry: cancels a pending re-subscribe and fires the
// subscription handle. That handle (from ApiBinary.subscribe) drops the local
// listener AND emits a StreamCloseRequest on the ORIGINAL subscribe correlation
// id, which is the only id the server's close handler can cancel by — so this is
// what actually frees the server-side slot + frame pump. The handle is set even
// while ApiBinary.subscribe is still resolving (it flips `pending.disposed`), so
// a teardown that races an in-flight subscribe still closes once it resolves.
function closeLidarSubscription(id, live) {
  live.closed = true;
  if (live.resubTimer != null) {
    window.clearTimeout(live.resubTimer);
    live.resubTimer = null;
  }
  if (live.unsub) {
    try { live.unsub(); } catch { /* ignore */ }
    live.unsub = null;
  }
}

function stopLidarLoop() {
  if (lidarTimer != null) {
    window.clearInterval(lidarTimer);
    lidarTimer = null;
  }
  // Close every still-open subscription before clearing, so unmount/error paths
  // that call stopLidarLoop directly never leak a stream.
  for (const [id, live] of lidarLive) closeLidarSubscription(id, live);
  lidarLive.clear();
}

// One tick of the shared staleness sweep: re-render every active card so the
// freshness badge flips to "nieaktualne" even when frames simply STOP arriving (no
// push would otherwise re-render). Carries no network traffic.
function sweepLidarStaleness() {
  for (const [id, live] of lidarLive) {
    if (live.lastFrameAtMs) refreshLidarCard(id);
  }
}

// Re-renders just one robot's LiDAR status from its current live state, without a
// full list refresh — driven by the PUSH handler on each new frame and by a
// periodic staleness sweep so the freshness badge flips even when frames stop.
function refreshLidarCard(id) {
  const el = cardEls.get(id);
  if (!el) return;
  const host = el.querySelector('[data-field="lidar"]');
  if (!host || host.hidden) return;
  const r = robots.find((x) => robotId(x) === id);
  const l = r ? lidar(r) : null;
  const offline = r ? !isControllable(r.status || '') : false;
  renderLidarStatus(host, {
    enabled: !!(l && l.enabled),
    available: !!(l && l.available),
    offline,
    snapshotPoints: Number(l?.pointCount ?? l?.point_count ?? 0),
    resolution: l?.resolution,
  });
}

// Sends a LiDAR enable/disable through the standard robot control path
// (lidar_on / lidar_off kinds → go2.lidar_on/off, routed locally or over the mesh).
async function handleLidarToggle(id, on, toggle) {
  if (!id) return;
  toggle.setAttribute('disabled', '');
  try {
    const resp = await ApiBinary.action('robotControlRequest', {
      robotId: id,
      kind: on ? 'lidar_on' : 'lidar_off',
      vx: 0, vy: 0, vyaw: 0, p1: 0, p2: 0, p3: 0, p4: 0,
    });
    if (resp.ok) {
      // React to the action RESULT immediately so the live state reflects the new
      // setting without waiting for (or depending on) the 4 s list refresh: close
      // the subscription on a now-disabled source at once; open one only AFTER a
      // confirmed enable. Also patch the local snapshot row so the toggle/status
      // stay consistent until the next authoritative refresh.
      const r = robots.find((x) => robotId(x) === id);
      const l = r ? lidar(r) : null;
      if (l) l.enabled = on;
      if (on) startRobotLidar(id);
      else stopRobotLidar(id);
      refreshLidarCard(id);
      toast(`LiDAR ${on ? 'włączony' : 'wyłączony'} ✓`, 'success');
    } else if (resp.rejected) {
      toast(`LiDAR: odrzucono — ${resp.rejected}`, 'error');
    } else {
      toast(`LiDAR: błąd — ${resp.error || 'nieznany'}`, 'error');
    }
  } catch (err) {
    toast(`LiDAR: ${err.message}`, 'error');
  } finally {
    toggle.removeAttribute('disabled');
  }
}

// One compact telemetry row (label + value), mirroring the metricRow pattern.
function telRow(label, value) {
  return `
    <div class="robots-telemetry-row">
      <span class="robots-telemetry-label">${escapeHtml(label)}</span>
      <span class="robots-telemetry-value">${escapeHtml(value)}</span>
    </div>`;
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
  const cam = cameraId(r);

  // Battery/RTT rows always render so updateCard() has a stable slot to fill;
  // they're hidden via [hidden] when the value is absent (mirrors null checks).
  const metrics = [
    metricRow('zap', 'Bateria', battery != null ? `${Math.round(Number(battery))}%` : '', 'battery', battery == null),
    metricRow('bolt', 'RTT', rtt != null ? `${Math.round(Number(rtt))} ms` : '', 'rtt', rtt == null),
    metricRow('host', 'Węzeł', '', 'owner', false, owner),
  ];

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

  // Controls sub-container is populated by buildControls() AFTER the card mounts,
  // so the action set can be rebuilt independently of the video node above it.
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
      ${cameraHtml}

      <div class="robots-telemetry" data-field="telemetry" hidden></div>

      <div class="robots-lidar" data-field="lidar" hidden></div>

      <div class="robots-controls" data-field="controls"></div>

      <div class="robots-estop">
        <tf-button variant="danger-solid" icon="stop" full-width
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

// Sends a control action. `params` carries optional vx/vy/vyaw (move) and p1..p4
// (parametered kinds). `requireConfirm` gates high-risk / acrobatic actions behind
// a modal — they are NEVER sent on a single click.
async function handleControl(id, kind, btn, params, label, requireConfirm = false) {
  if (!id || !kind) return;

  if (requireConfirm) {
    const ok = await tfConfirmAcrobatic(label || kind);
    if (!ok) return;
  }

  btn.setAttribute('loading', '');
  try {
    const resp = await ApiBinary.action('robotControlRequest', {
      robotId: id,
      kind,
      vx: Number(params?.vx ?? 0),
      vy: Number(params?.vy ?? 0),
      vyaw: Number(params?.vyaw ?? 0),
      p1: Number(params?.p1 ?? 0),
      p2: Number(params?.p2 ?? 0),
      p3: Number(params?.p3 ?? 0),
      p4: Number(params?.p4 ?? 0),
    });
    // A robot-level refusal is still a successful call carrying `rejected`;
    // `error` is an execution failure (RobotControlResponse, message_body.rs).
    const rejected = resp.rejected;
    const error = resp.error;
    const name = label || kind;
    if (resp.ok) {
      toast(`Robot ${shortNode(id)}: ${name} ✓`, 'success');
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

// Confirmation gate for acrobatic / high-risk actions. Returns true only when the
// operator explicitly confirms; ESC / backdrop / cancel resolve to false.
async function tfConfirmAcrobatic(label) {
  const choice = await window.customElements
    .get('tf-modal')
    .open({
      title: 'Potwierdź akcję wysokiego ryzyka',
      body: `„${label}" to akcja akrobatyczna / wysokiego ryzyka. Upewnij się, że robot ma wolną przestrzeń. Potwierdzić wykonanie?`,
      actions: [
        { label: 'Anuluj', value: false },
        { label: 'Wykonaj', value: true, primary: true },
      ],
    });
  return choice === true;
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

function plural(n, one, few, many) {
  const mod10 = n % 10;
  const mod100 = n % 100;
  if (n === 1) return one;
  if (mod10 >= 2 && mod10 <= 4 && (mod100 < 10 || mod100 >= 20)) return few;
  return many;
}

export default RobotsScreen;
