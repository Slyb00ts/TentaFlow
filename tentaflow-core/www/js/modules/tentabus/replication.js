// ===== File: modules/tentabus/replication.js — the Kopie i nody tab (T10): nodes, partitions needing attention, partitions per topic, leadership changes =====
//
// Everything comes from the replica lists the shell already polls every 10 s
// (one for the whole instance: nodes and leadership changes; one per topic:
// its partitions), counted over the reader's topics only. There is no button
// to change the copies: a topic's number of copies is chosen by the instance
// when the topic is created. What can be done here is moving a topic's
// leadership to a node with an in-sync copy (partitions.js), from a row that
// needs attention (every other partition is reached through its topic), and
// spreading the leadership of all topics between the nodes ("Rozłóż
// prowadzenie", rebalance-window.js). The "Rozkład prowadzenia" card shows how
// much each node leads (leader-balance.js) and holds the switch of the
// automatic return of a topic to its preferred node. Every value is patched
// in place on a poll: the cards, the table and the switch keep their nodes.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { patchHtml, patchKeyedList, setAttr, setText, setRowsIfChanged } from '/js/lib/dom-patch.js';
import { T, fmtCount, fmtSince, fmtWhen } from '/js/modules/tentabus/format.js';
import { nodeRows, userTopics } from '/js/modules/tentabus/model.js';
import { attentionRows, copyChipHtml, behindText, nodeNameHtml, transferBlocker, unavailableText } from '/js/modules/tentabus/partitions.js';
import { nodeNamer, nodeTitler, nodeLabel, nodeTitle } from '/js/modules/tentabus/node-label.js';
import {
  balanceModel, fmtNumber, fmtSigned, isOlderNode, leadershipUnits, pluralNumber, rebalanceAvailability,
} from '/js/modules/tentabus/leader-balance.js';
import '/js/components/tf-table.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-spinner.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

/** "Ostatni sygnał" of a node: this node answers now, others by their last heartbeat. */
export function lastSignal(node) {
  if (node.isLocal) return T('replication.signal_local');
  if (node.reachable === false) return T('replication.signal_down');
  const ms = Number(node.lastHeartbeatMsAgo);
  if (!Number.isFinite(ms)) return '—';
  const secs = ms / 1000;
  const text = new Intl.NumberFormat(I18n.getLanguage(), { maximumFractionDigits: secs < 10 ? 1 : 0 }).format(secs);
  return T('replication.signal_ago', { secs: text });
}

/**
 * One row per topic of "Partycje według topiku": how many partitions it has,
 * how many each node leads, and how many of them need attention.
 */
export function topicLeadRows({ topics, perTopic, nodes, attention }) {
  const name = nodeNamer(nodes);
  const titleOf = nodeTitler(nodes);
  const byTopic = new Map((perTopic || []).map((t) => [t.topic, t.partitions || []]));
  return userTopics(topics).map((t) => {
    const parts = byTopic.get(t.name) || [];
    const leads = new Map((nodes || []).map((n) => [n.nodeId, 0]));
    for (const p of parts) if (p.leaderNodeId) leads.set(p.leaderNodeId, (leads.get(p.leaderNodeId) || 0) + 1);
    return {
      name: t.name,
      partitions: Number(t.partitions) || parts.length,
      leads: [...leads.entries()].map(([id, n]) => ({ label: name(id), title: titleOf(id), n })),
      attention: (attention || []).filter((a) => a.topic === t.name).length,
    };
  }).sort((a, b) => a.name.localeCompare(b.name));
}

/**
 * One history out of the leadership changes every node reports. The audit log
 * is kept per node, so a move started from one node is on that node alone:
 * the lists are merged and a change several nodes know of is counted once
 * (same topic, partition, epoch and moment).
 */
export function mergeFailovers(lists) {
  const merged = new Map();
  for (const list of lists || []) {
    for (const f of list || []) {
      const key = `${f.topic}|${f.partition}|${f.toEpoch}|${f.atMs}`;
      if (!merged.has(key)) merged.set(key, f);
    }
  }
  return [...merged.values()];
}

