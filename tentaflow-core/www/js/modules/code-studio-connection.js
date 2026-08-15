// ===== File: code-studio-connection.js — owner node reachability (G01) =====
//
// The plan is explicit (§3.5, §19): an unreachable owner node is a PROJECTION
// OF CONNECTIVITY IN THE UI, never a status in the database. The session keeps
// running where it lives — agents, processes and the event log are on the owner
// node, not in this browser tab — so this module writes nothing anywhere. It
// watches one session's stream, and when the node stops answering it draws the
// overlay, retries with a growing backoff, and on recovery pulls the timeline
// FROM THE `seq` CURSOR instead of starting over (§13.3, §12.2).
//
// The card is the platform overlay's card: `createConnectionOverlay()` from
// connection-overlay.js builds it, this module only fills a variant. Both
// overlays never show at once — `isPlatformDown()` outranks us, because "the
// daemon is gone" is a bigger sentence than "one node is gone".
//
// Two losses, two owners:
//   * browser ↔ platform  — the socket died; ApiBinary lifecycle reports it and
//     the platform overlay covers the whole app. We stand down.
//   * owner node          — the socket is fine, but every session-scoped call
//     returns NotAvailable naming the node (dispatch `require_local`) and the
//     session stream ends with `workspace_not_local`. That is this overlay.

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { escapeHtml } from '/js/utils.js';
import { createConnectionOverlay, isPlatformDown, OFFLINE_ICON } from '/js/modules/connection-overlay.js';
import '/js/components/tf-progress-bar.js';

// Backoff: short enough that a blip costs seconds, capped so an hour-long
// outage does not hammer the mesh.
const RETRY_BASE_MS = 4000;
const RETRY_FACTOR = 1.8;
const RETRY_MAX_MS = 60000;

// Catch-up paging. The server clamps a timeline page at 500; 200 keeps one
// round trip small enough to show progress that means something.
const CATCHUP_PAGE = 200;
const CATCHUP_MAX_PAGES = 40;
// Events handed to the console per frame — the DOM work, not the network, is
// what makes a long catch-up feel slow.
const CATCHUP_CHUNK = 25;

// Stream ends that mean "the owner node cannot serve this right now": retry.
const RETRYABLE_END = new Set(['workspace_not_local', 'internal_error']);
// Ends that are a verdict about the session, not about connectivity.
const FINAL_END = new Set(['session_closed', 'not_found', 'permission_revoked']);
// Protocol error codes that name the node rather than the request.
const NODE_ERROR_CODES = new Set(['NotAvailable', 'NodeUnreachable', 'Internal']);

let overlay = null;
let keepEl = null;
let catchupEl = null;
let progressEl = null;
let progressNoteEl = null;

let ctx = null;
// idle | live | unreachable | catchup | platform_down | ended
let phase = 'idle';
let cursor = 0;
let attempt = 0;
let retryTimer = 0;
let streamOff = null;
let streamToken = 0;
let catchupToken = 0;
let lifecycleOff = null;
// A probe is in flight; a second "retry now" click must not open a parallel one.
let probing = false;

function t(key, vars) {
  return I18n.t(`code_studio.connection.${key}`, vars || null);
}

function nodeLabel() {
  return ctx?.nodeLabel || ctx?.nodeId || '';
}

function isNodeError(err) {
  return NODE_ERROR_CODES.has(String(err?.code ?? ''));
}

// ---------------------------------------------------------------------------
// Overlay
// ---------------------------------------------------------------------------

const CHECK_ICON = '<polyline points="20 6 9 17 4 12"/>';

