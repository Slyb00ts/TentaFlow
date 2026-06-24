// ===== File: robots.js — Robots core app (mesh robot list + tabbed detail) =====
// Talks to Core over the binary protocol via MessageBody::RobotsBody:
//   RobotsListRequest / RobotControlRequest / RobotCameraShareRequest.
// Two in-module views (no app.js route — state lives here):
//   LIST   — one card per robot advertised in the mesh (status, KPIs, an always-
//            present E-stop + a few quick controls routed through the SAME
//            capability dispatch as the full surface, capability chips and a
//            "Szczegóły" button that opens the detail).
//   DETAIL — a single robot in tabs (Przegląd / Kamera / LiDAR 3D / Sterowanie /
//            Informacje / Log): live camera, live LiDAR (data-path + wgpu voxel
//            view), the full CAPABILITY-DRIVEN control surface generated from the
//            robot's advertised `actions_meta` (label / risk / param schema), live
//            telemetry + the 3D robot model, and a control-outcome log.
// High-risk / acrobatic actions are gated behind a confirm dialog using the UNION
// of advertised risk and an authoritative client-side known-dangerous set (a
// hostile descriptor can't downgrade a flip to a one-click button). Parametered
// actions write values into FIXED p1..p4 slots BY NAME and refuse to send if a
// required param is missing (no slot shifting). An always-present E-stop is
// independent of the advertised metadata and stays clickable even when the robot
// reads offline — safety must never depend on status metadata. The server remains
// the real safety gate. The list/detail auto-refresh so mesh discovery state and
// live telemetry stay visible. tf-* components only; results surface via tf-toast.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
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
import '/js/components/tf-tabs.js';
import '/js/components/tf-video-stream.js';
import '/js/components/tf-robot-view.js';

// Mesh discovery refreshes the registry periodically; re-poll so status, battery
// and online/offline transitions appear without a manual reload.
const REFRESH_INTERVAL_MS = 4000;

// Real-time LiDAR PUSH: the open detail subscribes to its robot's canonical frame
// stream via the generic StreamHub rails (`streamId = "lidar:<robotId>"`), the
// SAME path camera video uses. The server pushes every canonical L1 frame as a
// StreamFrame whose `data` is the raw frame bytes; we decode it with the sdk-spec
// header decoder and feed the decoded points to the wgpu voxel renderer.
//
// Sliding window for the measured frame rate. Wide enough to smooth the ~5 fps
// arrival jitter, short enough to react quickly when streaming stops.
const LIDAR_FPS_WINDOW_MS = 2000;

// End-to-end latency is `now_wall - frame.timestampUs`. For the local case the
// addon, Core and browser share one wall clock, so the value is real; across
// machines (or with clock skew) it can be negative or absurdly large. Anything
// outside [0, 30 s] is treated as unmeasurable skew and dropped from the average
// rather than poisoning it — the cadence/decode numbers stay valid regardless.
const LIDAR_E2E_MAX_MS = 30000;

// A frame older than this reads as "stale" — at 5 fps a healthy stream lands a
// new frame every ~200 ms, so 1.5 s without one means the source went quiet. A
// periodic staleness sweep flips the badge even when frames simply stop arriving.
const LIDAR_STALE_AFTER_MS = 1500;

// Cadence of the freshness sweep that re-renders the open detail's LiDAR status so
// the badge can flip to "nieaktualne" when frames stop (no push would otherwise
// re-render). NOT a poll — it carries no network traffic.
const LIDAR_STALE_SWEEP_MS = 500;

// After the server ends a lidar stream because the subscriber lagged, wait this
// long before a single re-subscribe so the UI doesn't flap. We do NOT spin: only
// a `subscriber_lagged` end triggers one retry; any other end leaves it stale.
const LIDAR_RESUBSCRIBE_DELAY_MS = 1000;

// Locomotion magnitude (m/s) for the directional pad / quick-move buttons,
// matching the go2 driver's own move buttons. The owner re-clamps to its safety
// cap regardless.
const MOVE_SPEED = 0.3;
// Yaw rate (rad/s) for turning. The Go2 has a minimum effective turn rate (~0.5
// rad/s); below it the robot simply does not rotate. Linear speed (m/s) and yaw
// rate (rad/s) are DIFFERENT units, so turning uses its own higher magnitude with
// a floor that clears the threshold, instead of reusing the small linear speed.
const YAW_SPEED = 0.9;
const PAD_YAW_MIN = 0.7;
const PAD_YAW_MAX = 1.8;

// Bounded control-outcome history shown in the detail "Log" tab. Old entries are
// trimmed so a long session can't grow this without bound.
const LOG_MAX_ENTRIES = 200;

// Kinds the control surface must NOT render as a button: read-only telemetry, the
// camera tag, and the e-stop family (a dedicated, always-present STOP button owns
// safety so it can't depend on advertised metadata).
const NON_CONTROL_KINDS = new Set([
  'status', 'camera', 'estop', 'stop', 'reset_estop',
  // Obstacle avoidance is rendered as a dedicated toggle in buildControls, not a
  // generic action button, so exclude it from the generic control surface.
  'obstacle_avoid_on', 'obstacle_avoid_off',
]);

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
// robotId -> card element currently in the DOM (LIST view only). Lets renderList()
// do a keyed diff (append new, remove gone, update-in-place existing) instead of
// rebuilding innerHTML each poll.
let cardEls = new Map();

// Which view is active. null = LIST; a robot id = DETAIL of that robot. Managed
// entirely in-module (no router): the "Szczegóły" button sets it, the "Roboty"
// back button clears it.
let selectedRobotId = null;

// Shared-map ("Mapa scen") view state. When true the screen shows the combined
// real-world map of ALL robots (each robot's accumulated scene + camera-depth +
// live lidar), not the list/detail. Mutually exclusive with `selectedRobotId`.
let mapOpen = false;
// In shared-map mode the render fans every robot's clouds into ONE voxel view
// (union), instead of the single-robot detail render.
let sharedMapMode = false;

// Per-detail live LiDAR state. At most ONE entry exists — for the open detail's
// robot, ONLY while LiDAR is enabled AND the robot is online. Adding/removing it
// owns the single subscription lifecycle: every start has a matching close, so no
// subscription leaks on unmount / back / offline / toggle-off.
// Shape: { unsub:fn|null, closed:bool, resubTimer:number|null, resubUsed:bool,
//          frameTimes:number[], lastPointCount, lastFrameAtMs,
//          lastPoints:Float32Array|null,
//          timing:{atMs,decodeMs,intervalMs,e2eMs,hostMs,netMs}[] }.
let lidarLive = new Map();
// Single shared slow timer that re-renders the open detail's LiDAR status so the
// freshness badge flips to "nieaktualne" when frames stop arriving. NOT a poll.
let lidarTimer = null;

// The wgpu voxel renderer view for the open detail, plus the canvas it owns and a
// ResizeObserver that keeps the renderer sized to its container. Created lazily
// when a LiDAR canvas becomes visible (Przegląd tile or LiDAR 3D tab); disposed
// when leaving the detail or switching away from a LiDAR surface.
let voxelView = null;
let voxelCanvas = null;
let voxelResizeObs = null;
// Guards overlapping async voxel init (import + initVoxelView are async): a later
// teardown that races an in-flight init disposes the late-resolved view at once.
let voxelInitToken = 0;

// Bounded control-outcome log for the open detail's robot. Cleared when the
// detail changes robot or closes. Each entry: { atMs, level, text }.
let detailLog = [];

// =============================================================================
// Gamepad (browser controller) tuning + mapping — all in one place
// =============================================================================
//
// Hardware target is an 8BitDo Arcade Stick: the big lever is DIGITAL (8-way
// on/off) and there are NO analog mini-sticks. Depending on the stick's mode the
// lever surfaces EITHER as axes 0/1 (reported as discrete -1/0/1) OR as the
// standard-gamepad d-pad buttons 12–15. We read BOTH and OR them together.

// Axis deadzone: a digital lever reports ~±1 when pushed and ~0 at rest, but
// loose centering / drift can leave small non-zero values — anything under this
// magnitude counts as neutral.
const PAD_AXIS_DEADZONE = 0.3;

// Continuous move dispatch cadence. The poll runs at the display rAF rate
// (~60 Hz) but we only send a move at most this often so we don't flood the
// owner / mesh control plane. Matches the "~10 Hz" continuous-move budget.
const PAD_MOVE_INTERVAL_MS = 100;

// Continuous max-speed (m/s) bounds. The active max is the CAP the time-ramp
// climbs toward; the owner re-clamps to its own safety cap. Gamepad buttons 4/5
// and the UI +/- nudge it by PAD_SPEED_STEP within [PAD_SPEED_MIN, PAD_SPEED_MAX].
const PAD_SPEED_MIN = 0.1;
const PAD_SPEED_MAX = 1.0;
const PAD_SPEED_STEP = 0.1;
const PAD_SPEED_DEFAULT = 0.3;

// Time-ramp: while a direction is held the speed climbs from this floor up to the
// current max over PAD_RAMP_TIME_MS, so holding longer = faster from a digital
// (on/off) lever. Returning to neutral resets the ramp to the floor.
const PAD_SPEED_FLOOR = 0.12;
const PAD_RAMP_TIME_MS = 1000;

// Standard-gamepad button indices we ACT on. These are defaults — this unit's real
// indices are unknown until we read the live raw readout, so they live here as the
// single place to change. E-STOP has priority over every other mapped button. The
// e-stop index deliberately sits OUTSIDE 0–7 (the user-assigned block): it is a
// best-effort guess for this stick (the on-screen E-STOP always works regardless).
const PAD_BUTTONS = {
  lookDown: 0,    // INDEX 0 — pitch the body DOWN (tap nudges, hold ramps)
  sit: 1,         // INDEX 1 — sit (prefer `sit`, else `stand_down`)
  lookUp: 2,      // INDEX 2 — pitch the body UP (tap nudges, hold ramps)
  standUp: 3,     // INDEX 3 — stand up (`stand_up` / `recovery_stand` / `balance_stand`)
  speedUp: 4,     // INDEX 4 — increase max speed by PAD_SPEED_STEP
  hello: 5,       // INDEX 5 — hello wave
  speedDown: 6,   // INDEX 6 — decrease max speed by PAD_SPEED_STEP
  frontJump: 7,   // INDEX 7 — front jump, dispatched DIRECTLY (no confirm dialog)
  estop: 8,       // INDEX 8 (OUTSIDE 0–7) — emergency stop (priority, best-effort)
  resetEstop: 9,  // INDEX 9 — clear e-stop (exit emergency stop)
};

// Body-pitch (look up/down) tuning, used by buttons 0/2. The accumulator persists
// across toggle off/on like padMaxSpeed; the robot keeps looking where it was left.
//   - TAP (rising edge) nudges by PAD_PITCH_TAP_STEP.
//   - HOLD time-ramps the per-second pitch rate from a floor up to a cap over
//     PAD_PITCH_RAMP_TIME_MS, integrated per rAF frame (dt).
//   - The value is clamped to [PAD_PITCH_MIN, PAD_PITCH_MAX] (radians).
// Look (euler) is INDEPENDENT from drive (move): both can run the same tick.
const PAD_PITCH_MIN = -0.5;
const PAD_PITCH_MAX = 0.5;
const PAD_PITCH_TAP_STEP = 0.06;
const PAD_PITCH_RATE_FLOOR = 0.15;
const PAD_PITCH_RATE_CAP = 0.9;
const PAD_PITCH_RAMP_TIME_MS = 1000;

// Standard-gamepad d-pad button indices (when the lever surfaces as buttons, not
// axes). Up/Down/Left/Right.
const PAD_DPAD = { up: 12, down: 13, left: 14, right: 15 };

// Per-detail gamepad runtime. Exists only while a detail is open AND the pad is
// enabled. Mirrors the lidar loop lifecycle: every start has a matching stop, so
// the rAF loop + listeners never leak past detail close / unmount.
// Shape: { enabled, rafId, rampStartMs, rampActive, lastMoveAtMs, lastMoveZero,
//          prevButtons:Set, connected, padId, lastAxes:number[], lastButtons:number[],
//          lastTickMs, pitchHoldStartMs, pitchDownPrev, pitchUpPrev, lastEulerAtMs,
//          lastSentPitch }
let padState = null;

// Whether the pad is ON. Default true (DEFAULT ENABLED): the loop auto-starts when
// a detail opens; the toggle can turn it OFF and that off state persists while the
// detail stays open. Kept OUTSIDE padState so it survives the loop being destroyed.
let padEnabled = true;

// Current continuous max-speed (m/s) the time-ramp climbs toward. Adjusted by
// gamepad buttons 4/5 and the UI +/- in PAD_SPEED_STEP increments, clamped to
// [PAD_SPEED_MIN, PAD_SPEED_MAX]. Kept at module level so it survives a toggle
// off/on and the loop being destroyed.
let padMaxSpeed = PAD_SPEED_DEFAULT;

// Current body-pitch (radians) the look up/down buttons drive via the `euler`
// command. Held at module level so it survives a toggle off/on and the loop being
// destroyed; on RELEASE it stays put (the robot keeps looking there). Clamped to
// [PAD_PITCH_MIN, PAD_PITCH_MAX].
let padPitch = 0;

