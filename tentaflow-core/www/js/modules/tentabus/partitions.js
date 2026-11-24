// ===== File: modules/tentabus/partitions.js — partitions and their copies: the rows, and "Przenieś prowadzenie" =====
//
// Shared by a topic's "Partycje i kopie" section and the "Kopie i nody" tab.
// A partition has one leading node, which takes the writes, and copies on the
// other nodes of its replica set: a copy "in sync" (in the ISR) holds every
// message, a copy "behind" is still catching up, and one out of the ISR that
// the leader does not report as catching up has lost contact.
//
// Moving the leadership (`LeaderTransferRequest`) moves the WHOLE topic with
// its unprocessed messages (TB-04 K5): a topic led by two nodes would be read
// by neither, so the partition the request names only says where it started.
// The current leader runs the move, proposes a new placement with the next
// leader epoch for every partition and waits until a majority of its replicas
// has taken it. Only a node whose copy is in sync and caught up may take over,
// and the transfer is written to the audit log. The window offers exactly
// those nodes and says why every other one cannot; a node viewing a topic it
// does not lead cannot see the leader's lag, so the shell asks the leader for
// its own replica list and builds the choices from that.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, fmtCount, fmtBytes, fmtLagSize, fmtLagSeconds } from '/js/modules/tentabus/format.js';
import { isOlderNode } from '/js/modules/tentabus/leader-balance.js';
import { nodeNamer, nodeTitler } from '/js/modules/tentabus/node-label.js';
import '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-choice-card.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

/// Longest a transfer waits for the majority (`TRANSFER_MAJORITY_TIMEOUT`).
export const TRANSFER_WAIT_SECS = 5;

const labelOf = nodeNamer;

/**
 * The state of each copy of one partition (replica list answer):
 * `leader`, `ok` (in sync), `lag` (catching up, with how far), `out` (not in
 * sync and not reported as catching up — this node does not lead the
 * partition, or the copy lost contact). The row is the leader's own whenever
 * this node does not lead the partition (`overlayLeaderRows`): a follower's
 * view of the in-sync set trails the leader's.
 */
export function copyStates(p, nodes) {
  const label = labelOf(nodes);
  const idTitle = nodeTitler(nodes);
  const lagging = new Map((p?.lagging || []).map((l) => [l.nodeId, l]));
  // A pin only means something where there is a choice of nodes.
  const preferredId = (p?.replicas || []).length > 1 ? p?.preferredLeaderNodeId || null : null;
  return (p?.replicas || []).map((id) => {
    let state = 'out';
    if (id === p.leaderNodeId) state = 'leader';
    else if (lagging.has(id)) state = 'lag';
    else if ((p.isr || []).includes(id)) state = 'ok';
    const lag = lagging.get(id);
    return { nodeId: id, label: label(id), idTitle: idTitle(id), state, preferred: id === preferredId, lagBytes: Number(lag?.lagBytes) || 0, lagMs: Number(lag?.lagMs) || 0, reason: lag?.reason || '' };
  });
}

/**
 * Every node a partition's leadership could go to, with the reason it can
 * or cannot: `leader` (leads now), `ok` (in-sync copy on a node that
 * answers), `lag` (copy behind), `out` (copy not in sync), `down` (node does
 * not answer), `old` (node of an older version: it takes no forwarded
 * requests, so it cannot lead). Only `ok` may be chosen. `byLeader` marks
 * a verdict made from a partition row that is not this node's to judge (the
 * page is open on a node that does not lead it): `p` is then the leader's own
 * row, and the note names the leader.
 */
export function transferChoices(p, nodes) {
  const reach = new Map((nodes || []).map((n) => [n.nodeId, n.reachable !== false]));
  const older = new Map((nodes || []).map((n) => [n.nodeId, isOlderNode(n)]));
  const local = (nodes || []).find((n) => n.isLocal)?.nodeId || null;
  const label = labelOf(nodes);
  const byLeader = Boolean(p?.leaderNodeId) && local != null && p.leaderNodeId !== local;
  return copyStates(p, nodes).map((c) => {
    let state = c.state;
    if (state === 'ok' && reach.get(c.nodeId) === false) state = 'down';
    else if (state === 'ok' && older.get(c.nodeId)) state = 'old';
    return { ...c, state, byLeader, leaderLabel: label(p?.leaderNodeId) };
  });
}

