// ===== File: modules/tentabus/overview.js — the Przegląd tab (T01): traffic, busiest topics, alerts, live chart, replica state =====
//
// Drawn once per state (loading / error / empty instance / populated) and
// then painted in place on every poll: KPI tiles keep their elements, the
// topic, alert and node rows are keyed lists whose skeletons never change
// for a given key, and only their text slots move. A poll never rebuilds a
// row the pointer may be on.
//
// Every row and button leads somewhere the screen can already show: a topic
// row opens the topic, an alert opens the consumer, the unprocessed messages
// of its topic or the replica tab. The shell passes those moves in as `ctx.go`.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { setAttr, setText, patchHtml, patchKeyedList, paintStatCards, setClass } from '/js/lib/dom-patch.js';
import { T, fmtCount, fmtBytes, fmtSince, fmtLagSeconds, contentTypeLabel } from '/js/modules/tentabus/format.js';
import { computeAlerts, delayedGroups } from '/js/modules/tentabus/alerts.js';
import { overviewKpis, busiestTopics, nodeRows, snapshotTopicCount } from '/js/modules/tentabus/model.js';
import '/js/components/tf-stat-card.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-progress-bar.js';
import '/js/components/tf-stream-chart.js';
import '/js/components/tf-spinner.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

/// Seconds of traffic the live chart keeps on screen.
export const CHART_WINDOW_SECS = 300;

/**
 * Which body the tab needs. Emptiness is read from the snapshot (the topic
 * list may still be loading), and an instance with no topics is empty even
 * when an idle consumer group row survives from an older topic.
 */
export function overviewMode({ stats, error }) {
  if (!stats) return error ? 'error' : 'loading';
  return snapshotTopicCount(stats) === 0 ? 'empty' : 'full';
}

function loadingHtml() {
  return `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('shell.loading'))}</div>`;
}

/** T12: the error card of a tab that has nothing to show yet. */
export function loadErrorHtml({ kind, instanceLabel, titleKey }) {
  const icon = kind === 'denied' ? 'lock' : 'alert';
  const message = T(`shell.error.${kind}`, { name: instanceLabel });
  return `
    <div class="section-card tb-error-card">
      <tf-empty-state badge icon="${icon}" title="${escapeAttr(T(titleKey))}" message="${escapeAttr(message)}">
        ${kind === 'denied' ? '' : `<tf-button variant="primary" icon="refresh" data-go="retry">${escapeHtml(T('shell.retry'))}</tf-button>`}
      </tf-empty-state>
    </div>`;
}

function emptyHtml(instanceLabel) {
  return `
    <div class="tb-kpi" data-role="kpi"></div>
    <div class="section-card">
      <div class="section-card-head"><div class="title">${sprite('info')} ${escapeHtml(T('overview.first_steps'))}</div></div>
      <tf-empty-state badge icon="share" title="${escapeAttr(T('overview.empty_title', { name: instanceLabel }))}" message="${escapeAttr(T('overview.empty_sub'))}">
        <tf-button variant="primary" icon="share" data-go="tab" data-tab="topics">${escapeHtml(T('overview.empty_action'))}</tf-button>
      </tf-empty-state>
    </div>`;
}

function fullHtml() {
  return `
    <div class="tb-kpi" data-role="kpi"></div>
    <div class="tb-dash">
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('share')} ${escapeHtml(T('overview.busiest_title'))}</div>
          <div class="actions"><tf-button variant="ghost" size="sm" data-go="tab" data-tab="topics">${escapeHtml(T('overview.busiest_all'))}</tf-button></div>
        </div>
        <div class="section-sub">${escapeHtml(T('overview.busiest_sub'))}</div>
        <div data-role="topics"></div>
      </div>
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('alert')} ${escapeHtml(T('overview.alerts_title'))} <span data-role="alerts-count"></span></div>
        </div>
        <div data-role="alerts" class="tb-alert-list"></div>
        <div data-role="alerts-none" class="muted" hidden>${escapeHtml(T('overview.alerts_none'))}</div>
      </div>
    </div>
    <div class="tb-dash">
      <div class="section-card">
        <div class="chart-head">
          <div class="ch-title">${sprite('activity')} ${escapeHtml(T('overview.chart_title'))}</div>
          <div class="ch-val"><span class="sw primary"></span><span data-role="chart-val"></span></div>
        </div>
        <div class="tb-chart-box">
          <tf-stream-chart data-role="chart"></tf-stream-chart>
          <div class="live-label"><span class="live-dot"></span>${escapeHtml(T('overview.chart_live'))}</div>
        </div>
        <div class="muted mt-sm">${escapeHtml(T('overview.chart_note'))}</div>
      </div>
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('branch')} ${escapeHtml(T('overview.nodes_title'))}</div>
          <div class="actions"><tf-button variant="ghost" size="sm" data-go="tab" data-tab="replication">${escapeHtml(T('overview.nodes_link'))}</tf-button></div>
        </div>
        <div class="section-sub" data-role="nodes-sub"></div>
        <div data-role="nodes"></div>
      </div>
    </div>`;
}

