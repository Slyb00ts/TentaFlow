// ===== File: modules/tentabus/rebalance-window.js — "Rozłóż prowadzenie": the window that spreads topic leadership between nodes (TB-04) =====
//
// Three steps in one window, built once and painted in place:
//   - preview: the moves the plan would make (topic, partitions, from → to,
//     why), the topics that cannot move and why, and what each node leads
//     afterwards. Nothing has changed yet;
//   - progress: the same rows say where each move stands. The request answers
//     only when every move ended, so the rows follow the replica lists the
//     shell polls every 10 s (`refresh()`): a topic whose partitions are led
//     by the target has moved, the first one that has not is under way, the
//     rest wait. No request of its own is added;
//   - unknown: the request got no answer (a deadline, a lost connection) while
//     the node may still be moving topics. The rows keep following the polled
//     replica lists and say "sprawdzam" for what has not moved yet;
//   - result: how each move ended, with partial success and readable texts
//     for the errors. A topic moves whole or not at all, so a failed row says
//     that no partition of it changed node (`failed_partial` is the exception
//     and says what is left to gather).
// `bus.plan_changed` (the plan the window showed is no longer the plan) puts
// the window back on a fresh preview; the moves are never replayed from a
// stale plan.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { patchHtml, setAttr, setText, setRowsIfChanged } from '/js/lib/dom-patch.js';
import { T, fmtCount, fmtLagSize } from '/js/modules/tentabus/format.js';
import { balanceModel, leadershipUnits } from '/js/modules/tentabus/leader-balance.js';
import '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-table.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-spinner.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;
const chip = (status, label) => `<span class="tf-chip tf-chip--outline ${status}">${escapeHtml(label)}</span>`;
const sub = (text) => `<div class="tf-table__cell-sub">${escapeHtml(text)}</div>`;

/** How long a move waits for a copy that is catching up (`bus.transfer_target_behind` after it). */
export const CATCH_UP_WAIT_SECS = 2;

/** Failures after which nothing has started, so the window stays on its preview. */
const NOTHING_STARTED = /\b(?:bus\.)?(rebalance_busy|permission_denied|invalid_argument|replication_disabled|replication_internal_error)\b/;

/** An error that says nothing of what the node did: no answer in time, or no connection. */
export const isTransportFailure = (err) => err?.code == null && !/\bbus\./.test(String(err?.message || '')) && !NOTHING_STARTED.test(String(err?.message || ''));

const t = (key, params) => T(`rebalance.${key}`, params);
const hasKey = (key) => T(key) !== `tentabus.${key}`;

/** The stable `bus.*` code in an error message, without the prefix. */
export const errorCodeOf = (message) => /\bbus\.([a-z0-9_]+)/.exec(String(message || ''))?.[1] || null;

/** The reason a topic cannot move, in words. `nodeLabel(id)` names a node. */
export function blockerText(blocker, nodeLabel, pausedConsumers = []) {
  const reason = String(blocker.reason || '');
  const key = `blocker_${reason}`;
  const node = blocker.nodeId ? nodeLabel(blocker.nodeId) : t('node_unknown');
  if (reason === 'consumer_group_paused') {
    const names = (pausedConsumers || []).filter((c) => c.topic === blocker.topic || c.topic === `__dlq.${blocker.topic}`).map((c) => c.label);
    if (!names.length) return t('blocker_consumer_group_paused_plain');
    return t(key, { group: new Intl.ListFormat(I18n.getLanguage(), { type: 'conjunction' }).format([...new Set(names)]) });
  }
  return hasKey(`rebalance.${key}`) ? t(key, { node }) : t('blocker_other');
}

/** Why a move is made, with the wait for a copy that is still catching up. */
export function whyText(move, lagging, nodeLabel) {
  const why = t(move.returnsToPreferred ? 'why_return' : 'why_even');
  const parts = [...new Set(lagging.map((l) => l.partition))].sort((a, b) => a - b);
  if (!parts.length) return why;
  return t('why_waits', { why, parts: parts.join(', '), node: nodeLabel(move.toNodeId), secs: fmtCount(CATCH_UP_WAIT_SECS) });
}