/**
 * The leadership changes of the whole instance: this node's own (`own`) and
 * those every other node that answers has in its audit log (`read(nodeId)`
 * resolves that node's replica list, `{ failovers }`). A node that does not
 * answer or is not reachable yet breaks nothing, but its entries are then
 * missing: the `previous` merged list (the last poll's) is kept in the result
 * instead of dropping to what is left, and `incomplete` says so.
 * Resolves `{ failovers, incomplete }`.
 */
export async function collectFailovers({ own, nodes, read, previous = [] }) {
  const remote = (nodes || []).filter((n) => !n.isLocal);
  const asked = await Promise.all(remote.map((n) => (n.reachable === false
    ? Promise.resolve(null)
    : Promise.resolve().then(() => read(n.nodeId)).then((r) => r?.failovers || [], () => null))));
  const incomplete = asked.some((list) => list === null);
  const lists = asked.filter(Boolean);
  return { failovers: mergeFailovers([own, ...lists, ...(incomplete ? [previous] : [])]), incomplete };
}

/** Leadership changes of the reader's topics, newest first. */
export function readerFailovers(failovers, topics) {
  const names = new Set(userTopics(topics).map((t) => t.name));
  return (failovers || []).filter((f) => names.has(f.topic)).sort((a, b) => (Number(b.atMs) || 0) - (Number(a.atMs) || 0));
}

/**
 * The reason of a leadership change, in words. `manual_transfer` is an
 * administrator's "Przenieś prowadzenie"; the system's own moves name what
 * they were for: the one-off spread after an upgrade (`post_upgrade_spread`),
 * a topic's automatic return to its preferred node (`preferred_restore`), the
 * regathering of a topic failover had split (`split_gather`), "Rozłóż
 * prowadzenie" (`rebalance`) and the undoing of a move that failed
 * (`rollback`).
 */
export function failoverReason(f) {
  switch (f.reason) {
    case 'manual_transfer':
      return f.actorLabel ? T('replication.reason_manual_by', { name: f.actorLabel }) : T('replication.reason_manual');
    case 'post_upgrade_spread': return T('replication.reason_upgrade');
    case 'preferred_restore': return T('replication.reason_preferred_restore');
    case 'split_gather': return T('replication.reason_split_gather');
    case 'rebalance': return f.actorLabel ? T('replication.reason_rebalance_by', { name: f.actorLabel }) : T('replication.reason_rebalance');
    case 'rollback': return T('replication.reason_rollback');
    case 'lease_expired': return T('replication.reason_lease_expired', { node: f.fromNode || '—' });
    default: return T('replication.reason_other');
  }
}