/**
 * Builds the tab body for its current state (once per state change) and
 * wires the one delegated click handler. `ctx.go(action)` performs the
 * navigation: `{ kind: 'tab', tab }`, `{ kind: 'topic', topic }`,
 * `{ kind: 'group', group, topic }`, `{ kind: 'dlq', topic }`, `{ kind: 'retry' }`.
 */
export function drawOverview(body, ctx) {
  body.classList.add('tb-overview');
  if (!body.__tbWired) {
    body.__tbWired = true;
    const act = (el) => {
      const d = el.dataset;
      if (d.go === 'tab') ctx.go({ kind: 'tab', tab: d.tab });
      else if (d.go === 'topic') ctx.go({ kind: 'topic', topic: d.topic });
      else if (d.go === 'group') ctx.go({ kind: 'group', group: d.group, topic: d.topic });
      else if (d.go === 'dlq') ctx.go({ kind: 'dlq', topic: d.topic });
      else if (d.go === 'retry') ctx.go({ kind: 'retry' });
    };
    body.addEventListener('click', (e) => {
      const el = e.target.closest('[data-go]');
      if (!el || !body.contains(el)) return;
      e.stopPropagation();
      act(el);
    });
    body.addEventListener('keydown', (e) => {
      if (e.key !== 'Enter' && e.key !== ' ') return;
      const row = e.target.closest?.('[role="link"][data-go]');
      if (!row) return;
      e.preventDefault();
      act(row);
    });
  }
  paintOverview(body, ctx);
}

/** Everything a poll can move, written into the body that is already there. */
export function paintOverview(body, ctx) {
  const { stats, error, errorKind, topicList, nodes, replicaTopics, replicaLags, lagSeries, instanceLabel, stale, nowMs } = ctx.view();
  const mode = overviewMode({ stats, error });
  const sig = mode === 'error' ? `error:${errorKind}` : mode;
  if (body.__tbMode !== sig) {
    body.__tbMode = sig;
    if (mode === 'loading') patchHtml(body, loadingHtml());
    else if (mode === 'error') patchHtml(body, loadErrorHtml({ kind: errorKind, instanceLabel, titleKey: 'overview.error_title' }));
    else if (mode === 'empty') patchHtml(body, emptyHtml(instanceLabel));
    else {
      patchHtml(body, fullHtml());
      setupChart(body.querySelector('[data-role="chart"]'), ctx.view().ratePoints);
    }
  }
  setClass(body, 'is-stale', Boolean(stale));
  if (mode === 'loading' || mode === 'error') return;
  const kpi = overviewKpis({ stats, topicList });
  paintKpis(body.querySelector('[data-role="kpi"]'), kpi, stats, mode);
  if (mode === 'empty') return;
  paintTopics(body.querySelector('[data-role="topics"]'), busiestTopics({ stats, topicList }));
  paintAlerts(body, computeAlerts({ groups: stats.groups || [], topics: stats.topics || [], replicaLags: replicaLags || [], lagSeries: lagSeries || new Map(), nowMs }), nowMs);
  paintNodes(body, nodes, replicaTopics);
  setText(body.querySelector('[data-role="chart-val"]'), T('overview.chart_value', { value: fmtCount(kpi.rate) }));
}

/** One sample of the live chart; called on every successful stats poll. */
export function pushOverviewSample(body, rate, atMs) {
  const chart = body?.querySelector('[data-role="chart"]');
  if (chart && typeof chart.push === 'function') chart.push(atMs, { write: Number(rate) || 0 });
}