const RobotsScreen = {
  get title() { return 'Roboty'; },

  render() {
    // A single host the active view renders into so the poll can swap LIST↔DETAIL
    // without the screen shell flickering.
    return `<div id="robots-root" class="robots-root"></div>`;
  },

  async mount() {
    renderShell();
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
    disposeVoxel();
    stopPadLoop();
    // Pad is ON by default for the next visit; a toggle-off must not leak across a
    // full screen unmount. padMaxSpeed intentionally persists as a user preference.
    padEnabled = true;
    robots = [];
    inFlightRefresh = false;
    cardEls = new Map();
    selectedRobotId = null;
    mapOpen = false;
    sharedMapMode = false;
    detailLog = [];
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
function robotKind(r) { return field(r, 'kind', 'kind') || 'robot'; }
function firmware(r) { return field(r, 'firmware', 'firmware') ?? field(r, 'fw', 'fw'); }

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

// A robot is controllable unless its status reads as offline/lost/error.
function isControllable(status) {
  const s = String(status || '').toLowerCase();
  return s !== 'offline' && s !== 'lost' && s !== 'error';
}

// "Gotowy" (ready) is only true when the robot is actually connected — NOT while
// it is still connecting/pairing (those are controllable-ish but not ready).
function isOnlineStatus(status) {
  const s = String(status || '').toLowerCase();
  return s === 'online' || s === 'ready' || s === 'ok';
}

function findRobot(id) {
  return robots.find((x) => robotId(x) === id) || null;
}

async function loadRobots({ showSpinner }) {
  if (inFlightRefresh) return;
  inFlightRefresh = true;
  if (showSpinner) {
    const host = activeViewHost();
    if (host) host.innerHTML = '<div class="robots-loading"><tf-spinner></tf-spinner></div>';
  }
  try {
    robots = await ApiBinary.list('robotsListRequest', { arrayKey: 'robots' });
    if (!Array.isArray(robots)) robots = [];
    renderActiveView();
  } catch (err) {
    // A background poll failure must not wipe a working view; only surface the
    // error path on an explicit (spinner) load.
    if (showSpinner) {
      robots = [];
      renderError(err);
      toast(`Roboty: ${err.message}`, 'error');
    }
  } finally {
    inFlightRefresh = false;
  }
}

// =============================================================================
// View shell + dispatch
// =============================================================================

// Builds the persistent screen shell (a single root). The active view renders
// into it; switching LIST↔DETAIL only swaps the root's children.
function renderShell() {
  const root = byId('robots-root');
  if (!root) return;
  root.innerHTML = '';
}

function activeViewHost() {
  return byId('robots-root');
}

// Routes the current poll to whichever view is open: DETAIL when a robot is
// selected AND still present, otherwise LIST. A selected robot that disappears
// from the mesh falls back to the list (its subscriptions are torn down).
function renderActiveView() {
  if (mapOpen) {
    renderSharedMap();
  } else if (selectedRobotId && findRobot(selectedRobotId)) {
    renderDetail();
  } else {
    if (selectedRobotId) closeDetail();
    renderList();
  }
}

function renderError(err) {
  const host = activeViewHost();
  if (!host) return;
  // The error empty-state replaces the view; drop stale refs and close every
  // subscription (no card/detail remains on screen to be a live source).
  cardEls = new Map();
  closeDetail();
  selectedRobotId = null;
  host.innerHTML = '';
  const empty = document.createElement('tf-empty-state');
  empty.setAttribute('icon', 'alert');
  empty.setAttribute('title', 'Nie udało się wczytać robotów');
  empty.setAttribute('message', err.message || 'Błąd protokołu Roboty.');
  const retry = document.createElement('tf-button');
  retry.setAttribute('variant', 'primary');
  retry.textContent = 'Spróbuj ponownie';
  retry.addEventListener('click', () => loadRobots({ showSpinner: true }));
  empty.appendChild(retry);
  host.appendChild(empty);
}

// Opens the detail view for a robot: tears down list cards (none can be a live
// source while the detail is open), resets the per-robot log, then renders.
function openDetail(id) {
  if (!id || selectedRobotId === id) return;
  selectedRobotId = id;
  cardEls = new Map();
  detailLog = [];
  renderActiveView();
  // The pad runs at the DETAIL level (across ALL tabs), not just Sterowanie, so the
  // user can watch the camera/lidar while steering. Start it here when enabled; the
  // loop self-gates (only actuates when armed: enabled + detail open + online).
  if (padEnabled) startPadLoop(id);
}

// Returns to the list view, closing the detail's subscriptions + voxel view.
function backToList() {
  closeDetail();
  selectedRobotId = null;
  renderActiveView();
}

// Tears down everything that belongs to the open detail: the single LiDAR
// subscription, the staleness sweep and the wgpu voxel view. Called on back,
// robot-gone, error and unmount.
function closeDetail() {
  stopLidarLoop();
  disposeVoxel();
  stopPadLoop();
  // Pad is ON by default for each freshly opened detail; a per-session toggle-off
  // does not carry across to a different robot. padMaxSpeed intentionally persists.
  padEnabled = true;
}

// =============================================================================
// SHARED MAP view ("Mapa scen") — the combined real-world map of ALL robots.
// Each robot's accumulated scene map + camera-depth cloud (and live lidar as a
// fallback when its scene is still empty) are fanned into ONE wgpu voxel view.
// =============================================================================

function openSharedMap() {
  closeDetail();
  selectedRobotId = null;
  mapOpen = true;
  renderActiveView();
}

// Leave the map: tear down every per-robot subscription + the voxel view.
function backFromMap() {
  sharedMapMode = false;
  stopLidarLoop();
  disposeVoxel();
  mapOpen = false;
  renderActiveView();
}

function renderSharedMap() {
  const root = activeViewHost();
  if (!root) return;
  if (!byId('robots-map')) {
    cardEls = new Map();
    root.innerHTML = `
      <div class="robots-map" id="robots-map">
        <div class="robots-map-toolbar">
          <tf-button variant="ghost" icon="arrow-left" id="robots-map-back">Roboty</tf-button>
          <h1>${sprite('globe-grid')} Wspólna mapa scen</h1>
          <tf-badge tone="success" value="na żywo"></tf-badge>
          <span class="robots-map-spacer"></span>
        </div>
        <div class="robots-map-grid">
          <div class="robots-map-side" data-field="map-side"></div>
          <div class="robots-tile robots-tile-lidar robots-map-scene" data-field="map-scene">
            <div class="robots-voxel" data-field="voxel">
              <div class="robots-voxel-ph" data-voxel-ph>renderer się uruchamia…</div>
            </div>
          </div>
        </div>
      </div>`;
    byId('robots-map-back')?.addEventListener('click', () => backFromMap());
    sharedMapMode = true;
    if (lidarTimer == null) {
      lidarTimer = window.setInterval(sweepLidarStaleness, LIDAR_STALE_SWEEP_MS);
    }
    ensureVoxel(byId('robots-map').querySelector('[data-field="voxel"]'));
  }

  // Each poll: subscribe EVERY robot's clouds (scene + camera-depth + live lidar)
  // into the shared view — idempotent, so robots that appear later are picked up.
  // sharedMapMode makes the chunk handlers union across all robots.
  if (sharedMapMode) {
    const present = new Set();
    for (const r of robots) {
      const id = robotId(r);
      if (!id) continue;
      present.add(id);
      if (!lidarLive.has(id)) startRobotLidar(id);
      const live = lidarLive.get(id);
      // Attempt the camera-depth stream ONCE per robot per map session. The backend
      // rejects `scene-depth:` for non-local robots; without this guard the failure
      // (which clears depthSceneUnsub) would re-fire a failing subscribe every poll.
      if (live && !live.depthSceneUnsub && !live.depthTried) {
        live.depthOn = true;
        live.depthTried = true;
        openDepthSceneSubscription(id, live);
      }
    }
    // Drop subscriptions for robots that vanished from the mesh, so their stale
    // clouds stop contributing to the union + side-panel stats.
    let pruned = false;
    for (const id of [...lidarLive.keys()]) {
      if (!present.has(id)) {
        const live = lidarLive.get(id);
        if (live) closeLidarSubscription(id, live);
        lidarLive.delete(id);
        pruned = true;
      }
    }
    // A vanished robot leaves no future chunk to redraw, so re-union now (this also
    // clears both buffers when the last source is gone).
    if (pruned) renderSharedClouds();
  }
  renderMapSidePanel();
}

// Left info panel: robots list (status pills) + scene stats + layer legend.
function renderMapSidePanel() {
  const side = byId('robots-map')?.querySelector('[data-field="map-side"]');
  if (!side) return;
  const rows = robots
    .map((r) => {
      const id = robotId(r);
      const name = robotKind(r) || id;
      return `<tf-badge tone="${statusTone(r.status || '')}" value="${escapeAttr(name)}"></tf-badge>`;
    })
    .join('');
  // Aggregate occupied-cell counts across robots as a coarse "coverage" proxy.
  // Live render metrics: actual voxel counts the GPU renders (from the renderer),
  // FPS (main-thread rAF), and decode/upload cost — so the bottleneck is visible.
  const mapN = voxelView?.mapPointCount ? voxelView.mapPointCount() : 0;
  const ovN = voxelView?.overlayPointCount ? voxelView.overlayPointCount() : 0;
  const fmt = (n) => Number(n || 0).toLocaleString('pl-PL');
  const fpsTone = perfStats.fps >= 50 ? 'success' : perfStats.fps >= 25 ? 'warning' : 'danger';
  side.innerHTML = `
    <div class="robots-map-card">
      <h3>Sceny i roboty</h3>
      <div class="robots-map-tree">${rows || '<span class="robots-map-muted">Brak robotów</span>'}</div>
    </div>
    <div class="robots-map-card">
      <h3>Render</h3>
      <dl class="robots-map-kv">
        <dt>FPS</dt><dd><tf-badge tone="${fpsTone}" value="${perfStats.fps}"></tf-badge></dd>
        <dt>LiDAR voxele</dt><dd>${fmt(mapN)}</dd>
        <dt>Kamera voxele</dt><dd>${fmt(ovN)}</dd>
        <dt>Razem na GPU</dt><dd>${fmt(mapN + ovN)}</dd>
        <dt>Decode</dt><dd>${perfStats.decodeMs.toFixed(1)} ms</dd>
        <dt>Upload</dt><dd>${perfStats.uploadMs.toFixed(2)} ms</dd>
      </dl>
    </div>
    <div class="robots-map-card">
      <h3>Scena</h3>
      <dl class="robots-map-kv">
        <dt>Roboty</dt><dd>${robots.length}</dd>
        <dt>Georef</dt><dd>lokalna (brak GPS)</dd>
        <dt>Aktualizacja</dt><dd>na żywo</dd>
      </dl>
    </div>
    <div class="robots-map-card">
      <h3>Warstwy</h3>
      <div class="robots-map-legend">
        <span class="robots-map-leg"><i class="dot lidar"></i>LiDAR (głębia)</span>
        <span class="robots-map-leg"><i class="dot depth"></i>Kamera (depth)</span>
      </div>
    </div>`;
}

// Union every robot's clouds into the single voxel view. LiDAR layer prefers the
// accumulated scene, falling back to the live frame for robots whose scene is
// still empty; the camera-depth layer is the magenta overlay.
function renderSharedClouds() {
  if (!voxelView || !voxelView.setMapPoints) return;
  const lidarParts = [];
  const depthParts = [];
  for (const live of lidarLive.values()) {
    if (live.lastScenePoints && live.lastSceneCount) {
      lidarParts.push([live.lastScenePoints, live.lastSceneCount | 0]);
    } else if (live.lastPoints && live.lastPointCount) {
      lidarParts.push([live.lastPoints, live.lastPointCount | 0]);
    }
    if (live.lastDepthPoints && live.lastDepthCount) {
      depthParts.push([live.lastDepthPoints, live.lastDepthCount | 0]);
    }
  }
  unionInto(voxelView.setMapPoints.bind(voxelView), lidarParts);
  if (voxelView.setOverlayPoints) {
    unionInto(voxelView.setOverlayPoints.bind(voxelView), depthParts);
  }
}

// Sample `[points, count]` parts into ONE capped packed buffer and hand it to `fn`.
// Strides during the copy so the full (possibly millions-of-points) union is NEVER
// materialized — that big intermediate alloc could itself OOM the browser.
function unionInto(fn, parts) {
  let total = 0;
  for (const [, n] of parts) total += n;
  if (total === 0) {
    try { fn(new Float32Array(0), 0); } catch { /* ignore */ }
    return;
  }
  const stride = total > MAX_RENDER_POINTS ? Math.ceil(total / MAX_RENDER_POINTS) : 1;

  // Fast path: a single cloud within the render cap (the common 1-robot case) — hand
  // its Float32Array straight to the renderer, no per-frame copy/downsample at all.
  if (stride === 1 && parts.length === 1) {
    const [pts, n] = parts[0];
    try { fn(pts, n); } catch (e) { console.warn('[robots] shared map union render threw:', e?.message ?? e); }
    return;
  }

  const outN = Math.floor(total / stride);
  const buf = new Float32Array(outN * 3);
  let o = 0; // output point index
  if (stride === 1) {
    // No downsample: bulk-copy each part with a typed-array memcpy instead of an
    // element-by-element triple-assign.
    for (const [pts, n] of parts) {
      buf.set(pts.subarray(0, n * 3), o * 3);
      o += n;
    }
  } else {
    let gi = 0; // global input point index across all parts
    for (const [pts, n] of parts) {
      for (let i = 0; i < n; i += 1, gi += 1) {
        if (gi % stride !== 0 || o >= outN) continue;
        buf[o * 3] = pts[i * 3];
        buf[o * 3 + 1] = pts[i * 3 + 1];
        buf[o * 3 + 2] = pts[i * 3 + 2];
        o += 1;
      }
    }
  }
  try { fn(buf, o); } catch (e) { console.warn('[robots] shared map union render threw:', e?.message ?? e); }
}

// =============================================================================
// LIST view
// =============================================================================

// Keyed reconcile by robot id. Cards are created once and kept across polls; only
// mutable fields update in place. A card is recreated only when its robot appears
// or disappears.
function renderList() {
  let host = byId('robots-list');
  if (!host) {
    // First entry into the list view (or returning from detail): build the shell.
    const root = activeViewHost();
    if (!root) return;
    cardEls = new Map();
    root.innerHTML = `
      <div class="page-header">
        <div>
          <h1>${sprite('cpu')} Roboty</h1>
          <div class="sub" id="robots-sub">Roboty wykryte w sieci mesh — status, podgląd i sterowanie</div>
        </div>
        <div class="actions">
          <tf-button variant="secondary" icon="globe-grid" id="robots-open-map">Wspólna mapa</tf-button>
          <tf-button variant="ghost" icon="refresh" id="robots-refresh">Odśwież</tf-button>
        </div>
      </div>
      <div id="robots-list" class="robots-grid"></div>`;
    byId('robots-refresh')?.addEventListener('click', () => loadRobots({ showSpinner: true }));
    byId('robots-open-map')?.addEventListener('click', () => openSharedMap());
    host = byId('robots-list');
  }

  const sub = byId('robots-sub');
  if (sub) {
    sub.textContent = robots.length
      ? `${robots.length} ${plural(robots.length, 'robot', 'roboty', 'robotów')} w sieci mesh`
      : 'Roboty wykryte w sieci mesh — status, podgląd i sterowanie';
  }

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

  // The previous poll may have left a non-card child (spinner/empty-state) as the
  // sole content; clear it so card append order stays clean.
  if (cardEls.size === 0 && host.firstChild) host.innerHTML = '';

  const present = new Set();
  for (const r of robots) {
    const id = robotId(r);
    if (!id) continue;
    present.add(id);
    let el = cardEls.get(id);
    if (!el) {
      el = buildCard(r);
      cardEls.set(id, el);
      host.appendChild(el);
    }
    updateCard(el, r);
  }

  for (const [id, el] of cardEls) {
    if (!present.has(id)) {
      el.remove();
      cardEls.delete(id);
    }
  }
}

// Creates a list card element from cardMarkup(), then builds the quick controls
// and binds the e-stop / details handlers once. Listeners live on the persistent
// element, so update polls never rebind.
function buildCard(r) {
  const tmp = document.createElement('div');
  tmp.innerHTML = cardMarkup(r);
  const el = tmp.firstElementChild;
  const id = robotId(r);

  // E-stop is rendered as static markup (always present) — bind it once.
  el.querySelectorAll('[data-control="estop"]').forEach((btn) => {
    btn.addEventListener('click', () => handleControl(id, 'estop', btn));
  });
  el.querySelector('[data-open-detail]')?.addEventListener('click', () => openDetail(id));

  buildCardQuickControls(el, r);
  return el;
}

function cardMarkup(r) {
  const id = robotId(r);
  const status = r.status || '';
  const offlineClass = isControllable(status) ? '' : ' off';
  return `
    <article class="robots-card${offlineClass}" data-robot-card="${escapeAttr(id)}">
      <div class="robots-card-head">
        <tf-badge data-field="status" tone="${statusTone(status)}" value="${escapeAttr(statusLabel(status))}"></tf-badge>
        <span class="robots-card-title">${escapeHtml(robotKind(r))}</span>
        <span class="robots-card-spacer"></span>
        <span class="robots-card-id">${escapeHtml(id || '(bez id)')}</span>
      </div>

      <div class="robots-kpis">
        <div class="robots-kpi"><div class="robots-kpi-l">Bateria</div><div class="robots-kpi-v" data-kpi="battery">—</div></div>
        <div class="robots-kpi"><div class="robots-kpi-l">RTT</div><div class="robots-kpi-v" data-kpi="rtt">—</div></div>
        <div class="robots-kpi"><div class="robots-kpi-l">Status</div><div class="robots-kpi-v robots-kpi-status" data-kpi="status">—</div></div>
      </div>

      <div class="robots-card-ctl" data-field="quick"></div>

      <div class="robots-card-foot">
        <span class="robots-caps" data-field="caps"></span>
        <tf-button variant="ghost" size="sm" icon="chevron-right" data-open-detail>Szczegóły</tf-button>
      </div>
    </article>`;
}

// Quick controls on the list card: an always-present E-stop, Start (stand_up if
// advertised, else a generic resume), Hello (if advertised) and a 4-way dpad
// (forward/back/left/right) — ALL routed through the same handleControl()
// capability dispatch as the full surface. Rebuilt only when the advertised
// action signature changes.
function buildCardQuickControls(el, r) {
  const host = el.querySelector('[data-field="quick"]');
  if (!host) return;
  const meta = actionsMeta(r);
  const sig = `quick|${controlsSignature(meta)}`;
  if (host.dataset.quickSig === sig) return;
  host.dataset.quickSig = sig;
  host.innerHTML = '';

  const id = robotId(r);
  const controllable = meta.filter((a) => !NON_CONTROL_KINDS.has(a.kind) && !actionReadOnly(a));
  const byKind = (k) => controllable.find((a) => a.kind === k);

  // E-STOP — always present, never disabled (safety independent of metadata).
  const estop = document.createElement('tf-button');
  estop.setAttribute('variant', 'danger');
  estop.setAttribute('size', 'sm');
  estop.setAttribute('icon', 'stop');
  estop.dataset.control = 'estop';
  estop.textContent = 'E-STOP';
  estop.addEventListener('click', () => handleControl(id, 'estop', estop));
  host.appendChild(estop);

  // Start / stand up — only when advertised (and only parameterless variants on
  // the card; the full parametered surface lives in the detail).
  const stand = byKind('stand_up') || byKind('recovery_stand') || byKind('balance_stand');
  if (stand && actionParams(stand).length === 0) {
    host.appendChild(quickButton(id, stand, 'play', 'Start'));
  }

  // Hello — a friendly wave, when advertised.
  const hello = byKind('hello');
  if (hello && actionParams(hello).length === 0) {
    host.appendChild(quickButton(id, hello, 'sparkle', hello.label || 'Hello'));
  }

  // 4-way dpad — only when "move" is advertised. Spacer-grid layout via CSS.
  const move = byKind('move');
  if (move) host.appendChild(buildDpad(id, move));
}

// One quick action button on the card, routed through handleControl with the
// action's own high-risk gating (a quick button never bypasses confirm).
function quickButton(id, a, icon, label) {
  const high = isHighRisk(a);
  const btn = document.createElement('tf-button');
  btn.setAttribute('variant', high ? 'danger' : 'secondary');
  btn.setAttribute('size', 'sm');
  btn.setAttribute('icon', high ? 'alert' : icon);
  btn.dataset.control = a.kind;
  btn.textContent = label;
  btn.addEventListener('click', () => handleControl(id, a.kind, btn, null, label, high));
  return btn;
}

// Compact 4-way directional pad (forward/back/left/right at MOVE_SPEED) sending
// "move" through handleControl — the SAME dispatch the full surface uses.
function buildDpad(id, move) {
  const wrap = document.createElement('div');
  wrap.className = 'robots-dpad';
  const cells = [
    { sp: true },
    { label: 'Przód', icon: 'arrow', vx: MOVE_SPEED, vy: 0, vyaw: 0 },
    { sp: true },
    { label: 'Lewo', icon: 'arrow', vx: 0, vy: MOVE_SPEED, vyaw: 0 },
    { label: 'Tył', icon: 'arrow', vx: -MOVE_SPEED, vy: 0, vyaw: 0 },
    { label: 'Prawo', icon: 'arrow', vx: 0, vy: -MOVE_SPEED, vyaw: 0 },
  ];
  for (const c of cells) {
    if (c.sp) {
      const sp = document.createElement('span');
      sp.className = 'robots-dpad-sp';
      wrap.appendChild(sp);
      continue;
    }
    const btn = document.createElement('tf-button');
    btn.setAttribute('variant', 'outline');
    btn.setAttribute('size', 'sm');
    btn.setAttribute('icon', c.icon);
    btn.setAttribute('aria-label', c.label);
    btn.dataset.control = 'move';
    btn.addEventListener('click', () =>
      handleControl(id, 'move', btn, { vx: c.vx, vy: c.vy, vyaw: c.vyaw }, move.label || c.label),
    );
    wrap.appendChild(btn);
  }
  return wrap;
}

// Updates only the mutable fields of an existing list card.
function updateCard(el, r) {
  const status = r.status || '';
  const offline = !isControllable(status);
  el.classList.toggle('off', offline);

  const badge = el.querySelector('[data-field="status"]');
  if (badge) {
    badge.setAttribute('tone', statusTone(status));
    badge.setAttribute('value', statusLabel(status));
  }

  setKpi(el, 'battery', batteryPercent(r), (v) => `${Math.round(Number(v))}<small>%</small>`);
  setKpi(el, 'rtt', rttMs(r), (v) => `${Math.round(Number(v))}<small>ms</small>`);
  const statusKpi = el.querySelector('[data-kpi="status"]');
  if (statusKpi) {
    const ready = isOnlineStatus(status);
    statusKpi.textContent = ready ? 'Gotowy' : statusLabel(status);
    statusKpi.dataset.tone = statusTone(status);
  }

  updateCaps(el, r);
  buildCardQuickControls(el, r);

  // Offline robots can't take commands: disable every control EXCEPT the e-stop
  // family. STOP must stay clickable regardless of advertised status.
  el.querySelectorAll('[data-control]').forEach((btn) => {
    const ctrl = btn.dataset.control;
    if (ctrl === 'estop' || ctrl === 'stop' || ctrl === 'reset_estop') {
      btn.removeAttribute('disabled');
      return;
    }
    if (offline) btn.setAttribute('disabled', '');
    else btn.removeAttribute('disabled');
  });
}

// Writes a KPI value (HTML, for the <small> unit) or a placeholder when absent.
function setKpi(el, name, raw, format) {
  const node = el.querySelector(`[data-kpi="${name}"]`);
  if (!node) return;
  node.innerHTML = raw == null ? '—' : format(raw);
}

// Capability chips (camera / lidar / auto-reconnect) for the card footer.
function updateCaps(el, r) {
  const host = el.querySelector('[data-field="caps"]');
  if (!host) return;
  const caps = [];
  if (cameraId(r)) caps.push('kamera');
  if (lidar(r)) caps.push('lidar');
  const offline = !isControllable(r.status || '');
  host.innerHTML = '';
  if (caps.length) {
    const chip = document.createElement('tf-chip');
    chip.setAttribute('variant', 'tag');
    chip.setAttribute('tone', offline ? 'muted' : 'success');
    chip.setAttribute('label', caps.join(' · '));
    host.appendChild(chip);
  }
  if (offline) {
    const re = document.createElement('tf-chip');
    re.setAttribute('variant', 'tag');
    re.setAttribute('tone', 'warning');
    re.setAttribute('label', 'auto-reconnect');
    host.appendChild(re);
  }
}

// =============================================================================
// DETAIL view
// =============================================================================

const DETAIL_TABS = [
  { id: 'overview', label: 'Przegląd', icon: 'dashboard' },
  { id: 'camera', label: 'Kamera', icon: 'eye' },
  { id: 'lidar', label: 'LiDAR 3D', icon: 'globe-grid' },
  { id: 'control', label: 'Sterowanie', icon: 'grid-2x2' },
  { id: 'info', label: 'Informacje', icon: 'list' },
  { id: 'log', label: 'Log', icon: 'code' },
];

// Builds the detail shell once per selected robot, then keeps it updated in place
// across polls. The active tab panel is (re)rendered on tab change; persistent
// live nodes (camera, voxel, robot-view) are created once and never rebuilt by a
// poll.
function renderDetail() {
  const r = findRobot(selectedRobotId);
  if (!r) { backToList(); return; }
  const id = robotId(r);

  let shell = byId('robots-detail');
  if (!shell || shell.dataset.robot !== id) {
    // Building (or rebuilding for a different robot): tear down any prior live
    // surfaces before replacing the DOM.
    closeDetail();
    const root = activeViewHost();
    if (!root) return;
    root.innerHTML = detailMarkup(r);
    shell = byId('robots-detail');
    shell.dataset.robot = id;
    shell.dataset.activeTab = 'overview';

    shell.querySelector('[data-detail-back]')?.addEventListener('click', backToList);

    // Action row (E-STOP + capability-mapped Start / Lie / Hello).
    bindDetailActionRow(shell, id);

    const tabs = shell.querySelector('tf-tabs');
    tabs?.addEventListener('change', (e) => {
      const tabId = e.detail?.value;
      if (!tabId) return;
      shell.dataset.activeTab = tabId;
      renderDetailPanel(shell, tabId);
      // Apply live state to the freshly rendered panel at once (start/stop the
      // LiDAR subscription for the now-visible surface, feed telemetry) instead
      // of waiting up to one poll interval.
      const cur = findRobot(shell.dataset.robot);
      if (cur) updateDetailPanel(shell, cur);
    });
    renderDetailPanel(shell, 'overview');
  }

  updateDetailHeader(shell, r);
  // Refresh whichever panel is open (live telemetry / lidar status / controls).
  updateDetailPanel(shell, r);
}

function detailMarkup(r) {
  const id = robotId(r);
  const status = r.status || '';
  const fw = firmware(r);
  const idLine = fw ? `${id} · fw ${fw}` : id;
  return `
    <div id="robots-detail" class="robots-detail">
      <div class="robots-detail-toolbar">
        <tf-button variant="ghost" size="sm" icon="chevron-left" data-detail-back>Roboty</tf-button>
        <h1 class="robots-detail-name">${escapeHtml(robotKind(r))}</h1>
        <tf-badge data-field="status" tone="${statusTone(status)}" value="${escapeAttr(statusLabel(status))}"></tf-badge>
        <span class="robots-detail-id" data-field="idline">${escapeHtml(idLine)}</span>
        <span class="robots-detail-spacer"></span>
        <span class="robots-detail-conn" data-field="conn">połączenie auto</span>
      </div>

      <div class="robots-detail-kpis">
        <div class="robots-dkpi"><div class="robots-dkpi-l">${sprite('zap')} Bateria</div><div class="robots-dkpi-v" data-dkpi="battery">—</div><div class="robots-dkpi-d" data-dkpi="battery-sub">—</div></div>
        <div class="robots-dkpi"><div class="robots-dkpi-l">${sprite('bolt')} RTT</div><div class="robots-dkpi-v" data-dkpi="rtt">—</div><div class="robots-dkpi-d" data-dkpi="rtt-sub">—</div></div>
        <div class="robots-dkpi"><div class="robots-dkpi-l">${sprite('globe-grid')} LiDAR</div><div class="robots-dkpi-v" data-dkpi="lidar">—</div><div class="robots-dkpi-d" data-dkpi="lidar-sub">—</div></div>
        <div class="robots-dkpi"><div class="robots-dkpi-l">${sprite('cpu')} Tryb</div><div class="robots-dkpi-v robots-dkpi-mode" data-dkpi="mode">—</div><div class="robots-dkpi-d" data-dkpi="mode-sub">—</div></div>
      </div>

      <div class="robots-detail-actions" data-field="actionrow"></div>

      <tf-tabs variant="underline" value="overview">
        ${DETAIL_TABS.map((t) => `<tf-tab id="${t.id}" icon="${t.icon}" label="${escapeAttr(t.label)}"></tf-tab>`).join('')}
      </tf-tabs>

      <div class="robots-detail-panel" data-field="panel"></div>
    </div>`;
}

// E-STOP (always present + clickable), plus capability-mapped Start/Wstań,
// Połóż się (lie) and Hello in the action row. All routed through handleControl.
function bindDetailActionRow(shell, id) {
  const host = shell.querySelector('[data-field="actionrow"]');
  if (!host) return;
  host.innerHTML = '';
  const r = findRobot(id);
  const meta = r ? actionsMeta(r) : [];
  const controllable = meta.filter((a) => !NON_CONTROL_KINDS.has(a.kind) && !actionReadOnly(a));
  const byKind = (k) => controllable.find((a) => a.kind === k);

  const estop = document.createElement('tf-button');
  estop.setAttribute('variant', 'danger');
  estop.setAttribute('icon', 'stop');
  estop.dataset.control = 'estop';
  estop.textContent = 'E-STOP';
  estop.addEventListener('click', () => handleControl(id, 'estop', estop));
  host.appendChild(estop);

  // Clearing the latch has no on-screen path otherwise (the gamepad reset button is
  // the only one), which strands the robot whenever the pad is absent or disarmed.
  // `reset_estop` is a NON_CONTROL_KIND, so look it up in the full meta, not the
  // filtered control surface.
  if (meta.some((a) => a.kind === 'reset_estop')) {
    const reset = document.createElement('tf-button');
    reset.setAttribute('variant', 'outline');
    reset.setAttribute('icon', 'refresh');
    reset.dataset.control = 'reset_estop';
    reset.textContent = 'Reset e-stop';
    reset.addEventListener('click', () => handleControl(id, 'reset_estop', reset, null, 'Reset e-stop'));
    host.appendChild(reset);
  }

  const stand = byKind('stand_up') || byKind('recovery_stand') || byKind('balance_stand');
  if (stand && actionParams(stand).length === 0) {
    const b = document.createElement('tf-button');
    b.setAttribute('variant', 'primary');
    b.setAttribute('icon', 'play');
    b.dataset.control = stand.kind;
    b.textContent = 'Start / Wstań';
    b.addEventListener('click', () => handleControl(id, stand.kind, b, null, 'Start / Wstań', isHighRisk(stand)));
    host.appendChild(b);
  }

  const lie = byKind('stand_down') || byKind('sit') || byKind('lie_down');
  if (lie && actionParams(lie).length === 0) {
    const b = document.createElement('tf-button');
    b.setAttribute('variant', 'secondary');
    b.setAttribute('icon', 'pause');
    b.dataset.control = lie.kind;
    b.textContent = lie.label || 'Połóż się';
    b.addEventListener('click', () => handleControl(id, lie.kind, b, null, lie.label || 'Połóż się', isHighRisk(lie)));
    host.appendChild(b);
  }

  const hello = byKind('hello');
  if (hello && actionParams(hello).length === 0) {
    const b = document.createElement('tf-button');
    b.setAttribute('variant', 'ghost');
    b.setAttribute('icon', 'sparkle');
    b.dataset.control = hello.kind;
    b.textContent = hello.label || 'Hello';
    b.addEventListener('click', () => handleControl(id, hello.kind, b, null, hello.label || 'Hello', isHighRisk(hello)));
    host.appendChild(b);
  }
}

// Header KPIs + status badge + connection indicator, refreshed every poll.
function updateDetailHeader(shell, r) {
  const status = r.status || '';
  const offline = !isControllable(status);

  const badge = shell.querySelector('[data-field="status"]');
  if (badge) {
    badge.setAttribute('tone', statusTone(status));
    badge.setAttribute('value', statusLabel(status));
  }

  const fw = firmware(r);
  const idLine = fw ? `${robotId(r)} · fw ${fw}` : robotId(r);
  const idEl = shell.querySelector('[data-field="idline"]');
  if (idEl) idEl.textContent = idLine;

  const conn = shell.querySelector('[data-field="conn"]');
  if (conn) {
    conn.textContent = offline ? 'auto-reconnect' : 'połączenie auto';
    conn.dataset.state = offline ? 'reconnect' : 'live';
  }

  const battery = batteryPercent(r);
  setDkpi(shell, 'battery', battery == null ? '—' : `${Math.round(Number(battery))} <span class="robots-dkpi-u">%</span>`);
  const t = telemetry(r);
  const bat = t && t.battery ? t.battery : null;
  const volt = bat ? telNum(bat, 'voltage', 'voltage') : null;
  const curr = bat ? telNum(bat, 'current', 'current') : null;
  setDkpi(shell, 'battery-sub',
    volt != null || curr != null
      ? [volt != null ? `${volt.toFixed(1)} V` : null, curr != null ? `${curr.toFixed(1)} A` : null].filter(Boolean).join(' · ')
      : '—');

  const rtt = rttMs(r);
  setDkpi(shell, 'rtt', rtt == null ? '—' : `${Math.round(Number(rtt))} <span class="robots-dkpi-u">ms</span>`);
  setDkpi(shell, 'rtt-sub', offline ? 'offline' : 'stabilne');

  // LiDAR KPI: live kl/s + points + decode when a frame is flowing, else snapshot.
  const live = lidarLive.get(robotId(r));
  if (live && live.lastFrameAtMs) {
    const fps = computeLidarFps(live);
    const timing = computeLidarTiming(live);
    setDkpi(shell, 'lidar', `${fps.toFixed(1)} <span class="robots-dkpi-u">kl/s</span>`);
    setDkpi(shell, 'lidar-sub',
      `${live.lastPointCount} pkt · ${timing.decodeMs.toFixed(1)} ms`);
  } else {
    const l = lidar(r);
    const pts = l ? Number(l.pointCount ?? l.point_count ?? 0) : 0;
    setDkpi(shell, 'lidar', l ? (l.enabled ? '…' : 'wył.') : '—');
    setDkpi(shell, 'lidar-sub', l && pts > 0 ? `${pts} pkt` : (l ? 'oczekiwanie' : 'brak'));
  }

  const modeEl = shell.querySelector('[data-dkpi="mode"]');
  if (modeEl) {
    const ready = isOnlineStatus(status);
    modeEl.textContent = ready ? 'Gotowy' : statusLabel(status);
    modeEl.dataset.tone = statusTone(status);
  }
  setDkpi(shell, 'mode-sub', isOnlineStatus(status) ? 'e-stop zwolniony' : '—');

  // Keep the action row in sync with capability changes (signature compare).
  const arHost = shell.querySelector('[data-field="actionrow"]');
  const sig = `actionrow|${controlsSignature(actionsMeta(r))}`;
  if (arHost && arHost.dataset.sig !== sig) {
    arHost.dataset.sig = sig;
    bindDetailActionRow(shell, robotId(r));
  }
  // Apply offline disable to the action row (e-stop excepted).
  arHost?.querySelectorAll('[data-control]').forEach((btn) => {
    const ctrl = btn.dataset.control;
    if (ctrl === 'estop' || ctrl === 'stop' || ctrl === 'reset_estop') {
      btn.removeAttribute('disabled');
      return;
    }
    if (offline) btn.setAttribute('disabled', '');
    else btn.removeAttribute('disabled');
  });
}

function setDkpi(shell, name, html) {
  const node = shell.querySelector(`[data-dkpi="${name}"]`);
  if (node) node.innerHTML = html;
}

// Renders the active tab panel from scratch. Persistent live nodes (camera,
// voxel canvas, robot-view) are created here once and then fed in place by
// updateDetailPanel(); leaving a LiDAR surface disposes the voxel view.
function renderDetailPanel(shell, tabId) {
  const panel = shell.querySelector('[data-field="panel"]');
  if (!panel) return;
  const r = findRobot(shell.dataset.robot);
  if (!r) return;
  const id = robotId(r);

  // Switching away from any LiDAR surface tears down the voxel renderer so only
  // the visible surface holds a GPU context.
  if (tabId !== 'overview' && tabId !== 'lidar') disposeVoxel();

  // The gamepad loop runs at the DETAIL level (every tab), NOT just Sterowanie, so
  // switching tabs must NOT stop it — it is started in openDetail and stopped only
  // in closeDetail / unmount. The Pad UI/readout live in the Sterowanie tab; the
  // readout only updates when that section is present in the DOM.

  panel.innerHTML = '';

  if (tabId === 'overview') {
    panel.innerHTML = `
      <div class="robots-tiles">
        <div class="robots-tile" data-field="camera-tile"></div>
        <div class="robots-tile robots-tile-lidar" data-field="lidar-tile"></div>
      </div>
      <div class="robots-panels">
        <div class="robots-section" data-field="telemetry"></div>
        <div class="robots-section">
          <div class="robots-section-head"><h3>Szybkie sterowanie</h3></div>
          <div class="robots-controls" data-field="quickmove"></div>
        </div>
      </div>`;
    mountCameraTile(panel.querySelector('[data-field="camera-tile"]'), r, false);
    mountLidarTile(panel.querySelector('[data-field="lidar-tile"]'), r);
    buildQuickMove(panel.querySelector('[data-field="quickmove"]'), id, r);
    updateTelemetry(panel.querySelector('[data-field="telemetry"]'), r);
    return;
  }

  if (tabId === 'camera') {
    panel.innerHTML = `<div class="robots-tile robots-tile-full" data-field="camera-tile"></div>`;
    mountCameraTile(panel.querySelector('[data-field="camera-tile"]'), r, true);
    return;
  }

  if (tabId === 'lidar') {
    panel.innerHTML = `<div class="robots-tile robots-tile-lidar robots-tile-full" data-field="lidar-tile"></div>`;
    mountLidarTile(panel.querySelector('[data-field="lidar-tile"]'), r);
    return;
  }

  if (tabId === 'control') {
    panel.innerHTML = `<div class="robots-section">
        <div class="robots-section-head"><h3>Pełne sterowanie</h3></div>
        <div class="robots-controls" data-field="controls"></div>
      </div>
      <div class="robots-section" data-field="pad-section"></div>`;
    buildControls(panel.querySelector('[data-field="controls"]'), r);
    buildPadSection(panel.querySelector('[data-field="pad-section"]'), r);
    return;
  }

  if (tabId === 'info') {
    panel.innerHTML = `
      <div class="robots-panels">
        <div class="robots-section" data-field="telemetry-full"></div>
        <div class="robots-section">
          <div class="robots-section-head"><h3>Model 3D — stawy na żywo</h3></div>
          <div class="robots-robot3d" data-field="robot3d"></div>
        </div>
        <div class="robots-section" data-field="geo-anchor"></div>
      </div>`;
    updateTelemetryFull(panel.querySelector('[data-field="telemetry-full"]'), r);
    updateRobot3d(panel.querySelector('[data-field="robot3d"]'), r);
    buildGeoAnchor(panel.querySelector('[data-field="geo-anchor"]'), r);
    return;
  }

  if (tabId === 'log') {
    panel.innerHTML = `<div class="robots-section">
        <div class="robots-section-head"><h3>Dziennik zdarzeń</h3></div>
        <div class="robots-log" data-field="log"></div>
      </div>`;
    renderLog(panel.querySelector('[data-field="log"]'));
    return;
  }
}

// Geo-anchor panel: pin the robot's scene origin to a real-world lat/lon/alt +
// heading so the whole map (and every robot sharing the scene) gets TRUE-world
// coordinates. Shows the live WGS84 position once anchored. The server holds the
// anchor (persisted), so this is a thin set/get over the binary protocol.
function buildGeoAnchor(el, r) {
  if (!el) return;
  const id = robotId(r);
  el.innerHTML = `
    <div class="robots-section-head"><h3>Pozycja w świecie (geo-anchor)</h3></div>
    <p class="robots-geo-hint">Przypnij początek mapy do rzeczywistych współrzędnych. Kierunek = azymut osi +X sceny (° od północy, zgodnie z ruchem wskazówek).</p>
    <div class="robots-geo-form">
      <tf-input type="number" label="Szerokość (lat °)" data-geo="lat" placeholder="52.2297"></tf-input>
      <tf-input type="number" label="Długość (lon °)" data-geo="lon" placeholder="21.0122"></tf-input>
      <tf-input type="number" label="Wysokość (m)" data-geo="alt" placeholder="118.5"></tf-input>
      <tf-input type="number" label="Kierunek (°)" data-geo="heading" placeholder="0"></tf-input>
    </div>
    <div class="robots-geo-actions">
      <tf-button variant="primary" size="sm" data-geo-set>Ustaw kotwicę</tf-button>
      <tf-button variant="ghost" size="sm" data-geo-clear>Wyczyść</tf-button>
    </div>
    <div class="robots-geo-readout" data-geo-readout>—</div>`;

  const readout = el.querySelector('[data-geo-readout]');
  const val = (k) => {
    const v = el.querySelector(`tf-input[data-geo="${k}"]`)?.value;
    return v == null || v === '' ? null : Number(v);
  };
  const applyResp = (resp) => {
    if (!resp || !resp.ok) {
      readout.textContent = `Błąd: ${resp?.error || 'nieznany'}`;
      return;
    }
    if (resp.anchored) {
      for (const k of ['lat', 'lon', 'alt', 'heading']) {
        const inp = el.querySelector(`tf-input[data-geo="${k}"]`);
        if (inp && resp[k] != null) inp.value = String(resp[k]);
      }
    }
    if (resp.poseLat != null && resp.poseLon != null) {
      readout.textContent = `Pozycja: ${resp.poseLat.toFixed(6)}, ${resp.poseLon.toFixed(6)}`
        + (resp.poseAlt != null ? ` · ${resp.poseAlt.toFixed(1)} m n.p.m.` : '');
    } else if (resp.anchored) {
      readout.textContent = 'Kotwica ustawiona — czekam na pozycję robota…';
    } else {
      readout.textContent = 'Brak kotwicy — pozycja lokalna (metry sceny).';
    }
  };

  // Seed from the server's current anchor + live position.
  ApiBinary.action('robotGeoAnchorGetRequest', { robotId: id }).then(applyResp).catch(() => {
    readout.textContent = 'Nie udało się odczytać kotwicy.';
  });

  el.querySelector('[data-geo-set]')?.addEventListener('click', async (e) => {
    const btn = e.currentTarget;
    const lat = val('lat'); const lon = val('lon'); const alt = val('alt'); const heading = val('heading');
    if (lat == null || lon == null || alt == null || heading == null) {
      toast('Podaj lat, lon, wysokość i kierunek', 'error');
      return;
    }
    btn.setAttribute('loading', '');
    try {
      const resp = await ApiBinary.action('robotGeoAnchorSetRequest', { robotId: id, lat, lon, alt, heading });
      applyResp(resp);
      toast(resp.ok ? 'Kotwica ustawiona' : `Błąd: ${resp.error || 'nieznany'}`, resp.ok ? 'success' : 'error');
    } catch (err) {
      toast(`Błąd: ${err.message}`, 'error');
    } finally {
      btn.removeAttribute('loading');
    }
  });

  el.querySelector('[data-geo-clear]')?.addEventListener('click', async (e) => {
    const btn = e.currentTarget;
    btn.setAttribute('loading', '');
    try {
      const resp = await ApiBinary.action('robotGeoAnchorSetRequest', {
        robotId: id, lat: null, lon: null, alt: null, heading: null,
      });
      for (const k of ['lat', 'lon', 'alt', 'heading']) {
        const inp = el.querySelector(`tf-input[data-geo="${k}"]`);
        if (inp) inp.value = '';
      }
      applyResp(resp);
      toast('Kotwica wyczyszczona', 'success');
    } catch (err) {
      toast(`Błąd: ${err.message}`, 'error');
    } finally {
      btn.removeAttribute('loading');
    }
  });
}

// Refreshes the open panel in place from the latest poll — never rebuilds the
// persistent live nodes (camera / voxel / robot-view), only their fed data and
// the surrounding telemetry/controls/log.
function updateDetailPanel(shell, r) {
  const panel = shell.querySelector('[data-field="panel"]');
  if (!panel) return;
  const tabId = shell.dataset.activeTab || 'overview';
  const id = robotId(r);

  // The LiDAR subscription lifecycle is owned by whichever LiDAR surface is open
  // (Przegląd tile or LiDAR 3D tab): keep it running only while such a surface is
  // visible AND lidar is enabled AND the robot is online.
  const lidarSurfaceOpen = tabId === 'overview' || tabId === 'lidar';
  syncDetailLidar(r, lidarSurfaceOpen);

  if (tabId === 'overview') {
    reconcileCameraTile(panel.querySelector('[data-field="camera-tile"]'), r, false);
    updateTelemetry(panel.querySelector('[data-field="telemetry"]'), r);
    reconcileLidarTile(panel.querySelector('[data-field="lidar-tile"]'), r);
    refreshOfflineDisable(panel, r);
  } else if (tabId === 'camera') {
    reconcileCameraTile(panel.querySelector('[data-field="camera-tile"]'), r, true);
  } else if (tabId === 'lidar') {
    reconcileLidarTile(panel.querySelector('[data-field="lidar-tile"]'), r);
  } else if (tabId === 'control') {
    buildControls(panel.querySelector('[data-field="controls"]'), r);
    refreshOfflineDisable(panel, r);
    syncPadOnlineState(r);
  } else if (tabId === 'info') {
    updateTelemetryFull(panel.querySelector('[data-field="telemetry-full"]'), r);
    updateRobot3d(panel.querySelector('[data-field="robot3d"]'), r);
  } else if (tabId === 'log') {
    renderLog(panel.querySelector('[data-field="log"]'));
  }
  void id;
}

// Disables controls in the open panel for an offline robot (e-stop excepted).
function refreshOfflineDisable(panel, r) {
  const offline = !isControllable(r.status || '');
  panel.querySelectorAll('[data-control]').forEach((btn) => {
    const ctrl = btn.dataset.control;
    if (ctrl === 'estop' || ctrl === 'stop' || ctrl === 'reset_estop') {
      btn.removeAttribute('disabled');
      return;
    }
    if (offline) btn.setAttribute('disabled', '');
    else btn.removeAttribute('disabled');
  });
}

// =============================================================================
// Camera tile (created once, never torn down by a poll)
// =============================================================================

// Mounts the live camera stream into a tile. The <tf-video-stream> is keyed by
// stream-id so a poll never rebuilds the live MSE element. `full` controls tile
// height (full-size on the Kamera tab).
function mountCameraTile(host, r, full) {
  if (!host) return;
  const cam = cameraId(r);
  const id = robotId(r);
  // Key the tile by camera id + size so reconcileCameraTile() can detect a flip
  // and avoid rebuilding the live <tf-video-stream> when nothing changed.
  host.dataset.cam = String(cam ?? '');
  host.dataset.full = full ? '1' : '';
  if (!cam) {
    host.classList.add('robots-tile-empty');
    host.innerHTML = `<div class="robots-tile-ph">Brak kamery</div>`;
    return;
  }
  host.classList.remove('robots-tile-empty');
  const h = full ? 520 : 280;
  host.innerHTML = `
    <div class="robots-tile-top"><span class="robots-tile-title">Kamera</span><span class="robots-tile-rec">● na żywo</span><span class="robots-speed-badge" data-speed-badge>—</span></div>
    <tf-video-stream stream-id="camera:${escapeAttr(cam)}" label="${escapeAttr(id)}" height-px="${h}"></tf-video-stream>
    <div class="robots-tile-bottom">
      <tf-button variant="outline" size="sm" icon="image" data-share-camera="${escapeAttr(cam)}" data-robot="${escapeAttr(id)}">Dodaj do TentaVision</tf-button>
    </div>`;
  host.querySelector('[data-share-camera]')?.addEventListener('click', (e) => {
    const btn = e.currentTarget;
    handleShareCamera(btn.dataset.robot, btn.dataset.shareCamera, btn);
  });
  updateSpeedOverlays();
}

// Keyed camera reconcile across polls: rebuild the tile (and its live MSE element)
// ONLY when the camera id (or tile size) actually changed — a steady camera keeps
// the same <tf-video-stream> playing, mirroring the list's old keyed reconcile.
function reconcileCameraTile(host, r, full) {
  if (!host) return;
  const wantCam = String(cameraId(r) ?? '');
  const wantFull = full ? '1' : '';
  if (host.dataset.cam === wantCam && host.dataset.full === wantFull) return;
  mountCameraTile(host, r, full);
}

// =============================================================================
// LiDAR tile + wgpu voxel renderer lifecycle
// =============================================================================

// Mounts the LiDAR tile: an enable/disable toggle (routed via lidar_on/off), a
// live status line and the wgpu voxel canvas (lazily initialized). Built once per
// renderDetailPanel; updateLidarTile() refreshes text/state in place.
function mountLidarTile(host, r) {
  if (!host) return;
  const id = robotId(r);
  const l = lidar(r);
  if (!l) {
    // Capability absent: dispose any live voxel view so a stale point cloud can't
    // linger, then show the placeholder.
    disposeVoxel();
    host.dataset.hasLidar = '';
    host.classList.add('robots-tile-empty');
    host.innerHTML = `<div class="robots-tile-ph">Robot nie zgłasza LiDAR-u</div>`;
    return;
  }
  host.dataset.hasLidar = '1';
  host.classList.remove('robots-tile-empty');
  host.innerHTML = `
    <div class="robots-tile-top">
      <span class="robots-tile-title">LiDAR — głębia</span>
      <span class="robots-tile-rec" data-lidar-rec>● —</span>
      <span class="robots-speed-badge" data-speed-badge>—</span>
    </div>
    <div class="robots-voxel" data-field="voxel">
      <div class="robots-voxel-ph" data-voxel-ph>renderer się uruchamia…</div>
    </div>
    <div class="robots-tile-bottom robots-lidar-bar">
      <tf-toggle data-lidar-toggle></tf-toggle>
      <span class="robots-lidar-label">Strumień 3D</span>
      <span class="robots-lidar-status" data-lidar-status>—</span>
      <tf-toggle data-depth-toggle></tf-toggle>
      <span class="robots-lidar-label">Kamera (depth)</span>
    </div>
    <div class="robots-lidar-diag" data-lidar-diag hidden></div>`;

  const toggle = host.querySelector('[data-lidar-toggle]');
  toggle.addEventListener('change', (e) => {
    const on = e?.detail?.checked ?? e?.detail ?? toggle.checked ?? toggle.hasAttribute('checked');
    handleLidarToggle(id, !!on, toggle);
  });

  const depthToggle = host.querySelector('[data-depth-toggle]');
  depthToggle.addEventListener('change', (e) => {
    const on = e?.detail?.checked ?? e?.detail ?? depthToggle.checked ?? depthToggle.hasAttribute('checked');
    handleDepthToggle(id, !!on);
  });

  updateLidarTile(host, r);
  updateSpeedOverlays();
  // Kick off the voxel renderer for the now-visible canvas.
  ensureVoxel(host.querySelector('[data-field="voxel"]'));
}

// Keyed LiDAR reconcile across polls: rebuild the tile (and tear down the voxel
// view) when the capability appears or disappears; otherwise refresh in place.
function reconcileLidarTile(host, r) {
  if (!host) return;
  const want = lidar(r) ? '1' : '';
  if (host.dataset.hasLidar !== want) {
    mountLidarTile(host, r);
    return;
  }
  updateLidarTile(host, r);
}

// Refreshes the LiDAR tile's toggle state, status line and freshness in place.
function updateLidarTile(host, r) {
  if (!host || host.classList.contains('robots-tile-empty')) return;
  const l = lidar(r);
  if (!l) return;
  const offline = !isControllable(r.status || '');
  const enabled = !!l.enabled;
  const available = !!l.available;
  const snapshotPoints = Number(l.pointCount ?? l.point_count ?? 0);
  const resolution = l.resolution;

  const toggle = host.querySelector('[data-lidar-toggle]');
  if (toggle) {
    if (enabled) toggle.setAttribute('checked', '');
    else toggle.removeAttribute('checked');
    if (offline) toggle.setAttribute('disabled', '');
    else toggle.removeAttribute('disabled');
  }

  // The depth overlay piggybacks on the live scene subscription, so it's only
  // usable while the 3D stream is live. Disable it (and clear its checked state)
  // when there's no live entry, and otherwise mirror the actual overlay state so
  // the toggle can never read "on" without a `scene-depth` subscription.
  const depthToggle = host.querySelector('[data-depth-toggle]');
  if (depthToggle) {
    const live = lidarLive.get(robotId(r));
    if (!live) {
      depthToggle.removeAttribute('checked');
      depthToggle.setAttribute('disabled', '');
    } else {
      depthToggle.removeAttribute('disabled');
      if (live.depthOn) depthToggle.setAttribute('checked', '');
      else depthToggle.removeAttribute('checked');
    }
  }

  renderLidarStatus(host, { enabled, available, offline, snapshotPoints, resolution });
}

// Writes the LiDAR status text + freshness rec badge + diagnostics line. Called
// from updateLidarTile() (poll cadence) and from the PUSH handler (per frame).
function renderLidarStatus(host, { enabled, available, offline, snapshotPoints, resolution }) {
  const status = host.querySelector('[data-lidar-status]');
  const rec = host.querySelector('[data-lidar-rec]');
  const diag = host.querySelector('[data-lidar-diag]');
  if (!status) return;

  if (!enabled) {
    status.textContent = 'wyłączony';
    if (rec) { rec.textContent = '● off'; rec.dataset.tone = 'muted'; }
    if (diag) diag.hidden = true;
    return;
  }
  if (offline) {
    status.textContent = 'robot offline';
    if (rec) { rec.textContent = '● offline'; rec.dataset.tone = 'warning'; }
    if (diag) diag.hidden = true;
    return;
  }

  const res = typeof resolution === 'number' && Number.isFinite(resolution)
    ? `  ·  ${resolution.toFixed(2)} m`
    : '';

  const id = selectedRobotId || '';
  const live = lidarLive.get(id);
  const hasLiveFrame = !!(live && live.lastFrameAtMs);

  if (hasLiveFrame) {
    const points = live.lastPointCount;
    const fps = computeLidarFps(live);
    const ageMs = performance.now() - live.lastFrameAtMs;
    const stale = ageMs > LIDAR_STALE_AFTER_MS;
    status.textContent =
      `${points} ${plural(points, 'punkt', 'punkty', 'punktów')}  ·  ${fps.toFixed(1)} kl./s${res}`;
    if (rec) {
      rec.textContent = stale ? '● nieaktualne' : `● ${fps.toFixed(0)} Hz`;
      rec.dataset.tone = stale ? 'warning' : 'live';
    }
    if (diag) {
      const t = computeLidarTiming(live);
      if (t.deliveredHz > 0) {
        const e2e = t.e2eMs == null ? 'n/d' : `${t.e2eMs.toFixed(0)} ms`;
        const hop = (v) => (v == null ? '–' : v.toFixed(0));
        diag.hidden = false;
        diag.textContent =
          `dostarczanie ${t.deliveredHz.toFixed(1)} Hz  ·  dekodowanie ${t.decodeMs.toFixed(2)} ms  ·  opóźnienie ${e2e}`
          + `  ·  host/net/decode ${hop(t.hostMs)}/${hop(t.netMs)}/${t.decodeMs.toFixed(1)} ms`;
      } else {
        diag.hidden = true;
      }
    }
    return;
  }
  if (diag) diag.hidden = true;

  if (available && snapshotPoints > 0) {
    status.textContent =
      `${snapshotPoints} ${plural(snapshotPoints, 'punkt', 'punkty', 'punktów')}${res}`;
  } else {
    status.textContent = 'aktywny, oczekiwanie na dane…';
  }
  if (rec) { rec.textContent = '● łączenie'; rec.dataset.tone = 'info'; }
}

// Lazily initializes the wgpu voxel renderer over the tile's canvas container. The
// glue module may not exist yet (sibling renderer not built) — a failed import
// degrades to a "renderer niedostępny" placeholder instead of throwing. A new
// init invalidates any older in-flight init via voxelInitToken.
// Push the robot's world pose (from telemetry, odom frame) into the voxel view so
// the robot model sits at its real position/orientation and the radial colormap
// radiates from the robot. Absent pose leaves the previous one in place.
function applyRobotPose(id) {
  if (!voxelView) return;
  const r = findRobot(id);
  const t = r ? telemetry(r) : null;
  if (!t) return;
  const p = t.posePosition ?? t.pose_position;
  const q = t.poseOrientation ?? t.pose_orientation;
  if (!Array.isArray(p) || p.length < 3) return;
  const o = Array.isArray(q) && q.length >= 4 ? q : [0, 0, 0, 1];
  try { voxelView.setRobotPose(p[0], p[1], p[2], o[0], o[1], o[2], o[3]); } catch { /* ignore */ }
  // Push live leg joint angles so the viewer robot articulates like the real one.
  const j = t.joints;
  if (Array.isArray(j) && j.length >= 12 && voxelView.setRobotJoints) {
    try { voxelView.setRobotJoints(new Float32Array(j)); } catch { /* ignore */ }
  }
}

// Render perf instrumentation. `fps` is measured by a JS rAF on the SAME main
// thread the wgpu render loop runs on, so a stutter (heavy upload blocking the
// thread) shows up here. decode/upload are timed in the stream handlers.
const perfStats = { fps: 0, decodeMs: 0, uploadMs: 0, sceneN: 0, depthN: 0, liveN: 0 };
let perfRaf = null;
let perfFrames = 0;
let perfT0 = 0;
function startPerfMeter() {
  if (perfRaf != null) return;
  perfFrames = 0;
  perfT0 = performance.now();
  const tick = (t) => {
    perfFrames += 1;
    const dt = t - perfT0;
    if (dt >= 500) {
      perfStats.fps = Math.round((perfFrames * 1000) / dt);
      perfFrames = 0;
      perfT0 = t;
      // Surface live metrics: refresh the shared-map panel, else console for the detail.
      if (sharedMapMode) {
        try { renderMapSidePanel(); } catch { /* ignore */ }
      } else if (voxelView) {
        const m = voxelView.mapPointCount ? voxelView.mapPointCount() : perfStats.sceneN;
        const o = voxelView.overlayPointCount ? voxelView.overlayPointCount() : perfStats.depthN;
        console.debug(
          `[voxel] fps=${perfStats.fps} lidar=${m} kamera=${o} `
          + `decode=${perfStats.decodeMs.toFixed(1)}ms upload=${perfStats.uploadMs.toFixed(2)}ms`,
        );
      }
    }
    perfRaf = window.requestAnimationFrame(tick);
  };
  perfRaf = window.requestAnimationFrame(tick);
}
function stopPerfMeter() {
  if (perfRaf != null) { window.cancelAnimationFrame(perfRaf); perfRaf = null; }
  perfStats.fps = 0;
}

async function ensureVoxel(container) {
  if (!container) return;
  if (voxelView && voxelCanvas && container.contains(voxelCanvas)) return;
  disposeVoxel();

  const token = ++voxelInitToken;
  const ph = container.querySelector('[data-voxel-ph]');

  const canvas = document.createElement('canvas');
  canvas.className = 'robots-voxel-canvas';
  container.appendChild(canvas);

  // Resolution (meters per voxel) from the robot's snapshot, default 0.05 m.
  const r = findRobot(selectedRobotId);
  const l = r ? lidar(r) : null;
  const resolution = typeof l?.resolution === 'number' && Number.isFinite(l.resolution) ? l.resolution : 0.05;

  try {
    const mod = await import('/js/voxel/voxel_glue.js');
    // wasm-bindgen glue exports a default init that must run before use.
    if (typeof mod.default === 'function') await mod.default();
    if (token !== voxelInitToken) { canvas.remove(); return; }
    const view = await mod.initVoxelView(canvas, resolution);
    if (token !== voxelInitToken) {
      try { view.dispose(); } catch { /* ignore */ }
      canvas.remove();
      return;
    }
    voxelView = view;
    voxelCanvas = canvas;
    startPerfMeter();
    if (ph) ph.hidden = true;
    // Keep the renderer sized to its container.
    voxelResizeObs = new ResizeObserver(() => resizeVoxel());
    voxelResizeObs.observe(container);
    resizeVoxel();
    // Seed immediately so the view isn't blank until the next push. In shared-map
    // mode replay EVERY robot's cached clouds (frames may have arrived before this
    // async init finished); in the detail prefer the authoritative server map.
    if (sharedMapMode) {
      renderSharedClouds();
    } else {
      const live = lidarLive.get(selectedRobotId);
      if (live && voxelView) {
        if (live.lastScenePoints && voxelView.setMapPoints) {
          renderMap(selectedRobotId, live);
          renderOverlay(selectedRobotId, live);
        } else if (live.lastPoints) {
          try { voxelView.setPoints(live.lastPoints, live.lastPointCount); } catch { /* ignore */ }
        }
      }
      applyRobotPose(selectedRobotId);
    }
  } catch (err) {
    if (token !== voxelInitToken) { canvas.remove(); return; }
    canvas.remove();
    if (ph) {
      ph.hidden = false;
      ph.textContent = 'renderer niedostępny';
    }
    console.warn('[robots] voxel renderer unavailable:', err?.message ?? err);
  }
}

function resizeVoxel() {
  if (!voxelView || !voxelCanvas) return;
  const parent = voxelCanvas.parentElement;
  if (!parent) return;
  const w = Math.max(1, Math.floor(parent.clientWidth));
  const h = Math.max(1, Math.floor(parent.clientHeight));
  try { voxelView.resize(w, h); } catch { /* ignore */ }
}

// Tears down the voxel renderer + its canvas + resize observer. Invalidates any
// in-flight init via the token bump so a racing init disposes itself.
function disposeVoxel() {
  voxelInitToken += 1;
  if (voxelResizeObs) {
    try { voxelResizeObs.disconnect(); } catch { /* ignore */ }
    voxelResizeObs = null;
  }
  if (voxelView) {
    try { voxelView.dispose(); } catch { /* ignore */ }
    voxelView = null;
  }
  if (voxelCanvas) {
    try { voxelCanvas.remove(); } catch { /* ignore */ }
    voxelCanvas = null;
  }
  stopPerfMeter();
}

// =============================================================================
// Capability-driven control surface (full — Sterowanie tab)
// =============================================================================

// Renders the capability-driven controls grouped Ruch / Pozy / Akcje / Akrobacje.
// Rebuilt only when the advertised action signature actually changed.
function buildControls(host, r) {
  if (!host) return;
  const meta = actionsMeta(r);
  // Fold online state into the signature so a connect/reconnect rebuilds the panel.
  // The addon forces obstacle avoidance OFF on every connect, so rebuilding resets
  // the (non-authoritative) toggle to its unchecked default instead of leaving a
  // stale ON from a previous session.
  const sig = `${controlsSignature(meta)}|online:${isOnlineStatus(r.status || '') ? 1 : 0}`;
  if (host.dataset.controlsSig === sig) return;
  host.dataset.controlsSig = sig;
  host.innerHTML = '';

  const id = robotId(r);
  const controllable = meta.filter((a) => !NON_CONTROL_KINDS.has(a.kind) && !actionReadOnly(a));

  const move = controllable.find((a) => a.kind === 'move');
  if (move) host.appendChild(buildMoveGroup(id, move));

  const parametered = controllable.filter((a) => a.kind !== 'move' && actionParams(a).length > 0);
  if (parametered.length) {
    host.appendChild(buildGroup('Pozy', parametered.map((a) => buildParameteredControl(id, a))));
  }

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
    host.appendChild(buildGroup('Akrobacje — wymagają potwierdzenia', acrobatic.map((a) => buildSimpleButton(id, a))));
  }

  if (meta.some((a) => a.kind === 'obstacle_avoid_on')) {
    host.appendChild(buildObstacleAvoidGroup(id));
  }

  if (!host.children.length) {
    const ph = document.createElement('div');
    ph.className = 'robots-controls-empty';
    ph.textContent = 'Robot nie zgłasza żadnych sterowalnych akcji.';
    host.appendChild(ph);
  }
}

// Dedicated obstacle-avoidance toggle, mirroring the LiDAR enable toggle. Default
// UNCHECKED: the addon turns obstacle avoidance OFF on every connect (manual-driving
// default), so the UI starts off too. Sends obstacle_avoid_on/off via the standard
// robot control path.
function buildObstacleAvoidGroup(id) {
  const wrap = document.createElement('div');
  wrap.className = 'robots-control-group';
  wrap.innerHTML = `
    <div class="robots-control-group-title">Omijanie przeszkód</div>
    <div class="robots-pad-bar">
      <tf-toggle data-obstacle-avoid-toggle></tf-toggle>
      <span class="robots-pad-label">Autonomiczne omijanie przeszkód</span>
    </div>
    <div class="robots-pad-hint">
      Wyłącz, aby robot mógł się obracać; włączone = robot sam unika przeszkód
      (może blokować skręt).
    </div>`;
  const toggle = wrap.querySelector('[data-obstacle-avoid-toggle]');
  toggle.addEventListener('change', (e) => {
    const on = e?.detail?.checked ?? e?.detail ?? toggle.checked ?? toggle.hasAttribute('checked');
    handleObstacleAvoidToggle(id, !!on, toggle);
  });
  return wrap;
}

// Reflect a boolean onto the obstacle-avoid toggle's `checked` attribute (same
// programmatic-set pattern as the LiDAR/pad toggles).
function setObstacleAvoidToggle(toggle, on) {
  if (on) toggle.setAttribute('checked', '');
  else toggle.removeAttribute('checked');
}

// Sends an obstacle-avoidance enable/disable through the standard robot control
// path (obstacle_avoid_on / obstacle_avoid_off → go2.obstacle_avoid_on/off).
// There is no obstacle-avoid status field on the robot (the addon forces it OFF on
// every connect), so the toggle is non-authoritative: on any failed/rejected
// command we revert it to its pre-click state so the displayed state never claims a
// success that did not reach the robot.
async function handleObstacleAvoidToggle(id, on, toggle) {
  if (!id) return;
  const prev = !on;
  toggle.setAttribute('disabled', '');
  try {
    const resp = await ApiBinary.action('robotControlRequest', {
      robotId: id,
      kind: on ? 'obstacle_avoid_on' : 'obstacle_avoid_off',
      vx: 0, vy: 0, vyaw: 0, p1: 0, p2: 0, p3: 0, p4: 0,
    });
    if (resp.ok) {
      setObstacleAvoidToggle(toggle, on);
      pushLog('success', `Omijanie przeszkód ${on ? 'włączone' : 'wyłączone'}`);
      toast(`Omijanie przeszkód ${on ? 'włączone' : 'wyłączone'} ✓`, 'success');
    } else if (resp.rejected) {
      setObstacleAvoidToggle(toggle, prev);
      pushLog('error', `Omijanie przeszkód odrzucone: ${resp.rejected}`);
      toast(`Omijanie przeszkód: odrzucono — ${resp.rejected}`, 'error');
    } else {
      setObstacleAvoidToggle(toggle, prev);
      pushLog('error', `Omijanie przeszkód błąd: ${resp.error || 'nieznany'}`);
      toast(`Omijanie przeszkód: błąd — ${resp.error || 'nieznany'}`, 'error');
    }
  } catch (err) {
    setObstacleAvoidToggle(toggle, prev);
    pushLog('error', `Omijanie przeszkód: ${err.message}`);
    toast(`Omijanie przeszkód: ${err.message}`, 'error');
  } finally {
    toggle.removeAttribute('disabled');
  }
}

// Quick-move row for the Przegląd tab: forward/back + rotate L/R, routed through
// the same handleControl dispatch. Always shown when "move" is advertised.
function buildQuickMove(host, id, r) {
  if (!host) return;
  host.innerHTML = '';
  const move = actionsMeta(r).find((a) => a.kind === 'move' && !actionReadOnly(a));
  if (!move) {
    const ph = document.createElement('div');
    ph.className = 'robots-controls-empty';
    ph.textContent = 'Brak sterowania ruchem.';
    host.appendChild(ph);
    return;
  }
  const dirs = [
    { label: 'Naprzód', vx: MOVE_SPEED, vy: 0, vyaw: 0 },
    { label: 'Tył', vx: -MOVE_SPEED, vy: 0, vyaw: 0 },
    { label: 'Obrót L', vx: 0, vy: 0, vyaw: YAW_SPEED },
    { label: 'Obrót P', vx: 0, vy: 0, vyaw: -YAW_SPEED },
  ];
  const row = document.createElement('div');
  row.className = 'robots-controls-row';
  for (const d of dirs) {
    const btn = document.createElement('tf-button');
    btn.setAttribute('variant', 'secondary');
    btn.setAttribute('size', 'sm');
    btn.dataset.control = 'move';
    btn.textContent = d.label;
    btn.addEventListener('click', () =>
      handleControl(id, 'move', btn, { vx: d.vx, vy: d.vy, vyaw: d.vyaw }, move.label || d.label));
    row.appendChild(btn);
  }
  host.appendChild(row);
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
    { label: 'Obrót L', icon: 'rotate', vx: 0, vy: 0, vyaw: YAW_SPEED },
    { label: 'Obrót P', icon: 'rotate', vx: 0, vy: 0, vyaw: -YAW_SPEED },
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

  const required = REQUIRED_PARAMS[a.kind];
  const advertisedNames = new Set(params.map((p) => p.name));
  const missingRequired = required
    ? required.filter((name) => !advertisedNames.has(name))
    : [];

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

// =============================================================================
// Telemetry panel + 3D robot model
// =============================================================================

// Rebuilds a compact telemetry summary IN PLACE. Only fields actually present
// render; an absent snapshot hides the whole panel.
function updateTelemetry(host, r) {
  if (!host) return;
  const rows = telemetryRows(r);
  if (!rows.length) {
    host.innerHTML = `<div class="robots-section-head"><h3>Telemetria</h3></div><div class="robots-controls-empty">Brak telemetrii.</div>`;
    return;
  }
  host.innerHTML = `
    <div class="robots-section-head"><h3>Telemetria</h3></div>
    <dl class="robots-kv">${rows.join('')}</dl>`;
}

// Full telemetry list for the Informacje tab (same source, all rows).
function updateTelemetryFull(host, r) {
  if (!host) return;
  const rows = telemetryRows(r);
  if (!rows.length) {
    host.innerHTML = `<div class="robots-section-head"><h3>Telemetria</h3></div><div class="robots-controls-empty">Robot nie raportuje telemetrii.</div>`;
    return;
  }
  host.innerHTML = `
    <div class="robots-section-head"><h3>Telemetria</h3></div>
    <dl class="robots-kv">${rows.join('')}</dl>`;
}

// Builds the telemetry key/value rows (label/value <dt>/<dd>) from the snapshot.
// IMU RPY in degrees; battery V/A; temperatures; foot force; velocities; gait.
function telemetryRows(r) {
  const t = telemetry(r);
  if (!t) return [];
  const rows = [];
  const mode = telNum(t, 'mode', 'mode');
  const gait = telNum(t, 'gaitType', 'gait_type');
  if (mode != null) rows.push(kvRow('Tryb', String(Math.round(mode))));
  if (gait != null) rows.push(kvRow('Chód', String(Math.round(gait))));
  const bh = telNum(t, 'bodyHeight', 'body_height');
  if (bh != null) rows.push(kvRow('Wys. ciała', `${bh.toFixed(2)} m`));

  const vx = telNum(t, 'vx', 'vx');
  const vy = telNum(t, 'vy', 'vy');
  const vyaw = telNum(t, 'vyaw', 'vyaw');
  if (vx != null || vy != null || vyaw != null) {
    const parts = [];
    if (vx != null) parts.push(`vx ${vx.toFixed(2)}`);
    if (vy != null) parts.push(`vy ${vy.toFixed(2)}`);
    if (vyaw != null) parts.push(`yaw ${vyaw.toFixed(2)}`);
    rows.push(kvRow('Prędkość', parts.join('  ·  ')));
  }

  const imu = t.imu || null;
  if (imu) {
    const roll = radToDeg(telNum(imu, 'roll', 'roll'));
    const pitch = radToDeg(telNum(imu, 'pitch', 'pitch'));
    const yaw = radToDeg(telNum(imu, 'yaw', 'yaw'));
    if (roll != null || pitch != null || yaw != null) {
      const parts = [];
      if (roll != null) parts.push(`${roll.toFixed(1)}°`);
      if (pitch != null) parts.push(`${pitch.toFixed(1)}°`);
      if (yaw != null) parts.push(`${yaw.toFixed(1)}°`);
      rows.push(kvRow('Orientacja (RPY)', parts.join(' / ')));
    }
    const imuTemp = telNum(imu, 'temperature', 'temperature');
    if (imuTemp != null) rows.push(kvRow('Temp. IMU', `${imuTemp.toFixed(0)} °C`));
  }

  const foot = Array.isArray(t.footForce ?? t.foot_force) ? t.footForce ?? t.foot_force : [];
  if (foot.length) {
    const vals = foot.map((f) => (Number.isFinite(Number(f)) ? Math.round(Number(f)) : '—'));
    rows.push(kvRow('Siły stóp (FL/FR/RL/RR)', vals.join('  ·  ')));
  }

  const bat = t.battery || null;
  if (bat) {
    const soc = telNum(bat, 'soc', 'soc');
    const volt = telNum(bat, 'voltage', 'voltage');
    const curr = telNum(bat, 'current', 'current');
    const temp = telNum(bat, 'temperature', 'temperature');
    if (soc != null) rows.push(kvRow('Bateria SOC', `${Math.round(soc)} %`));
    if (volt != null) rows.push(kvRow('Napięcie', `${volt.toFixed(1)} V`));
    if (curr != null) rows.push(kvRow('Prąd', `${curr.toFixed(1)} A`));
    if (temp != null) rows.push(kvRow('Temp. baterii', `${temp.toFixed(0)} °C`));
  }

  return rows;
}

// One telemetry key/value row (<dt>/<dd>) for the .robots-kv definition list.
function kvRow(label, value) {
  return `<dt>${escapeHtml(label)}</dt><dd>${escapeHtml(value)}</dd>`;
}

// Live 3D robot (interim Three.js) IN PLACE: shows the articulated robot driven by
// the telemetry joint angles + IMU orientation. Hidden until 12 joint angles are
// present and the robot is online. The <tf-robot-view> is created once and fed
// each refresh so the WebGL context is never re-created.
function updateRobot3d(host, r) {
  if (!host) return;
  const t = telemetry(r);
  const joints = t && Array.isArray(t.joints) && t.joints.length >= 12 ? t.joints : null;
  // Mount the model whenever the robot exists so the real Go2 is visible even
  // offline (rest pose); it animates only once joint angles arrive.
  let view = host.querySelector('tf-robot-view');
  if (!view) {
    host.innerHTML = '';
    view = document.createElement('tf-robot-view');
    view.className = 'robots-robot3d-view';
    host.appendChild(view);
    host.dataset.has3d = '1';
  }
  if (joints) {
    const imu = (t && t.imu) || {};
    const rpy = [Number(imu.roll) || 0, Number(imu.pitch) || 0, Number(imu.yaw) || 0];
    view.setPose({ joints, rpy });
  }
}

// =============================================================================
// Live LiDAR data path (subscription + decode + voxel feed)
// =============================================================================

// Measured frame rate over the sliding window.
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

// Owns the single detail LiDAR subscription. Subscribes only when a LiDAR surface
// is open AND lidar is enabled AND the robot is online; otherwise closes it.
function syncDetailLidar(r, surfaceOpen) {
  const id = robotId(r);
  const l = lidar(r);
  const offline = !isControllable(r.status || '');
  const want = !!(surfaceOpen && l && l.enabled && !offline);
  if (want) startRobotLidar(id);
  else stopRobotLidar(id);
}

// Opens the real-time PUSH subscription (idempotent) and ensures the staleness
// sweep is running.
function startRobotLidar(id) {
  if (!id) return;
  if (lidarLive.has(id)) return;
  const live = {
    unsub: null,
    sceneUnsub: null,
    depthSceneUnsub: null,
    depthOn: false,
    closed: false,
    resubTimer: null,
    resubUsed: false,
    frameTimes: [],
    lastPointCount: 0,
    lastFrameAtMs: 0,
    lastPoints: null,
    lastScenePoints: null,
    lastSceneCount: 0,
    lastDepthPoints: null,
    lastDepthCount: 0,
    timing: [],
  };
  lidarLive.set(id, live);
  if (lidarTimer == null) {
    lidarTimer = window.setInterval(sweepLidarStaleness, LIDAR_STALE_SWEEP_MS);
  }
  openLidarSubscription(id, live);
  // The server-side SHARED MAP is the source of truth: subscribe to `scene:<id>`
  // and render its full snapshots via setMapPoints. The live `lidar:` frames above
  // still union on top between snapshots for low-latency feel.
  openSceneSubscription(id, live);
}

// Subscribes to `lidar:<id>` and wires the per-frame / end / error handlers.
// Mirrors tf-video-stream's deferred-unsub guard: a teardown that races an
// in-flight subscribe still closes once it resolves.
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

// Subscribes to `scene:<id>` (the server-side accumulated shared map) and renders
// each full snapshot via the renderer's authoritative replace. Mirrors
// openLidarSubscription's deferred-unsub guard.
function openSceneSubscription(id, live) {
  const pending = { disposed: false };
  live.sceneUnsub = () => {
    pending.disposed = true;
  };
  ApiBinary.subscribe(
    'streamSubscribeRequest',
    { streamId: `scene:${id}` },
    {
      onChunk: (body) => onSceneChunk(id, live, body),
      onEnd: () => { live.sceneUnsub = null; },
      onError: (err) => {
        console.warn('[robots] scene subscribe error:', err?.message ?? err);
      },
    },
  )
    .then((unsub) => {
      if (pending.disposed || !lidarLive.has(id) || lidarLive.get(id) !== live) {
        try { unsub(); } catch { /* ignore */ }
        return;
      }
      live.sceneUnsub = () => {
        try { unsub(); } catch (e) { console.warn('[robots] scene unsub threw:', e); }
      };
    })
    .catch((err) => {
      console.warn('[robots] scene subscribe failed:', err?.message ?? err);
    });
}

// A server scene-map snapshot: decode the canonical bytes and REPLACE the rendered
// map with the authoritative deduplicated set (server is the source of truth).
function onSceneChunk(id, live, body) {
  if (!body || typeof body !== 'object') return;
  if (body.variant !== 'StreamFrame') return;
  const data = body.data;
  if (!(data instanceof Uint8Array) || data.byteLength === 0) return;
  if (lidarLive.get(id) !== live) return;
  const t = performance.now();
  decodeFrameAsync(`scene:${id}`, data, (f) => {
    perfStats.decodeMs = performance.now() - t;
    if (lidarLive.get(id) !== live) return;
    if (!f || !f.hasFrame) return;
    // Stash the authoritative map so a view that mounts AFTER this snapshot (renderer
    // init is async, and the server skips re-sending an unchanged map) can replay it.
    live.lastScenePoints = f.points ?? null;
    live.lastSceneCount = Number(f.pointCount ?? 0);
    renderMap(id, live);
  });
}

// A camera depth-map snapshot (`scene:<id>-depth`): same canonical frame as the
// LiDAR scene, but the cloud reconstructed from the camera. Stashed separately and
// rendered as a distinct-colour overlay (renderOverlay → setOverlayPoints).
function onDepthSceneChunk(id, live, body) {
  if (!body || typeof body !== 'object') return;
  if (body.variant !== 'StreamFrame') return;
  const data = body.data;
  if (!(data instanceof Uint8Array) || data.byteLength === 0) return;
  if (lidarLive.get(id) !== live) return;
  decodeFrameAsync(`depth:${id}`, data, (f) => {
    if (lidarLive.get(id) !== live) return;
    if (!f || !f.hasFrame) return;
    live.lastDepthPoints = f.points ?? null;
    live.lastDepthCount = Number(f.pointCount ?? 0);
    renderOverlay(id, live);
  });
}

// Hard ceiling on points pushed to the wgpu renderer in ONE call. The accumulated
// camera-depth map can grow to millions of voxels; feeding that raw (×union of
// several robots) exhausts the wasm renderer's linear memory and OOMs it mid robot-
// model load. Downsample by stride above this — visually equivalent for a dense map.
const MAX_RENDER_POINTS = 300000;
function capPoints(pts, count) {
  const n = count | 0;
  if (!pts || n <= MAX_RENDER_POINTS) return [pts, n];
  const stride = Math.ceil(n / MAX_RENDER_POINTS);
  const outN = Math.floor(n / stride);
  const out = new Float32Array(outN * 3);
  for (let i = 0, o = 0; o < outN; i += stride, o += 1) {
    out[o * 3] = pts[i * 3];
    out[o * 3 + 1] = pts[i * 3 + 1];
    out[o * 3 + 2] = pts[i * 3 + 2];
  }
  return [out, outN];
}

// Render the LiDAR shared map (authoritative replace, radial colormap).
function renderMap(id, live) {
  if (sharedMapMode) { renderSharedClouds(); return; }
  if (!voxelView || id !== selectedRobotId || !voxelView.setMapPoints) return;
  if (!live.lastScenePoints) return;
  try {
    const [pts, cnt] = capPoints(live.lastScenePoints, live.lastSceneCount);
    const t = performance.now();
    voxelView.setMapPoints(pts, cnt);
    perfStats.uploadMs = performance.now() - t;
    perfStats.sceneN = cnt;
    applyRobotPose(id);
  } catch (e) {
    console.warn('[robots] voxel setMapPoints threw:', e?.message ?? e);
  }
}

// Render the camera-depth overlay as a SECOND cloud in a distinct colour (the
// renderer's setOverlayPoints), or clear it when the overlay is off. Kept separate
// from the LiDAR map so the two can be compared/calibrated against each other.
function renderOverlay(id, live) {
  if (sharedMapMode) { renderSharedClouds(); return; }
  if (!voxelView || id !== selectedRobotId || !voxelView.setOverlayPoints) return;
  try {
    if (live.depthOn && live.lastDepthPoints) {
      const [pts, cnt] = capPoints(live.lastDepthPoints, live.lastDepthCount);
      const t = performance.now();
      voxelView.setOverlayPoints(pts, cnt);
      perfStats.uploadMs = performance.now() - t;
      perfStats.depthN = cnt;
    } else {
      voxelView.setOverlayPoints(new Float32Array(0), 0);
      perfStats.depthN = 0;
    }
  } catch (e) {
    console.warn('[robots] voxel setOverlayPoints threw:', e?.message ?? e);
  }
}

// Toggle the camera-depth calibration overlay: subscribe/unsubscribe `scene:<id>-depth`
// and re-render. Off clears the stashed depth cloud so it stops being unioned.
function handleDepthToggle(id, on) {
  const live = lidarLive.get(id);
  if (!live) {
    // No live scene subscription to overlay onto — reset the control so it can't
    // sit visually "on" with nothing behind it.
    const t = byId('robots-detail')?.querySelector('[data-depth-toggle]');
    if (t) t.removeAttribute('checked');
    return;
  }
  live.depthOn = !!on;
  if (on) {
    if (!live.depthSceneUnsub) openDepthSceneSubscription(id, live);
  } else {
    if (live.depthSceneUnsub) {
      try { live.depthSceneUnsub(); } catch { /* ignore */ }
      live.depthSceneUnsub = null;
    }
    live.lastDepthPoints = null;
    live.lastDepthCount = 0;
  }
  renderOverlay(id, live);
}

// Reset the depth overlay to OFF after a subscribe error / terminal end so the
// toggle reflects reality (not stuck "on") and a later toggle can retry. Clears the
// stashed cloud, re-renders (LiDAR-only / empty), and resyncs the toggle UI.
function resetDepthOverlay(id, live) {
  if (lidarLive.get(id) !== live) return;
  live.depthOn = false;
  live.depthSceneUnsub = null;
  live.lastDepthPoints = null;
  live.lastDepthCount = 0;
  renderOverlay(id, live);
  refreshLidarUi(id);
}

// Subscribes to `scene-depth:<id>` (the camera-reconstructed cloud). Mirrors
// openSceneSubscription's deferred-unsub guard.
function openDepthSceneSubscription(id, live) {
  const pending = { disposed: false };
  live.depthSceneUnsub = () => { pending.disposed = true; };
  ApiBinary.subscribe(
    'streamSubscribeRequest',
    { streamId: `scene-depth:${id}` },
    {
      onChunk: (body) => onDepthSceneChunk(id, live, body),
      onEnd: () => { if (!pending.disposed) resetDepthOverlay(id, live); },
      onError: (err) => {
        console.warn('[robots] depth-scene subscribe error:', err?.message ?? err);
        if (!pending.disposed) resetDepthOverlay(id, live);
      },
    },
  )
    .then((unsub) => {
      if (pending.disposed || !lidarLive.has(id) || lidarLive.get(id) !== live) {
        try { unsub(); } catch { /* ignore */ }
        return;
      }
      live.depthSceneUnsub = () => {
        pending.disposed = true;
        try { unsub(); } catch (e) { console.warn('[robots] depth-scene unsub threw:', e); }
      };
    })
    .catch((err) => {
      console.warn('[robots] depth-scene subscribe failed:', err?.message ?? err);
      // Only the CURRENT attempt may reset shared state — a stale promise that
      // rejects after an off/on toggle must not clobber the newer subscription.
      if (!pending.disposed) resetDepthOverlay(id, live);
    });
}

// Off-main-thread frame decode (see lidar-decode-worker.js). One worker owns its own
// protocol-wasm instance; `decodeFrameAsync` posts a frame's raw bytes (transferable)
// and runs `cb` on the main thread with the decoded cloud for GPU upload. Latest-wins:
// a newer frame for the same streamKey supersedes an in-flight one (stale result
// dropped), so a burst never queues redundant uploads and the main thread never blocks
// on the ~8 ms i16→world reconstruction.
let lidarWorker = null;
let lidarWorkerSeq = 0;
const lidarDecodePending = new Map(); // id → { streamKey, cb }
const lidarDecodeBusy = new Set(); // streamKeys with an in-flight worker decode
const lidarDecodeQueued = new Map(); // streamKey → { buf, cb } latest NOT-YET-posted frame

function ensureLidarWorker() {
  if (lidarWorker) return lidarWorker;
  lidarWorker = new Worker(new URL('./lidar-decode-worker.js', import.meta.url), { type: 'module' });
  lidarWorker.onmessage = (e) => {
    const d = e.data || {};
    if (d.fatal) { console.error('[robots] lidar decode worker fatal:', d.fatal); return; }
    const pend = lidarDecodePending.get(d.id);
    if (!pend) return;
    lidarDecodePending.delete(d.id);
    lidarDecodeBusy.delete(pend.streamKey);
    // Render the just-decoded frame — it is the freshest DECODED data available.
    // (Intermediate frames that arrived while the worker was busy were already
    // dropped BEFORE decode in `decodeFrameAsync`, so this never renders stale work
    // and a continuous producer can't starve the render: every decode is shown.)
    pend.cb(d);
    // If a newer frame is queued, start decoding it now (latest-wins next).
    const q = lidarDecodeQueued.get(pend.streamKey);
    if (q) {
      lidarDecodeQueued.delete(pend.streamKey);
      postDecode(pend.streamKey, q.buf, q.cb);
    }
  };
  lidarWorker.onerror = (e) => console.error('[robots] lidar decode worker error:', e?.message ?? e);
  return lidarWorker;
}

function postDecode(streamKey, buf, cb) {
  const w = ensureLidarWorker();
  const reqId = (lidarWorkerSeq += 1);
  lidarDecodePending.set(reqId, { streamKey, cb });
  lidarDecodeBusy.add(streamKey);
  w.postMessage({ id: reqId, streamKey, bytes: buf }, [buf]);
}

// Decode `data` (a Uint8Array slice of a StreamFrame body) off-thread; `cb(frame)`
// runs on the main thread with { hasFrame, points, pointCount, frameSeq, ... }. While
// the worker is busy on this stream the newest frame is COALESCED (older one dropped
// BEFORE decode), so a fast producer never backs up the worker with stale frames.
function decodeFrameAsync(streamKey, data, cb) {
  ensureLidarWorker();
  // Copy the frame bytes into a fresh transferable buffer NOW (the source may be a
  // view into a larger WS buffer that gets reused/detached before we post). One small
  // copy buys a full off-thread decode of the whole cloud.
  const buf = data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength);
  if (lidarDecodeBusy.has(streamKey)) {
    lidarDecodeQueued.set(streamKey, { buf, cb }); // keep only the newest; drop older
    return;
  }
  postDecode(streamKey, buf, cb);
}

// A pushed frame: decode the RAW canonical bytes OFF-THREAD, then (on the main
// thread) update live counters + timing, stash points, feed the wgpu voxel view and
// refresh the open LiDAR status in place.
function onLidarChunk(id, live, body) {
  if (!body || typeof body !== 'object') return;
  if (body.variant !== 'StreamFrame') return;
  const data = body.data;
  if (!(data instanceof Uint8Array) || data.byteLength === 0) return;
  if (lidarLive.get(id) !== live) return;
  const tOnChunk = Date.now() * 1000;
  const arrivalMs = performance.now();
  const intervalMs = live.lastFrameAtMs ? arrivalMs - live.lastFrameAtMs : 0;
  decodeFrameAsync(`lidar:${id}`, data, (f) => onLidarDecoded(id, live, f, tOnChunk, arrivalMs, intervalMs));
}

// Main-thread continuation once the worker returns a decoded live-lidar frame.
function onLidarDecoded(id, live, f, tOnChunk, arrivalMs, intervalMs) {
  if (lidarLive.get(id) !== live) return;
  const decodeMs = (performance.now() - arrivalMs);
  if (!f || !f.hasFrame) return;
  live.lastPointCount = Number(f.pointCount ?? 0);
  live.lastFrameAtMs = arrivalMs;
  live.frameTimes.push(live.lastFrameAtMs);
  const cutoff = live.lastFrameAtMs - LIDAR_FPS_WINDOW_MS;
  while (live.frameTimes.length && live.frameTimes[0] < cutoff) live.frameTimes.shift();

  const stampUs = Number(f.timestampUs ?? f.timestamp_us ?? 0);
  const hostSendUs = Number(f.hostSendUs ?? f.host_send_us ?? 0);
  const tDone = Date.now() * 1000;
  const plausible = (ms) => ms != null && ms >= 0 && ms <= LIDAR_E2E_MAX_MS;
  let hostMs = null;
  let netMs = null;
  let totalMs = null;
  const decodeStageMs = (tDone - tOnChunk) / 1000;
  if (stampUs > 0 && hostSendUs > 0) {
    const h = (hostSendUs - stampUs) / 1000;
    const n = (tOnChunk - hostSendUs) / 1000;
    if (plausible(h)) hostMs = h;
    if (plausible(n)) netMs = n;
  }
  if (stampUs > 0) {
    const t = (tDone - stampUs) / 1000;
    if (plausible(t)) totalMs = t;
  }
  const fmt = (ms) => (ms == null ? 'n/d' : ms.toFixed(1));
  console.debug(
    `[lidar latency] host=${fmt(hostMs)}ms net+unwrap=${fmt(netMs)}ms `
    + `decode=${decodeStageMs.toFixed(1)}ms total=${fmt(totalMs)}ms pts=${live.lastPointCount}`,
  );
  const e2eMs = totalMs;
  live.timing.push({ atMs: live.lastFrameAtMs, decodeMs, intervalMs, e2eMs, hostMs, netMs });
  while (live.timing.length && live.timing[0].atMs < cutoff) live.timing.shift();
  live.lastPoints = f.points ?? null;

  // Feed the wgpu voxel renderer. In shared-map mode union every robot's live
  // lidar (fallback when its accumulated scene is still empty); in the detail push
  // only the selected robot's live frame.
  if (sharedMapMode) {
    renderSharedClouds();
  } else if (voxelView && id === selectedRobotId && live.lastPoints) {
    try {
      voxelView.setPoints(live.lastPoints, live.lastPointCount);
      applyRobotPose(id);
    } catch (e) {
      console.warn('[robots] voxel setPoints threw:', e?.message ?? e);
    }
  }

  refreshLidarUi(id);
}

// Rolling timing diagnostics over the same ~2 s window as the FPS counter.
function computeLidarTiming(live) {
  const samples = live.timing;
  let decodeSum = 0;
  let intervalSum = 0;
  let intervalN = 0;
  let e2eSum = 0;
  let e2eN = 0;
  let hostSum = 0;
  let hostN = 0;
  let netSum = 0;
  let netN = 0;
  for (const s of samples) {
    decodeSum += s.decodeMs;
    if (s.intervalMs > 0) {
      intervalSum += s.intervalMs;
      intervalN += 1;
    }
    if (s.e2eMs != null) {
      e2eSum += s.e2eMs;
      e2eN += 1;
    }
    if (s.hostMs != null) {
      hostSum += s.hostMs;
      hostN += 1;
    }
    if (s.netMs != null) {
      netSum += s.netMs;
      netN += 1;
    }
  }
  const avgIntervalMs = intervalN ? intervalSum / intervalN : 0;
  return {
    decodeMs: samples.length ? decodeSum / samples.length : 0,
    deliveredIntervalMs: avgIntervalMs,
    deliveredHz: avgIntervalMs > 0 ? 1000 / avgIntervalMs : 0,
    e2eMs: e2eN ? e2eSum / e2eN : null,
    hostMs: hostN ? hostSum / hostN : null,
    netMs: netN ? netSum / netN : null,
  };
}

// Stream ended. `subscriber_lagged` → re-subscribe ONCE per live session after a
// short backoff (mirrors tf-video-stream). Any other end leaves it stale.
function onLidarEnd(id, live, body) {
  if (lidarLive.get(id) !== live) return;
  const reason = String(body?.reason ?? '');
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
  live.unsub = null;
}

function onLidarError(id, live, err) {
  if (lidarLive.get(id) !== live) return;
  console.warn('[robots] lidar stream error:', err?.message ?? err);
}

// Closes a robot's PUSH subscription, drops its live state and stops the shared
// staleness sweep once no robot remains active. Every start has a matching close.
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
// subscription handle (which emits the StreamCloseRequest on the original
// correlation id — the only id the server's close handler can cancel by).
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
  if (live.sceneUnsub) {
    try { live.sceneUnsub(); } catch { /* ignore */ }
    live.sceneUnsub = null;
  }
  if (live.depthSceneUnsub) {
    try { live.depthSceneUnsub(); } catch { /* ignore */ }
    live.depthSceneUnsub = null;
  }
}

