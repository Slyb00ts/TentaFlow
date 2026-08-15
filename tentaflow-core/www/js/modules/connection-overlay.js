// ===== File: connection-overlay.js — connection overlays (platform + variants) =====
//
// One mechanism, two users.
//
// `init()` mounts the PLATFORM overlay: the browser lost the daemon, so nothing
// on the page can talk to anything. It reacts to the ApiBinary lifecycle and
// blurs the whole app.
//
// `createConnectionOverlay()` hands the same card — head with a pulsing dot,
// ringed icon, backoff ring with a 1 Hz countdown, timestamped log, footer — to
// a module that has to report a DIFFERENT loss while the platform socket is
// healthy. Code Studio uses it for an unreachable owner node (G01): the browser
// reaches the platform, the workspace's node does not answer. There is no
// second overlay implementation: the ring transition, the countdown dedup and
// the animation pause on hide live here once.
//
// `isPlatformDown()` is the arbiter — a module overlay must not cover the
// platform one, because "the daemon is gone" outranks "one node is gone".
//
// The platform card answers three questions in this order: WHOSE connection
// died (this device or the node), WHAT happens to the work that was running,
// and WHEN we try again. A lost connection is a degradation, not a crash — the
// node keeps executing — so the card wears the warning tone, not the danger one.

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-button.js';

const MAX_LOG_ENTRIES = 30;
const AUTO_HIDE_DELAY_MS = 500;

// Ring radius in the 50x50 viewBox below; the circumference drives the
// stroke-dasharray so the fill sweeps exactly once per retry window.
const RING_RADIUS = 20;

/** Crossed-out signal: the default card icon, and the one G01 wears too. */
export const OFFLINE_ICON = `
  <path d="M1 1l22 22"/>
  <path d="M16.72 11.06A10.94 10.94 0 0 1 19 12.55"/>
  <path d="M5 12.55a10.94 10.94 0 0 1 5.17-2.39"/>
  <path d="M10.71 5.05A16 16 0 0 1 22.58 9"/>
  <path d="M1.42 9a15.91 15.91 0 0 1 4.7-2.88"/>
  <path d="M8.53 16.11a6 6 0 0 1 6.95 0"/>
  <line x1="12" y1="20" x2="12.01" y2="20"/>
`;

const TONES = ['danger', 'warn', 'ok'];

function escapeHtml(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) => (
    { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]
  ));
}

function timeStr() {
  return new Date().toTimeString().slice(0, 8);
}

/**
 * Builds one overlay card and returns its controller. Nothing is shown until
 * `show()`; the element is appended to <body> immediately, because a card that
 * only exists while visible cannot fade out.
 */