// Seeded with the samples the screen collected before this tab was drawn:
// the chart shows traffic since the screen opened, not since the tab did.
function setupChart(chart, points = []) {
  if (!chart) return;
  chart.height = 150;
  chart.window = CHART_WINDOW_SECS;
  chart.legend = { position: 'none' };
  chart.tooltip = { valueFormat: (v) => `${fmtCount(Math.round(v))} /s` };
  chart.yAxis = { min: 0, ticks: 4, integer: true, format: (v) => fmtCount(v) };
  // One unit on the whole axis: seconds before now.
  chart.xAxis = { format: (secs) => (secs === 0 ? T('overview.chart_now') : T('fmt.seconds', { count: fmtCount(secs), n: secs })) };
  chart.series = [{ id: 'write', name: T('overview.chart_series'), tone: 'primary', style: 'solid', showInLegend: false, points: points.map((p) => ({ x: p.x, y: p.y })) }];
}

function paintKpis(host, k, stats, mode) {
  const n = (v) => Number(v) || 0;
  // The descriptions are sentences: they follow the value, as in the mockup.
  // Each tile leads to the tab with its details (T01: "każdy kafel prowadzi do
  // szczegółów"), keyboard included.
  const TILE_TAB = { rate: 'topics', topics: 'topics', groups: 'groups', dlq: 'dlq' };
  const tiles = (specs) => paintStatCards(host, specs.map((sp) => ({
    ...sp,
    className: 'clickable',
    attrs: {
      ...sp.attrs,
      'delta-position': 'under-value',
      role: 'link',
      tabindex: '0',
      'aria-label': `${sp.attrs.label}: ${T(`shell.tabs.${TILE_TAB[sp.key]}`)}`,
      'data-go': 'tab',
      'data-tab': TILE_TAB[sp.key],
    },
  })));
  if (mode === 'empty') {
    tiles([
      { key: 'rate', attrs: { label: T('overview.kpi_rate_label'), icon: 'activity', value: fmtCount(k.rate), suffix: T('overview.rate_suffix'), delta: T('overview.kpi_rate_empty') } },
      { key: 'topics', attrs: { label: T('overview.kpi_topics_label'), icon: 'share', value: fmtCount(0), suffix: null, delta: T('overview.kpi_topics_empty') } },
      { key: 'groups', attrs: { label: T('overview.kpi_groups_label'), icon: 'users', value: fmtCount(k.groups), delta: T('overview.kpi_groups_empty') } },
      { key: 'dlq', attrs: { label: T('overview.kpi_dlq_label'), icon: 'inbox', value: fmtCount(k.dlq), delta: T('overview.kpi_dlq_none') } },
    ]);
    return;
  }
  const delayed = delayedGroups(stats.groups);
  const groupsTile = k.groups === 0
    ? { label: T('overview.kpi_groups_label'), icon: 'users', value: fmtCount(0), suffix: null, accent: null, 'delta-type': 'neutral', delta: T('overview.kpi_groups_empty') }
    : {
      label: T('overview.kpi_delayed_label'),
      icon: 'users',
      value: fmtCount(delayed.length),
      suffix: T('overview.kpi_delayed_suffix', { count: fmtCount(k.groups) }),
      accent: delayed.length ? 'warning' : null,
      'delta-type': delayed.length ? 'warn' : 'neutral',
      delta: delayed.length ? [...new Set(delayed.map((g) => g.group))].sort().join(', ') : T('overview.kpi_delayed_none'),
    };
  tiles([
    {
      key: 'rate',
      attrs: {
        label: T('overview.kpi_rate_label'),
        icon: 'activity',
        value: fmtCount(k.rate),
        suffix: T('overview.rate_suffix'),
        delta: k.writingTopics
          ? T('overview.kpi_rate_delta', { count: fmtCount(k.writingTopics), n: k.writingTopics })
          : T('overview.kpi_rate_idle'),
      },
    },
    {
      key: 'topics',
      attrs: {
        label: T('overview.kpi_topics_label'),
        icon: 'share',
        value: fmtCount(k.topics),
        suffix: T('overview.kpi_topics_suffix', { count: fmtCount(k.partitions), n: k.partitions }),
        delta: T('overview.kpi_topics_delta', { size: fmtBytes(k.bytesOnDisk) }),
      },
    },
    { key: 'groups', attrs: groupsTile },
    {
      key: 'dlq',
      attrs: {
        label: T('overview.kpi_dlq_label'),
        icon: 'inbox',
        value: fmtCount(k.dlq),
        accent: n(k.dlq) > 0 ? 'warning' : null,
        delta: n(k.dlq) > 0 ? T('overview.kpi_dlq_delta') : T('overview.kpi_dlq_none'),
      },
    },
  ]);
}