function skeleton() {
  return `
    <div data-role="notice"></div>
    <div class="section-card">
      <div class="section-card-head"><div class="title">${sprite('cpu')} ${escapeHtml(T('replication.nodes_title'))} <span data-role="nodes-count"></span></div></div>
      <div class="section-sub" data-role="nodes-sub"></div>
      <div class="tb-node-grid" data-role="nodes"></div>
    </div>
    <div class="section-card" data-role="balance-card" hidden>
      <div class="section-card-head">
        <div class="title">${sprite('branch')} ${escapeHtml(T('replication.balance_title'))}</div>
        <div class="actions"><tf-button variant="secondary" size="sm" icon="branch" data-role="rebalance" hidden>${escapeHtml(T('replication.balance_button'))}</tf-button></div>
      </div>
      <div class="section-sub">${escapeHtml(T('replication.balance_sub'))}</div>
      <div class="tb-balance-alert" data-role="balance-alert"></div>
      <tf-table data-role="balance">
        <tf-column key="node" label="${escapeAttr(T('replication.balance_col_node'))}" renderer="html"></tf-column>
        <tf-column key="leads" label="${escapeAttr(T('replication.balance_col_leads'))}" renderer="html"></tf-column>
        <tf-column key="dev" label="${escapeAttr(T('replication.balance_col_dev'))}" renderer="html"></tf-column>
        <tf-column key="away" label="${escapeAttr(T('replication.balance_col_away'))}" renderer="html" fill></tf-column>
      </tf-table>
      <div class="tb-table-footer" data-role="balance-summary"></div>
      <div class="tb-toggle-card">
        <tf-toggle data-role="return" aria-label="${escapeAttr(T('replication.return_title'))}"></tf-toggle>
        <div class="tc-text"><div class="tc-name">${escapeHtml(T('replication.return_title'))}</div><div class="tc-sub" data-role="return-sub">${escapeHtml(T('replication.return_sub'))}</div></div>
      </div>
    </div>
    <div class="section-card">
      <div class="section-card-head"><div class="title">${sprite('alert')} ${escapeHtml(T('replication.attention_title'))} <span data-role="attn-count"></span></div></div>
      <div class="section-sub">${escapeHtml(T('replication.attention_sub'))}</div>
      <tf-table data-role="attention">
        <tf-column key="where" label="${escapeAttr(T('replication.col_where'))}" renderer="html"></tf-column>
        <tf-column key="leader" label="${escapeAttr(T('partitions.col_leader'))}" renderer="html"></tf-column>
        <tf-column key="copies" label="${escapeAttr(T('partitions.col_copies'))}" renderer="html" fill></tf-column>
        <tf-column key="behind" label="${escapeAttr(T('replication.col_behind'))}" renderer="html"></tf-column>
      </tf-table>
      <div class="muted" data-role="attn-none" hidden>${escapeHtml(T('replication.attention_none'))}</div>
      <div class="tb-table-footer" data-role="attn-foot" hidden>${escapeHtml(T('partitions.legend_just_changed'))}</div>
    </div>
    <div class="section-card">
      <div class="section-card-head"><div class="title">${sprite('layers')} ${escapeHtml(T('replication.topics_title'))} <span data-role="topics-count"></span></div></div>
      <div class="section-sub" data-role="topics-sub"></div>
      <div data-role="topics"></div>
      <div class="muted" data-role="topics-none" hidden>${escapeHtml(T('replication.topics_none'))}</div>
    </div>
    <div class="section-card">
      <div class="section-card-head"><div class="title">${sprite('clock')} ${escapeHtml(T('replication.changes_title'))} <span data-role="changes-count"></span></div></div>
      <div class="section-sub">${escapeHtml(T('replication.changes_sub'))}</div>
      <div data-role="changes"></div>
      <div class="muted" data-role="changes-partial" hidden>${escapeHtml(T('replication.changes_partial'))}</div>
      <div class="muted" data-role="changes-none" hidden>${escapeHtml(T('replication.changes_none'))}</div>
    </div>`;
}

function countChip(host, n, status = 'neutral') {
  patchHtml(host, `<tf-chip size="sm" variant="outline" status="${status}"></tf-chip>`);
  setAttr(host.firstElementChild, 'label', fmtCount(n));
}

function nodeCardSkeleton(id, label, title) {
  return `
    <div class="tb-node-card" data-node="${escapeAttr(id)}">
      <div class="tb-node-card-head">${sprite('cpu')}<span class="mono tb-node-name"${title ? ` title="${escapeAttr(title)}"` : ''}>${escapeHtml(label)}</span><tf-chip size="sm" variant="outline" status="warn" data-role="version" hidden></tf-chip><tf-chip size="sm" variant="outline" dot data-role="state"></tf-chip></div>
      <div class="tb-stat-rows">
        <div class="sr"><span class="k">${escapeHtml(T('replication.node_leads'))}</span><span class="v" data-role="leads"></span></div>
        <div class="sr"><span class="k">${escapeHtml(T('replication.node_holds'))}</span><span class="v" data-role="holds"></span></div>
        <div class="sr"><span class="k">${escapeHtml(T('replication.node_in_sync'))}</span><span class="v" data-role="sync"></span></div>
        <div class="sr"><span class="k">${escapeHtml(T('replication.node_signal'))}</span><span class="v" data-role="signal"></span></div>
      </div>
    </div>`;
}