/** The refusals of a plan that concern the whole topic and so stop a move of it to any node. */
const TOPIC_WIDE_REASONS = new Set(['consumer_group_paused', 'pause_state_unreadable']);

/**
 * The reason a plan (`LeaderBalancePlanResponse.blockers`) gives for not
 * moving `topic` to any node, or `null`. Only a paused consumer (or a pause
 * that cannot be read) blocks the topic as a whole: the other blockers
 * (no leader now, no improvement, a node that is behind) say nothing about a
 * move an administrator chooses by hand.
 */
export function topicWideBlocker(blockers, topic) {
  return (blockers || []).find((b) => b.topic === topic && TOPIC_WIDE_REASONS.has(b.reason)) || null;
}

/** Lets one call at a time through: `gate(fn)` answers `undefined` without calling `fn` while an earlier call has not finished. */
export function exclusive() {
  let busy = false;
  return async (fn) => {
    if (busy) return undefined;
    busy = true;
    try {
      return await fn();
    } finally {
      busy = false;
    }
  };
}

/**
 * What the window "Przenieś prowadzenie" is built from. On the node that leads the partition its own row is
 * the truth. On any other node the leader is asked for its row (`readLeader()`: the leader's own replica list,
 * with the lag of every copy in bytes) and for a plan of the topic (`readPlan()`: its blockers), and the
 * answers replace what this node can see. Either read failing (or answering nothing) leaves the window to open
 * with what this node has: the leader still refuses a copy that is behind when the move is sent.
 * `describeBlocker(blocker)` words the reason a topic as a whole cannot move; only a paused consumer is one.
 */
export async function transferContext({ topic, partition, replica, nodes, readLeader, readPlan, describeBlocker }) {
  const local = (nodes || []).find((n) => n.isLocal);
  if (!local || !replica?.leaderNodeId || local.nodeId === replica.leaderNodeId) return { row: replica, blockedNote: '' };
  const [leaders, plan] = await Promise.all([
    Promise.resolve().then(readLeader).catch(() => null),
    Promise.resolve().then(readPlan).catch(() => null),
  ]);
  const leaderRow = (leaders?.partitions || []).find((p) => Number(p.partition) === Number(partition));
  const row = leaderRow ? { ...leaderRow, preferredLeaderNodeId: leaderRow.preferredLeaderNodeId ?? replica.preferredLeaderNodeId } : replica;
  const blocker = topicWideBlocker(plan?.blockers, topic);
  return { row, blockedNote: blocker ? describeBlocker(blocker) : '' };
}

/**
 * Whether this node's own row of a partition another node leads leaves a
 * doubt only the leader can settle: a copy that is behind or out of sync, or
 * no usable leader. A row that shows every copy in sync is taken as it is
 * (the leader is asked when a move is sent), so a healthy topic costs no
 * call to another node.
 */
export function needsLeaderVerdict(p, nodes) {
  return Boolean(p?.unavailableReason) || copyStates(p, nodes).some((c) => c.state === 'lag' || c.state === 'out');
}

/**
 * Asks the leaders about the partitions this node does not lead.
 * `lists` = `[{ key, partitions }]` (a topic or its `__dlq.` topic and this
 * node's rows of it); `read(key, leaderId)` resolves the leader's own replica
 * list (`{ partitions }`) and may throw. Resolves `Map` key -> `Map` leaderId ->
 * that leader's rows of the key, or `null` when it did not answer. A list led
 * wholly by this node, or whose partitions need no verdict (`needsLeaderVerdict`), is not asked about. One call per
 * list and leader, never more.
 */