/** The words of one move's failure; the code is the `error_code` of its result. */
export function moveErrorText(code, nodeLabel, nodeId, { detail = '', lagBytes = 0 } = {}) {
  const name = String(code || '').replace(/^bus\./, '');
  if (name === 'transfer_target_behind') {
    // The server's detail names the partition: "'<node>' has not caught up with the leader of '<topic>'/<partition>".
    const partition = /'\/(\d+)/.exec(String(detail || ''))?.[1];
    if (partition == null) return t('error_transfer_target_behind_any', { node: nodeLabel(nodeId) });
    const behind = lagBytes > 0 ? t('error_lag_suffix', { size: fmtLagSize(lagBytes) }) : '';
    return t('error_transfer_target_behind', { partition, node: nodeLabel(nodeId), behind });
  }
  const key = `error_${name}`;
  return hasKey(`rebalance.${key}`) ? t(key, { node: nodeLabel(nodeId) }) : t('error_other');
}

/** What each node leads once `moves` are done: `[{ nodeId, label, partitions, topics }]`. */
export function afterBalance({ moves, perNode = [], units, nodes }) {
  const model = balanceModel({ units, nodes });
  const after = new Map(model.rows.map((r) => [r.nodeId, { nodeId: r.nodeId, label: r.label, partitions: r.partitions, topics: new Set(r.topics) }]));
  for (const move of moves) {
    const unit = units.find((u) => u.name === move.topic);
    const target = after.get(move.toNodeId);
    if (!unit || !target) continue;
    for (const node of after.values()) node.topics.delete(unit.name);
    target.topics.add(unit.name);
  }
  // The server's own count of partitions after the plan (a topic with its `__dlq.` partitions) replaces any recount here.
  for (const load of perNode) {
    const node = after.get(load.nodeId);
    if (node && Number.isFinite(Number(load.partitionsAfter))) node.partitions = Number(load.partitionsAfter);
  }
  return [...after.values()].map((n) => ({ ...n, topics: n.topics.size }));
}

/** "Serwer A będzie prowadził 22 partycje (4 topiki), a Serwer B 13 partycji (3 topiki)." */
export function afterSentence(after) {
  const piece = (n, first) => t(first ? 'after_first' : 'after_next', {
    node: n.label,
    partitions: T('replication.balance_partitions', { count: fmtCount(n.partitions), n: n.partitions }),
    topics: T('replication.balance_topics', { count: fmtCount(n.topics), n: n.topics }),
  });
  const pieces = after.map((n, i) => piece(n, i === 0));
  const last = pieces.pop();
  return `${pieces.length ? pieces.join(t('after_sep')) + t('after_sep_last') : ''}${last || ''}${t('after_end')}`;
}

/** How far one move has got according to the replica lists: partitions led by the target, and its copies still catching up. */
export function moveProgress(move, snapshot) {
  const parts = (snapshot.replicaTopics || []).find((x) => x.topic === move.topic)?.partitions || [];
  const wanted = new Set(move.partitions);
  const done = parts.filter((p) => wanted.has(Number(p.partition)) && p.leaderNodeId === move.toNodeId).length;
  const lagging = parts.flatMap((p) => (p.lagging || [])
    .filter((l) => l.nodeId === move.toNodeId)
    .map((l) => ({ partition: Number(p.partition), lagBytes: Number(l.lagBytes) || 0 })));
  return { done, total: move.partitions.length, lagging };
}

/**
 * Opens the window. `loadPlan()` resolves `{ moves, perNode, blockers }` (may
 * throw); `apply(moves)` resolves `results` (may throw); `observe()` =
 * `{ nodes, replicaTopics, topics, leaderReturn, pausedConsumers }` of the shell's latest poll;
 * `describeError(err)` words a failure; `onDone({ results, moved, total })`
 * runs after the results were shown and the window closed.
 * Returns `{ win, refresh }`: call `refresh()` after every poll.
 */
