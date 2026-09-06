// ===== File: modules/tentaquant/run-model.js — one `runs` row, as the browser reads it =====
//
// Everything the run table (Q08) and the run detail derive from a `RunInfo` —
// its tier, the node that ran it, where it came from, how long it took, what a
// person may do to it — lives here as one function each, so both views say the
// same thing about the same row and the rules are checkable without a DOM.
//
// The two acts a person may perform on a run (pin, cancel) sit here as well,
// for the same reason: they are the same act from the table and from the
// detail, and `dispatch/tentaquant.rs` allows both only on one's OWN run.

import { toast } from '/js/utils.js';
import { T, fmtAgo, errMessage, parseServerTs, shortId } from '/js/modules/tentaquant/format.js';

/// Statuses `runs.status` takes, in the order the filter offers them.
export const RUN_STATUSES = ['created', 'queued', 'running', 'succeeded', 'failed', 'cancelled'];

/// The kinds `runs.kind` takes; anything else renders under `runs.kind_other`
/// rather than as a raw wire word.
const RUN_KINDS = ['cell', 'circuit', 'program', 'kata', 'flow'];

/// The `tf-chip` STATUS each run status wears. Every value here has to be one
/// of that component's own statuses: a chip given a word it does not know
/// falls back to `info` and paints a failed run BLUE, which is exactly the
/// message the status column exists to prevent.
const STATUS_TONE = {
  created: 'neutral',
  queued: 'info',
  running: 'accent',
  succeeded: 'ok',
  failed: 'err',
  cancelled: 'warn',
};

/// The tier a run executed on, read off `runs.target` (`browser` or
/// `core:<node_id>`). An unknown prefix answers '' rather than guessing.
export function runTier(run) {
  const target = String((run && run.target) || '');
  if (target === 'browser') return 'T0';
  if (target.startsWith('core:')) return 'T1';
  return '';
}

/// Which machine ran it, in the name the laboratory knows the node by. The run
/// row carries the node id only, so the fleet list resolves it; a node that has
/// since left the fleet keeps its id rather than disappearing.
export function runNodeName(run, nodes) {
  if (runTier(run) === 'T0') return T('targets.node_browser');
  const nodeId = run && (run.nodeId ?? run.node_id);
  if (!nodeId) return '';
  const node = (nodes || []).find((n) => n.nodeId === nodeId);
  return node ? node.nodeName : shortId(nodeId);
}

/// Where the run came from, as the second line of its project cell. A run
/// started from a notebook names the cell; a Studio run says so; anything else
/// falls back to the kind, which is never rendered raw.
export function runSourceLabel(run) {
  const kind = String((run && run.kind) || '');
  const cellId = run && (run.cellId ?? run.cell_id);
  if (run && (run.notebookId ?? run.notebook_id)) {
    return cellId ? T('runs.source_cell', { cell: shortId(cellId) }) : T('runs.source_notebook');
  }
  if (kind === 'circuit') return T('runs.source_circuit');
  return T(`runs.kind_${RUN_KINDS.includes(kind) ? kind : 'other'}`);
}

export function runStatusTone(status) {
  return STATUS_TONE[String(status || '')] || 'neutral';
}

/// The status as a person reads it. A failed run carries the reason: the whole
/// point of the column is that a red row says WHAT went wrong.
export function runStatusLabel(run) {
  const status = String((run && run.status) || '');
  const label = RUN_STATUSES.includes(status) ? T(`runs.status_${status}`) : status;
  return status === 'failed' && run && run.error ? `${label} · ${run.error}` : label;
}

/// How long the run took. A finished run measured it (`metrics.durationMs`); a
/// live one is measured against the clock, which is why `now` is a parameter
/// and not a call — a table has to render the same way in a test twice.
export function runDurationMs(run, now = Date.now()) {
  const metrics = run && run.metrics;
  const measured = Number(metrics && (metrics.durationMs ?? metrics.duration_ms));
  if (Number.isFinite(measured) && measured > 0) return measured;
  const started = parseServerTs(run && (run.startedAt ?? run.started_at));
  if (!started) return null;
  const ended = parseServerTs(run && (run.endedAt ?? run.ended_at));
  return Math.max(0, (ended ? ended.getTime() : now) - started.getTime());
}