function stopLidarLoop() {
  if (lidarTimer != null) {
    window.clearInterval(lidarTimer);
    lidarTimer = null;
  }
  for (const [id, live] of lidarLive) closeLidarSubscription(id, live);
  lidarLive.clear();
}

// One tick of the shared staleness sweep: re-render the open LiDAR status so the
// freshness badge flips to "nieaktualne" when frames stop. Carries no traffic.
function sweepLidarStaleness() {
  for (const [id, live] of lidarLive) {
    if (live.lastFrameAtMs) refreshLidarUi(id);
  }
}

// Re-renders the open detail's LiDAR status + KPI from current live state, without
// a full poll — driven by the PUSH handler and the staleness sweep.
function refreshLidarUi(id) {
  if (id !== selectedRobotId) return;
  const shell = byId('robots-detail');
  if (!shell) return;
  const r = findRobot(id);
  if (r) updateDetailHeader(shell, r);
  const tile = shell.querySelector('[data-field="lidar-tile"]');
  if (tile && r) updateLidarTile(tile, r);
}

// Sends a LiDAR enable/disable through the standard robot control path
// (lidar_on / lidar_off → go2.lidar_on/off, routed locally or over the mesh).
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
      const r = findRobot(id);
      const l = r ? lidar(r) : null;
      if (l) l.enabled = on;
      // Gate the (re)subscription through the CURRENT detail/tab state: an in-flight
      // lidar_on must not resurrect a subscription if the user already left the
      // LiDAR surface (tab switch / back to list) while the request was pending.
      const shell = byId('robots-detail');
      const tabId = shell?.dataset.activeTab || 'overview';
      const surfaceOpen = !!r && id === selectedRobotId && (tabId === 'overview' || tabId === 'lidar');
      if (r) syncDetailLidar(r, surfaceOpen);
      else stopRobotLidar(id);
      refreshLidarUi(id);
      pushLog('success', `LiDAR ${on ? 'włączony' : 'wyłączony'}`);
      toast(`LiDAR ${on ? 'włączony' : 'wyłączony'} ✓`, 'success');
    } else if (resp.rejected) {
      pushLog('error', `LiDAR odrzucony: ${resp.rejected}`);
      toast(`LiDAR: odrzucono — ${resp.rejected}`, 'error');
    } else {
      pushLog('error', `LiDAR błąd: ${resp.error || 'nieznany'}`);
      toast(`LiDAR: błąd — ${resp.error || 'nieznany'}`, 'error');
    }
  } catch (err) {
    pushLog('error', `LiDAR: ${err.message}`);
    toast(`LiDAR: ${err.message}`, 'error');
  } finally {
    toggle.removeAttribute('disabled');
  }
}