export async function askLeaders({ lists, localId, nodes, read }) {
  const asks = [];
  for (const { key, partitions } of lists) {
    const leaders = new Set((partitions || []).filter((p) => needsLeaderVerdict(p, nodes)).map((p) => p.leaderNodeId).filter((id) => id && id !== localId));
    for (const leaderId of leaders) {
      asks.push(Promise.resolve().then(() => read(key, leaderId)).then(
        (answer) => ({ key, leaderId, rows: Array.isArray(answer?.partitions) ? answer.partitions : null }),
        () => ({ key, leaderId, rows: null }),
      ));
    }
  }
  const answers = new Map();
  for (const { key, leaderId, rows } of await Promise.all(asks)) {
    if (!answers.has(key)) answers.set(key, new Map());
    answers.get(key).set(leaderId, rows);
  }
  return answers;
}

/**
 * The partitions of one topic as the leaders see them. A partition this node
 * does not lead is replaced by its leader's own row (the pin falls back to
 * this node's, as the leader's list may omit it). With no answer yet from
 * that leader the row is kept and marked `awaitsLeader`; with an answer that
 * failed, or that does not hold the partition, the row stays as this node
 * has it. `answers` = one entry of `askLeaders`'s result (leaderId -> rows).
 */
export function overlayLeaderRows(partitions, answers, localId, nodes) {
  return (partitions || []).map((p) => {
    if (!p.leaderNodeId || p.leaderNodeId === localId || !needsLeaderVerdict(p, nodes)) return p;
    if (!answers?.has(p.leaderNodeId)) return { ...p, awaitsLeader: true };
    const row = (answers.get(p.leaderNodeId) || []).find((r) => Number(r.partition) === Number(p.partition) && r.leaderNodeId === p.leaderNodeId);
    return row ? { ...row, preferredLeaderNodeId: row.preferredLeaderNodeId ?? p.preferredLeaderNodeId } : p;
  });
}

/**
 * Why "Przenieś prowadzenie" cannot run for a partition, or `null` when it can.
 * A partition another node leads is judged from the leader's own row: until
 * that answer is in (`awaitsLeader`, see `overlayLeaderRows`) the reason is
 * the neutral "sprawdzam", never a verdict from this node's trailing view.
 */
export function transferBlocker(p, nodes, justMoved = false) {
  if (justMoved) return T('partitions.blocked_just_moved');
  if (!p) return T('partitions.blocked_unknown');
  if (!p.leaderNodeId) return T('partitions.blocked_no_leader');
  if (p.awaitsLeader) return T('partitions.blocked_checking');
  const choices = transferChoices(p, nodes);
  if (choices.some((c) => c.state === 'ok')) return null;
  if ((p.replicas || []).length <= 1) return T('partitions.blocked_single');
  return T('partitions.blocked_no_candidate');
}

/**
 * Rows of a topic's partition table: the topic's own partitions
 * (`TopicDetailResponse.partitions`: size, message numbers) joined with the
 * replica list of the topic (leader and copies, by partition number).
 */
export function partitionRows({ detailPartitions, replicaPartitions, nodes }) {
  const label = labelOf(nodes);
  const byNumber = new Map((replicaPartitions || []).map((p) => [Number(p.partition), p]));
  return (detailPartitions || []).map((d) => {
    const r = byNumber.get(Number(d.partition)) || null;
    const earliest = Number(d.earliestOffset) || 0;
    const next = Number(d.highWatermark) || 0;
    const leaderId = r?.leaderNodeId || d.leaderNodeId || null;
    return {
      partition: Number(d.partition),
      sizeBytes: Number(d.sizeBytes) || 0,
      range: next > earliest ? { from: earliest, to: next - 1 } : null,
      leaderId,
      leader: label(leaderId),
      leaderTitle: nodeTitler(nodes)(leaderId),
      copies: r ? copyStates(r, nodes) : [],
      replica: r,
      unavailable: r?.unavailableReason || null,
    };
  });
}

/**
 * The partitions of every topic that need attention on the "Kopie i nody"
 * tab: a copy behind or out of sync, or a partition without a usable leader.
 * `perTopic` = `[{ topic, partitions }]` (a replica list per topic).
 */
