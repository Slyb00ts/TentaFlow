// ============ File: flows-builder/process-token-overlay.js — token view of one process run turned into a canvas overlay and a polling feed ============

export const POLL_INTERVAL_MS = 3000;

// Statuses after which the engine writes nothing more; one last read still shows the final path.
const TERMINAL_STATUSES = new Set(['Completed', 'Cancelled', 'Error']);

export function instanceIsTerminal(status) {
  return TERMINAL_STATUSES.has(status);
}

// Priority for the icon and the lead wording of an element that holds tokens of several kinds.
const WAIT_ORDER = ['UserTask', 'Job', 'Call', 'Message', 'Timer', 'Scope', 'Gateway', 'Ready'];

const WAIT_ICONS = {
  UserTask: 'user', Job: 'chip', Timer: 'clock', Message: 'mail', Call: 'external-link',
  Gateway: 'pause', Scope: 'bpmn-subprocess', Ready: 'play',
};

/** What a live token waits for: one of the wait names, `Ready` when it waits for nothing. */
export function tokenWaitKind(token) {
  const wait = token.wait;
  if (typeof wait === 'string') return WAIT_ORDER.includes(wait) ? wait : 'Ready';
  if (wait && typeof wait === 'object') return Object.keys(wait).find((key) => WAIT_ORDER.includes(key)) ?? 'Ready';
  return token.status === 'Joining' ? 'Gateway' : 'Ready';
}

/**
 * A repeated element keeps one coordinator token beside one token per live
 * occurrence. The coordinator has neither a wait nor an occurrence; counting it
 * would show one token more than the run has work in flight.
 */
export function isCoordinatorToken(token, tokens, repeatingNodeIds) {
  if (token.wait != null || token.occurrence != null) return false;
  if (repeatingNodeIds.has(token.nodeId)) return true;
  return tokens.some((other) => other !== token && other.nodeId === token.nodeId
    && other.scopeId === token.scopeId && other.occurrence != null);
}

/**
 * Maps one decoded token view onto what the canvas draws. `repeatingNodeIds` are
 * the elements of the model that carry a repetition. Do not pass a `not_modified`
 * answer: it carries no collections and means "keep what you drew".
 */
export function buildTokenOverlay(response, { repeatingNodeIds = new Set() } = {}) {
  const tokens = response.tokens ?? [];
  const nodes = new Map();
  const entryOf = (nodeId) => {
    if (!nodes.has(nodeId)) {
      nodes.set(nodeId, { nodeId, tokens: [], incidentIds: [], kinds: [], count: 0, inside: 0, displayCount: 0, occurrenceCount: 0 });
    }
    return nodes.get(nodeId);
  };
  for (const token of tokens) {
    const coordinator = isCoordinatorToken(token, tokens, repeatingNodeIds);
    const incidentIds = token.incidentIds ?? [];
    if (coordinator && incidentIds.length === 0) continue;
    const entry = entryOf(token.nodeId);
    for (const id of incidentIds) if (!entry.incidentIds.includes(id)) entry.incidentIds.push(id);
    if (coordinator) continue;
    const kind = tokenWaitKind(token);
    entry.tokens.push({ ...token, kind });
    entry.count += 1;
    if (!entry.kinds.includes(kind)) entry.kinds.push(kind);
    if (kind === 'Scope') entry.inside += token.wait.Scope.activeTokens ?? 0;
    if (token.occurrence != null) entry.occurrenceCount += 1;
  }
  for (const entry of nodes.values()) {
    entry.kinds.sort((left, right) => WAIT_ORDER.indexOf(left) - WAIT_ORDER.indexOf(right));
    // A sub-process token is only the door; the work is the tokens inside it.
    entry.displayCount = entry.kinds.includes('Scope') ? entry.inside + entry.tokens.filter((token) => token.kind !== 'Scope').length : entry.count;
    entry.tone = entry.incidentIds.length ? 'incident'
      : entry.tokens.some((token) => token.status !== 'Ready') ? 'waiting' : 'active';
    entry.primaryKind = entry.kinds[0] ?? 'Ready';
  }
  return {
    revision: response.instanceRevision, version: response.version, processId: response.processId,
    truncated: !!response.truncated, nodes,
    takenEdgeIds: new Set(response.takenEdgeIds ?? []),
    visitedNodeIds: new Set(response.visitedNodeIds ?? []),
  };
}

/**
 * The part of the overlay the canvas draws: per element a count, a tone, an icon
 * and the sentence a screen reader gets. `translate(key, values)` resolves the
 * `bpmn.*` keys, so this stays free of the i18n module.
 */