function topicRowSkeleton(name) {
  return `
    <div class="topic-mini" role="link" tabindex="0" data-go="topic" data-topic="${escapeAttr(name)}">
      <div class="tm-ico">${sprite('layers')}</div>
      <div class="tm-main">
        <div class="tm-name"><span class="mono">${escapeHtml(name)}</span><span data-role="chip"></span></div>
        <div class="tm-sub" data-role="sub"></div>
      </div>
      ${sprite('chevron-right')}
    </div>`;
}

function changeRowSkeleton(key) {
  return `
    <div class="job-row" data-change="${escapeAttr(key)}">
      <div class="job-ico">${sprite('zap')}</div>
      <div class="job-main">
        <div class="job-name" data-role="name"></div>
        <div class="job-sub"><span data-role="when"></span><span data-role="took"></span><span data-role="why"></span></div>
      </div>
    </div>`;
}

/**
 * Draws or repaints the tab from `ctx.view()` = `{ nodes, replicaTopics,
 * failovers, topics, canAdmin, notice, justMoved, leaderReturn, nowMs }`
 * (`nodes` is `null` until the replica list answered; `notice` =
 * `{ title, text, tone? }`; `leaderReturn` = the switch of the automatic
 * return: `true`, `false`, or `null` while it has not been read).
 * `ctx.go(action)`: `{ kind: 'topic', topic }`, `{ kind: 'transfer', topic,
 * partition }`, `{ kind: 'rebalance' }`, `{ kind: 'leader-return', enabled }`.
 */
export function drawReplication(body, ctx) {
  const view = ctx.view();
  const mode = view.nodes == null ? 'loading' : view.nodes.length ? 'full' : 'empty';
  if (body.__tbMode !== mode) {
    body.__tbMode = mode;
    patchHtml(body, mode === 'loading'
      ? `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('shell.loading'))}</div>`
      : mode === 'empty' ? `<div class="tb-state" data-role="no-nodes">${escapeHtml(T('replication.no_nodes'))}</div>` : skeleton());
    if (!body.__tbWired) {
      body.__tbWired = true;
      body.addEventListener('click', (e) => {
        const el = e.target.closest('[data-go="topic"]');
        if (el && body.contains(el)) ctx.go({ kind: 'topic', topic: el.dataset.topic });
      });
      body.addEventListener('keydown', (e) => {
        if (e.key !== 'Enter' && e.key !== ' ') return;
        const el = e.target.closest?.('[data-go="topic"]');
        if (!el || !body.contains(el)) return;
        e.preventDefault();
        ctx.go({ kind: 'topic', topic: el.dataset.topic });
      });
      body.addEventListener('click', (e) => {
        const button = e.target.closest('[data-role="rebalance"]');
        if (button && body.contains(button) && !button.hasAttribute('disabled')) ctx.go({ kind: 'rebalance' });
      });
      body.addEventListener('change', (e) => {
        const toggle = e.target.closest?.('[data-role="return"]');
        if (toggle && body.contains(toggle)) ctx.go({ kind: 'leader-return', enabled: Boolean(e.detail?.checked) });
      });
    }
  }
  if (mode === 'full') paint(body, view, ctx);
}

const nodeCell = (label, title = '') => `<span class="tf-table__cell--mono"${title ? ` title="${escapeAttr(title)}"` : ''}><span class="tf-table__cell-title">${escapeHtml(label)}</span></span>`;

/** The topics a node leads, named when there are few of them. */
function topicsText(names) {
  const count = fmtCount(names.length);
  return names.length > 0 && names.length <= 4
    ? T('replication.balance_topics_named', { count, n: names.length, list: names.join(', ') })
    : T('replication.balance_topics', { count, n: names.length });
}

/** How long a topic has to be in sync on its preferred node before it returns there (the server's rule). */
const RETURN_AFTER_MS = 5 * 60_000;

/**
 * What the warning above the table says, or `null` when nothing needs saying.
 * `view.reachableSince` (`Map` nodeId → when this page saw the node answer
 * again) gives "odpowiada od 3 min" and the earliest return; a node this page
 * has always seen answering has no such time, and the sentence leaves it out.
 */