function ensureOverlay() {
  if (overlay) return overlay;

  overlay = createConnectionOverlay({
    variantClass: 'go-conn',
    titleText: t('title'),
    iconSvg: OFFLINE_ICON,
    iconTone: 'warn',
    withExtra: true,
    // Only the Code Studio shell dims — the sidebar stays legible, because
    // "go somewhere else in TentaFlow" is one of the two offered answers.
    dim: { resolve: () => document.querySelector('.cs-session-view .cs-shell'), className: 'go-dimmed' },
    actions: [
      { id: 'other', label: t('btn_other'), variant: 'ghost', icon: 'list' },
      { spacer: true },
      { id: 'retry', label: t('btn_retry'), variant: 'primary', icon: 'refresh' },
    ],
    onAction: (action) => {
      if (action === 'other') {
        ctx?.onLeave?.();
        return;
      }
      if (action === 'retry') {
        overlay.log('info', t('log_manual_retry'));
        runRetry(true);
      }
    },
  });

  overlay.extraEl.innerHTML = `
    <div class="go-keep">
      <svg class="icon" aria-hidden="true"><use href="#i-check"/></svg>
      <p><b>${escapeHtml(t('keep_lead'))}</b> ${escapeHtml(t('keep_body'))}</p>
    </div>
    <div class="go-catchup" hidden>
      <tf-progress-bar tone="success" size="sm" value="0"></tf-progress-bar>
      <div class="go-catchup-note"></div>
    </div>
  `;
  keepEl = overlay.extraEl.querySelector('.go-keep');
  catchupEl = overlay.extraEl.querySelector('.go-catchup');
  progressEl = catchupEl.querySelector('tf-progress-bar');
  progressNoteEl = catchupEl.querySelector('.go-catchup-note');
  return overlay;
}

/** The "node unreachable" face of the card. */
function showUnreachable() {
  const card = ensureOverlay();
  card.setTone('warn');
  card.setIcon(OFFLINE_ICON);
  card.setTitle(t('title'));
  card.setHeading(t('heading', { node: nodeLabel() }));
  card.setDesc(t('desc'));
  keepEl.hidden = false;
  catchupEl.hidden = true;
  card.setRetryVisible(true);
  card.setLogVisible(true);
  card.setFootVisible(true);
  card.show();
}

/** The "connection is back, pulling the timeline" face of the card. */
function showCatchUp(total, fromSeq) {
  const card = ensureOverlay();
  card.setTone('ok');
  card.setIcon(CHECK_ICON);
  card.setTitle(t('back_title'));
  card.setHeading(t('back_heading'));
  card.setDesc(t('back_desc', { count: total, seq: fromSeq }));
  keepEl.hidden = true;
  catchupEl.hidden = false;
  card.setRetryVisible(false);
  card.setLogVisible(false);
  card.setFootVisible(false);
  setCatchUpProgress(0, total);
  card.show();
}

function setCatchUpProgress(done, total) {
  const pct = total > 0 ? Math.round((done / total) * 100) : 100;
  progressEl.setAttribute('value', String(pct));
  progressNoteEl.textContent = t('back_progress', { done, total });
}

function hideOverlay(immediate = false) {
  if (overlay) overlay.hide({ immediate });
}

// ---------------------------------------------------------------------------
// Session stream — the live path AND the detector
// ---------------------------------------------------------------------------

function closeStream() {
  streamToken += 1;
  if (streamOff) {
    try { streamOff(); } catch (err) { console.warn('[code-studio] stream close failed:', err?.message ?? err); }
    streamOff = null;
  }
}

function openStream() {
  closeStream();
  if (!ctx) return;
  const token = streamToken;
  const { workspaceId, sessionId } = ctx;

  ApiBinary.subscribe('codeStudioSessionStreamRequest', {
    workspaceId, sessionId, afterSeq: cursor,
  }, {
    onChunk: (body) => {
      if (token !== streamToken) return;
      const seq = Number(body?.seq ?? 0);
      if (seq > cursor) cursor = seq;
      // The console dedupes by seq, so an event its own timeline poll already
      // rendered is dropped instead of appended twice.
      ctx.applyEvent(body);
    },
    onEnd: (body) => {
      if (token !== streamToken) return;
      const reason = String(body?.reason ?? '');
      // Drops the local listener too — the server is done with this stream, the
      // browser must not keep a handler for a correlation id nobody feeds.
      closeStream();
      if (FINAL_END.has(reason)) {
        phase = 'ended';
        hideOverlay();
        return;
      }
      if (RETRYABLE_END.has(reason) || reason === '') {
        enterUnreachable(t('log_stream_lost'));
        return;
      }
      // An end we do not have a rule for is still a dead stream; say what the
      // server said instead of inventing a diagnosis.
      enterUnreachable(t('log_stream_end', { reason }));
    },
    onError: (body) => {
      if (token !== streamToken) return;
      closeStream();
      enterUnreachable(String(body?.message ?? t('log_stream_lost')));
    },
  }).then((off) => {
    if (token !== streamToken) {
      try { off(); } catch { /* the subscription is already stale */ }
      return;
    }
    streamOff = off;
  }).catch((err) => {
    if (token !== streamToken) return;
    enterUnreachable(err?.message ?? t('log_stream_lost'));
  });
}

