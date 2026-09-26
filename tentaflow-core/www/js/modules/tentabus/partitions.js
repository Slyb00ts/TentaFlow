// ===== File: modules/tentabus/partitions.js — partitions and their copies: the rows, and "Przenieś prowadzenie" =====
//
// Shared by a topic's "Partycje i kopie" section and the "Kopie i nody" tab.
// A partition has one leading node, which takes the writes, and copies on the
// other nodes of its replica set: a copy "in sync" (in the ISR) holds every
// message, a copy "behind" is still catching up, and one out of the ISR that
// the leader does not report as catching up has lost contact.
//
// Moving the leadership (`LeaderTransferRequest`) proposes a new placement
// with the next leader epoch and waits until a majority of the partition's
// replicas has taken it, at most 5 seconds (`replication::manager::
// transfer_leader`); the old leader stops taking writes, so a write in flight
// at that moment can be refused and has to be sent again by its program.
// Only a node whose copy is in sync may take over (`NotAReplica` otherwise),
// and the transfer is written to the audit log. The window offers exactly
// those nodes and says why every other one cannot.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, fmtCount, fmtBytes, fmtLagSeconds } from '/js/modules/tentabus/format.js';
import '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-choice-card.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

/// Longest a transfer waits for the majority (`TRANSFER_MAJORITY_TIMEOUT`).
export const TRANSFER_WAIT_SECS = 5;

const labelOf = (nodes) => {
  const map = new Map((nodes || []).map((n) => [n.nodeId, n.label || n.nodeId]));
  return (id) => (id ? map.get(id) || id : '');
};

/**
 * The state of each copy of one partition (replica list answer):
 * `leader`, `ok` (in sync), `lag` (catching up, with how far), `out` (not in
 * sync and not reported as catching up — this node does not lead the
 * partition, or the copy lost contact).
 */
export function copyStates(p, nodes) {
  const label = labelOf(nodes);
  const lagging = new Map((p?.lagging || []).map((l) => [l.nodeId, l]));
  return (p?.replicas || []).map((id) => {
    let state = 'out';
    if (id === p.leaderNodeId) state = 'leader';
    else if (lagging.has(id)) state = 'lag';
    else if ((p.isr || []).includes(id)) state = 'ok';
    const lag = lagging.get(id);
    return { nodeId: id, label: label(id), state, lagBytes: Number(lag?.lagBytes) || 0, lagMs: Number(lag?.lagMs) || 0, reason: lag?.reason || '' };
  });
}

/**
 * Every node a partition's leadership could go to, with the reason it can
 * or cannot: `leader` (leads now), `ok` (in-sync copy on a node that
 * answers), `lag` (copy behind), `out` (copy not in sync), `down` (node does
 * not answer). Only `ok` may be chosen.
 */
export function transferChoices(p, nodes) {
  const reach = new Map((nodes || []).map((n) => [n.nodeId, n.reachable !== false]));
  return copyStates(p, nodes).map((c) => ({
    ...c,
    state: c.state === 'ok' && reach.get(c.nodeId) === false ? 'down' : c.state,
  }));
}

/** Why "Przenieś prowadzenie" cannot run for a partition, or `null` when it can. */
export function transferBlocker(p, nodes, justMoved = false) {
  if (justMoved) return T('partitions.blocked_just_moved');
  if (!p) return T('partitions.blocked_unknown');
  if (!p.leaderNodeId) return T('partitions.blocked_no_leader');
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
        copies,
        behind,
        replica: p,
        unavailable: p.unavailableReason || null,
      });
    }
  }
  return out.sort((a, b) => a.topic.localeCompare(b.topic) || a.partition - b.partition);
}

/** The words of a copy's state for its chip. */
export function copyChipHtml(c) {
  const tone = { leader: 'ok', ok: 'ok', lag: 'warn', out: 'warn', down: 'err' }[c.state] || 'neutral';
  const title = T(`partitions.copy_${c.state}`);
  return `<span class="tf-chip tf-chip--outline ${tone}" title="${escapeAttr(title)}">${escapeHtml(c.label)}</span>`;
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
 * Opens "Przenieś prowadzenie" for one partition. `choices` =
 * `transferChoices(...)`; `transfer(nodeId)` sends the request (may throw:
 * the window stays with `describeError(err)`); `onDone({ nodeId, label })`
 * runs after the window closed.
 */
export function openLeaderTransfer({ topic, partition, choices, transfer, describeError, onDone }) {
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
    if (c.state === 'lag') return T('partitions.choice_lag', { size: fmtBytes(c.lagBytes) });
    return T(`partitions.choice_${c.state}`);
  };
  win.innerHTML = `
    <div slot="body" class="stack">
      <div class="tb-explain-box">${escapeHtml(T('partitions.transfer_explain', { partition: fmtCount(partition) }))}</div>
      <tf-choice-group id="tb-transfer-target" value="${escapeAttr(selected)}" columns="1" aria-label="${escapeAttr(T('partitions.transfer_target'))}">
        ${choices.map((c) => `<tf-choice-card value="${escapeAttr(c.nodeId)}" icon="cpu" heading="${escapeAttr(c.label)}" description="${escapeAttr(noteOf(c))}" ${c.state === 'ok' ? '' : 'disabled'}></tf-choice-card>`).join('')}
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
    const c = choices.find((x) => x.nodeId === selected && x.state === 'ok');
    win.querySelector('[data-role="impact"]').innerHTML = c
      ? `${sprite('info')}<div><b>${escapeHtml(T('partitions.transfer_will_happen'))}</b> ${escapeHtml(T('partitions.transfer_impact', { partition: fmtCount(partition), node: c.label, secs: fmtCount(TRANSFER_WAIT_SECS) }))}</div>`
      : `${sprite('info')}<div>${escapeHtml(T('partitions.transfer_pick'))}</div>`;
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
    const target = choices.find((x) => x.nodeId === selected && x.state === 'ok');
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