// =============================================================================
// Gamepad (browser controller) — Pad / Kontroler section in the Sterowanie tab
// =============================================================================

// Builds the "Pad / Kontroler" UI: an enable toggle (ON by default), a continuous
// max-speed control (value chip + +/- buttons, primary path is gamepad buttons
// 4/5), and a LIVE RAW READOUT (connection status, gamepad id, every axis value,
// pressed button indices) so we can see exactly what the arcade stick reports. The
// rAF poll loop is owned by startPadLoop/stopPadLoop; this only renders the UI and
// binds the toggle/speed controls.
function buildPadSection(host, r) {
  if (!host) return;
  const id = robotId(r);
  host.innerHTML = `
    <div class="robots-section-head"><h3>Pad / Kontroler</h3></div>
    <div class="robots-pad">
      <div class="robots-pad-bar">
        <tf-toggle data-pad-toggle></tf-toggle>
        <span class="robots-pad-label">Sterowanie padem</span>
        <span class="robots-pad-spacer"></span>
        <div class="robots-pad-speed">
          <span class="robots-pad-speed-lbl">Maks. prędkość</span>
          <tf-button variant="outline" size="sm" data-pad-speed-down aria-label="Zmniejsz prędkość">−</tf-button>
          <span class="robots-pad-speed-val" data-pad-speed-val>—</span>
          <tf-button variant="outline" size="sm" icon="plus" data-pad-speed-up aria-label="Zwiększ prędkość"></tf-button>
        </div>
      </div>
      <div class="robots-pad-hint">
        Ruch: góra/dół = przód/tył, lewo/prawo = obrót. Przytrzymanie kierunku
        płynnie rozpędza robota (rampa do maks. prędkości). Mapa przycisków padu:
        <ul class="robots-pad-map">
          <li><b>0</b> — patrz w dół (tap = krok, przytrzymanie = płynnie)</li>
          <li><b>1</b> — usiądź</li>
          <li><b>2</b> — patrz w górę (tap = krok, przytrzymanie = płynnie)</li>
          <li><b>3</b> — wstań</li>
          <li><b>4</b> — szybciej (+${PAD_SPEED_STEP.toFixed(1)} m/s)</li>
          <li><b>5</b> — Hello</li>
          <li><b>6</b> — wolniej (−${PAD_SPEED_STEP.toFixed(1)} m/s)</li>
          <li><b>7</b> — Front Jump (od razu, bez potwierdzenia)</li>
          <li><b>${PAD_BUTTONS.estop}</b> — E-STOP (zgadywany dla tego pada; przycisk
            E-STOP na ekranie działa zawsze, niezależnie od mapowania)</li>
          <li><b>${PAD_BUTTONS.resetEstop}</b> — wyjście z E-STOP (reset)</li>
        </ul>
        Patrzenie (pochylenie) jest niezależne od jazdy — po puszczeniu przycisku
        robot pozostaje w danym pochyleniu; aby wrócić do poziomu, naciśnij przeciwny
        kierunek.
      </div>
      <div class="robots-pad-readout" data-pad-readout>
        <div class="robots-pad-conn" data-pad-conn>
          Brak pada — naciśnij dowolny przycisk, aby wykryć pada.
        </div>
        <dl class="robots-kv robots-pad-kv">
          <dt>Urządzenie</dt><dd data-pad-rd-id>—</dd>
          <dt>Stan</dt><dd data-pad-rd-armed>—</dd>
          <dt>Kierunek</dt><dd data-pad-rd-dir>—</dd>
          <dt>Maks. prędkość</dt><dd data-pad-rd-max>—</dd>
          <dt>Prędkość (rampa)</dt><dd data-pad-rd-speed>—</dd>
          <dt>Pochylenie</dt><dd data-pad-rd-pitch>—</dd>
          <dt>Osie</dt><dd data-pad-rd-axes>—</dd>
          <dt>Wciśnięte przyciski</dt><dd data-pad-rd-buttons>—</dd>
        </dl>
      </div>
    </div>`;

  host.querySelector('[data-pad-speed-down]')?.addEventListener('click', () => adjustMaxSpeed(-PAD_SPEED_STEP));
  host.querySelector('[data-pad-speed-up]')?.addEventListener('click', () => adjustMaxSpeed(PAD_SPEED_STEP));

  const toggle = host.querySelector('[data-pad-toggle]');
  toggle.addEventListener('change', (e) => {
    const on = e?.detail?.checked ?? e?.detail ?? toggle.checked ?? toggle.hasAttribute('checked');
    padEnabled = !!on;
    if (padEnabled) startPadLoop(id);
    else stopPadLoop();
    renderPadReadout();
  });

  // Reflect the persisted enabled state (ON by default; OFF persists while open).
  if (padEnabled) toggle.setAttribute('checked', '');

  syncSpeedUi();
}