// ---------------------------------------------------------------------------
// Unreachable state + retry loop
// ---------------------------------------------------------------------------

function clearRetryTimer() {
  if (retryTimer) {
    window.clearTimeout(retryTimer);
    retryTimer = 0;
  }
}

/**
 * Puts the card on screen in its unreachable form. Returns false when the
 * platform socket is the real casualty — that overlay owns the screen, ours
 * would stack a second modal on it and say the wrong thing.
 */
function surfaceUnreachable() {
  if (isPlatformDown()) {
    yieldToPlatform();
    return false;
  }
  closeStream();
  phase = 'unreachable';
  showUnreachable();
  return true;
}

function enterUnreachable(logLine) {
  if (!ctx || phase === 'ended') return;
  const repeat = phase === 'unreachable';
  if (!surfaceUnreachable()) return;
  if (logLine) overlay.log(repeat ? 'warn' : 'err', logLine);
  scheduleRetry();
}

function retryDelay() {
  const raw = RETRY_BASE_MS * (RETRY_FACTOR ** Math.max(0, attempt - 1));
  return Math.min(RETRY_MAX_MS, Math.round(raw));
}

/**
 * `attempt` is the ONE number the card knows: it is raised here, at the moment
 * an attempt is announced, and every line that mentions a try — the header, the
 * scheduling entry, the failure entry — reads that same variable. The header can
 * therefore never run ahead of the log.
 */
function scheduleRetry() {
  clearRetryTimer();
  attempt += 1;
  const delay = retryDelay();
  const seconds = Math.round(delay / 1000);
  overlay.setRetryLines(t('retry_line'), t('retry_attempt', { attempt, delay: seconds }));
  overlay.startCountdown(delay);
  overlay.log('info', t('log_retry_scheduled', { attempt, delay: seconds }));
  retryTimer = window.setTimeout(() => { retryTimer = 0; runRetry(false); }, delay);
}

async function runRetry(manual) {
  if (!ctx || phase === 'ended' || probing) return;
  clearRetryTimer();
  if (isPlatformDown()) {
    yieldToPlatform();
    return;
  }
  // A click skips the wait but is still a try, so it gets its own number
  // instead of borrowing the one the countdown was already showing.
  if (manual) {
    attempt += 1;
    overlay?.setRetryLines(t('retry_line'), t('retry_attempt_now', { attempt }));
  }
  overlay?.stopCountdown();

  probing = true;
  let page;
  try {
    // One session-scoped call answers both questions: is the owner node serving
    // us again, and what happened while it was not.
    page = await ApiBinary.one('codeStudioSessionTimelineRequest', {
      workspaceId: ctx.workspaceId, sessionId: ctx.sessionId,
      afterSeq: cursor, limit: CATCHUP_PAGE,
    });
  } catch (err) {
    probing = false;
    if (!ctx) return;
    if (!surfaceUnreachable()) return;
    await logProbeFailure(err);
    if (!ctx || phase !== 'unreachable') return;
    scheduleRetry();
    return;
  }
  probing = false;
  if (!ctx) return;
  overlay?.log('ok', t('log_restored'));
  await catchUp(page);
}

/** A visible registry row with no stream is a different sentence than silence. */
async function logProbeFailure(err) {
  if (!ctx) return;
  if (!isNodeError(err)) {
    overlay?.log('warn', String(err?.message ?? ''));
    return;
  }
  // The registry is mesh-synced and NOT gated by `require_local`, so it answers
  // even when the owner node does not.
  let registryVisible = false;
  try {
    await ApiBinary.one('codeStudioWorkspaceGetRequest', { workspaceId: ctx.workspaceId });
    registryVisible = true;
  } catch { registryVisible = false; }
  if (!ctx) return;
  if (registryVisible) {
    overlay?.log('warn', t('log_registry_no_stream'));
  } else {
    overlay?.log('warn', t('log_no_answer', { node: nodeLabel(), attempt }));
  }
}

// ---------------------------------------------------------------------------
// Catch-up from the `seq` cursor
// ---------------------------------------------------------------------------

/**
 * Drains the timeline from the cursor, then feeds the console in chunks so the
 * progress bar tracks real work. `firstPage` is the response that proved the
 * node is back, so the probe is not paid for twice.
 */