function balanceAlert(model, view, availability, name) {
  if (availability.older.length) {
    return { tone: 'info', title: T('replication.balance_old_info_title', { nodes: availability.older.join(', ') }), message: T('replication.balance_old_info_text') };
  }
  const pluralCount = (n) => ({ count: fmtCount(n), n });
  if (!model.away && !model.split.length) return null;
  // A node that does not answer is the first thing to say: whatever it is preferred for cannot go back before it is there again.
  const down = model.rows.find((r) => !r.reachable && r.away.length > 0);
  if (down) {
    const title = T('replication.balance_down_title', { node: down.label, ...pluralCount(down.away.length) });
    return { tone: 'warning', title, message: view.leaderReturn === false ? T('replication.balance_away_manual') : T('replication.balance_down_text', { node: down.label }) };
  }
  if (!model.away) {
    return { tone: 'warning', title: T('replication.balance_split_title', pluralCount(model.split.length)), message: T('replication.balance_split_text') };
  }
  const idle = model.rows.find((r) => r.reachable && r.partitions === 0 && r.away.length > 0);
  const away = model.rows.flatMap((r) => r.away);
  const since = idle ? view.reachableSince?.get(idle.nodeId) : null;
  const upFor = since != null ? Math.max(0, view.nowMs - since) : null;
  let title;
  if (idle && upFor != null && upFor < 60_000) title = T('replication.balance_idle_just_now_title', { node: idle.label });
  else if (idle && upFor != null) title = T('replication.balance_idle_since_title', { node: idle.label, since: fmtSince(since, view.nowMs) });
  else if (idle) title = T('replication.balance_idle_title', { node: idle.label });
  else if (model.split.length) title = T('replication.balance_split_title', pluralCount(model.split.length));
  else title = T('replication.balance_away_title', { count: fmtCount(away.length), n: away.length });
  if (view.leaderReturn === false) return { tone: 'warning', title, message: T('replication.balance_away_manual') };
  const where = [...new Set(away.map((u) => name(u.preferredId)))].join(', ');
  let sentence = away.length === 1
    ? T('replication.balance_away_auto_one', { node: where })
    : T('replication.balance_away_auto_many', { count: fmtCount(away.length), n: away.length, node: where });
  if (upFor != null && upFor < RETURN_AFTER_MS) {
    sentence += ` — ${T('replication.balance_away_eta', { min: Math.max(1, Math.ceil((RETURN_AFTER_MS - upFor) / 60_000)) })}`;
  }
  const behind = away.filter((u) => (view.replicaTopics || []).find((t) => t.topic === u.name)?.partitions
    ?.some((p) => (p.lagging || []).some((l) => l.nodeId === u.preferredId))).map((u) => u.name);
  if (behind.length) {
    const list = new Intl.ListFormat(I18n.getLanguage(), { type: 'conjunction' }).format(behind);
    sentence += `; ${T('replication.balance_away_catching', { list })}`;
  }
  // The offer is only made while the button that carries it out is there and enabled.
  const canSpreadNow = Boolean(view.canAdmin) && !availability.reason;
  return { tone: 'warning', title, message: canSpreadNow ? `${sentence}. ${T('replication.balance_away_now')}` : `${sentence}.` };
}

/**
 * "Rozkład prowadzenia": the table, the warning, the button of the spread and
 * the switch of the automatic return. The card needs two nodes to mean
 * anything, so a single-node instance does not show it.
 */
