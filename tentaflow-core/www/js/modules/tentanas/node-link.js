// ===== File: modules/tentanas/node-link.js — the selected node stopped answering (n18c) =====
//
// A node the dashboard manages through the mesh can stop answering while the
// browser's own socket to the platform is fine: the forwarding node answers
// every request addressed to it with `NodeUnreachable` (dispatch/app_route.rs).
// Before this module the screen then failed tile by tile — a toast per poll, a
// "load failed" banner per tab — and nothing said the one fact that mattered:
// the NODE is gone, not the data, and its NAS keeps serving.
//
// n18c draws that as the shared connection overlay (connection-overlay.js,
// the card Code Studio's G01 wears too): the node's name, what is paused, what
// keeps running, a countdown ring with a growing backoff (to 30 s), "Połącz
// teraz", and a way out to the fleet.
//
// The overlay is a PROJECTION OF CONNECTIVITY: nothing is written anywhere. Its
// other half is what the screen's requests do meanwhile. A request for the
// lost node is not sent — it is PARKED and sent when the node answers again.
// The polls therefore stop on their own (each one awaits its parked request),
// raise no toast per tick, and resume where they were once the probe gets an
// answer: no tab is redrawn, each poll simply patches what it gets, as it
// always does (owner's rule: incremental updates only). The request that
// FAILED is not re-sent: it may have run on the node before the answer was
// lost, so its caller gets an error — worded, never the transport's own,
// which names the node by its id (`lostNodeError`).

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { createConnectionOverlay, isPlatformDown, OFFLINE_ICON } from '/js/modules/connection-overlay.js';
import { T } from '/js/modules/tentanas/format.js';
import { nodeT } from '/js/modules/tentanas/node-phrase.js';

// n18c: "próba 3 · backoff do 30 s" with a 7 s countdown — doubling from 2 s,
// capped at 30 s so a long outage costs one probe per half minute.
export const RETRY_BASE_MS = 2000;
export const RETRY_MAX_MS = 30000;

/** The wait before probe number `attempt` (1-based). */
export function retryDelayMs(attempt) {
  const n = Math.max(1, Number(attempt) || 1);
  return Math.min(RETRY_MAX_MS, RETRY_BASE_MS * 2 ** (n - 1));
}

/**
 * Whether an error says the ADDRESSED NODE did not answer — not that it
 * answered with a refusal. The forwarding node answers with the protocol code
 * `NodeUnreachable`; the transport surfaces it either as `err.code` (the
 * shim) or in the message (`protocol error NodeUnreachable: …`).
 */
export function isNodeUnreachable(err) {
  if (!err) return false;
  if (err.code === 'NodeUnreachable') return true;
  return /^protocol error NodeUnreachable\b/.test(String(err.message || ''));
}

const CHECK_ICON = '<polyline points="20 6 9 17 4 12"/>';

/**
 * The error a caller gets for a request the lost node never answered: worded
 * with the node's NAME and keeping the code, so `errMessage` and every other
 * reader still know what happened. The transport's own error is never handed
 * on — its sentence is "node '<64-hex id>' did not answer", and a caller
 * prints what it catches (a toast, a banner, a tab body).
 */
export function lostNodeError(node) {
  const err = new Error(nodeT('unreachable.title', node));
  err.code = 'NodeUnreachable';
  return err;
}

/**
 * One link watcher per screen. `screen` must expose `nodeId`, `nodes`,
 * `disposed`, `loadNodes()` and `leaveToFleet()`, and may expose
 * `nodeRecovered(nodeId)`.
 *
 * `send(node, fn)` wraps a request for `node`: it is sent at once while the
 * node answers, parked while it does not, and its failure with
 * `NodeUnreachable` raises the overlay (only for the node the screen shows —
 * the fleet view lists an unreachable node as a row of its own).
 */
