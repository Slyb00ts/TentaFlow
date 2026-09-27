// ===== File: modules/tentabus/replication.js — the Kopie i nody tab (T10): nodes, partitions needing attention, partitions per topic, leadership changes =====
//
// Everything comes from the replica lists the shell already polls every 10 s
// (one for the whole instance: nodes and leadership changes; one per topic:
// its partitions), counted over the reader's topics only. There is no button
// to change the copies: a topic's number of copies is chosen by the instance
// when the topic is created. What can be done here is moving a partition's
// leadership to a node with an in-sync copy (partitions.js), from a row that
// needs attention; every other partition is reached through its topic.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { patchHtml, patchKeyedList, setAttr, setText, setRowsIfChanged } from '/js/lib/dom-patch.js';
import { T, fmtCount, fmtWhen } from '/js/modules/tentabus/format.js';
import { nodeRows, userTopics } from '/js/modules/tentabus/model.js';
import { attentionRows, copyChipHtml, behindText, transferBlocker, unavailableText } from '/js/modules/tentabus/partitions.js';
import '/js/components/tf-table.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-alert.js';
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
  const labels = new Map((nodes || []).map((n) => [n.nodeId, n.label || n.nodeId]));
  const byTopic = new Map((perTopic || []).map((t) => [t.topic, t.partitions || []]));
  return userTopics(topics).map((t) => {
    const parts = byTopic.get(t.name) || [];
    const leads = new Map((nodes || []).map((n) => [n.nodeId, 0]));
    for (const p of parts) if (p.leaderNodeId) leads.set(p.leaderNodeId, (leads.get(p.leaderNodeId) || 0) + 1);
    return {
      name: t.name,
      partitions: Number(t.partitions) || parts.length,
      leads: [...leads.entries()].map(([id, n]) => ({ label: labels.get(id) || id, n })),
      attention: (attention || []).filter((a) => a.topic === t.name).length,
    };
  }).sort((a, b) => a.name.localeCompare(b.name));
}

/** Leadership changes of the reader's topics, newest first. */
export function readerFailovers(failovers, topics) {
  const names = new Set(userTopics(topics).map((t) => t.name));
  return (failovers || []).filter((f) => names.has(f.topic)).sort((a, b) => (Number(b.atMs) || 0) - (Number(a.atMs) || 0));
}

/** The reason of a leadership change, in words. */
export function failoverReason(f) {
  if (f.reason === 'manual_transfer') {
    return f.actorLabel ? T('replication.reason_manual_by', { name: f.actorLabel }) : T('replication.reason_manual');
  }
  if (f.reason === 'lease_expired') return T('replication.reason_lease_expired', { node: f.fromNode || '—' });
  return T('replication.reason_other');
}

function skeleton() {
  return `
    <div data-role="notice"></div>
    <div class="section-card">
      <div class="section-card-head"><div class="title">${sprite('cpu')} ${escapeHtml(T('replication.nodes_title'))} <span data-role="nodes-count"></span></div></div>
      <div class="section-sub" data-role="nodes-sub"></div>
      <div class="tb-node-grid" data-role="nodes"></div>
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
      <div class="muted" data-role="changes-none" hidden>${escapeHtml(T('replication.changes_none'))}</div>
    </div>`;
}

function countChip(host, n, status = 'neutral') {
  patchHtml(host, `<tf-chip size="sm" variant="outline" status="${status}"></tf-chip>`);
  setAttr(host.firstElementChild, 'label', fmtCount(n));
}

function nodeCardSkeleton(id, label) {
  return `
    <div class="tb-node-card" data-node="${escapeAttr(id)}">
      <div class="tb-node-card-head">${sprite('cpu')}<span class="mono tb-node-name">${escapeHtml(label)}</span><tf-chip size="sm" variant="outline" dot data-role="state"></tf-chip></div>
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
 * failovers, topics, canAdmin, notice, justMoved, nowMs }` (`nodes` is
 * `null` until the replica list answered). `ctx.go(action)`: `{ kind:
 * 'topic', topic }`, `{ kind: 'transfer', topic, partition }`.
 */
export function drawReplication(body, ctx) {
  const view = ctx.view();
  const mode = view.nodes == null ? 'loading' : 'full';
  if (body.__tbMode !== mode) {
    body.__tbMode = mode;
    patchHtml(body, mode === 'loading'
      ? `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('shell.loading'))}</div>`
      : skeleton());
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
    }
  }
  if (mode === 'full') paint(body, view, ctx);
}

function paint(body, view, ctx) {
  const { nodes, replicaTopics, notice } = view;
  const moved = view.justMoved || new Set();
  patchHtml(body.querySelector('[data-role="notice"]'), notice
    ? `<tf-alert tone="success" title="${escapeAttr(notice.title)}" message="${escapeAttr(notice.text || '')}"></tf-alert>`
    : '');

  setText(body.querySelector('[data-role="topics-sub"]'), T(view.canAdmin ? 'replication.topics_sub' : 'replication.topics_sub_read'));

  // Nodes.
  const counts = nodeRows(nodes, replicaTopics || []) || [];
  countChip(body.querySelector('[data-role="nodes-count"]'), nodes.length);
  setText(body.querySelector('[data-role="nodes-sub"]'), nodes.length === 1 ? T('replication.nodes_sub_single') : T('replication.nodes_sub'));
  const nodesHost = body.querySelector('[data-role="nodes"]');
  patchKeyedList(nodesHost, nodes.map((n) => ({ key: n.nodeId, html: nodeCardSkeleton(n.nodeId, n.label || n.nodeId) })));
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
  });

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
      leader: a.leader ? `<span class="tf-table__cell--mono">${escapeHtml(a.leader)}</span>` : '—',
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
    setText(row.querySelector('[data-role="sub"]'), [
      T('overview.topic_partitions', { count: fmtCount(r.partitions), n: r.partitions }),
      leads ? T('replication.topic_leads', { list: leads }) : '',
    ].filter(Boolean).join(' · '));
  });

  // Leadership changes.
  const changes = readerFailovers(view.failovers, view.topics);
  countChip(body.querySelector('[data-role="changes-count"]'), changes.length);
  body.querySelector('[data-role="changes-none"]').hidden = changes.length > 0;
  const changesHost = body.querySelector('[data-role="changes"]');
  const labels = new Map((nodes || []).map((n) => [n.nodeId, n.label || n.nodeId]));
  const keyOf = (f) => `${f.topic}|${f.partition}|${f.atMs}|${f.toEpoch}`;
  patchKeyedList(changesHost, changes.map((f) => ({ key: keyOf(f), html: changeRowSkeleton(keyOf(f)) })));
  changes.forEach((f, i) => {
    const row = changesHost.children[i];
    if (!row) return;
    setText(row.querySelector('[data-role="name"]'), T('replication.change_name', {
      topic: f.topic,
      partition: fmtCount(f.partition),
      from: labels.get(f.fromNode) || f.fromNode || '—',
      to: labels.get(f.toNode) || f.toNode,
    }));
    setText(row.querySelector('[data-role="when"]'), fmtWhen(f.atMs, view.nowMs));
    const secs = (Number(f.durationMs) || 0) / 1000;
    setText(row.querySelector('[data-role="took"]'), T('replication.change_took', {
      secs: new Intl.NumberFormat(I18n.getLanguage(), { maximumFractionDigits: 1 }).format(secs),
    }));
    setText(row.querySelector('[data-role="why"]'), failoverReason({ ...f, fromNode: labels.get(f.fromNode) || f.fromNode }));
  });
}