export function attentionRows(perTopic, nodes) {
  const out = [];
  for (const { topic, partitions } of perTopic || []) {
    for (const p of partitions || []) {
      const copies = copyStates(p, nodes);
      const behind = copies.filter((c) => c.state === 'lag' || c.state === 'out');
      if (!behind.length && !p.unavailableReason) continue;
      out.push({
        key: `${topic}:${p.partition}`,
        topic,
        partition: Number(p.partition),
        leaderId: p.leaderNodeId || null,
        leader: labelOf(nodes)(p.leaderNodeId),
        leaderTitle: nodeTitler(nodes)(p.leaderNodeId),
        copies,
        behind,
        replica: p,
        unavailable: p.unavailableReason || null,
      });
    }
  }
  return out.sort((a, b) => a.topic.localeCompare(b.topic) || a.partition - b.partition);
}

/** A node's name in a table cell; a node shown by its short id carries the full id as a tooltip. */
export function nodeNameHtml(label, title = '') {
  return `<span class="tf-table__cell--mono tb-node-ref"${title ? ` title="${escapeAttr(title)}"` : ''}>${escapeHtml(label)}</span>`;
}

const PIN = '<svg class="icon tb-pin" aria-hidden="true"><use href="#i-pin"/></svg>';

/** The words of a copy's state for its chip; the preferred node's copy carries a pin. */
export function copyChipHtml(c) {
  const tone = { leader: 'ok', ok: 'ok', lag: 'warn', out: 'warn', down: 'err', old: 'warn' }[c.state] || 'neutral';
  const full = [T(`partitions.copy_${c.state}`), c.preferred ? T('partitions.copy_preferred') : '', c.idTitle || ''].filter(Boolean).join(' · ');
  return `<span class="tf-chip tf-chip--outline ${tone}" title="${escapeAttr(full)}">${c.preferred ? PIN : ''}${escapeHtml(c.label)}</span>`;
}

/** The legend entry that explains the pin; shown where a pinned copy is. */
export function preferredLegendHtml() {
  return `<span data-role="legend-preferred"><span class="tf-chip tf-chip--outline ok">${PIN}${escapeHtml(T('partitions.legend_preferred_chip'))}</span> ${escapeHtml(T('partitions.legend_preferred'))}</span>`;
}

/** "mac-studio: 87 MB · 4 s" — how far each copy behind trails. */
export function behindText(behind) {
  return behind.map((c) => (c.state === 'lag'
    ? T('partitions.behind_lag', { node: c.label, size: fmtBytes(c.lagBytes), secs: fmtLagSeconds(c.lagMs) })
    : T('partitions.behind_out', { node: c.label }))).join(' · ');
}

/** The reason word of a partition without a usable leader. */
export function unavailableText(reason) {
  const key = String(reason || '').replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase();
  const known = ['no_isr', 'no_assignment', 'epoch_fenced'];
  return T(`partitions.unavailable_${known.includes(key) ? key : 'other'}`);
}

/** The partition-number cell: "od 30 412 118 do 72 118 211" or "brak wiadomości". */
export function rangeText(range) {
  return range
    ? T('partitions.range', { from: fmtCount(range.from), to: fmtCount(range.to) })
    : T('partitions.range_empty');
}

/**
 * Opens "Przenieś prowadzenie" from one partition of `topic`; the whole topic
 * moves, so the window says the topic's name and what goes with it. `choices` =
 * `transferChoices(...)`; `blockedNote` (optional) = a reason that stops the
 * whole topic, shown instead of an offer to move; `transfer(nodeId)` sends the
 * request (may throw: the window stays with `describeError(err)`);
 * `onDone({ nodeId, label })` runs after the window closed.
 */
