// ===== File: modules/tentabus/topic-state.js — a topic's Stan section: its traffic, what needs attention and who reads it =====
//
// Read only. Four tiles from the live stats snapshot and the topic's own
// partitions (the time of the oldest message kept), the alerts of this topic
// with a button to the place where each is dealt with — the same rules as
// the overview's alerts, so a topic flagged here is the one Przegląd names —
// and the consumers reading it, each leading to its page. Everything here is
// painted in place on every poll: the skeleton never changes for a given set
// of alerts and consumers.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { patchHtml, patchKeyedList, paintStatCards, setAttr, setText } from '/js/lib/dom-patch.js';
import { T, fmtCount, fmtBytes, fmtDate, fmtSince, fmtLagSeconds } from '/js/modules/tentabus/format.js';
import { computeAlerts, isLagging } from '/js/modules/tentabus/alerts.js';
import { userRate } from '/js/modules/tentabus/model.js';
import { storageFacts } from '/js/modules/tentabus/topic-settings.js';
import '/js/components/tf-stat-card.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-progress-bar.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

/**
 * The four tiles, from the topic's stats row (`null` before the snapshot
 * lists it — a topic created a moment ago has no traffic yet), the whole
 * snapshot and the topic's partitions.
 */
export function stateKpis({ topicStats, stats, partitions, groups, nowMs }) {
  const rate = Number(topicStats?.msgsInPerSec) || 0;
  const all = userRate(stats);
  const lagging = groups.filter((g) => isLagging(g, nowMs)).map((g) => g.group).sort();
  const facts = storageFacts(partitions);
  return {
    rate,
    share: all > 0 ? Math.round((rate / all) * 100) : null,
    waiting: Number(topicStats?.totalLag) || 0,
    lagging,
    dlq: Number(topicStats?.dlqDepth) || 0,
    dlqLastHour: Number(topicStats?.dlqLastHour) || 0,
    bytes: Number(topicStats?.totalBytesOnDisk) || 0,
    oldestMs: facts.oldestMs,
  };
}

/** The consumers of this topic, most waiting first, with the share of the longest queue. */
export function topicConsumers(groups) {
  const rows = [...groups]
    .filter((g) => !String(g.group || '').startsWith('tf-'))
    .sort((a, b) => (Number(b.lagTotal) || 0) - (Number(a.lagTotal) || 0) || a.group.localeCompare(b.group));
  const max = rows.reduce((m, g) => Math.max(m, Number(g.lagTotal) || 0), 0);
  return rows.map((g) => ({
    group: g.group,
    paused: Boolean(g.paused),
    waiting: g.lagTotal == null ? null : Number(g.lagTotal),
    share: max > 0 && g.lagTotal != null ? Math.round((Number(g.lagTotal) / max) * 100) : 0,
  }));
}

/**
 * The alerts of one topic: its consumers that fall behind or wait paused,
 * its unprocessed messages of the last hour, and replicas of its partitions
 * behind their leader.
 */
export function topicAlerts({ topic, stats, replicaLags, lagSeries, nowMs }) {
  return computeAlerts({
    groups: (stats?.groups || []).filter((g) => g.topic === topic),
    topics: (stats?.topics || []).filter((t) => t.topic === topic),
    replicaLags: (replicaLags || []).filter((r) => r.topic === topic),
    lagSeries: lagSeries || new Map(),
    nowMs,
  });
}

function tiles(host, k) {
  paintStatCards(host, [
    {
      key: 'rate',
      attrs: {
        label: T('detail.state.kpi_rate'),
        icon: 'activity',
        value: fmtCount(k.rate),
        suffix: T('overview.rate_suffix'),
        delta: k.share == null ? T('detail.state.kpi_rate_idle') : T('detail.state.kpi_rate_share', { percent: fmtCount(k.share) }),
        'delta-position': 'under-value',
      },
    },
    {
      key: 'waiting',
      attrs: {
        label: T('detail.state.kpi_waiting'),
        icon: 'users',
        value: fmtCount(k.waiting),
        accent: k.lagging.length ? 'warning' : null,
        'delta-type': k.lagging.length ? 'warn' : 'neutral',
        delta: k.lagging.length
          ? T('detail.state.kpi_waiting_lagging', { names: k.lagging.join(', '), n: k.lagging.length })
          : T('detail.state.kpi_waiting_ok'),
        'delta-position': 'under-value',
      },
    },
    {
      key: 'dlq',
      attrs: {
        label: T('detail.state.kpi_dlq'),
        icon: 'inbox',
        value: fmtCount(k.dlq),
        accent: k.dlq > 0 ? 'warning' : null,
        delta: T('detail.state.kpi_dlq_hour', { count: fmtCount(k.dlqLastHour), n: k.dlqLastHour }),
        'delta-position': 'under-value',
      },
    },
    {
      key: 'disk',
      attrs: {
        label: T('detail.state.kpi_disk'),
        icon: 'database',
        value: fmtBytes(k.bytes),
        delta: k.oldestMs != null ? T('detail.state.kpi_disk_oldest', { date: fmtDate(k.oldestMs) }) : T('detail.state.kpi_disk_empty'),
        'delta-position': 'under-value',
      },
    },
  ]);
}