export function createConnectionOverlay(config = {}) {
  const {
    variantClass = '',
    titleText = '',
    iconSvg = OFFLINE_ICON,
    iconTone = 'danger',
    headingText = '',
    descText = '',
    withRetry = true,
    withLog = true,
    withExtra = false,
    actions = [],
    dim = null,
    onAction = null,
  } = config;

  const el = document.createElement('div');
  el.className = `conn-overlay${variantClass ? ` ${variantClass}` : ''}`;
  el.setAttribute('aria-live', 'assertive');

  const titleId = `conn-overlay-title-${Math.random().toString(36).slice(2, 8)}`;
  el.innerHTML = `
    <div class="conn-overlay-card" role="dialog" aria-labelledby="${titleId}">
      <div class="conn-overlay-head">
        <div class="dot"></div>
        <h3 id="${titleId}">${escapeHtml(titleText)}</h3>
      </div>
      <div class="conn-overlay-body">
        <div class="conn-overlay-icon">
          <svg viewBox="0 0 24 24" aria-hidden="true">${iconSvg}</svg>
        </div>
        <div class="conn-overlay-heading">${escapeHtml(headingText)}</div>
        <div class="conn-overlay-desc">${escapeHtml(descText)}</div>
        <div class="conn-overlay-extra"${withExtra ? '' : ' hidden'}></div>

        <div class="conn-retry"${withRetry ? '' : ' hidden'}>
          <div class="conn-retry-ring">
            <svg viewBox="0 0 50 50" aria-hidden="true">
              <circle class="track" cx="25" cy="25" r="${RING_RADIUS}"/>
              <circle class="fill" cx="25" cy="25" r="${RING_RADIUS}"/>
            </svg>
            <div class="countdown">–</div>
          </div>
          <div class="conn-retry-info">
            <div class="line-1"></div>
            <div class="line-2"></div>
          </div>
        </div>

        <div class="conn-log tf-scroll" role="log"${withLog ? '' : ' hidden'}></div>
      </div>
      <div class="conn-overlay-foot"${actions.length ? '' : ' hidden'}>
        ${actions.map((a) => (a.spacer
    ? '<span class="conn-foot-spacer"></span>'
    : `<tf-button data-action="${escapeHtml(a.id)}"${a.variant ? ` variant="${escapeHtml(a.variant)}"` : ''}${a.icon ? ` icon="${escapeHtml(a.icon)}"` : ''}>${escapeHtml(a.label)}</tf-button>`)).join('')}
      </div>
    </div>
  `;

  document.body.appendChild(el);

  const card = el.querySelector('.conn-overlay-card');
  const dotEl = card.querySelector('.conn-overlay-head .dot');
  const headingEl = card.querySelector('.conn-overlay-heading');
  const descEl = card.querySelector('.conn-overlay-desc');
  const extraEl = card.querySelector('.conn-overlay-extra');
  const iconEl = card.querySelector('.conn-overlay-icon');
  const retryEl = card.querySelector('.conn-retry');
  const ring = card.querySelector('.conn-retry-ring .fill');
  const countdownEl = card.querySelector('.conn-retry-ring .countdown');
  const line1El = card.querySelector('.conn-retry-info .line-1');
  const line2El = card.querySelector('.conn-retry-info .line-2');
  const logEl = card.querySelector('.conn-log');
  const footEl = card.querySelector('.conn-overlay-foot');
  const titleEl = card.querySelector('.conn-overlay-head h3');

  const ringCirc = 2 * Math.PI * RING_RADIUS;
  ring.setAttribute('stroke-dasharray', String(ringCirc));
  ring.style.strokeDashoffset = '0';

  let hideTimer = null;
  let countdownTimer = null;
  let countdownUntil = 0;
  let lastSecondsShown = -1;
  let visible = false;

  function applyTone(tone) {
    const next = TONES.includes(tone) ? tone : 'danger';
    for (const name of TONES) {
      const on = name === next;
      iconEl.classList.toggle(`tone-${name}`, on);
      dotEl.classList.toggle(`tone-${name}`, on);
    }
  }
  applyTone(iconTone);

  function dimTarget() {
    if (!dim || typeof dim.resolve !== 'function') return null;
    return dim.resolve();
  }

  // 1 Hz text write, deduplicated: the ring itself is a CSS transition, so the
  // timer only owns the digits. An overlay that is not visible runs no timer.
  function tickCountdown() {
    if (!visible || countdownUntil <= 0) return;
    const seconds = Math.ceil(Math.max(0, countdownUntil - Date.now()) / 1000);
    if (seconds === lastSecondsShown) return;
    lastSecondsShown = seconds;
    countdownEl.textContent = seconds > 0 ? `${seconds}s` : '…';
  }

  function stopCountdown() {
    if (countdownTimer) {
      clearInterval(countdownTimer);
      countdownTimer = null;
    }
    lastSecondsShown = -1;
  }

  // The ring is filled by one CSS transition per scheduled retry — the GPU
  // interpolates over delayMs, JS writes nothing per frame.
  function armRing(delayMs) {
    if (delayMs <= 0) return;
    ring.style.transition = 'none';
    ring.style.strokeDashoffset = '0';
    ring.getBoundingClientRect();
    ring.style.transition = `stroke-dashoffset ${delayMs}ms linear`;
    ring.style.strokeDashoffset = String(ringCirc);
  }

  function resetRing() {
    ring.style.transition = 'none';
    ring.style.strokeDashoffset = '0';
  }

  const controller = {
    el,
    extraEl,

    isVisible() { return visible; },

    show() {
      if (hideTimer) {
        clearTimeout(hideTimer);
        hideTimer = null;
      }
      visible = true;
      el.classList.add('visible');
      const target = dimTarget();
      if (target && dim.className) target.classList.add(dim.className);
      if (!countdownTimer) {
        tickCountdown();
        countdownTimer = setInterval(tickCountdown, 1000);
      }
    },

    /** `immediate` skips the fade-out window (used when a view is torn down). */
    hide({ immediate = false } = {}) {
      if (hideTimer) clearTimeout(hideTimer);
      visible = false;
      stopCountdown();
      resetRing();
      const drop = () => {
        el.classList.remove('visible');
        const target = dimTarget();
        if (target && dim.className) target.classList.remove(dim.className);
      };
      if (immediate) {
        hideTimer = null;
        drop();
        return;
      }
      hideTimer = setTimeout(drop, AUTO_HIDE_DELAY_MS);
    },

    setTitle(text) { titleEl.textContent = String(text ?? ''); },
    setHeading(text) { headingEl.textContent = String(text ?? ''); },
    setDesc(text) { descEl.textContent = String(text ?? ''); },
    setTone(tone) { applyTone(tone); },
    setIcon(svg) { iconEl.querySelector('svg').innerHTML = svg; },

    setRetryVisible(on) {
      retryEl.hidden = !on;
      if (!on) {
        countdownUntil = 0;
        resetRing();
      }
    },
    setLogVisible(on) { logEl.hidden = !on; },
    setFootVisible(on) { footEl.hidden = !on; },

    setRetryLines(line1, line2) {
      line1El.textContent = String(line1 ?? '');
      line2El.textContent = String(line2 ?? '');
    },

    /** Starts the digits and the ring sweep for one retry window. */
    startCountdown(delayMs) {
      countdownUntil = Date.now() + Math.max(0, delayMs);
      lastSecondsShown = -1;
      tickCountdown();
      if (visible && !countdownTimer) countdownTimer = setInterval(tickCountdown, 1000);
      armRing(delayMs);
    },

    stopCountdown() {
      countdownUntil = 0;
      stopCountdown();
      resetRing();
      countdownEl.textContent = '…';
    },

    log(kind, msg) {
      const entry = document.createElement('div');
      entry.className = `conn-log-entry ${kind}`;
      const ts = document.createElement('span');
      ts.className = 'ts';
      ts.textContent = timeStr();
      const m = document.createElement('span');
      m.className = 'msg';
      m.textContent = String(msg ?? '');
      entry.appendChild(ts);
      entry.appendChild(m);
      logEl.appendChild(entry);
      while (logEl.children.length > MAX_LOG_ENTRIES) logEl.removeChild(logEl.firstChild);
      logEl.scrollTop = logEl.scrollHeight;
    },

    destroy() {
      stopCountdown();
      if (hideTimer) clearTimeout(hideTimer);
      const target = dimTarget();
      if (target && dim?.className) target.classList.remove(dim.className);
      if (el.parentNode) el.parentNode.removeChild(el);
    },
  };

  card.addEventListener('click', (event) => {
    const trigger = event.target.closest('[data-action]');
    if (!trigger || !card.contains(trigger)) return;
    onAction?.(trigger.getAttribute('data-action'), controller);
  });

  return controller;
}