export function openLeaderTransfer({ topic, partition, choices, blockedNote = '', transfer, describeError, onDone }) {
  const pick = choices.find((c) => c.state === 'ok') || null;
  let selected = pick?.nodeId || '';
  const win = document.createElement('tf-window');
  win.className = 'tb-window tb-transfer-window';
  win.setAttribute('title', T('partitions.transfer_title', { topic, partition: fmtCount(partition) }));
  win.setAttribute('icon', 'branch');
  win.setAttribute('buttons', 'close');
  win.setAttribute('modal', '');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '580');
  win.setAttribute('min-width', '360');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  const noteOf = (c) => {
    if (c.state === 'lag') {
      if (c.byLeader) return c.lagBytes > 0 ? T('partitions.choice_lag_by', { size: fmtLagSize(c.lagBytes), owner: c.leaderLabel }) : T('partitions.choice_lag_leader', { owner: c.leaderLabel });
      return T('partitions.choice_lag', { size: fmtLagSize(c.lagBytes) });
    }
    if (c.state === 'out' && c.byLeader) return T('partitions.choice_out_leader', { owner: c.leaderLabel });
    return T(`partitions.choice_${c.state}`);
  };
  const headingOf = (c) => (c.preferred ? T('partitions.choice_preferred_tag', { label: c.label }) : c.label);
  win.innerHTML = `
    <div slot="body" class="stack">
      <div class="tb-explain-box">${escapeHtml(T('partitions.transfer_explain', { topic }))}</div>
      <tf-choice-group id="tb-transfer-target" value="${escapeAttr(selected)}" columns="1" aria-label="${escapeAttr(T('partitions.transfer_target'))}">
        ${choices.map((c) => `<tf-choice-card value="${escapeAttr(c.nodeId)}" icon="cpu" heading="${escapeAttr(headingOf(c))}" description="${escapeAttr(noteOf(c))}" ${c.state === 'ok' && !blockedNote ? '' : 'disabled'}></tf-choice-card>`).join('')}
      </tf-choice-group>
      <div class="tb-will-happen" data-role="impact" aria-live="polite"></div>
      <div class="tb-window-error" role="alert" data-role="error" hidden>${sprite('alert')}<span></span></div>
    </div>
    <div slot="footer">
      <span class="tb-foot-note">${sprite('file-text')}${escapeHtml(T('partitions.transfer_audit'))}</span>
      <tf-button variant="ghost" data-act="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="branch" data-act="move">${escapeHtml(T('partitions.transfer_button'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  const move = win.querySelector('[data-act="move"]');
  const cancel = win.querySelector('[data-act="cancel"]');
  let busy = false;
  const sync = () => {
    const c = blockedNote ? null : choices.find((x) => x.nodeId === selected && x.state === 'ok');
    win.querySelector('[data-role="impact"]').innerHTML = c
      ? `${sprite('info')}<div><b>${escapeHtml(T('partitions.transfer_will_happen'))}</b> ${escapeHtml(T('partitions.transfer_impact', { topic, node: c.label, secs: fmtCount(TRANSFER_WAIT_SECS) }))}</div>`
      : `${sprite('info')}<div>${escapeHtml(blockedNote || T('partitions.transfer_pick'))}</div>`;
    move.toggleAttribute('disabled', busy || !c);
    cancel.toggleAttribute('disabled', busy);
  };
  win.querySelector('#tb-transfer-target').addEventListener('change', (e) => { selected = e.detail?.value || ''; sync(); });
  sync();
  win.addEventListener('close-request', (e) => { if (busy) e.preventDefault(); });
  win.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-act]');
    if (!btn || btn.hasAttribute('disabled')) return;
    if (btn.dataset.act === 'cancel') { win.close(true); return; }
    const target = blockedNote ? null : choices.find((x) => x.nodeId === selected && x.state === 'ok');
    if (!target) return;
    busy = true;
    sync();
    const errEl = win.querySelector('[data-role="error"]');
    errEl.hidden = true;
    try {
      await transfer(target.nodeId);
    } catch (err) {
      busy = false;
      sync();
      errEl.querySelector('span').textContent = describeError(err);
      errEl.hidden = false;
      return;
    }
    win.close(true);
    onDone?.({ nodeId: target.nodeId, label: target.label });
  });
  return win;
}