export function runIsLive(run) {
  return ['created', 'queued', 'running'].includes(String(run && run.status));
}

/// Whether the caller may stop or pin this run. Both are acts on one's OWN run
/// (`dispatch/tentaquant.rs`): a supervisor reads everybody's runs and reaches
/// into none of them, so the buttons follow that rule here rather than letting
/// the server refuse a click that looked available.
export function canControlRun(run, userId) {
  return Boolean(run && userId && String(run.userId) === String(userId));
}

/// The event line of Q08, built from what a run row actually knows. Only two
/// moments carry a timestamp — the run was accepted, and it ended — so only
/// those two print one; the stages between are marked reached or pending and
/// say nothing they cannot prove.
export function runTimeline(run) {
  const status = String((run && run.status) || '');
  const terminal = !runIsLive(run) && Boolean(status);
  const reached = {
    created: Boolean(status),
    queued: status !== 'created' && Boolean(status),
    running: status === 'running' || terminal,
    done: terminal,
  };
  const outcome = status === 'succeeded' ? 'succeeded' : (status === 'cancelled' ? 'cancelled' : 'failed');
  return [
    { id: 'created', at: run && (run.startedAt ?? run.started_at), state: stageState(reached.created, status === 'created') },
    { id: 'queued', at: null, state: stageState(reached.queued, status === 'queued') },
    { id: 'running', at: null, state: stageState(reached.running, status === 'running') },
    {
      id: 'done',
      at: terminal ? (run.endedAt ?? run.ended_at) : null,
      state: stageState(reached.done, false),
      outcome: terminal ? outcome : '',
    },
  ];
}

function stageState(reached, current) {
  if (current) return 'current';
  return reached ? 'done' : 'pending';
}

export function matchesRun(run, query, projectName) {
  const q = String(query || '').trim().toLowerCase();
  if (!q) return true;
  return [run.runId, projectName, run.userName, run.target, run.status]
    .filter(Boolean).join(' ').toLowerCase()
    .includes(q);
}

/// The rows a filter set leaves. `user` is either 'all' or a user id — the
/// supervisor's filter of plan §13.1 item 6 — and 'all' is what everybody else
/// has, because everybody else only ever sees their own runs.
export function filterRuns(runs, filters = {}, projectNames = new Map()) {
  const tier = filters.tier || 'all';
  const status = filters.status || 'all';
  const user = filters.user || 'all';
  return (runs || []).filter((run) => {
    if (tier !== 'all' && runTier(run) !== tier) return false;
    if (status !== 'all' && String(run.status) !== status) return false;
    if (user !== 'all' && String(run.userId) !== user) return false;
    return matchesRun(run, filters.query, projectNames.get(run.projectId));
  });
}

/// The people a supervisor may filter by: whoever actually appears in the list,
/// by id, sorted by the name they are shown under.
export function runUsers(runs) {
  const byId = new Map();
  for (const run of runs || []) {
    if (run.userId && !byId.has(run.userId)) byId.set(run.userId, run.userName || run.userId);
  }
  return Array.from(byId, ([userId, name]) => ({ userId, name }))
    .sort((a, b) => a.name.localeCompare(b.name));
}

/// The second line of a run's project cell: where it came from and when.
export function runSourceLine(run) {
  return `${runSourceLabel(run)} · ${fmtAgo(run.startedAt)}`;
}

// ---------------------------------------------------------------------------
// The two acts
// ---------------------------------------------------------------------------

export async function setRunPinned(screen, run, pinned, { projectId = null } = {}) {
  try {
    await screen.tq('tentaQuantRunPinRequest', { runId: run.runId, pinned });
    toast(T(pinned ? 'runs.pinned_ok' : 'runs.unpinned_ok'), 'success');
  } catch (e) {
    toast(`${T('runs.action_failed')}: ${errMessage(e)}`, 'error');
    return false;
  }
  await screen.reloadRuns({ projectId });
  return true;
}

export async function cancelRun(screen, run, { projectId = null } = {}) {
  try {
    await screen.tq('tentaQuantRunCancelRequest', { runId: run.runId });
    toast(T('runs.cancel_ok'), 'success');
  } catch (e) {
    toast(`${T('runs.action_failed')}: ${errMessage(e)}`, 'error');
    return false;
  }
  await screen.reloadRuns({ projectId });
  return true;
}