// ---------------------------------------------------------------------------
// Platform overlay — the browser lost the daemon
// ---------------------------------------------------------------------------

let overlay = null;
let mounted = false;
let platformState = 'ok'; // ok | disconnected
let connectivityOff = null;
let learnedNode = '';
let learningNode = false;
// A node running without mesh security has no identity endpoint at all, and
// asking again on every reconnect would write an audited failure each time. Two
// tries cover "the socket opened before the session was signed in".
let nameAttemptsLeft = 2;
// The card is drawn when nothing answers, so the name cannot be fetched then.
// It is learned while the socket is healthy and survives a reload inside the
// same tab; a different origin is a different node, so it keys on the origin.
const NODE_NAME_KEY = `tf.node-name:${window.location.origin}`;

function readStoredNodeName() {
  try { return window.sessionStorage.getItem(NODE_NAME_KEY) || ''; } catch { return ''; }
}

function storeNodeName(name) {
  try { window.sessionStorage.setItem(NODE_NAME_KEY, name); } catch { /* private mode */ }
}

/**
 * Asks the node what it is called. `hostname` is the same field the mesh screens
 * label nodes with, so the overlay names the machine the way the rest of the UI
 * does instead of quoting the address bar back at the user.
 */
async function learnNodeName() {
  if (learnedNode || learningNode || nameAttemptsLeft <= 0) return;
  learningNode = true;
  nameAttemptsLeft -= 1;
  try {
    const body = await ApiBinary.one('meshIdentityRequest');
    const name = String(body?.hostname ?? body?.host_name ?? '').trim();
    if (name) {
      learnedNode = name;
      storeNodeName(name);
      applyCause();
    }
  } catch {
    // The node cannot tell us its name; the card falls back to the address and
    // says so rather than passing the address off as a name.
  } finally {
    learningNode = false;
  }
}