export function createNodeLink(screen, { transport = ApiBinary, setTimer = setTimeout, clearTimer = clearTimeout } = {}) {
  let lost = null; // { nodeId, attempt, timer, parked: [{ run, resolve, reject }], probing }
  let overlay = null;
  let keepEl = null;

  const nodeOf = (nodeId) => (screen.nodes || []).find((n) => n.nodeId === nodeId) || { nodeId, nodeName: '' };

  function ensureOverlay() {
    if (overlay) return overlay;
    overlay = createConnectionOverlay({
      variantClass: 'nas-conn',
      titleText: '',
      iconSvg: OFFLINE_ICON,
      iconTone: 'danger',
      withExtra: true,
      withLog: true,
      dim: { resolve: () => document.getElementById('nas-root'), className: 'nas-link-lost' },
      actions: [
        { id: 'fleet', label: T('unreachable.btn_fleet'), variant: 'ghost', icon: 'chevron-left' },
        { spacer: true },
        { id: 'refresh', label: T('unreachable.btn_refresh'), variant: 'ghost' },
        { id: 'retry', label: T('unreachable.btn_retry'), variant: 'primary', icon: 'refresh' },
      ],
      onAction: (action) => {
        if (!lost) return;
        if (action === 'fleet') { leave(); screen.leaveToFleet(); return; }
        if (action === 'retry') { overlay.log('info', T('unreachable.log_manual')); probe(); return; }
        if (action === 'refresh') refreshMesh();
      },
    });
    overlay.extraEl.innerHTML = `<div class="nas-conn-keep"><svg class="icon" aria-hidden="true"><use href="#i-check"/></svg><div data-role="keep"></div></div>`;
    keepEl = overlay.extraEl.querySelector('[data-role="keep"]');
    return overlay;
  }

  // Dialogs open when the node went away (a retype confirm, a wizard) are
  // made inert until it answers or the screen leaves it: a click or an Enter
  // there would park a destructive request that runs minutes later, long
  // after the card said operations are paused.
  let inertDialogs = [];
  function freezeDialogs() {
    inertDialogs = [...document.querySelectorAll('tf-window, .tf-modal-card, .tf-modal-backdrop')]
      .filter((el) => !el.inert && !overlay?.el.contains(el));
    for (const el of inertDialogs) el.inert = true;
  }
  function thawDialogs() {
    for (const el of inertDialogs) el.inert = false;
    inertDialogs = [];
  }

  function paintLost() {
    const card = ensureOverlay();
    freezeDialogs();
    const node = nodeOf(lost.nodeId);
    card.setTone('danger');
    card.setIcon(OFFLINE_ICON);
    card.setTitle(nodeT('unreachable.title', node));
    card.setHeading(T('unreachable.heading'));
    card.setDesc(nodeT('unreachable.desc', node));
    keepEl.textContent = nodeT('unreachable.keep', node);
    card.setRetryVisible(true);
    card.setFootVisible(true);
    // The platform overlay outranks this one: "the daemon is gone" covers
    // "one node is gone", and the two never stack.
    if (!isPlatformDown()) card.show();
  }

  function schedule() {
    if (!lost) return;
    lost.attempt += 1;
    const delay = retryDelayMs(lost.attempt);
    overlay.setRetryLines(T('unreachable.retry_line'), T('unreachable.retry_attempt', { attempt: lost.attempt, max: RETRY_MAX_MS / 1000 }));
    overlay.startCountdown(delay);
    clearTimer(lost.timer);
    lost.timer = setTimer(() => probe(), delay);
  }

  // One cheap read, sent straight to the transport so it is never parked
  // behind itself. Any answer — a refusal included — means the node is back.
  async function probe() {
    if (!lost || lost.probing) return;
    const current = lost;
    current.probing = true;
    clearTimer(current.timer);
    overlay.stopCountdown();
    try {
      await transport.action('tentaNasEnvironmentRequest', { refresh: false }, { targetNodeId: current.nodeId });
      if (lost === current) recovered();
    } catch (err) {
      if (lost !== current) return;
      current.probing = false;
      if (!isNodeUnreachable(err)) { recovered(); return; }
      overlay.log('warn', nodeT('unreachable.log_no_answer', nodeOf(current.nodeId), { attempt: current.attempt }));
      schedule();
    }
  }

  // "Odśwież": the mesh's own view of the node (the fleet list's `online`),
  // which tells a node the mesh no longer sees from one that is connected and
  // silent. Then a probe, because the list may be the older news.
  async function refreshMesh() {
    try { await screen.loadNodes(); } catch { /* the probe below still answers */ }
    if (!lost || screen.disposed) return;
    const node = nodeOf(lost.nodeId);
    overlay.log('info', nodeT(node.online ? 'unreachable.log_mesh_connected' : 'unreachable.log_mesh_offline', node));
    probe();
  }

  function recovered() {
    const parked = lost ? lost.parked : [];
    const nodeId = lost?.nodeId;
    clearTimer(lost?.timer);
    lost = null;
    thawDialogs();
    if (overlay) {
      overlay.log('ok', T('unreachable.log_restored'));
      overlay.setTone('ok');
      overlay.setIcon(CHECK_ICON);
      overlay.hide();
    }
    for (const p of parked) p.run().then(p.resolve, p.reject);
    screen.nodeRecovered?.(nodeId);
  }

  // The screen leaves the node (fleet button, another node, unmount): the
  // parked requests are refused with the error that parked them, so every
  // awaiting caller finishes; the callers already stop on `disposed` or on a
  // body that is no longer connected.
  function leave() {
    if (!lost) return;
    const { parked, error, timer } = lost;
    clearTimer(timer);
    lost = null;
    thawDialogs();
    if (overlay) overlay.hide({ immediate: true });
    for (const p of parked) p.reject(error);
  }

  function fail(nodeId) {
    if (screen.disposed || nodeId !== screen.nodeId) return;
    if (lost && lost.nodeId === nodeId) return;
    leave();
    lost = { nodeId, attempt: 0, timer: 0, parked: [], probing: false, error: lostNodeError(nodeOf(nodeId)) };
    paintLost();
    // Worded, never the transport's sentence: that one carries the node id
    // ("node '<64 hex>' did not answer"), and no id reaches the screen.
    overlay.log('warn', nodeT('unreachable.log_lost', nodeOf(nodeId)));
    schedule();
  }

  return {
    isLost: (nodeId) => Boolean(lost && lost.nodeId === nodeId),

    send(node, run) {
      if (!node || node.isLocal) return run();
      if (lost && lost.nodeId === node.nodeId) {
        return new Promise((resolve, reject) => lost.parked.push({ run, resolve, reject }));
      }
      return run().catch((err) => {
        if (!isNodeUnreachable(err)) throw err;
        fail(node.nodeId);
        throw lostNodeError(node);
      });
    },

    leave,

    destroy() {
      leave();
      if (overlay) { overlay.destroy(); overlay = null; keepEl = null; }
    },
  };
}

// Exported for the tests: the words the card is made of, in the reader's
// language, so a missing key fails a test instead of printing itself.
export const NODE_LINK_KEYS = [
  'unreachable.title', 'unreachable.title_unnamed', 'unreachable.heading', 'unreachable.desc', 'unreachable.desc_unnamed',
  'unreachable.keep', 'unreachable.keep_unnamed', 'unreachable.retry_line', 'unreachable.retry_attempt',
  'unreachable.btn_fleet', 'unreachable.btn_refresh', 'unreachable.btn_retry', 'unreachable.log_manual',
  'unreachable.error', 'unreachable.log_lost', 'unreachable.log_lost_unnamed',
  'unreachable.log_no_answer', 'unreachable.log_no_answer_unnamed', 'unreachable.log_restored',
  'unreachable.log_mesh_connected', 'unreachable.log_mesh_connected_unnamed',
  'unreachable.log_mesh_offline', 'unreachable.log_mesh_offline_unnamed',
];
