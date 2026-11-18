// ===== File: modules/sla/exec-state.js — the "Stan SLA" card above a flow's execution history =====
//
// One row per node, because the limits are counted on every node on its own
// (a flow that reads a topic has one row: the node that leads the reading, and
// "Wstrzymania limitem" replaces "Odmowy 429", since the bus is slowed down, not
// refused). Below the nodes, the breaches with their history.
//
// The card is drawn once (and again only when the set of nodes changes). Every
// poll afterwards writes its numbers into the cells that are already there, so
// nothing blinks and a selection or scroll position survives.

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeAttr, escapeHtml } from '/js/utils.js';
import { setAttr, setText } from '/js/lib/dom-patch.js';
import '/js/components/tf-chip.js';
import { T, breachCells, clockText, dayAndClock, bounded, leadingReading, nodeCells, overallState, stateChip, stateLabel, durationText, windowName, windowPer, effectiveQueue, DEFAULT_QUEUE_TIMEOUT_MS } from './model.js';

const POLL_MS = 5000;
const BREACH_PAGE = 20;
const NODE_DEADLINE_MS = 4000;
const MUTED = 'font-size:12px;color:var(--text-3)';
const NODE_ID_SHORT = 8;

const defaultDeps = {
  call: (kind, payload, options) => ApiBinary.action(kind, payload, options),
  nodes: async () => (await ApiBinary.list('meshNodeListRequest', { arrayKey: 'nodes' })),
  now: () => Date.now(),
};

const shortNode = (id) => String(id).slice(0, NODE_ID_SHORT);

const cellHtml = (name) => `<span data-v="${name}"></span><div data-v="${name}-sub" style="${MUTED}"></div>`;

function cardHtml({ bus, policyName, windowTitle, nodes }) {
  const refusedHeader = T(bus ? 'head.held' : 'head.refused');
  const rows = nodes.map((node) => `<tr data-node="${escapeAttr(node.nodeId)}">
      <td><span data-v="node">${escapeHtml(node.name)}</span>${node.isLocal ? ` <tf-chip size="sm" variant="outline" status="neutral" label="${escapeAttr(T('node_local'))}"></tf-chip>` : ''}<div data-v="node-sub" style="${MUTED}"></div></td>
      <td>${cellHtml('inFlight')}</td><td>${cellHtml('window')}</td><td>${cellHtml('p95')}</td><td>${cellHtml('errors')}</td><td>${cellHtml('refused')}</td>
      <td><tf-chip data-v="chip" status="info"></tf-chip></td></tr>`).join('');
  return `
    <div class="card-header" style="flex-wrap:wrap;gap:8px">
      <h3 class="card-title">${escapeHtml(T('title'))} <tf-chip data-v="overall" status="info" hidden></tf-chip></h3>
      <span class="hint" style="margin:0">${escapeHtml(T('policy_prefix'))} <a href="#" data-go="policies" title="${escapeAttr(T('open_policies'))}">${escapeHtml(policyName)}</a> · ${escapeHtml(T('evaluated_over'))} · ${escapeHtml(T(bus ? 'cluster_limit' : 'per_node'))}</span>
    </div>
    <table class="data-table"><thead><tr>
      <th>${escapeHtml(T('head.node'))}</th><th>${escapeHtml(T('head.in_flight'))}</th><th data-v="window-head">${escapeHtml(windowTitle)}</th>
      <th>${escapeHtml(T('head.p95'))}</th><th>${escapeHtml(T('head.errors'))}</th><th>${escapeHtml(refusedHeader)}</th><th>${escapeHtml(T('head.state'))}</th>
    </tr></thead><tbody data-v="nodes">${rows}</tbody></table>
    <div class="an-tfoot"><span data-v="foot-cluster"></span><span data-v="foot-queue"></span></div>
    <div style="padding:10px 14px;font-size:12px;font-weight:700;display:flex;align-items:center;gap:8px">${escapeHtml(T('breaches'))} <tf-chip data-v="open-count" size="sm" status="err" hidden></tf-chip></div>
    <table class="data-table" data-v="breach-table" hidden><thead><tr>
      <th>${escapeHtml(T('breach_head.kind'))}</th><th>${escapeHtml(T('head.node'))}</th><th>${escapeHtml(T('breach_head.threshold'))}</th>
      <th>${escapeHtml(T('breach_head.peak'))}</th><th>${escapeHtml(T('head.state'))}</th><th>${escapeHtml(T('breach_head.opened'))}</th><th>${escapeHtml(T('breach_head.closed'))}</th>
    </tr></thead><tbody data-v="breaches"></tbody></table>
    <div class="an-tfoot" data-v="breach-empty" style="justify-content:flex-start">${escapeHtml(T('breaches_none'))}</div>
    <div class="an-tfoot" data-v="breach-more-row" hidden style="justify-content:flex-start;gap:12px"><span data-v="breach-shown"></span><tf-button variant="ghost" size="sm" data-v="breach-more">${escapeHtml(T('breaches_more'))}</tf-button></div>
    <div class="an-tfoot"><span>${escapeHtml(T('breach_foot_history'))}</span><span>${escapeHtml(T('breach_foot_closing'))}</span></div>`;
}