// Nudges the continuous max-speed by `delta` m/s, clamped to [MIN, MAX] and snapped
// to the PAD_SPEED_STEP grid so float drift can't accumulate. Refreshes the readout
// and the camera/lidar speed overlays so the displayed value tracks live.
function adjustMaxSpeed(delta) {
  const next = Math.round((padMaxSpeed + delta) / PAD_SPEED_STEP) * PAD_SPEED_STEP;
  padMaxSpeed = Math.min(PAD_SPEED_MAX, Math.max(PAD_SPEED_MIN, Number(next.toFixed(2))));
  syncSpeedUi();
}

// Pushes the current max-speed everywhere it is shown: the Pad readout (if open),
// the Pad +/- value chip, and the camera/lidar speed overlays.
function syncSpeedUi() {
  renderPadReadout();
  updateSpeedOverlays();
  const shell = byId('robots-detail');
  const valEl = shell?.querySelector('[data-pad-speed-val]');
  if (valEl) valEl.textContent = `${padMaxSpeed.toFixed(2)} m/s`;
}

// Writes the current max-speed into every speed badge overlaid on the open detail's
// media tiles (camera + lidar, on the Przegląd tiles and the full-size tabs). The
// badges only exist while a media tile is mounted; missing badges are skipped.
function updateSpeedOverlays() {
  const shell = byId('robots-detail');
  if (!shell) return;
  const text = `${padMaxSpeed.toFixed(2)} m/s`;
  shell.querySelectorAll('[data-speed-badge]').forEach((el) => {
    el.textContent = text;
  });
}