function alertSkeleton(a, canAdmin) {
  let go;
  let action;
  if (a.kind === 'lagging' || a.kind === 'paused') {
    go = `data-go="group" data-group="${escapeAttr(a.group)}"`;
    action = T('alerts.act_group');
  } else if (a.kind === 'dlq') {
    go = 'data-go="dlq"';
    action = T(canAdmin ? 'detail.state.act_dlq_admin' : 'detail.state.act_dlq');
  } else {
    go = 'data-go="section" data-section="partitions"';
    action = T('detail.state.act_partitions');
  }
  return `
    <div class="tb-alert ${a.tone}" data-alert="${escapeAttr(a.key)}">
      <div class="tb-alert-main">
        <div class="tb-alert-title" data-role="title"></div>
        <div class="tb-alert-meta"><span data-role="m1"></span><span data-role="m2"></span></div>
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
      return {
        title: T('detail.state.alert_dlq_title', { count: fmtCount(a.total), n: a.total }),
        m1: T('detail.state.kpi_dlq_hour', { count: fmtCount(a.lastHour), n: a.lastHour }),
      };
    case 'paused':
      return { title: T('alerts.paused_title', { group: a.group }), m1: waiting(a.waiting) };
    default:
      return {
        title: T('alerts.replica_title', { node: a.nodeLabel }),
        m1: T('detail.state.alert_replica_partitions', { count: fmtCount(a.partitions), n: a.partitions }),
        m2: T('alerts.replica_lag', { size: fmtBytes(a.lagBytes), secs: fmtLagSeconds(a.maxLagMs) }),
      };
  }
}

function consumerSkeleton(group) {
  return `
    <div class="job-row clickable tb-consumer-row" role="link" tabindex="0" data-go="group" data-group="${escapeAttr(group)}">
      <div class="job-ico">${sprite('users')}</div>
      <div class="job-main">
        <div class="job-name"><span class="mono">${escapeHtml(group)}</span></div>
        <div class="job-sub" data-role="state"></div>
      </div>
      <div class="tb-lag-cell"><span class="v" data-role="waiting"></span><tf-progress-bar data-role="bar" size="sm"></tf-progress-bar></div>
      ${sprite('chevron-right')}
    </div>`;
}

/** Builds the section once and paints its numbers; call again on every poll. */
export function paintStateSection(host, view) {
  const { topic, stats, partitions, replicaLags, lagSeries, access, nowMs } = view;
  if (host.__tbState !== 'built') {
    host.__tbState = 'built';
    patchHtml(host, `
      <div class="tb-kpi" data-role="kpi"></div>
      <div class="section-card">
        <div class="section-card-head"><div class="title">${sprite('alert')} ${escapeHtml(T('detail.state.attention_title'))} <span data-role="attn-count"></span></div></div>
        <div class="tb-alert-list" data-role="alerts"></div>
        <div class="muted" data-role="alerts-none" hidden>${escapeHtml(T('detail.state.attention_none'))}</div>
      </div>
      <div class="section-card">
        <div class="section-card-head"><div class="title">${sprite('users')} ${escapeHtml(T('detail.state.consumers_title'))} <span data-role="consumers-count"></span></div></div>
        <div class="section-sub">${escapeHtml(T('detail.state.consumers_sub'))}</div>
        <div data-role="consumers"></div>
        <div class="muted" data-role="consumers-none" hidden>${escapeHtml(T('detail.state.consumers_none'))}</div>
      </div>`);
  }
  const name = topic.name;
  const groups = (stats?.groups || []).filter((g) => g.topic === name);
  const topicStats = (stats?.topics || []).find((t) => t.topic === name) || null;
  tiles(host.querySelector('[data-role="kpi"]'), stateKpis({ topicStats, stats, partitions, groups, nowMs }));

  const alerts = topicAlerts({ topic: name, stats, replicaLags, lagSeries, nowMs });
  const alertsHost = host.querySelector('[data-role="alerts"]');
  const canAdmin = Boolean(access?.canAdmin);
  patchKeyedList(alertsHost, alerts.map((a) => ({ key: `${a.key}:${canAdmin}`, html: alertSkeleton(a, canAdmin) })));
  alerts.forEach((a, i) => {
    const el = alertsHost.children[i];
    if (!el) return;
    const t = alertTexts(a, nowMs);
    setText(el.querySelector('[data-role="title"]'), t.title);
    setText(el.querySelector('[data-role="m1"]'), t.m1 || '');
    setText(el.querySelector('[data-role="m2"]'), t.m2 || '');
  });
  const count = host.querySelector('[data-role="attn-count"]');
  patchHtml(count, alerts.length ? '<tf-chip size="sm" variant="outline" status="warn"></tf-chip>' : '');
  setAttr(count.firstElementChild, 'label', fmtCount(alerts.length));
  host.querySelector('[data-role="alerts-none"]').hidden = alerts.length > 0;

  const consumers = topicConsumers(groups);
  const list = host.querySelector('[data-role="consumers"]');
  patchKeyedList(list, consumers.map((c) => ({ key: c.group, html: consumerSkeleton(c.group) })));
  consumers.forEach((c, i) => {
    const el = list.children[i];
    if (!el) return;
    setText(el.querySelector('[data-role="state"]'), T(c.paused ? 'detail.state.consumer_paused' : 'detail.state.consumer_running'));
    setText(el.querySelector('[data-role="waiting"]'), c.waiting == null ? '—' : T('detail.state.consumer_waiting', { count: fmtCount(c.waiting) }));
    const bar = el.querySelector('[data-role="bar"]');
    setAttr(bar, 'value', String(c.share));
    setAttr(bar, 'tone', isLagging(groups.find((g) => g.group === c.group), nowMs) || c.paused ? 'warning' : 'accent');
  });
  const cCount = host.querySelector('[data-role="consumers-count"]');
  patchHtml(cCount, consumers.length ? '<tf-chip size="sm" variant="outline" status="neutral"></tf-chip>' : '');
  setAttr(cCount.firstElementChild, 'label', fmtCount(consumers.length));
  host.querySelector('[data-role="consumers-none"]').hidden = consumers.length > 0;
}