function topicRowSkeleton(name) {
  return `
    <div class="topic-mini" role="link" tabindex="0" data-go="topic" data-topic="${escapeAttr(name)}">
      <div class="tm-ico">${sprite('share')}</div>
      <div class="tm-main">
        <div class="tm-name"><span class="mono">${escapeHtml(name)}</span><span data-role="waiting"></span></div>
        <div class="tm-sub" data-role="sub"></div>
        <tf-progress-bar data-role="bar" size="sm" tone="accent"></tf-progress-bar>
      </div>
      <div class="kv-inline"><span class="v" data-role="rate"></span><span class="k">${escapeHtml(T('overview.rate_suffix'))}</span></div>
    </div>`;
}

function paintTopics(host, rows) {
  patchKeyedList(host, rows.map((r) => ({ key: r.name, html: topicRowSkeleton(r.name) })));
  rows.forEach((r, i) => {
    const row = host.children[i];
    if (!row) return;
    const waiting = row.querySelector('[data-role="waiting"]');
    patchHtml(waiting, r.waiting > 0 ? '<tf-chip size="sm" variant="outline" status="warn"></tf-chip>' : '');
    setAttr(waiting.firstElementChild, 'label', T('overview.topic_waiting', { count: fmtCount(r.waiting) }));
    const sub = [
      contentTypeLabel(r.contentType),
      T('overview.topic_partitions', { count: fmtCount(r.partitions), n: r.partitions }),
      fmtBytes(r.bytes),
    ].filter(Boolean).join(' · ');
    setText(row.querySelector('[data-role="sub"]'), sub);
    setAttr(row.querySelector('[data-role="bar"]'), 'value', String(r.share));
    setText(row.querySelector('[data-role="rate"]'), fmtCount(r.rate));
  });
}

// The card's SHAPE (tone, which button, where it leads) is fixed per alert
// key; the numbers are text slots painted on every poll.
function alertSkeleton(a) {
  let go;
  let action;
  if (a.kind === 'lagging' || a.kind === 'paused') {
    go = `data-go="group" data-group="${escapeAttr(a.group)}" data-topic="${escapeAttr(a.topic)}"`;
    action = T('alerts.act_group');
  } else if (a.kind === 'dlq') {
    go = `data-go="dlq" data-topic="${escapeAttr(a.topic)}"`;
    action = T('alerts.act_dlq');
  } else {
    go = 'data-go="tab" data-tab="replication"';
    action = T('alerts.act_replication');
  }
  const meta = {
    lagging: `<span>${sprite('inbox')}<span data-role="m1"></span></span><span>${sprite('share')}<span class="mono">${escapeHtml(a.topic)}</span></span><span>${sprite('clock')}<span data-role="m2"></span></span>`,
    dlq: `<span>${sprite('share')}<span class="mono">${escapeHtml(a.topic || '')}</span></span><span>${sprite('arrow-up')}<span data-role="m1"></span></span>`,
    paused: `<span>${sprite('inbox')}<span data-role="m1"></span></span><span>${sprite('share')}<span class="mono">${escapeHtml(a.topic || '')}</span></span>`,
    replica: `<span>${sprite('layers')}<span data-role="m1"></span></span><span>${sprite('clock')}<span data-role="m2"></span></span>`,
  }[a.kind];
  return `
    <div class="tb-alert ${a.tone}" data-alert="${escapeAttr(a.key)}">
      <div class="tb-alert-main">
        <div class="tb-alert-title" data-role="title"></div>
        <div class="tb-alert-meta">${meta}</div>
      </div>
      <tf-button variant="secondary" size="sm" ${go}>${escapeHtml(action)}</tf-button>
    </div>`;
}