// Starts the rAF poll loop + connect/disconnect listeners (idempotent). The loop
// only ACTS while enabled, a detail is open and the robot is online; otherwise it
// just keeps the raw readout live so the user can identify the device's inputs.
function startPadLoop(id) {
  if (padState && padState.enabled) {
    padState.robotId = id;
    return;
  }
  padState = {
    enabled: true,
    robotId: id,
    rafId: null,
    rampStartMs: 0,
    rampActive: false,
    lastMoveAtMs: 0,
    lastMoveZero: true,
    prevButtons: new Set(),
    connected: false,
    padId: '',
    lastAxes: [],
    lastButtons: [],
    // Per-frame dt source for the pitch ramp integration.
    lastTickMs: 0,
    // When a look up/down button started being HELD (0 = not held).
    pitchHoldStartMs: 0,
    // Own rising-edge prev-state for the look buttons (separate from prevButtons,
    // which handlePadButtons overwrites earlier in the tick).
    pitchDownPrev: false,
    pitchUpPrev: false,
    // Rate-limit + change-detection for the euler dispatch.
    lastEulerAtMs: 0,
    lastSentPitch: padPitch,
  };
  window.addEventListener('gamepadconnected', onGamepadConnected);
  window.addEventListener('gamepaddisconnected', onGamepadDisconnected);
  padState.rafId = window.requestAnimationFrame(padTick);
  renderPadReadout();
}