export function canvasOverlay(overlay, translate) {
  const nodes = new Map();
  for (const entry of overlay.nodes.values()) {
    const kinds = entry.kinds.map((kind) => translate(`diagram_wait_${kind.toLowerCase()}`));
    const parts = [];
    if (entry.displayCount > 0) parts.push(translate('diagram_token_count', { count: entry.displayCount }));
    if (kinds.length) parts.push(kinds.join(', '));
    if (entry.incidentIds.length) parts.push(translate('diagram_incident_count', { count: entry.incidentIds.length }));
    nodes.set(entry.nodeId, {
      count: entry.displayCount, tone: entry.tone,
      icon: entry.tone === 'incident' ? 'alert' : WAIT_ICONS[entry.primaryKind],
      label: parts.join('. '),
    });
  }
  return { nodes, takenEdgeIds: overlay.takenEdgeIds, visitedNodeIds: overlay.visitedNodeIds };
}

/** Ids of the elements of a model body that repeat. */
export function repeatingNodeIds(body) {
  return new Set((body?.nodes ?? []).filter((node) => node.repeat != null).map((node) => node.id));
}

/** The path of sub-process elements the canvas navigates to for a trail of drilled scopes. */
export function trailPath(trail) {
  return trail.map((step) => step.nodeId);
}

/** Opens the scope a token holds; opening the one already shown changes nothing. */
export function drillInto(trail, step) {
  if (trail.at(-1)?.scopeId === step.scopeId) return trail;
  return [...trail, step];
}

/** Back to the root (`depth` 0) or to the first `depth` steps of the trail. */
export function ascendTo(trail, depth) {
  return depth >= trail.length ? trail : trail.slice(0, Math.max(0, depth));
}

/**
 * Reads the token view of the shown scope on a timer, keeping the last answer of
 * every (instance, scope). One request is in flight at a time; a read asked for
 * meanwhile runs right after it. A read that starts once the run is terminal is
 * the last one. The caller stops the feed while the diagram is hidden.
 */
export function createTokenFeed({ instanceId, request, isTerminal, onOverlay, onError,
  interval = POLL_INTERVAL_MS, setTimer = setTimeout, clearTimer = clearTimeout }) {
  const cache = new Map();
  let scopeId = null;
  let timer = null;
  let inFlight = false;
  let rerun = false;
  let running = false;
  let disposed = false;
  const keyOf = (scope) => `${instanceId}|${scope ?? ''}`;

  function schedule() {
    clearTimer(timer);
    timer = running && !disposed ? setTimer(() => { timer = null; read(); }, interval) : null;
  }

  async function read() {
    if (disposed || !running) return;
    if (inFlight) { rerun = true; return; }
    inFlight = true;
    const scope = scopeId;
    const key = keyOf(scope);
    const terminalAtStart = isTerminal();
    try {
      const cached = cache.get(key);
      const response = await request({ instanceId, scopeId: scope, ifRevision: cached?.revision ?? null });
      if (disposed) return;
      let entry = cached;
      if (!response.notModified) {
        if (!cached || response.instanceRevision >= cached.revision) {
          entry = { revision: response.instanceRevision, response };
          cache.set(key, entry);
        }
      }
      // The answer of a scope that is no longer shown only fills the cache.
      if (entry && key === keyOf(scopeId)) onOverlay(entry.response, { fresh: entry !== cached });
    } catch (error) {
      if (!disposed && key === keyOf(scopeId)) onError(error);
    } finally {
      inFlight = false;
    }
    if (disposed) return;
    if (terminalAtStart) { running = false; clearTimer(timer); timer = null; return; }
    if (rerun) { rerun = false; read(); return; }
    schedule();
  }

  return {
    /** Reads now and then every `interval`; calling it again restarts the clock. */
    start() {
      if (disposed) return;
      running = true;
      clearTimer(timer);
      timer = null;
      read();
    },
    stop() { running = false; clearTimer(timer); timer = null; },
    /** Reads soon after the run changed, without waiting for the clock. */
    poke() { if (running && !disposed) { clearTimer(timer); timer = null; read(); } },
    /** Shows another scope: the cached answer at once, then a conditional read. */
    setScope(next) {
      scopeId = next ?? null;
      const cached = cache.get(keyOf(scopeId));
      if (cached) onOverlay(cached.response, { fresh: false });
      if (running) { clearTimer(timer); timer = null; read(); }
    },
    get scopeId() { return scopeId; },
    get running() { return running; },
    cached: (scope) => cache.get(keyOf(scope))?.response ?? null,
    dispose() { disposed = true; running = false; clearTimer(timer); timer = null; },
  };
}