/**
 * Seeds the name from a directory a screen has ALREADY loaded — the mesh node
 * list, the Code Studio workspace registry. Both name the local node exactly the
 * way this card should, and a node that runs without mesh security has no
 * identity endpoint to ask, so this is the only source it will ever get.
 */
export function rememberNodeName(name) {
  const value = String(name ?? '').trim();
  if (!value || value === learnedNode) return;
  learnedNode = value;
  storeNodeName(value);
  applyCause();
}

/** True while the card has a real node name and not just the address it dials. */
function hasNodeName() {
  return !!learnedNode;
}

/**
 * The node this tab talks to: its own name when we managed to learn one, and
 * otherwise the address this tab dials — host AND port, because a bare
 * "localhost" reads like a name and is not one.
 */
function nodeLabel() {
  return learnedNode || window.location.host || I18n.t('connection.node_fallback');
}

/**
 * Two different losses wear the same overlay, and the sentence must not lie
 * about which one happened: a device without network is the user's problem to
 * fix, an unanswering node is ours. A third thing the card must not fake is
 * knowing the machine — an address gets its own sentence.
 */
function applyCause() {
  if (!overlay) return;
  const node = nodeLabel();
  const offline = !navigator.onLine;
  const named = hasNodeName();
  // A crossed-out signal would blame the browser's network for a node that
  // simply stopped answering, so each cause carries its own picture.
  overlay.setIcon(offline ? OFFLINE_ICON : '<use href="#i-host"/>');
  overlay.setTitle(I18n.t(offline ? 'connection.title_offline' : 'connection.title_node'));
  if (offline) overlay.setHeading(I18n.t('connection.heading_offline'));
  else overlay.setHeading(I18n.t(named ? 'connection.heading_node' : 'connection.heading_node_addr', { node }));
  const descKey = offline
    ? (named ? 'connection.desc_offline' : 'connection.desc_offline_addr')
    : (named ? 'connection.desc_node' : 'connection.desc_node_addr');
  overlay.setDesc(I18n.t(descKey, { node }));
}

/** True while the platform socket is down; a module overlay must yield to it. */
export function isPlatformDown() {
  return platformState !== 'ok';
}

// Blur ONLY #app-root (or .main-app). Blurring <body> would blur the overlay
// itself, which lives in the same tree.
function appRoot() {
  return document.getElementById('app-root') || document.querySelector('.main-app') || null;
}