function paintBalance(body, view) {
  const { nodes } = view;
  const card = body.querySelector('[data-role="balance-card"]');
  setAttr(card, 'hidden', nodes.length < 2);
  if (nodes.length < 2) return;
  const language = I18n.getLanguage();
  const name = nodeNamer(nodes);
  const units = leadershipUnits({ topics: view.topics, perTopic: view.replicaTopics });
  const model = balanceModel({ units, nodes });
  const availability = rebalanceAvailability(nodes);

  const button = body.querySelector('[data-role="rebalance"]');
  setAttr(button, 'hidden', !view.canAdmin);
  setAttr(button, 'disabled', Boolean(availability.reason));
  setAttr(button, 'title', availability.reason);

  const alert = balanceAlert(model, view, availability, name);
  patchHtml(body.querySelector('[data-role="balance-alert"]'), alert
    ? `<tf-alert tone="${escapeAttr(alert.tone)}" title="${escapeAttr(alert.title)}" message="${escapeAttr(alert.message)}"></tf-alert>`
    : '');

  const uneven = model.verdict === 'uneven';
  const rows = model.rows.map((r) => {
    const awayNames = r.away.map((u) => u.name);
    const leaders = new Set(r.away.map((u) => u.leaderId));
    const hasSplit = r.away.some((u) => u.split);
    const where = (u) => [...u.led.keys()].sort().map(name).join(', ');
    let awayCell = '0';
    if (awayNames.length) {
      const leader = leaders.size === 1 ? name([...leaders][0]) : '';
      const list = hasSplit
        ? r.away.map((u) => (u.split ? T('replication.balance_away_split_item', { name: u.name, nodes: where(u) }) : `${u.name} (${name(u.leaderId)})`)).join(', ')
        : leaders.size === 1
        ? T(awayNames.length === 1 ? 'replication.balance_away_one' : 'replication.balance_away_many', { list: awayNames.join(', '), node: leader })
        : r.away.map((u) => `${u.name} (${name(u.leaderId)})`).join(', ');
      awayCell = `<span class="tf-chip tf-chip--outline warn">${escapeHtml(T('replication.balance_away_chip', { count: fmtCount(awayNames.length), n: awayNames.length }))}</span> <span class="tf-table__cell-sub">${escapeHtml(list)}</span>`;
    }
    const dev = T('replication.balance_dev', { value: fmtSigned(r.deviation, language), n: pluralNumber(Math.abs(r.deviation)) });
    return {
      node: nodeCell(r.label, r.title),
      leads: `<div class="tf-table__cell-title">${escapeHtml(T('replication.balance_partitions', { count: fmtCount(r.partitions), n: r.partitions }))}</div><div class="tf-table__cell-sub">${escapeHtml(topicsText(r.topics))}</div>`,
      dev: uneven ? `<span class="tb-warn-text">${escapeHtml(dev)}</span>` : escapeHtml(dev),
      away: awayCell,
      _key: r.nodeId,
    };
  });
  setRowsIfChanged(body.querySelector('[data-role="balance"]'), rows);

  const summary = [T('replication.balance_mean', { mean: fmtNumber(model.mean, language), n: pluralNumber(model.mean) })];
  if (!availability.older.length && units.length) {
    summary.push(T(`replication.balance_${model.verdict === 'uneven' ? 'uneven' : model.verdict === 'equal' ? 'even_equal' : 'even_less'}`, {
      gap: fmtCount(model.gap), n: model.gap, smallest: fmtCount(model.smallest), m: model.smallest,
    }));
  }
  setText(body.querySelector('[data-role="balance-summary"]'), summary.join(' '));

  // The switch is a platform setting: only the platform administrator changes it, whatever the reader may do on the bus.
  const toggle = body.querySelector('[data-role="return"]');
  setAttr(toggle, 'checked', view.leaderReturn === true);
  setAttr(toggle, 'disabled', !view.platformAdmin || view.leaderReturn == null);
  setText(body.querySelector('[data-role="return-sub"]'), view.platformAdmin ? T('replication.return_sub') : `${T('replication.return_sub')} ${T('replication.return_admin_only')}`);
}