/** `hidden` loses to `.an-tfoot { display: flex }`, so the footers of this card also set `display`. */
function setHidden(el, hidden) {
  el.hidden = hidden;
  el.style.display = hidden ? 'none' : '';
}

const rowOf = (root, nodeId) => [...root.querySelectorAll('tr[data-node]')].find((tr) => tr.dataset.node === nodeId);
const put = (scope, name, text) => setText(scope.querySelector(`[data-v="${name}"]`), text);

function paintNodeRow(tr, cells) {
  for (const name of ['inFlight', 'window', 'p95', 'errors', 'refused']) {
    put(tr, name, cells[name]);
    put(tr, `${name}-sub`, cells[`${name}Sub`]);
  }
  put(tr, 'node-sub', cells.nodeSub);
  const chip = tr.querySelector('[data-v="chip"]');
  setAttr(chip, 'status', cells.chipStatus);
  setAttr(chip, 'label', cells.label);
}

function breachRowHtml(breach, { nodeName, cells }) {
  const open = breach.resolvedAtMs == null;
  return `<td>${escapeHtml(cells.kind)}</td><td>${escapeHtml(nodeName)}</td><td>${escapeHtml(cells.threshold)}</td>`
    + `<td><code>${escapeHtml(cells.peak)}</code></td>`
    + `<td><tf-chip status="${open ? 'err' : 'ok'}" label="${escapeAttr(T(open ? 'breach_open' : 'breach_closed'))}"></tf-chip></td>`
    + `<td>${escapeHtml(dayAndClock(breach.raisedAtMs))}</td><td>${open ? '<span style="color:var(--text-3)">—</span>' : escapeHtml(dayAndClock(breach.resolvedAtMs))}</td>`;
}

/**
 * Keeps `tbody` in step with `items` (`{ key, html }`): a row whose html did not change is left
 * alone, a changed or new one is built, one that is gone is removed. Order follows `items`.
 */
function syncRows(tbody, items) {
  const existing = new Map([...tbody.children].map((tr) => [tr.dataset.key, tr]));
  let cursor = tbody.firstElementChild;
  for (const item of items) {
    let tr = existing.get(item.key);
    if (!tr) {
      tr = document.createElement('tr');
      tr.dataset.key = item.key;
    }
    if (tr.__html !== item.html) {
      tr.innerHTML = item.html;
      tr.__html = item.html;
    }
    existing.delete(item.key);
    if (tr !== cursor) tbody.insertBefore(tr, cursor);
    else cursor = cursor.nextElementSibling;
  }
  for (const stale of existing.values()) stale.remove();
}

/**
 * Starts the card in `host`. `flow` = `{ id, slaPolicyId, busConsumer }` from the flow list;
 * `onOpenPolicies()` opens Przepływy → Polityki SLA. Returns `{ refresh, ready, dispose }`.
 * Each node is asked on its own deadline (`nodeDeadlineMs`): a node that is slow shows as
 * unreachable and never holds the others back.
 */