// Stops the loop, removes listeners and sends a single stop if the robot was still
// being driven. Safe to call when no loop is running.
function stopPadLoop() {
  if (!padState) return;
  const wasDriving = !padState.lastMoveZero && padState.robotId;
  if (padState.rafId != null) {
    window.cancelAnimationFrame(padState.rafId);
    padState.rafId = null;
  }
  window.removeEventListener('gamepadconnected', onGamepadConnected);
  window.removeEventListener('gamepaddisconnected', onGamepadDisconnected);
  const id = padState.robotId;
  padState.enabled = false;
  padState = null;
  if (wasDriving && id) sendPadStop(id);
}

function onGamepadConnected(e) {
  if (!padState) return;
  padState.connected = true;
  padState.padId = e?.gamepad?.id || '';
  renderPadReadout();
}

function onGamepadDisconnected() {
  if (!padState) return;
  // Another pad may remain; padTick() re-derives `connected` from the live list.
  padState.connected = false;
  padState.padId = '';
  // Stop the robot if a disconnect happens mid-drive.
  if (!padState.lastMoveZero && padState.robotId) sendPadStop(padState.robotId);
  padState.lastMoveZero = true;
  padState.rampActive = false;
  renderPadReadout();
}

// Returns the first connected gamepad, or null. navigator.getGamepads() returns a
// sparse array with null holes; the API only populates entries after a button
// press (hence the "naciśnij dowolny przycisk" hint).
function firstGamepad() {
  const pads = navigator.getGamepads ? navigator.getGamepads() : [];
  for (const p of pads) {
    if (p) return p;
  }
  return null;
}

// One rAF tick: reads the live pad, updates the raw readout every frame, and (when
// armed: enabled + detail open + robot online) translates input into a rate-limited
// continuous move plus discrete button actions.
function padTick() {
  if (!padState || !padState.enabled) return;
  const pad = firstGamepad();

  if (pad) {
    padState.connected = true;
    padState.padId = pad.id || '';
    padState.lastAxes = Array.from(pad.axes || []);
    padState.lastButtons = Array.from(pad.buttons || []).map((b) => (b ? b.value : 0));
  } else {
    padState.connected = false;
    padState.lastAxes = [];
    padState.lastButtons = [];
  }

  const r = padState.robotId ? findRobot(padState.robotId) : null;
  const armed = !!(
    pad &&
    selectedRobotId &&
    padState.robotId === selectedRobotId &&
    r &&
    isControllable(r.status || '')
  );

  // Per-frame dt (seconds) for the pitch ramp integration; clamped so a backgrounded
  // tab that resumes after a long gap can't jump the accumulator in one big step.
  const tickNow = performance.now();
  const dt = padState.lastTickMs ? Math.min(0.1, (tickNow - padState.lastTickMs) / 1000) : 0;
  padState.lastTickMs = tickNow;

  if (armed) {
    // E-STOP takes the whole tick: when it fires, motion is already cut inside
    // handlePadButtons and no other action / move may run the same frame.
    const estopped = handlePadButtons(pad, r);
    if (!estopped) {
      // Look (euler) and drive (move) are independent commands — both may run.
      handlePadLook(pad, dt);
      handlePadMovement(pad);
    } else {
      // E-stop consumed the tick: still sync the look prev-state from the live pad
      // so a look button held DURING e-stop isn't seen as a fresh rising edge the
      // moment e-stop clears. Reset the hold ramp too.
      syncLookPrevState(pad);
    }
  } else {
    // Disarmed (offline / detail changed): keep the look prev-state in sync with the
    // live pad so a held look button doesn't fire on re-arm, and reset the hold ramp.
    // The accumulator value itself persists (robot stays where it was commanded).
    syncLookPrevState(pad);
    if (!padState.lastMoveZero) {
      // Was driving → send one stop.
      if (padState.robotId) sendPadStop(padState.robotId);
      padState.lastMoveZero = true;
      padState.rampActive = false;
    }
  }

  renderPadReadout();
  padState.rafId = window.requestAnimationFrame(padTick);
}

// Reads the directional intent from BOTH axes (digital lever as -1/0/1, deadzoned)
// and the standard d-pad buttons, OR'd together. Returns {fwd, turn} each in
// {-1, 0, 1}: fwd +1 = forward, turn +1 = turn left (counter-clockwise).
function readPadDirection(pad) {
  const axes = pad.axes || [];
  const ax = (i) => (typeof axes[i] === 'number' ? axes[i] : 0);
  const dz = PAD_AXIS_DEADZONE;

  // Axis 1 = vertical (up is negative in the standard mapping); axis 0 = horizontal.
  let fwd = 0;
  let turn = 0;
  if (ax(1) <= -dz) fwd = 1;
  else if (ax(1) >= dz) fwd = -1;
  if (ax(0) <= -dz) turn = 1;
  else if (ax(0) >= dz) turn = -1;

  // D-pad buttons override / supplement when the lever surfaces as buttons.
  if (buttonPressed(pad, PAD_DPAD.up)) fwd = 1;
  else if (buttonPressed(pad, PAD_DPAD.down)) fwd = -1;
  if (buttonPressed(pad, PAD_DPAD.left)) turn = 1;
  else if (buttonPressed(pad, PAD_DPAD.right)) turn = -1;

  return { fwd, turn };
}

function buttonPressed(pad, idx) {
  const b = pad.buttons && pad.buttons[idx];
  return !!(b && (b.pressed || b.value > 0.5));
}