export function openRebalanceWindow({ instanceLabel, loadPlan, apply, observe, describeError, onDone }) {
  const state = { phase: 'loading', plan: null, results: null, notice: null, error: '', busy: false };
  const nodeLabel = (id) => {
    const node = (observe().nodes || []).find((n) => n.nodeId === id);
    return node?.label || id || t('node_unknown');
  };

  const win = document.createElement('tf-window');
  win.className = 'tb-window tb-rebalance-window';
  win.setAttribute('title', t('title', { instance: instanceLabel }));
  win.setAttribute('icon', 'branch');
  win.setAttribute('buttons', 'close');
  win.setAttribute('modal', '');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '760');
  win.setAttribute('min-width', '360');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      <div data-role="top"></div>
      <div class="tb-state" data-role="loading"><tf-spinner size="sm"></tf-spinner>${escapeHtml(t('loading'))}</div>
      <div class="field" data-role="moves-field" hidden>
        <label data-role="moves-label"></label>
        <tf-table data-role="moves">
          <tf-column key="topic" label="${escapeAttr(t('col_topic'))}" renderer="html"></tf-column>
          <tf-column key="parts" label="${escapeAttr(t('col_partitions'))}" align="num"></tf-column>
          <tf-column key="route" label="${escapeAttr(t('col_route'))}" renderer="html"></tf-column>
          <tf-column key="detail" label="${escapeAttr(t('col_why'))}" renderer="html" fill></tf-column>
        </tf-table>
      </div>
      <div class="field" data-role="blockers-field" hidden>
        <label data-role="blockers-label"></label>
        <tf-table data-role="blockers">
          <tf-column key="topic" label="${escapeAttr(t('col_topic'))}" renderer="html"></tf-column>
          <tf-column key="reason" label="${escapeAttr(t('col_reason'))}" renderer="html" fill></tf-column>
        </tf-table>
      </div>
      <div class="tb-will-happen" data-role="impact" aria-live="polite" hidden></div>
      <div class="tb-window-error" role="alert" data-role="error" hidden>${sprite('alert')}<span></span></div>
    </div>
    <div slot="footer">
      <span class="tb-foot-note">${sprite('file-text')}<span data-role="note"></span></span>
      <tf-button variant="ghost" data-act="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="branch" data-act="apply">${escapeHtml(t('apply'))}</tf-button>
      <tf-button variant="primary" data-act="close" hidden>${escapeHtml(t('close'))}</tf-button>
    </div>`;
  document.body.appendChild(win);

  const q = (role) => win.querySelector(`[data-role="${role}"]`);
  const act = (name) => win.querySelector(`[data-act="${name}"]`);
  const movesTable = q('moves');
  const blockersTable = q('blockers');

  const snapshot = () => {
    const view = observe();
    return { nodes: view.nodes || [], replicaTopics: view.replicaTopics || [], topics: view.topics || [], leaderReturn: view.leaderReturn, pausedConsumers: view.pausedConsumers || [] };
  };

  const topAlert = (tone, title, message) => `<tf-alert tone="${escapeAttr(tone)}" title="${escapeAttr(title)}" message="${escapeAttr(message)}"></tf-alert>`;

  /** The cells of the moves table in the current phase. */
  const moveRows = () => {
    const view = snapshot();
    const base = (m) => ({
      topic: `<span class="tf-table__cell--mono"><span class="tf-table__cell-title">${escapeHtml(m.topic)}</span></span>`,
      parts: String(m.partitions.length),
      route: `<span style="white-space:nowrap">${escapeHtml(t('route', { from: nodeLabel(m.fromNodeId), to: nodeLabel(m.toNodeId) }))}</span>`,
      _key: m.topic,
    });
    const moves = state.plan.moves;
    if (state.phase === 'preview') {
      return moves.map((m) => ({ ...base(m), detail: escapeHtml(whyText(m, moveProgress(m, view).lagging, nodeLabel)) }));
    }
    if (state.phase === 'progress') {
      const progress = moves.map((m) => moveProgress(m, view));
      const current = progress.findIndex((p) => p.done < p.total);
      return moves.map((m, i) => {
        const p = progress[i];
        let detail;
        if (p.done >= p.total) detail = chip('ok', t('state_moved_progress', { done: fmtCount(p.done), total: fmtCount(p.total) }));
        else if (i === current && p.lagging.length) {
          const size = fmtLagSize(p.lagging.reduce((s, l) => s + l.lagBytes, 0));
          detail = `${chip('info', t('state_waiting_copy'))}${sub(t('state_waiting_copy_sub', { parts: p.lagging.map((l) => l.partition).join(', '), node: nodeLabel(m.toNodeId), size }))}`;
        } else if (i === current) {
          detail = chip('info', t('state_moving')) + (p.done > 0 ? sub(t('state_moved_progress', { done: fmtCount(p.done), total: fmtCount(p.total) })) : '');
        } else detail = chip('neutral', t('state_queued'));
        return { ...base(m), detail };
      });
    }
    if (state.phase === 'unknown' || state.phase === 'failed') {
      // No answer says how each move ended: only the polled replica lists do.
      return moves.map((m) => {
        const p = moveProgress(m, view);
        if (p.done >= p.total) return { ...base(m), detail: chip('ok', t('state_moved')) };
        return { ...base(m), detail: state.phase === 'unknown' ? chip('neutral', t('state_checking')) : chip('neutral', t('state_failed')) };
      });
    }
    const firstFailed = (state.results || []).find((r) => r.status !== 'moved' && r.status !== 'not_attempted');
    return moves.map((m) => {
      const r = (state.results || []).find((x) => x.topic === m.topic);
      let detail = chip('neutral', t('state_not_attempted'));
      if (!r || r.status === 'not_attempted') {
        detail += firstFailed ? sub(t('state_not_attempted_sub', { topic: firstFailed.topic })) : '';
      } else if (r.status === 'moved') detail = chip('ok', t('state_moved'));
      else {
        const behind = (view.replicaTopics.find((x) => x.topic === m.topic)?.partitions || []);
        const partition = /'\/(\d+)/.exec(String(r.detail || ''))?.[1];
        const lagBytes = Number(behind.find((x) => String(x.partition) === partition)?.lagging?.find((l) => l.nodeId === r.toNodeId)?.lagBytes) || 0;
        const error = moveErrorText(r.errorCode, nodeLabel, r.toNodeId, { detail: r.detail, lagBytes });
        detail = r.status === 'failed_partial'
          ? `${chip('err', t('state_partial'))}${sub(t(view.leaderReturn === false ? 'partial_sub_manual' : 'partial_sub', { error, node: nodeLabel(r.toNodeId) }))}`
          : `${chip('err', t('state_failed'))}${sub(t('failed_sub', { error }))}`;
      }
      return { ...base(m), detail };
    });
  };

  const blockerRows = () => {
    const view = snapshot();
    const units = leadershipUnits({ topics: view.topics, perTopic: view.replicaTopics });
    return state.plan.blockers.map((b) => {
      const unit = units.find((u) => u.name === b.topic);
      let note = '';
      if (unit) {
        note = unit.preferredId
          ? t('blocker_sub', { count: fmtCount(unit.partitions), n: unit.partitions, node: nodeLabel(unit.preferredId) })
          : t('blocker_sub_plain', { count: fmtCount(unit.partitions), n: unit.partitions });
      }
      return {
        topic: `<span class="tf-table__cell--mono"><span class="tf-table__cell-title">${escapeHtml(b.topic)}</span></span>${note ? sub(note) : ''}`,
        reason: escapeHtml(blockerText(b, nodeLabel, view.pausedConsumers)),
        _key: `${b.topic}|${b.reason}|${b.nodeId || ''}`,
      };
    });
  };

  /** The sentence about what each node leads afterwards, and which topics stay. */
  const impactHtml = () => {
    const view = snapshot();
    const units = leadershipUnits({ topics: view.topics, perTopic: view.replicaTopics });
    const parts = [afterSentence(afterBalance({ moves: state.plan.moves, perNode: state.plan.perNode, units, nodes: view.nodes }))];
    const stays = [...new Set(state.plan.blockers.map((b) => b.topic))].map((name) => units.find((u) => u.name === name)).filter(Boolean);
    if (stays.length === 1) parts.push(t('after_stays_one', { topic: stays[0].name, node: nodeLabel(stays[0].leaderId) }));
    else if (stays.length > 1) parts.push(t('after_stays_many', { list: stays.map((u) => u.name).join(', ') }));
    parts.push(t('after_retry'));
    return `${sprite('info')}<div><b>${escapeHtml(t('after_title'))}</b> ${escapeHtml(parts.join(' '))}</div>`;
  };

  const resultAlert = () => {
    const results = state.results || [];
    const total = state.plan.moves.length;
    const moved = results.filter((r) => r.status === 'moved').length;
    if (moved === total) return topAlert('success', t('result_ok_title'), t('result_ok_text', { count: fmtCount(total), n: total }));
    const stayed = results.filter((r) => r.status !== 'moved');
    const names = new Intl.ListFormat(I18n.getLanguage(), { type: 'conjunction' }).format(stayed.map((r) => r.topic));
    const holders = [...new Set(stayed.map((r) => r.fromNodeId))];
    const stay = holders.length === 1 ? t('result_stay_one', { list: names, node: nodeLabel(holders[0]) }) : t('result_stay_many', { list: names });
    const behind = stayed.find((r) => r.errorCode === 'bus.transfer_target_behind');
    const auto = stayed.some((r) => state.plan.moves.find((m) => m.topic === r.topic)?.returnsToPreferred) && snapshot().leaderReturn !== false;
    let retry;
    if (behind) retry = t(auto ? 'result_retry_behind_auto' : 'result_retry_behind', { node: nodeLabel(behind.toNodeId) });
    else retry = t(auto ? 'result_retry_auto' : 'result_retry');
    return topAlert('danger', t('result_title', { done: fmtCount(moved), total: fmtCount(total), n: total }), `${stay} ${retry}`);
  };

  /** Paints the window for the current phase; only what differs is written. */
  const paint = () => {
    const { phase, plan } = state;
    const planned = phase !== 'loading' && plan;
    q('loading').hidden = phase !== 'loading' || Boolean(state.error);
    q('moves-field').hidden = !planned || (phase === 'preview' && plan.moves.length === 0);
    q('blockers-field').hidden = !planned || plan.blockers.length === 0;
    const errorEl = q('error');
    errorEl.hidden = !state.error;
    setText(errorEl.querySelector('span'), state.error);

    let top = '';
    if (state.notice) top = topAlert('warning', state.notice.title, state.notice.text);
    if (planned && phase === 'preview') {
      top += plan.moves.length === 0
        ? topAlert('info', t('nothing_title'), t('nothing_text'))
        : `<div class="tb-explain-box">${escapeHtml(t('explain'))}</div>`;
    } else if (planned && phase === 'progress') {
      const view = snapshot();
      const done = plan.moves.filter((m) => { const p = moveProgress(m, view); return p.done >= p.total; }).length;
      top += topAlert('info', t('progress_title'), t('progress_text', { done: fmtCount(done), total: fmtCount(plan.moves.length), n: plan.moves.length }));
    } else if (planned && phase === 'unknown') {
      const view = snapshot();
      const all = plan.moves.every((m) => { const p = moveProgress(m, view); return p.done >= p.total; });
      top += all
        ? topAlert('success', t('result_ok_title'), t('result_ok_text', { count: fmtCount(plan.moves.length), n: plan.moves.length }))
        : topAlert('warning', t('result_unknown_title'), t('result_unknown_text'));
    } else if (planned && phase === 'result') {
      top += resultAlert();
    } else if (planned && phase === 'failed') {
      top += topAlert('danger', t('apply_failed_title'), t('apply_failed_text', { error: state.failure }));
    } else if (phase === 'preview') {
      top += `<div class="tb-explain-box">${escapeHtml(t('explain'))}</div>`;
    }
    patchHtml(q('top'), top);

    if (planned) {
      const label = q('moves-label');
      setText(label, t('moves', { count: fmtCount(plan.moves.length) }));
      const detailColumn = movesTable.querySelector('tf-column[key="detail"]');
      const wanted = phase === 'preview' ? t('col_why') : t('col_state');
      if (detailColumn.getAttribute('label') !== wanted) {
        setAttr(detailColumn, 'label', wanted);
        movesTable.__tfRowsSig = null;
      }
      setRowsIfChanged(movesTable, moveRows());
      setText(q('blockers-label'), t(phase === 'preview' ? 'skipped_preview' : 'skipped_after', { count: fmtCount(plan.blockers.length) }));
      setRowsIfChanged(blockersTable, blockerRows());
    }
    const impact = q('impact');
    impact.hidden = !(planned && phase === 'preview' && plan.moves.length > 0);
    if (!impact.hidden) patchHtml(impact, impactHtml());

    const finished = phase === 'result' || phase === 'failed' || phase === 'unknown';
    setText(q('note'), t(finished ? 'audit_after' : 'audit_before'));
    act('cancel').hidden = finished;
    act('apply').hidden = finished;
    act('close').hidden = !finished && !(phase === 'loading' && state.error);
    setAttr(act('cancel'), 'disabled', state.busy);
    setAttr(act('apply'), 'disabled', state.busy || phase !== 'preview' || !plan || plan.moves.length === 0);
  };

  const load = async () => {
    state.phase = 'loading';
    state.error = '';
    paint();
    try {
      const plan = await loadPlan();
      state.plan = { moves: plan?.moves || [], perNode: plan?.perNode || [], blockers: plan?.blockers || [] };
      state.phase = 'preview';
    } catch (err) {
      state.error = t('load_error', { error: describeError(err) });
    }
    paint();
  };

  const run = async () => {
    if (state.busy || state.phase !== 'preview' || !state.plan?.moves.length) return;
    state.busy = true;
    state.phase = 'progress';
    state.error = '';
    state.notice = null;
    paint();
    let results = null;
    try {
      results = await apply(state.plan.moves);
    } catch (err) {
      state.busy = false;
      const message = String(err?.message || '');
      if (errorCodeOf(message) === 'plan_changed') {
        state.notice = { title: t('plan_changed_title'), text: t('plan_changed_text') };
        await load();
        return;
      }
      if (NOTHING_STARTED.test(message)) {
        state.phase = 'preview';
        state.error = t('apply_error', { error: describeError(err) });
        paint();
        return;
      }
      if (isTransportFailure(err)) {
        // No answer in time or no connection says nothing of what the node did, and it may still be moving topics.
        state.phase = 'unknown';
      } else {
        state.phase = 'failed';
        state.failure = describeError(err);
      }
      paint();
      onDone?.({ results: null, moved: 0, total: state.plan.moves.length, unknown: true });
      return;
    }
    state.busy = false;
    state.results = results || [];
    state.phase = 'result';
    paint();
    onDone?.({ results: state.results, moved: state.results.filter((r) => r.status === 'moved').length, total: state.plan.moves.length });
  };

  win.addEventListener('close-request', (e) => { if (state.busy) e.preventDefault(); });
  win.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-act]');
    if (!btn || btn.hasAttribute('disabled')) return;
    if (btn.dataset.act === 'cancel' || btn.dataset.act === 'close') win.close(true);
    else if (btn.dataset.act === 'apply') run();
  });

  load();
  return { win, refresh: () => { if (['progress', 'unknown', 'failed'].includes(state.phase) && win.isConnected) paint(); } };
}