export function startSlaState({ host, flow, onOpenPolicies = () => {}, deps = {}, intervalMs = POLL_MS, nodeDeadlineMs = NODE_DEADLINE_MS }) {
  const api = { ...defaultDeps, ...deps };
  const bus = Boolean(flow.busConsumer);
  let disposed = false;
  let busy = false;
  let layout = null;
  let breachLimit = BREACH_PAGE;
  let context = null;
  let breachState = { breaches: [], total: 0 };
  const readings = new Map();

  host.className = 'card';
  host.style.marginTop = 'var(--space-4)';
  host.addEventListener('click', (event) => {
    const target = event.target.closest?.('[data-go="policies"], [data-v="breach-more"]');
    if (!target) return;
    event.preventDefault();
    if (target.dataset.go === 'policies') {
      onOpenPolicies();
      return;
    }
    breachLimit += BREACH_PAGE;
    loadBreaches().then(() => { if (!disposed && context) paintBreaches(); });
  });

  const ask = (kind, payload, node) => (node.isLocal || !node.nodeId
    ? api.call(kind, payload)
    : api.call(kind, payload, { targetNodeId: node.nodeId, timeoutMs: nodeDeadlineMs }));

  async function loadNodes() {
    const nodes = (await bounded(api.nodes(), nodeDeadlineMs)) ?? [];
    const list = nodes.length ? nodes : [{ nodeId: '', hostname: T('node_this'), isLocal: true }];
    return list.map((node) => ({ nodeId: node.nodeId, name: node.hostname || shortNode(node.nodeId), isLocal: Boolean(node.isLocal) }));
  }

  async function readNode(node) {
    const status = await bounded(
      ask('slaStatusRequest', { flowId: flow.id }, node).then((response) => response.statuses?.[0] ?? null),
      nodeDeadlineMs,
    );
    let consumer = null;
    if (bus && status) {
      consumer = await bounded(
        ask('flowListRequest', {}, node).then((response) => response.flows?.find((item) => item.id === flow.id)?.busConsumer ?? null),
        nodeDeadlineMs,
      );
    }
    return { node, status, consumer };
  }

  async function loadBreaches() {
    const response = await bounded(api.call('slaBreachListRequest', { flowId: flow.id, offset: 0, limit: breachLimit }), nodeDeadlineMs);
    if (response && !disposed) breachState = { breaches: response.breaches ?? [], total: Number(response.total) || 0 };
  }

  const shownNodes = () => {
    const all = context.nodes.map((node) => readings.get(node.nodeId) ?? { node, status: null, consumer: null });
    if (!bus) return all;
    return [leadingReading(all)];
  };

  function draw() {
    const shown = shownNodes().map((r) => r.node);
    const signature = shown.map((node) => node.nodeId).join('|');
    if (layout && layout.signature === signature && layout.policyName === context.policy.spec.name) return;
    host.innerHTML = cardHtml({
      bus,
      policyName: context.policy.spec.name,
      windowTitle: T('head.window', { window: windowName(context.policy.spec.callWindowSecs ?? 60).toLowerCase() }),
      nodes: shown,
    });
    layout = { signature, policyName: context.policy.spec.name };
  }

  function paintNodes() {
    const nowMs = api.now();
    const spec = context.policy.spec;
    const shown = shownNodes();
    const states = [];
    for (const reading of shown) {
      if (!readings.has(reading.node.nodeId)) continue;
      const cells = nodeCells({ status: reading.status, spec, consumer: reading.consumer, nowMs, bus });
      if (cells.state !== 'unreachable') states.push(cells.state);
      const tr = rowOf(host, reading.node.nodeId);
      if (tr) paintNodeRow(tr, cells);
    }
    const overall = overallState(states);
    const chip = host.querySelector('[data-v="overall"]');
    chip.hidden = overall === null;
    if (overall) {
      setAttr(chip, 'status', stateChip(overall));
      setAttr(chip, 'label', stateLabel(overall));
    }
    paintFooters(shown, spec, nowMs);
  }

  function paintFooters(shown, spec, nowMs) {
    let left = '';
    let right = '';
    if (bus) {
      const first = shown[0];
      const throttledUntil = first ? nodeCells({ status: first.status, spec, consumer: first.consumer, nowMs, bus }).throttledUntil : null;
      if (throttledUntil != null) left = T('foot.resumes', { time: clockText(throttledUntil, true) });
      if (spec.maxConcurrency != null) right = T('foot.parallel', { count: spec.maxConcurrency });
    } else {
      if (shown.length > 1 && (spec.maxConcurrency != null || spec.callLimit != null)) {
        const bits = [];
        if (spec.maxConcurrency != null) bits.push(T('foot.cluster_threads', { count: spec.maxConcurrency * shown.length }));
        if (spec.callLimit != null) bits.push(T('foot.cluster_calls', { count: spec.callLimit * shown.length, window: windowPer(spec.callWindowSecs) }));
        left = T('foot.cluster', { nodes: shown.length, n: shown.length, limits: bits.join(` ${T('foot.and')} `) });
      }
      if (spec.maxConcurrency != null && effectiveQueue(spec) > 0) right = T('foot.queue', { wait: durationText(spec.queueTimeoutMs ?? DEFAULT_QUEUE_TIMEOUT_MS) });
    }
    setText(host.querySelector('[data-v="foot-cluster"]'), left);
    setText(host.querySelector('[data-v="foot-queue"]'), right);
  }

  /** Open breaches counted from what every node reports, not from the page of history below. */
  function openCount() {
    let open = 0;
    for (const reading of readings.values()) open += reading.status?.openBreaches?.length ?? 0;
    return open;
  }

  function paintBreaches() {
    const { breaches, total } = breachState;
    const names = new Map(context.nodes.map((n) => [n.nodeId, n.name]));
    const open = openCount();
    const count = host.querySelector('[data-v="open-count"]');
    setHidden(count, open === 0);
    setAttr(count, 'label', T('breaches_open', { count: open, n: open }));
    host.querySelector('[data-v="breach-table"]').hidden = breaches.length === 0;
    setHidden(host.querySelector('[data-v="breach-empty"]'), breaches.length > 0);
    setHidden(host.querySelector('[data-v="breach-more-row"]'), breaches.length === 0 || breaches.length >= total);
    setText(host.querySelector('[data-v="breach-shown"]'), T('breaches_shown', { shown: breaches.length, total }));
    syncRows(host.querySelector('[data-v="breaches"]'), breaches.map((breach) => ({
      key: breach.id,
      html: breachRowHtml(breach, { nodeName: names.get(breach.nodeId) ?? shortNode(breach.nodeId), cells: breachCells(breach, context.policy.spec) }),
    })));
  }

  let nodes = null;
  async function refresh() {
    if (disposed || busy) return;
    busy = true;
    try {
      nodes ??= await loadNodes();
      const policyResponse = await bounded(api.call('slaPolicyGetRequest', { policyId: flow.slaPolicyId }), nodeDeadlineMs);
      if (disposed) return;
      if (!policyResponse?.policy) throw new Error('policy');
      context = { policy: policyResponse.policy, nodes };
      const asking = nodes.map((node) => readNode(node).then((reading) => {
        if (disposed) return;
        readings.set(node.nodeId, reading);
        if (!bus) {
          draw();
          paintNodes();
          paintBreaches();
        }
      }));
      const history = loadBreaches();
      if (bus) await Promise.all(asking);
      if (disposed) return;
      draw();
      paintNodes();
      await Promise.all([...asking, history]);
      if (disposed) return;
      draw();
      paintNodes();
      paintBreaches();
    } catch (err) {
      if (!disposed && !layout) host.innerHTML = `<div class="empty-state"><div class="empty-state-text">${escapeHtml(T('load_error'))}</div></div>`;
    } finally {
      busy = false;
    }
  }

  const first = refresh();
  const timer = setInterval(() => { if (!document.hidden) refresh(); }, intervalMs);
  return {
    refresh,
    ready: first,
    dispose() {
      disposed = true;
      clearInterval(timer);
      host.replaceChildren();
    },
  };
}