async function catchUp(firstPage) {
  const token = ++catchupToken;
  phase = 'catchup';

  // Where the gap started: the cursor we asked from, which is the last event
  // the console already holds.
  const fromSeq = cursor;
  const backlog = [];
  let page = firstPage;
  let guard = 0;
  while (page && guard < CATCHUP_MAX_PAGES) {
    guard += 1;
    const events = page.events || [];
    for (const ev of events) backlog.push(ev);
    const next = Number(page.next_seq ?? page.nextSeq ?? 0);
    if (next > cursor) cursor = next;
    const hasMore = !!(page.has_more ?? page.hasMore) && events.length > 0;
    if (!hasMore) break;
    try {
      page = await ApiBinary.one('codeStudioSessionTimelineRequest', {
        workspaceId: ctx.workspaceId, sessionId: ctx.sessionId,
        afterSeq: cursor, limit: CATCHUP_PAGE,
      });
    } catch (err) {
      if (token !== catchupToken || !ctx) return;
      if (!surfaceUnreachable()) return;
      await logProbeFailure(err);
      if (!ctx || phase !== 'unreachable') return;
      scheduleRetry();
      return;
    }
    if (token !== catchupToken || !ctx) return;
  }

  const total = backlog.length;
  if (total === 0) {
    finishCatchUp();
    return;
  }

  showCatchUp(total, fromSeq);

  let done = 0;
  while (done < total) {
    if (token !== catchupToken || !ctx) return;
    const slice = backlog.slice(done, done + CATCHUP_CHUNK);
    ctx.applyEvent({ events: slice });
    done += slice.length;
    setCatchUpProgress(done, total);
    // eslint-disable-next-line no-await-in-loop
    await new Promise((resolve) => window.requestAnimationFrame(resolve));
  }
  if (token !== catchupToken || !ctx) return;
  finishCatchUp();
}

function finishCatchUp() {
  attempt = 0;
  phase = 'live';
  hideOverlay();
  openStream();
}

// ---------------------------------------------------------------------------
// Platform socket arbitration
// ---------------------------------------------------------------------------

function yieldToPlatform() {
  clearRetryTimer();
  closeStream();
  catchupToken += 1;
  probing = false;
  if (phase !== 'ended') phase = 'platform_down';
  if (overlay?.isVisible()) overlay.log('warn', t('log_platform_down'));
  hideOverlay(true);
}

function onLifecycle(ev) {
  if (!ctx) return;
  switch (ev.type) {
    case 'disconnected':
      yieldToPlatform();
      break;
    case 'close':
      if (!ev.info?.local) yieldToPlatform();
      break;
    case 'open':
      // The socket came back. Whatever accrued in the gap is pulled from the
      // cursor by the same catch-up that a node outage uses.
      if (phase === 'platform_down') {
        attempt = 0;
        runRetry(true);
      }
      break;
    default:
      break;
  }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/**
 * Starts watching one session.
 * `context`: { workspaceId, sessionId, nodeId, nodeLabel, applyEvent, onLeave }
 * `applyEvent` is the console's own ingest — this module never renders a
 * timeline itself, it only makes sure the console gets every event exactly once.
 */
export function attachSession(context) {
  detachSession();
  if (!context?.workspaceId || !context?.sessionId || typeof context.applyEvent !== 'function') return;

  ctx = {
    workspaceId: String(context.workspaceId),
    sessionId: String(context.sessionId),
    nodeId: String(context.nodeId ?? ''),
    nodeLabel: String(context.nodeLabel ?? ''),
    applyEvent: context.applyEvent,
    onLeave: typeof context.onLeave === 'function' ? context.onLeave : null,
  };
  cursor = 0;
  attempt = 0;
  phase = isPlatformDown() ? 'platform_down' : 'live';
  lifecycleOff = ApiBinary.onLifecycle(onLifecycle);
  if (phase === 'live') openStream();
}

/** Stops watching. Nothing about the session changes — only this tab stops looking. */
export function detachSession() {
  clearRetryTimer();
  closeStream();
  catchupToken += 1;
  if (lifecycleOff) {
    lifecycleOff();
    lifecycleOff = null;
  }
  if (overlay) {
    overlay.destroy();
    overlay = null;
    keepEl = null;
    catchupEl = null;
    progressEl = null;
    progressNoteEl = null;
  }
  ctx = null;
  phase = 'idle';
  cursor = 0;
  attempt = 0;
  probing = false;
}