export function init() {
  if (mounted) return;
  mounted = true;
  learnedNode = readStoredNodeName();

  overlay = createConnectionOverlay({
    iconTone: 'warn',
    withExtra: true,
    dim: { resolve: appRoot, className: 'app-blurred' },
    // Waiting is not the only thing to do. Every screen of the app is behind
    // this socket, so there is no other place in TentaFlow to send the user —
    // but a tab can be stale (a restarted node ships a new front) and reloading
    // is the one exit that does not depend on the retry loop.
    actions: [
      { id: 'reload', label: I18n.t('connection.btn_reload'), variant: 'ghost', icon: 'refresh' },
      { spacer: true },
      { id: 'retry-now', label: I18n.t('connection.btn_retry_now'), variant: 'primary', icon: 'refresh' },
    ],
    onAction: (action) => {
      if (action === 'reload') {
        window.location.reload();
        return;
      }
      if (action !== 'retry-now') return;
      overlay.log('info', I18n.t('connection.log_manual_retry'));
      ApiBinary.reconnectNow();
    },
  });
  overlay.extraEl.innerHTML = `
    <div class="conn-keep">
      <svg class="icon" aria-hidden="true"><use href="#i-check"/></svg>
      <p><b>${escapeHtml(I18n.t('connection.keep_lead'))}</b> ${escapeHtml(I18n.t('connection.keep_body'))}</p>
    </div>
  `;
  applyCause();
  overlay.setRetryLines(I18n.t('connection.next_attempt', { node: nodeLabel() }), '');

  // The device can come back online long before the node answers; the card must
  // then stop blaming the browser.
  const onConnectivity = () => {
    if (platformState === 'ok') return;
    applyCause();
    overlay.log('info', I18n.t(navigator.onLine ? 'connection.log_back_online' : 'connection.log_lost_offline'));
  };
  window.addEventListener('online', onConnectivity);
  window.addEventListener('offline', onConnectivity);
  connectivityOff = () => {
    window.removeEventListener('online', onConnectivity);
    window.removeEventListener('offline', onConnectivity);
  };

  ApiBinary.onLifecycle((ev) => {
    switch (ev.type) {
      case 'disconnected':
        platformState = 'disconnected';
        applyCause();
        overlay.log('err', I18n.t(navigator.onLine ? 'connection.log_lost_node' : 'connection.log_lost_offline', { node: nodeLabel() }));
        overlay.show();
        break;
      // The attempt number has ONE source: the reconnect scheduler. The header
      // and the log line written here quote the same `ev.info.attempt`, so the
      // last number in the log is always the number in the header.
      case 'reconnect-scheduled': {
        platformState = 'disconnected';
        const delay = Math.round((ev.info?.delayMs ?? 0) / 1000);
        const attempt = ev.info?.attempt ?? 0;
        applyCause();
        overlay.setRetryLines(
          I18n.t('connection.next_attempt', { node: nodeLabel() }),
          I18n.t('connection.attempt_hint', { attempt, delay }),
        );
        overlay.log('warn', I18n.t('connection.log_retry_scheduled', { attempt, delay }));
        overlay.show();
        overlay.startCountdown(ev.info?.delayMs ?? 0);
        break;
      }
      case 'reconnect-attempt':
        overlay.log('info', I18n.t('connection.log_retry_attempt', {
          attempt: ev.info?.attempt ?? 0, node: nodeLabel(),
        }));
        break;
      case 'open':
        // The only moment the node can be asked what it is called.
        void learnNodeName();
        if (platformState !== 'ok') {
          overlay.log('ok', I18n.t('connection.log_restored'));
          platformState = 'ok';
          overlay.hide();
        }
        break;
      case 'close':
        // A close from user intent (setJwt/clearSession) is not a failure.
        if (ev.info?.local) break;
        if (platformState === 'ok') {
          platformState = 'disconnected';
          applyCause();
          overlay.show();
        }
        break;
      default:
        break;
    }
  });
}

/** Destroy — for tests / HMR. */
export function destroy() {
  if (!mounted) return;
  connectivityOff?.();
  connectivityOff = null;
  overlay?.destroy();
  overlay = null;
  platformState = 'ok';
  learnedNode = '';
  learningNode = false;
  nameAttemptsLeft = 2;
  mounted = false;
}