function alertTexts(a, nowMs) {
  const waiting = (n) => T('alerts.waiting', { count: fmtCount(n), n });
  switch (a.kind) {
    case 'lagging':
      return {
        title: T('alerts.lagging_title', { group: a.group }),
        m1: waiting(a.waiting),
        m2: T(a.wording === 'rising' ? 'alerts.rising_since' : 'alerts.waiting_since', { duration: fmtSince(a.risingSinceMs, nowMs) }),
      };
    case 'dlq':
      return { title: T('alerts.dlq_title'), m1: T('alerts.dlq_meta', { count: fmtCount(a.lastHour), total: fmtCount(a.total) }) };
    case 'paused':
      return { title: T('alerts.paused_title', { group: a.group }), m1: waiting(a.waiting) };
    default: {
      const where = a.topics.length === 1
        ? T('alerts.replica_partitions_topic', { count: fmtCount(a.partitions), n: a.partitions, topic: a.topics[0] })
        : T('alerts.replica_partitions_topics', { count: fmtCount(a.partitions), n: a.partitions, topics: fmtCount(a.topics.length), t: a.topics.length });
      return { title: T('alerts.replica_title', { node: a.nodeLabel }), m1: where, m2: T('alerts.replica_lag', { size: fmtBytes(a.lagBytes), secs: fmtLagSeconds(a.maxLagMs) }) };
    }
  }
}

function paintAlerts(body, alerts, nowMs) {
  const host = body.querySelector('[data-role="alerts"]');
  patchKeyedList(host, alerts.map((a) => ({ key: a.key, html: alertSkeleton(a) })));
  alerts.forEach((a, i) => {
    const card = host.children[i];
    if (!card) return;
    const t = alertTexts(a, nowMs);
    setText(card.querySelector('[data-role="title"]'), t.title);
    setText(card.querySelector('[data-role="m1"]'), t.m1);
    setText(card.querySelector('[data-role="m2"]'), t.m2 || '');
  });
  const count = body.querySelector('[data-role="alerts-count"]');
  patchHtml(count, alerts.length ? '<tf-chip size="sm" variant="outline" status="err"></tf-chip>' : '');
  setAttr(count.firstElementChild, 'label', fmtCount(alerts.length));
  body.querySelector('[data-role="alerts-none"]').hidden = alerts.length > 0;
}

function nodeRowSkeleton(nodeId, label) {
  return `
    <div class="job-row clickable" role="link" tabindex="0" data-go="tab" data-tab="replication" data-node="${escapeAttr(nodeId)}">
      <div class="job-ico">${sprite('cpu')}</div>
      <div class="job-main">
        <div class="job-name"><span class="mono">${escapeHtml(label)}</span> <tf-chip size="sm" variant="outline" dot data-role="state"></tf-chip></div>
        <div class="job-sub"><span data-role="leads"></span><span data-role="holds"></span></div>
      </div>
      <tf-chip size="sm" variant="outline" data-role="sync"></tf-chip>
    </div>`;
}

// With one node there are no copies on other nodes to count: the row says
// what the node leads, and the sentence above it says why nothing else.
function paintNodes(body, nodes, replicaTopics) {
  const rows = nodes == null ? null : nodeRows(nodes, replicaTopics);
  const host = body.querySelector('[data-role="nodes"]');
  if (rows == null) {
    setText(body.querySelector('[data-role="nodes-sub"]'), T('overview.nodes_sub'));
    patchHtml(host, `<div class="muted">${escapeHtml(I18n.t('common.loading'))}</div>`);
    return;
  }
  setText(body.querySelector('[data-role="nodes-sub"]'), rows.length === 1 ? T('overview.nodes_sub_single') : T('overview.nodes_sub'));
  patchKeyedList(host, rows.map((r) => ({ key: r.nodeId, html: nodeRowSkeleton(r.nodeId, r.label) })));
  rows.forEach((r, i) => {
    const row = host.children[i];
    if (!row) return;
    const state = row.querySelector('[data-role="state"]');
    setAttr(state, 'status', r.reachable ? 'ok' : 'err');
    setAttr(state, 'label', T(r.reachable ? 'overview.node_up' : 'overview.node_down'));
    setText(row.querySelector('[data-role="leads"]'), T('overview.node_leads', { count: fmtCount(r.leads), n: r.leads }));
    setText(row.querySelector('[data-role="holds"]'), rows.length === 1 ? '' : T('overview.node_holds', { count: fmtCount(r.holds), n: r.holds }));
    const sync = row.querySelector('[data-role="sync"]');
    setAttr(sync, 'status', r.inSync >= r.total ? 'ok' : 'warn');
    setAttr(sync, 'label', T('overview.node_in_sync', { isr: fmtCount(r.inSync), total: fmtCount(r.total) }));
  });
}