// Time-ramp continuous move at PAD_MOVE_INTERVAL_MS. While a direction is held the
// magnitude climbs from PAD_SPEED_FLOOR to the active gear cap over PAD_RAMP_TIME_MS;
// neutral resets the ramp and sends a single stop (no stop spam).
function handlePadMovement(pad) {
  const { fwd, turn } = readPadDirection(pad);
  const now = performance.now();

  if (fwd === 0 && turn === 0) {
    padState.rampActive = false;
    if (!padState.lastMoveZero) {
      sendPadMove(padState.robotId, 0, 0, 0);
      padState.lastMoveZero = true;
      padState.lastMoveAtMs = now;
    }
    return;
  }

  if (!padState.rampActive) {
    padState.rampActive = true;
    padState.rampStartMs = now;
  }

  if (now - padState.lastMoveAtMs < PAD_MOVE_INTERVAL_MS) return;

  // The ramp climbs from the floor to the current max; if the max is below the
  // floor (min clamp), the floor itself is capped so we never exceed the max.
  const cap = padMaxSpeed;
  const floor = Math.min(PAD_SPEED_FLOOR, cap);
  const held = Math.min(1, (now - padState.rampStartMs) / PAD_RAMP_TIME_MS);
  const speed = floor + (cap - floor) * held;

  const vx = fwd * speed;
  // Yaw uses its OWN magnitude (rad/s), not the linear speed (m/s): the linear gear
  // is too small to clear the Go2's minimum effective turn rate, so turning needs a
  // floored, higher rate that still scales a bit with the gear.
  const yawMag = Math.min(Math.max(speed * 2.0, PAD_YAW_MIN), PAD_YAW_MAX);
  // Invert the turn while REVERSING so left/right matches the operator's view
  // (RC-car style). Pure in-place turn (fwd == 0) is unchanged.
  const vyaw = (fwd < 0 ? -turn : turn) * yawMag;
  sendPadMove(padState.robotId, vx, 0, vyaw);
  padState.lastMoveZero = false;
  padState.lastMoveAtMs = now;
}

// Discrete button actions on the rising edge only (press, not hold). E-STOP has
// PRIORITY: when its rising edge is seen it cuts motion, dispatches the safety
// stop and RETURNS TRUE so the caller suppresses movement / other actions this
// tick. The rest route through handleControl so confirm/toast/log still work, and
// are skipped gracefully when the kind isn't advertised. Returns whether e-stop
// consumed the tick.
function handlePadButtons(pad, r) {
  const pressed = new Set();
  for (const [name, idx] of Object.entries(PAD_BUTTONS)) {
    if (buttonPressed(pad, idx)) pressed.add(name);
  }

  const rising = (name) => pressed.has(name) && !padState.prevButtons.has(name);

  const id = robotId(r);
  const meta = actionsMeta(r).filter((a) => !actionReadOnly(a));
  const byKind = (k) => meta.find((a) => a.kind === k);

  // E-STOP — priority, always works (independent of advertised metadata). Cut
  // motion at once and consume the whole tick. We dispatch the safety stop only on
  // the rising edge (no per-frame e-stop spam), but suppress ALL movement / other
  // actions for as long as the e-stop input stays pressed — a held panic button
  // must never let a non-zero move slip through on a later frame.
  if (pressed.has('estop')) {
    if (!padState.lastMoveZero) {
      sendPadMove(id, 0, 0, 0);
      padState.lastMoveZero = true;
      padState.rampActive = false;
    }
    if (rising('estop')) handleControl(id, 'estop', makeVirtualButton());
    padState.prevButtons = pressed;
    return true;
  }

  // Exit e-stop on the rising edge. Always sent (safety reset must work even if the
  // kind isn't in advertised metadata), mirroring the e-stop button.
  if (rising('resetEstop')) handleControl(id, 'reset_estop', makeVirtualButton());

  // Speed buttons step the continuous max-speed (rising-edge so a hold steps once
  // per press). adjustMaxSpeed clamps and refreshes the readout + speed overlays.
  if (rising('speedDown')) adjustMaxSpeed(-PAD_SPEED_STEP);
  if (rising('speedUp')) adjustMaxSpeed(PAD_SPEED_STEP);

  // Discrete posture/gesture actions fire on the rising edge only. These route
  // through handleControl WITHOUT requireConfirm (gamepad buttons never pop a
  // confirm dialog) and are skipped gracefully when the kind isn't advertised.
  if (rising('standUp')) {
    const a = byKind('stand_up') || byKind('recovery_stand') || byKind('balance_stand');
    if (a) handleControl(id, a.kind, makeVirtualButton(), null, a.label, false);
  }

  if (rising('sit')) {
    const a = byKind('sit') || byKind('stand_down');
    if (a) handleControl(id, a.kind, makeVirtualButton(), null, a.label, false);
  }

  if (rising('hello')) {
    const a = byKind('hello');
    if (a) handleControl(id, a.kind, makeVirtualButton(), null, a.label, false);
  }

  // Front jump is dispatched DIRECTLY on the rising edge, no confirm dialog and no
  // advertised-kind check: the user wants it instant on the pad. (`front_jump` is a
  // server-allowlisted kind; the owner stays the real safety gate.)
  if (rising('frontJump')) {
    handleControl(id, 'front_jump', makeVirtualButton(), null, 'Front Jump', false);
  }

  padState.prevButtons = pressed;
  return false;
}

// handleControl() toggles `loading` on the passed button element; gamepad actions
// have no real DOM button, so give it a detached throwaway that safely absorbs
// the attribute mutations.
function makeVirtualButton() {
  return document.createElement('span');
}

// Look up/down: drives the persistent `padPitch` accumulator from buttons 0 (down)
// and 2 (up), then dispatches a rate-limited `euler` command when the pitch changed.
//   - TAP (rising edge) nudges padPitch by ±PAD_PITCH_TAP_STEP.
//   - HOLD time-ramps: the per-second rate climbs from PAD_PITCH_RATE_FLOOR to
//     PAD_PITCH_RATE_CAP over PAD_PITCH_RAMP_TIME_MS, integrated with the frame dt.
//   - RELEASE holds padPitch where it is; press the opposite direction to level out.
// `dt` is the frame delta in SECONDS. Look is independent of drive.
function handlePadLook(pad, dt) {
  const downHeld = buttonPressed(pad, PAD_BUTTONS.lookDown);
  const upHeld = buttonPressed(pad, PAD_BUTTONS.lookUp);
  // Own prev-state for the look buttons. handlePadButtons overwrites padState.prevButtons
  // before this runs, so we can't reuse it for rising-edge detection here.
  const downRising = downHeld && !padState.pitchDownPrev;
  const upRising = upHeld && !padState.pitchUpPrev;
  padState.pitchDownPrev = downHeld;
  padState.pitchUpPrev = upHeld;

  let pitch = padPitch;

  // Tap nudges fire immediately on the rising edge (single discrete step).
  if (downRising) pitch -= PAD_PITCH_TAP_STEP;
  if (upRising) pitch += PAD_PITCH_TAP_STEP;

  // Continuous hold ramp: while exactly one direction is held, integrate a rate that
  // climbs over time. Opposing presses cancel (no net ramp). The tap above already
  // gave the press its initial kick; the ramp only adds the continuous portion. A
  // rising edge (including a direct up↔down switch) restarts the ramp from the floor.
  const oneDirHeld = (downHeld || upHeld) && !(downHeld && upHeld);
  if (oneDirHeld) {
    if (!padState.pitchHoldStartMs || downRising || upRising) {
      padState.pitchHoldStartMs = performance.now();
    }
    const held = Math.min(1, (performance.now() - padState.pitchHoldStartMs) / PAD_PITCH_RAMP_TIME_MS);
    const rate = PAD_PITCH_RATE_FLOOR + (PAD_PITCH_RATE_CAP - PAD_PITCH_RATE_FLOOR) * held;
    const sign = upHeld ? 1 : -1;
    pitch += sign * rate * dt;
  } else {
    padState.pitchHoldStartMs = 0;
  }

  pitch = Math.min(PAD_PITCH_MAX, Math.max(PAD_PITCH_MIN, pitch));

  if (pitch !== padPitch) {
    padPitch = pitch;
    renderPadReadout();
  }

  // Dispatch only when the pitch actually changed since the last SUCCESSFUL send,
  // rate-limited like moves. lastSentPitch advances only after the request resolves
  // ok, so a failed/rejected euler stays "unsent" and is retried on the next eligible
  // tick instead of being silently swallowed by the only-on-change guard.
  const now = performance.now();
  if (
    padPitch !== padState.lastSentPitch &&
    now - padState.lastEulerAtMs >= PAD_MOVE_INTERVAL_MS
  ) {
    padState.lastEulerAtMs = now;
    sendPadEuler(padState.robotId, padPitch);
  }
}

// Keeps the look-button rising-edge prev-state aligned with the live pad and clears
// the hold ramp, for frames where the look handler does NOT run (e-stop held or pad
// disarmed). Without this a button still held when control resumes would be misread
// as a fresh press and fire an unwanted tap/ramp.
function syncLookPrevState(pad) {
  padState.pitchDownPrev = !!pad && buttonPressed(pad, PAD_BUTTONS.lookDown);
  padState.pitchUpPrev = !!pad && buttonPressed(pad, PAD_BUTTONS.lookUp);
  padState.pitchHoldStartMs = 0;
}

// Continuous move sent DIRECTLY (no toast/log/confirm) so the poll loop can stream
// at 10 Hz without spamming. Rate limiting + neutral-stop logic live in the caller.
function sendPadMove(id, vx, vy, vyaw) {
  if (!id) return;
  ApiBinary.action('robotControlRequest', {
    robotId: id,
    kind: 'move',
    vx, vy, vyaw,
    p1: 0, p2: 0, p3: 0, p4: 0,
  }).catch(() => { /* transient control-plane errors are non-fatal for a stream */ });
}

function sendPadStop(id) {
  sendPadMove(id, 0, 0, 0);
}

// Body-pitch sent DIRECTLY (no toast/log/confirm) via the Go2 `euler` command.
// Euler param slots: p1 = roll, p2 = pitch, p3 = yaw — so pitch goes in p2. Sent
// at the move cadence; the caller rate-limits + only calls on an actual change.
// lastSentPitch is advanced ONLY when the request resolves ok, so a failed/rejected
// euler stays "unsent" and is retried on a later tick (the loop may have been torn
// down by then — guard padState before writing).
function sendPadEuler(id, pitch) {
  if (!id) return;
  ApiBinary.action('robotControlRequest', {
    robotId: id,
    kind: 'euler',
    vx: 0, vy: 0, vyaw: 0,
    p1: 0, p2: pitch, p3: 0, p4: 0,
  }).then((resp) => {
    if (resp && resp.ok && padState) padState.lastSentPitch = pitch;
  }).catch(() => { /* transient control-plane errors are non-fatal for a stream */ });
}

// Re-derives whether the pad loop is armed when the poll reports a status change
// (e.g. robot goes offline mid-session): if driving while now offline, stop.
function syncPadOnlineState(r) {
  if (!padState || !padState.enabled) return;
  const offline = !isControllable(r.status || '');
  if (offline && !padState.lastMoveZero && padState.robotId) {
    sendPadStop(padState.robotId);
    padState.lastMoveZero = true;
    padState.rampActive = false;
  }
}

// Writes the LIVE RAW READOUT into the Pad section (if it's in the DOM): connection
// status + hint, the gamepad id, every axis value (numeric) and the indices of
// currently-pressed buttons. Drives the active gear + ramped-speed display too.
function renderPadReadout() {
  const shell = byId('robots-detail');
  if (!shell) return;
  const section = shell.querySelector('[data-pad-readout]');
  if (!section) return;

  const conn = section.querySelector('[data-pad-conn]');
  const idEl = section.querySelector('[data-pad-rd-id]');
  const maxEl = section.querySelector('[data-pad-rd-max]');
  const speedEl = section.querySelector('[data-pad-rd-speed]');
  const pitchEl = section.querySelector('[data-pad-rd-pitch]');
  const axesEl = section.querySelector('[data-pad-rd-axes]');
  const buttonsEl = section.querySelector('[data-pad-rd-buttons]');

  const on = !!(padState && padState.enabled);
  const connected = !!(padState && padState.connected);

  if (conn) {
    if (!on) {
      conn.textContent = 'Sterowanie padem wyłączone.';
      conn.dataset.state = 'off';
    } else if (connected) {
      conn.textContent = '● Pad podłączony';
      conn.dataset.state = 'live';
    } else {
      conn.textContent = 'Brak pada — naciśnij dowolny przycisk, aby wykryć pada.';
      conn.dataset.state = 'wait';
    }
  }

  if (idEl) idEl.textContent = padState && padState.padId ? padState.padId : '—';

  if (maxEl) maxEl.textContent = `${padMaxSpeed.toFixed(2)} m/s`;

  if (speedEl) {
    if (padState && padState.rampActive) {
      const cap = padMaxSpeed;
      const floor = Math.min(PAD_SPEED_FLOOR, cap);
      const held = Math.min(1, (performance.now() - padState.rampStartMs) / PAD_RAMP_TIME_MS);
      const speed = floor + (cap - floor) * held;
      speedEl.textContent = `${speed.toFixed(2)} m/s (${Math.round(held * 100)} %)`;
    } else {
      speedEl.textContent = 'neutralnie';
    }
  }

  if (pitchEl) pitchEl.textContent = `${padPitch.toFixed(2)} rad`;

  if (axesEl) {
    const axes = (padState && padState.lastAxes) || [];
    axesEl.textContent = axes.length
      ? axes.map((v, i) => `[${i}] ${Number(v).toFixed(3)}`).join('   ')
      : '—';
  }

  if (buttonsEl) {
    const btns = (padState && padState.lastButtons) || [];
    const downIdx = btns
      .map((v, i) => (v > 0.5 ? i : -1))
      .filter((i) => i >= 0);
    buttonsEl.textContent = downIdx.length ? downIdx.join(', ') : '—';
  }

  // Surface the EXACT arm gate `padTick` uses, so a silently-disarmed pad (input
  // shows in the readout but the robot never gets a command) explains itself.
  const armedEl = section.querySelector('[data-pad-rd-armed]');
  const dirEl = section.querySelector('[data-pad-rd-dir]');
  const pad = firstGamepad();
  let reason;
  if (!on) reason = 'sterowanie padem wyłączone';
  else if (!pad) reason = 'brak pada';
  else if (!selectedRobotId) reason = 'brak otwartego robota';
  else if (!padState || padState.robotId !== selectedRobotId) reason = 'pad przypięty do innego robota';
  else {
    const r = findRobot(padState.robotId);
    if (!r) reason = 'robot nieznany';
    else if (!isControllable(r.status || '')) reason = `niesterowalny (${r.status || '?'})`;
    else reason = null;
  }
  const armed = reason === null;
  if (armedEl) {
    armedEl.textContent = armed ? '● uzbrojony' : `○ ${reason}`;
    armedEl.dataset.state = armed ? 'live' : 'off';
  }
  if (dirEl) {
    if (pad) {
      const { fwd, turn } = readPadDirection(pad);
      const word = (v, pos, neg) => (v > 0 ? pos : v < 0 ? neg : '0');
      dirEl.textContent = `przód/tył ${word(fwd, '+1 przód', '-1 tył')} · obrót ${word(turn, '+1 lewo', '-1 prawo')}`;
    } else {
      dirEl.textContent = '—';
    }
  }
}

// =============================================================================
// Control dispatch + camera share + log
// =============================================================================

// Owner node ids are endpoint-id hex; show a short prefix.
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
  const name = label || kind;
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
    const rejected = resp.rejected;
    const error = resp.error;
    if (resp.ok) {
      pushLog('success', `${name} ✓`);
      toast(`Robot ${shortNode(id)}: ${name} ✓`, 'success');
    } else if (rejected) {
      pushLog('error', `${name}: odrzucono — ${rejected}`);
      toast(`Robot ${shortNode(id)}: odrzucono — ${rejected}`, 'error');
    } else {
      pushLog('error', `${name}: błąd — ${error || 'nieznany'}`);
      toast(`Robot ${shortNode(id)}: błąd — ${error || 'nieznany'}`, 'error');
    }
  } catch (err) {
    pushLog('error', `${name}: ${err.message}`);
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
      pushLog('success', 'Kamera dodana do TentaVision');
      toast(resp.note || 'Kamera dodana do TentaVision ✓', 'success');
    } else {
      pushLog('error', `Udostępnienie kamery: ${resp.error || 'nieznany'}`);
      toast(`Udostępnienie kamery: błąd — ${resp.error || 'nieznany'}`, 'error');
    }
  } catch (err) {
    pushLog('error', `Udostępnienie kamery: ${err.message}`);
    toast(`Udostępnienie kamery: ${err.message}`, 'error');
  } finally {
    btn.removeAttribute('loading');
  }
}

// Appends a control-outcome entry to the bounded detail log and re-renders the
// Log tab if it is open. The log is per-open-detail (cleared on robot change).
function pushLog(level, text) {
  detailLog.push({ atMs: Date.now(), level, text });
  if (detailLog.length > LOG_MAX_ENTRIES) {
    detailLog.splice(0, detailLog.length - LOG_MAX_ENTRIES);
  }
  const shell = byId('robots-detail');
  if (shell && shell.dataset.activeTab === 'log') {
    renderLog(shell.querySelector('[data-field="log"]'));
  }
}

// Renders the detail Log tab: newest-first list of control outcomes.
function renderLog(host) {
  if (!host) return;
  if (!detailLog.length) {
    host.innerHTML = '<div class="robots-controls-empty">Brak zdarzeń — wyślij polecenie, aby zobaczyć tu jego wynik.</div>';
    return;
  }
  const rows = detailLog
    .slice()
    .reverse()
    .map((e) => {
      const time = new Date(e.atMs).toLocaleTimeString('pl-PL', { hour12: false });
      return `<div class="robots-log-row robots-log-${escapeAttr(e.level)}">
          <span class="robots-log-time">${escapeHtml(time)}</span>
          <span class="robots-log-text">${escapeHtml(e.text)}</span>
        </div>`;
    })
    .join('');
  host.innerHTML = rows;
}

function plural(n, one, few, many) {
  const mod10 = n % 10;
  const mod100 = n % 100;
  if (n === 1) return one;
  if (mod10 >= 2 && mod10 <= 4 && (mod100 < 10 || mod100 >= 20)) return few;
  return many;
}

export default RobotsScreen;