function paint(body, view, ctx) {
  const { nodes, replicaTopics, notice } = view;
  const moved = view.justMoved || new Set();
  patchHtml(body.querySelector('[data-role="notice"]'), notice
    ? `<tf-alert tone="${escapeAttr(notice.tone || 'success')}" title="${escapeAttr(notice.title)}" message="${escapeAttr(notice.text || '')}"></tf-alert>`
    : '');

  setText(body.querySelector('[data-role="topics-sub"]'), T(view.canAdmin ? 'replication.topics_sub' : 'replication.topics_sub_read'));

  // Nodes.
  const counts = nodeRows(nodes, replicaTopics || []) || [];
  countChip(body.querySelector('[data-role="nodes-count"]'), nodes.length);
  setText(body.querySelector('[data-role="nodes-sub"]'), nodes.length === 1 ? T('replication.nodes_sub_single') : T('replication.nodes_sub'));
  const nodesHost = body.querySelector('[data-role="nodes"]');
  patchKeyedList(nodesHost, nodes.map((n) => ({ key: n.nodeId, html: nodeCardSkeleton(n.nodeId, nodeLabel(n), nodeTitle(n)) })));
  nodes.forEach((n, i) => {
    const card = nodesHost.children[i];
    if (!card) return;
    const c = counts.find((r) => r.nodeId === n.nodeId) || { leads: 0, holds: 0, inSync: 0, total: 0 };
    const state = card.querySelector('[data-role="state"]');
    setAttr(state, 'status', n.reachable === false ? 'err' : 'ok');
    setAttr(state, 'label', T(n.reachable === false ? 'overview.node_down' : 'overview.node_up'));
    setText(card.querySelector('[data-role="leads"]'), fmtCount(c.leads));
    setText(card.querySelector('[data-role="holds"]'), fmtCount(c.holds));
    const sync = card.querySelector('[data-role="sync"]');
    setText(sync, T('replication.in_sync_of', { isr: fmtCount(c.inSync), total: fmtCount(c.total) }));
    sync.classList.toggle('is-warn', c.inSync < c.total);
    setText(card.querySelector('[data-role="signal"]'), lastSignal(n));
    const version = card.querySelector('[data-role="version"]');
    const older = nodes.length > 1 && isOlderNode(n);
    setAttr(version, 'hidden', !older);
    setAttr(version, 'label', older ? T('replication.balance_old_chip') : null);
    setAttr(version, 'title', older ? T('replication.balance_old_title', { node: nodeLabel(n) }) : null);
  });

  paintBalance(body, view);

  // Partitions that need attention.
  const attention = attentionRows(replicaTopics || [], nodes);
  countChip(body.querySelector('[data-role="attn-count"]'), attention.length, attention.length ? 'warn' : 'neutral');
  const table = body.querySelector('[data-role="attention"]');
  table.hidden = attention.length === 0;
  body.querySelector('[data-role="attn-none"]').hidden = attention.length > 0;
  const rows = attention.map((a) => {
    const justMoved = moved.has(a.key);
    return {
      where: `<span class="tf-table__cell--mono"><span class="tf-table__cell-title">${escapeHtml(a.topic)}</span></span><div class="tf-table__cell-sub">${escapeHtml(T('partitions.name_lower', { n: fmtCount(a.partition) }))}${justMoved ? ` · ${escapeHtml(T('partitions.just_changed'))}` : ''}</div>`,
      leader: a.leader ? nodeNameHtml(a.leader, a.leaderTitle) : '—',
      copies: a.copies.map(copyChipHtml).join(' '),
      behind: a.unavailable
        ? `<span class="tf-chip tf-chip--outline err">${escapeHtml(unavailableText(a.unavailable))}</span>`
        : `<span class="tb-warn-text">${escapeHtml(behindText(a.behind))}</span>`,
      _key: a.key,
      _topic: a.topic,
      _partition: a.partition,
      _blocker: transferBlocker(a.replica, nodes, justMoved),
    };
  });
  const canAdmin = Boolean(view.canAdmin);
  table.rowActionsKey = (row) => `${row._key}|${row._blocker}|${canAdmin}`;
  if (table.__tbAdmin !== canAdmin) {
    table.__tbAdmin = canAdmin;
    table.rowActions = canAdmin ? (row, idx, currentRow) => {
      const live = () => currentRow?.() ?? row;
      const b = document.createElement('tf-button');
      b.setAttribute('variant', 'secondary');
      b.setAttribute('size', 'sm');
      b.setAttribute('icon', 'branch');
      b.textContent = T('partitions.transfer_button');
      if (row._blocker) {
        b.setAttribute('disabled', '');
        b.title = row._blocker;
      }
      b.addEventListener('click', (e) => {
        e.stopPropagation();
        const r = live();
        if (!r._blocker) ctx.go({ kind: 'transfer', topic: r._topic, partition: r._partition });
      });
      return b;
    } : null;
  }
  setRowsIfChanged(table, rows);
  body.querySelector('[data-role="attn-foot"]').hidden = !attention.some((a) => moved.has(a.key));

  // Partitions per topic.
  const topicRows = topicLeadRows({ topics: view.topics, perTopic: replicaTopics, nodes, attention });
  const partitionsTotal = topicRows.reduce((s, r) => s + r.partitions, 0);
  countChip(body.querySelector('[data-role="topics-count"]'), partitionsTotal);
  const topicsHost = body.querySelector('[data-role="topics"]');
  body.querySelector('[data-role="topics-none"]').hidden = topicRows.length > 0;
  patchKeyedList(topicsHost, topicRows.map((r) => ({ key: r.name, html: topicRowSkeleton(r.name) })));
  topicRows.forEach((r, i) => {
    const row = topicsHost.children[i];
    if (!row) return;
    const chip = row.querySelector('[data-role="chip"]');
    patchHtml(chip, r.attention ? '<tf-chip size="sm" variant="outline" status="warn"></tf-chip>' : '');
    setAttr(chip.firstElementChild, 'label', T('replication.topic_attention', { count: fmtCount(r.attention), n: r.attention }));
    const leads = r.leads.map((l) => `${l.label} ${fmtCount(l.n)}`).join(', ');
    setAttr(row.querySelector('[data-role="sub"]'), 'title', r.leads.filter((l) => l.title).map((l) => `${l.label}: ${l.title}`).join('\n') || null);
    setText(row.querySelector('[data-role="sub"]'), [
      T('overview.topic_partitions', { count: fmtCount(r.partitions), n: r.partitions }),
      leads ? T('replication.topic_leads', { list: leads }) : '',
    ].filter(Boolean).join(' · '));
  });

  // Leadership changes.
  const changes = readerFailovers(view.failovers, view.topics);
  countChip(body.querySelector('[data-role="changes-count"]'), changes.length);
  body.querySelector('[data-role="changes-none"]').hidden = changes.length > 0;
  body.querySelector('[data-role="changes-partial"]').hidden = !view.failoversIncomplete;
  const changesHost = body.querySelector('[data-role="changes"]');
  const name = nodeNamer(nodes);
  const titleOf = nodeTitler(nodes);
  const keyOf = (f) => `${f.topic}|${f.partition}|${f.atMs}|${f.toEpoch}`;
  patchKeyedList(changesHost, changes.map((f) => ({ key: keyOf(f), html: changeRowSkeleton(keyOf(f)) })));
  changes.forEach((f, i) => {
    const row = changesHost.children[i];
    if (!row) return;
    setText(row.querySelector('[data-role="name"]'), T('replication.change_name', {
      topic: f.topic,
      partition: fmtCount(f.partition),
      from: name(f.fromNode) || '—',
      to: name(f.toNode),
    }));
    // A node shown by its short id: the full ids of the two nodes of the move.
    setAttr(row.querySelector('[data-role="name"]'), 'title', [titleOf(f.fromNode), titleOf(f.toNode)].filter(Boolean).join(' → ') || null);
    setText(row.querySelector('[data-role="when"]'), fmtWhen(f.atMs, view.nowMs));
    const secs = (Number(f.durationMs) || 0) / 1000;
    setText(row.querySelector('[data-role="took"]'), secs < 1
      ? T('replication.change_took_short')
      : T('replication.change_took', { secs: new Intl.NumberFormat(I18n.getLanguage(), { maximumFractionDigits: 1 }).format(secs) }));
    setText(row.querySelector('[data-role="why"]'), failoverReason({ ...f, fromNode: name(f.fromNode) }));
  });
}
