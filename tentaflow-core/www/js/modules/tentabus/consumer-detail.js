// ===== File: modules/tentabus/consumer-detail.js — a consumer's page: title, section menu and one section at a time =====
//
// The page lives under the Odbiorcy tab (PROJEKT-SZCZEGOLOW.md): "← Wszyscy
// odbiorcy", the consumer's name with the one sentence the server can vouch
// for ("czyta topik …" — a consumer has no description of its own), "Wstrzymaj"
// or "Wznów" on the right, then a vertical section menu (a "Sekcja: …" list
// on a phone) beside exactly one section:
//   - Stan: what waits and whether it grows, how fast the consumer reads, its
//     state, its own unprocessed messages, what needs attention, and the topic
//     it reads;
//   - Miejsce czytania: per partition the last message read, the newest one
//     and what waits, with "Przesuń" in the row (offset-move.js);
//   - Ustawienia: values only. How the consumer confirms is chosen by its
//     program each time it connects, so it carries a lock instead of "Zmień";
//     the retries belong to the topic and lead there.
//
// Rights come from the consumer's topic (`TopicDetailResponse.access`): the
// same administration that may pause it may move its reading place. Without
// it the page shows no change buttons and says who can.
//
// Drawn once per consumer and state, then painted in place on every poll.
// The shell passes the moves in as `ctx.go`.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { patchHtml, patchKeyedList, paintStatCards, setAttr, setText, setRowsIfChanged } from '/js/lib/dom-patch.js';
import { T, fmtCount, fmtSince, fmtWhen, fmtDuration, contentTypeLabel, consumerLabel, isKeyGroup } from '/js/modules/tentabus/format.js';
import { CONSUMER_SECTIONS } from '/js/modules/tentabus/routes.js';
import { isLagging, isPausedWithBacklog, lagWording } from '/js/modules/tentabus/alerts.js';
import { loadErrorHtml } from '/js/modules/tentabus/overview.js';
import { valueRow, settingsCard } from '/js/modules/tentabus/topic-settings.js';
import { commitModeLabel, commitModeHint } from '/js/modules/tentabus/consumers.js';
import { partitionPlace, lastRead, lastMessage } from '/js/modules/tentabus/offset-move.js';
import { headerText } from '/js/modules/tentabus/payload.js';
import { UNPROCESSED_PAGE } from '/js/modules/tentabus/unprocessed.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-select.js';
import '/js/components/tf-button.js';
import '/js/components/tf-table.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-stat-card.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-spinner.js';
import '/js/components/tf-progress-bar.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

const SECTION_ICONS = { state: 'gauge', position: 'history', settings: 'settings' };
const HOUR_MS = 3_600_000;

/**
 * The consumer's own unprocessed messages among the newest page of its
 * topic's (`records`, newest first): a message is this consumer's when its
 * program's failures sent it there (`dlq.group_id`). `partial` = the topic
 * keeps more than the page, so the count covers only the newest ones.
 */
export function consumerDlq({ records, hasMore, group, nowMs }) {
  const mine = (records || []).filter((r) => headerText(r.headers, 'dlq.group_id') === group);
  const arrivals = mine.map((r) => Number(headerText(r.headers, 'dlq.last_failed_at_ms')) || Number(r.timestampMs) || 0);
  return {
    count: mine.length,
    lastHour: arrivals.filter((ms) => ms >= nowMs - HOUR_MS).length,
    lastAtMs: arrivals.length ? Math.max(...arrivals) : null,
    partial: Boolean(hasMore),
  };
}

/** One row per partition of the consumer's topic, most waiting first share-wise. */
export function positionRows({ detail, topicPartitions }) {
  const earliest = new Map((topicPartitions || []).map((p) => [Number(p.partition), Number(p.earliestOffset) || 0]));
  const places = (detail?.partitions || [])
    .map((p) => partitionPlace({ ...p, earliestOffset: earliest.get(Number(p.partition)) ?? 0 }))
    .sort((a, b) => a.partition - b.partition);
  const max = places.reduce((m, p) => Math.max(m, p.waiting), 0);
  return places.map((p) => ({ ...p, share: max > 0 ? Math.round((p.waiting / max) * 100) : 0 }));
}

/**
 * The four Stan tiles. `live` = this consumer's row of the stats snapshot
 * (newer than the page's own answer), `topicStats` = its topic's row.
 */
export function consumerKpis({ live, detail, topicStats, dlq, samples, nowMs }) {
  const partitions = detail?.partitions || [];
  const waiting = live?.lagTotal != null ? Number(live.lagTotal) : partitions.reduce((s, p) => s + (Number(p.lag) || 0), 0);
  const paused = live ? Boolean(live.paused) : Boolean(detail?.paused);
  const perMin = live?.consumeRatePerMin;
  return {
    waiting,
    risingSinceMs: live?.lagRisingSinceMs ?? null,
    wording: lagWording(samples, nowMs),
    ratePerMin: perMin == null ? null : Number(perMin),
    topicRate: topicStats ? Number(topicStats.msgsInPerSec) || 0 : null,
    paused,
    commitMode: detail?.commitMode || '',
    dlq,
  };
}

/** "ok. 400" /s, or per minute for a consumer reading less than one a second. */
export function rateText(perMin) {
  if (perMin == null) return { value: '—', suffix: '' };
  if (perMin === 0) return { value: fmtCount(0), suffix: T('consumer.rate_per_sec') };
  if (perMin < 60) return { value: fmtCount(perMin), suffix: T('consumer.rate_per_min') };
  return { value: T('consumer.rate_about', { count: fmtCount(Math.round(perMin / 60)) }), suffix: T('consumer.rate_per_sec') };
}

/** What needs attention on this consumer, each with the place where it is dealt with. */
export function consumerAlerts({ live, kpis, nowMs }) {
  const out = [];
  const row = live ? { ...live, lagTotal: kpis.waiting } : null;
  if (row && isLagging(row, nowMs)) out.push({ key: 'lagging', kind: 'lagging' });
  if (row && isPausedWithBacklog(row)) out.push({ key: 'paused', kind: 'paused' });
  if (kpis.dlq && kpis.dlq.count > 0) out.push({ key: 'dlq', kind: 'dlq' });
  return out;
}

/** "czyta topik wyniki-badan" — the one description the server can vouch for. */
export const consumerSubline = (topic) => T('consumer.reads_topic', { topic });

/** Who may pause this consumer and move its reading place. */
export function whoCanChangeConsumer(topic, adminLabels) {
  const names = (adminLabels || []).filter(Boolean);
  if (!names.length) return T('consumer.who_can_instance');
  return T('consumer.who_can_topic', { topic, names: new Intl.ListFormat(I18n.getLanguage(), { type: 'conjunction' }).format(names) });
}

function pageHtml(group) {
  return `
    <div class="tb-detail">
      <div class="tb-back"><tf-button variant="ghost" icon="chevron-left" data-go="back">${escapeHtml(T('consumer.back'))}</tf-button></div>
      <div class="tb-title-row">
        <div class="tb-title-main">
          <h1 class="tb-title mono">${escapeHtml(group)}</h1>
          <div class="tb-title-desc" data-role="desc"></div>
        </div>
        <div class="tb-title-actions" data-role="actions"></div>
      </div>
      <tf-select class="tb-section-pick" data-role="pick" prefix="${escapeAttr(T('detail.section_prefix'))}" aria-label="${escapeAttr(T('detail.section_prefix'))}"></tf-select>
      <div class="tb-drill">
        <tf-tabs class="tb-section-nav" orientation="vertical" data-role="menu" aria-label="${escapeAttr(T('consumer.sections'))}">
          ${CONSUMER_SECTIONS.map((s) => `<tf-tab id="${s}" icon="${SECTION_ICONS[s]}">${escapeHtml(T(`consumer.section.${s}`))}</tf-tab>`).join('')}
        </tf-tabs>
        <div class="tb-section">
          ${CONSUMER_SECTIONS.map((s) => `<div data-section="${s}" hidden></div>`).join('')}
        </div>
      </div>
    </div>`;
}

const backHtml = () => `<div class="tb-back"><tf-button variant="ghost" icon="chevron-left" data-go="back">${escapeHtml(T('consumer.back'))}</tf-button></div>`;

function missingHtml(group) {
  return `
    ${backHtml()}
    <div class="section-card">
      <tf-empty-state badge icon="users" title="${escapeAttr(T('consumer.missing_title', { name: group }))}" message="${escapeAttr(T('consumer.missing_text'))}">
        <tf-button variant="primary" icon="users" data-go="back">${escapeHtml(T('consumer.back'))}</tf-button>
      </tf-empty-state>
    </div>`;
}

/**
 * Draws or repaints the page from `ctx.view()` = `{ group, topic, section,
 * data: { detail, topicDetail, dlq, samples } | null, error, errorKind,
 * stats, notice, justMoved, busy, instanceLabel, nowMs }`.
 * `ctx.go(action)`: `{ kind: 'back' | 'pause' | 'resume' | 'topic' |
 * 'topic-settings' | 'dlq' | 'retry' }`, `{ kind: 'section', section }`,
 * `{ kind: 'move', partition }`.
 */
export function drawConsumerDetail(body, ctx) {
  const view = ctx.view();
  let mode = 'page';
  if (!view.data) {
    if (!view.error) mode = 'loading';
    else mode = /\bbus\.group_not_found\b/.test(String(view.error?.message || '')) ? 'missing' : `error:${view.errorKind}`;
  }
  const sig = `${view.group}\u0000${view.topic}|${mode}`;
  if (body.__tbDetail !== sig) {
    body.__tbDetail = sig;
    if (mode === 'loading') patchHtml(body, `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('shell.loading'))}</div>`);
    else if (mode === 'missing') patchHtml(body, missingHtml(view.group));
    else if (mode.startsWith('error:')) patchHtml(body, `${backHtml()}${loadErrorHtml({ kind: view.errorKind, instanceLabel: view.instanceLabel, titleKey: 'consumer.error_title' })}`);
    else {
      patchHtml(body, pageHtml(view.group));
      body.querySelector('[data-role="menu"]').addEventListener('change', (e) => ctx.go({ kind: 'section', section: e.detail?.value }));
      body.querySelector('[data-role="pick"]').addEventListener('change', (e) => ctx.go({ kind: 'section', section: e.detail?.value }));
    }
    if (!body.__tbWired) {
      body.__tbWired = true;
      body.addEventListener('click', (e) => {
        const el = e.target.closest('[data-go]');
        if (!el || !body.contains(el) || el.hasAttribute('disabled')) return;
        act(ctx, el);
      });
      body.addEventListener('keydown', (e) => {
        if (e.key !== 'Enter' && e.key !== ' ') return;
        const row = e.target.closest?.('[role="link"][data-go]');
        if (!row || !body.contains(row)) return;
        e.preventDefault();
        act(ctx, row);
      });
    }
  }
  if (mode === 'page') paintPage(body, view, ctx);
}

function act(ctx, el) {
  const d = el.dataset;
  if (d.go === 'section') ctx.go({ kind: 'section', section: d.section });
  else ctx.go({ kind: d.go });
}

function paintPage(body, view, ctx) {
  const { detail, topicDetail } = view.data;
  const access = topicDetail?.access || { canRead: false, canWrite: false, canAdmin: false };
  const canAdmin = Boolean(access.canAdmin);
  const section = CONSUMER_SECTIONS.includes(view.section) ? view.section : CONSUMER_SECTIONS[0];
  const live = (view.stats?.groups || []).find((g) => g.group === view.group && g.topic === view.topic) || null;
  const paused = live ? Boolean(live.paused) : Boolean(detail.paused);

  const title = body.querySelector('.tb-title');
  setText(title, consumerLabel(detail));
  title.classList.toggle('mono', !isKeyGroup(detail));
  setText(body.querySelector('[data-role="desc"]'), consumerSubline(view.topic));
  const actions = body.querySelector('[data-role="actions"]');
  patchHtml(actions, canAdmin
    ? `<tf-button variant="secondary" icon="${paused ? 'play' : 'pause'}" data-go="${paused ? 'resume' : 'pause'}" data-role="toggle">${escapeHtml(T(paused ? 'consumers.resume' : 'consumers.pause'))}</tf-button>`
    : `<div class="tb-title-note">${escapeHtml(whoCanChangeConsumer(view.topic, topicDetail?.adminLabels))}</div>`);
  setAttr(actions.querySelector('[data-role="toggle"]'), 'disabled', view.busy === true);

  const menu = body.querySelector('[data-role="menu"]');
  setAttr(menu.querySelector('tf-tab#position'), 'count', fmtCount((detail.partitions || []).length));
  if (menu.getAttribute('value') !== section) menu.value = section;
  const pick = body.querySelector('[data-role="pick"]');
  if (!pick.__tbBuilt) {
    pick.__tbBuilt = true;
    pick.setOptions(CONSUMER_SECTIONS.map((s) => ({ value: s, label: T(`consumer.section.${s}`) })), section);
  } else if (pick.value !== section) {
    pick.value = section;
  }

  for (const s of CONSUMER_SECTIONS) body.querySelector(`.tb-section > [data-section="${s}"]`).hidden = s !== section;
  const host = body.querySelector(`.tb-section > [data-section="${section}"]`);
  const notice = view.notice?.section === section ? view.notice : null;
  const sectionView = { ...view, live, paused, canAdmin, access, notice };
  if (section === 'state') paintStateSection(host, sectionView);
  else if (section === 'position') paintPositionSection(host, sectionView, ctx);
  else patchHtml(host, settingsHtml(sectionView));
}

function noticeHtml(notice) {
  return notice
    ? `<tf-alert tone="${escapeAttr(notice.tone || 'success')}" title="${escapeAttr(notice.title)}" message="${escapeAttr(notice.text || '')}"></tf-alert>`
    : '';
}

// ---------------------------------------------------------------------------
// Stan
// ---------------------------------------------------------------------------

function tiles(host, k, nowMs) {
  const rate = rateText(k.ratePerMin);
  let waitingDelta;
  let waitingTone = 'neutral';
  if (k.waiting === 0) waitingDelta = T('consumer.kpi_waiting_none');
  else if (k.risingSinceMs != null) {
    waitingDelta = T(k.wording === 'rising' ? 'alerts.rising_since' : 'alerts.waiting_since', { duration: fmtSince(k.risingSinceMs, nowMs) });
    waitingTone = 'warn';
  } else waitingDelta = T('consumer.kpi_waiting_steady');
  let dlqDelta = '';
  if (k.dlq) {
    dlqDelta = T('detail.state.kpi_dlq_hour', { count: fmtCount(k.dlq.lastHour), n: k.dlq.lastHour });
    if (k.dlq.partial) dlqDelta = `${dlqDelta} · ${T('consumer.kpi_dlq_partial', { count: fmtCount(UNPROCESSED_PAGE) })}`;
  } else dlqDelta = T('consumer.kpi_dlq_unknown');
  paintStatCards(host, [
    {
      key: 'waiting',
      attrs: {
        label: T('consumer.kpi_waiting'),
        icon: 'inbox',
        value: fmtCount(k.waiting),
        accent: waitingTone === 'warn' ? 'warning' : null,
        'delta-type': waitingTone,
        delta: waitingDelta,
        'delta-position': 'under-value',
      },
    },
    {
      key: 'rate',
      attrs: {
        label: T('consumer.kpi_rate'),
        icon: 'activity',
        value: rate.value,
        suffix: rate.suffix || null,
        delta: k.ratePerMin == null
          ? T('consumer.kpi_rate_unknown')
          : (k.topicRate == null ? '' : T('consumer.kpi_rate_topic', { count: fmtCount(k.topicRate) })),
        'delta-position': 'under-value',
      },
    },
    {
      key: 'state',
      attrs: {
        label: T('consumer.kpi_state'),
        icon: 'check',
        value: T(k.paused ? 'consumers.state_paused' : 'consumers.state_running'),
        accent: k.paused ? 'warning' : 'success',
        delta: k.paused ? T('consumer.kpi_state_paused') : T('consumer.kpi_state_running', { mode: commitModeLabel(k.commitMode) }),
        'delta-position': 'under-value',
      },
    },
    {
      key: 'dlq',
      attrs: {
        label: T('consumer.kpi_dlq'),
        icon: 'alert',
        value: k.dlq ? fmtCount(k.dlq.count) : '—',
        accent: k.dlq?.count > 0 ? 'warning' : null,
        delta: dlqDelta,
        'delta-position': 'under-value',
      },
    },
  ]);
}

function alertSkeleton(a, canAdmin) {
  const action = a.kind === 'dlq'
    ? `<tf-button variant="secondary" size="sm" data-go="dlq">${escapeHtml(T(canAdmin ? 'detail.state.act_dlq_admin' : 'detail.state.act_dlq'))}</tf-button>`
    : `<tf-button variant="secondary" size="sm" data-go="section" data-section="position">${escapeHtml(T('consumer.act_position'))}</tf-button>`;
  return `
    <div class="tb-alert warning" data-alert="${escapeAttr(a.key)}">
      <div class="tb-alert-main">
        <div class="tb-alert-title" data-role="title"></div>
        <div class="tb-alert-meta"><span data-role="m1"></span></div>
      </div>
      ${action}
    </div>`;
}

function alertTexts(a, k, nowMs) {
  const waiting = T('alerts.waiting', { count: fmtCount(k.waiting), n: k.waiting });
  if (a.kind === 'lagging') {
    const since = T(k.wording === 'rising' ? 'alerts.rising_since' : 'alerts.waiting_since', { duration: fmtSince(k.risingSinceMs, nowMs) });
    return { title: T('consumer.alert_lagging'), m1: `${waiting}, ${since}` };
  }
  if (a.kind === 'paused') return { title: T('consumer.alert_paused'), m1: T('consumer.alert_paused_meta', { waiting }) };
  return {
    title: T('consumer.alert_dlq', { count: fmtCount(k.dlq.count), n: k.dlq.count }),
    m1: k.dlq.lastAtMs ? T('consumer.alert_dlq_last', { when: fmtWhen(k.dlq.lastAtMs, nowMs) }) : '',
  };
}

function paintStateSection(host, view) {
  const { data, stats, nowMs, canAdmin, live } = view;
  if (host.__tbState !== 'built') {
    host.__tbState = 'built';
    patchHtml(host, `
      <div data-role="notice"></div>
      <div class="tb-kpi" data-role="kpi"></div>
      <div class="section-card">
        <div class="section-card-head"><div class="title">${sprite('alert')} ${escapeHtml(T('detail.state.attention_title'))} <span data-role="attn-count"></span></div></div>
        <div class="tb-alert-list" data-role="alerts"></div>
        <div class="muted" data-role="alerts-none" hidden>${escapeHtml(T('detail.state.attention_none'))}</div>
      </div>
      <div class="section-card">
        <div class="section-card-head"><div class="title">${sprite('share')} ${escapeHtml(T('consumer.topic_title'))}</div></div>
        <div class="job-row clickable" role="link" tabindex="0" data-go="topic" data-role="topic">
          <div class="job-ico">${sprite('share')}</div>
          <div class="job-main">
            <div class="job-name"><span class="mono" data-role="topic-name"></span></div>
            <div class="job-sub" data-role="topic-sub"></div>
          </div>
          <div class="kv-inline"><span class="v" data-role="topic-rate"></span><span class="k">${escapeHtml(T('overview.rate_suffix'))}</span></div>
          ${sprite('chevron-right')}
        </div>
      </div>`);
  }
  patchHtml(host.querySelector('[data-role="notice"]'), noticeHtml(view.notice));
  const topicStats = (stats?.topics || []).find((t) => t.topic === view.topic) || null;
  const k = consumerKpis({ live, detail: data.detail, topicStats, dlq: data.dlq, samples: data.samples, nowMs });
  tiles(host.querySelector('[data-role="kpi"]'), k, nowMs);

  const alerts = consumerAlerts({ live, kpis: k, nowMs });
  const list = host.querySelector('[data-role="alerts"]');
  patchKeyedList(list, alerts.map((a) => ({ key: `${a.key}:${canAdmin}`, html: alertSkeleton(a, canAdmin) })));
  alerts.forEach((a, i) => {
    const el = list.children[i];
    if (!el) return;
    const t = alertTexts(a, k, nowMs);
    setText(el.querySelector('[data-role="title"]'), t.title);
    setText(el.querySelector('[data-role="m1"]'), t.m1);
  });
  const count = host.querySelector('[data-role="attn-count"]');
  patchHtml(count, alerts.length ? '<tf-chip size="sm" variant="outline" status="warn"></tf-chip>' : '');
  setAttr(count.firstElementChild, 'label', fmtCount(alerts.length));
  host.querySelector('[data-role="alerts-none"]').hidden = alerts.length > 0;

  const cfg = data.topicDetail?.topic;
  setText(host.querySelector('[data-role="topic-name"]'), view.topic);
  const sub = [
    cfg ? contentTypeLabel(cfg.contentType) : '',
    cfg ? T('consumer.topic_partitions', { count: fmtCount(cfg.partitions), n: Number(cfg.partitions) || 0 }) : '',
  ].filter(Boolean).join(' · ');
  setText(host.querySelector('[data-role="topic-sub"]'), sub);
  setText(host.querySelector('[data-role="topic-rate"]'), topicStats ? fmtCount(Number(topicStats.msgsInPerSec) || 0) : '—');
}

// ---------------------------------------------------------------------------
// Miejsce czytania
// ---------------------------------------------------------------------------

function positionSkeleton() {
  return `
    <div data-role="notice"></div>
    <div class="section-card">
      <div class="section-card-head"><div class="title">${sprite('layers')} ${escapeHtml(T('position.title'))} <span data-role="count"></span></div></div>
      <div class="section-sub">${escapeHtml(T('position.explain'))}</div>
      <tf-table data-role="table">
        <tf-column key="partition" label="${escapeAttr(T('position.col_partition'))}" renderer="html"></tf-column>
        <tf-column key="read" label="${escapeAttr(T('position.col_read'))}" align="num"></tf-column>
        <tf-column key="last" label="${escapeAttr(T('position.col_last'))}" align="num"></tf-column>
        <tf-column key="waiting" label="${escapeAttr(T('position.col_waiting'))}" renderer="html" align="num" fill></tf-column>
      </tf-table>
      <div class="tb-table-footer" data-role="footer"></div>
    </div>`;
}

function paintPositionSection(host, view, ctx) {
  if (host.__tbPosition !== 'built') {
    host.__tbPosition = 'built';
    patchHtml(host, positionSkeleton());
  }
  const { data, canAdmin, justMoved } = view;
  patchHtml(host.querySelector('[data-role="notice"]'), noticeHtml(view.notice));
  const rows = positionRows({ detail: data.detail, topicPartitions: data.topicDetail?.partitions });
  const moved = justMoved || new Set();
  const table = host.querySelector('[data-role="table"]');
  const tableRows = rows.map((r) => {
    const read = lastRead(r);
    const last = lastMessage(r);
    const tone = view.paused || r.share >= 50 ? 'warning' : 'accent';
    return {
      partition: `<span class="tf-table__cell-title">${escapeHtml(T('position.name', { n: fmtCount(r.partition) }))}</span>${moved.has(r.partition) ? `<div class="tf-table__cell-sub">${escapeHtml(T('position.just_changed'))}</div>` : ''}`,
      read: read == null ? T('position.none_read') : fmtCount(read),
      last: last == null ? T('position.empty') : fmtCount(last),
      waiting: `<div class="tf-table__cell-title">${escapeHtml(fmtCount(r.waiting))}</div><tf-progress-bar size="sm" value="${r.share}" tone="${tone}"></tf-progress-bar>`,
      _key: String(r.partition),
      _partition: r.partition,
      _moved: moved.has(r.partition),
    };
  });
  table.rowActionsKey = (row) => `${row._partition}|${row._moved}|${canAdmin}`;
  if (table.__tbAdmin !== canAdmin) {
    table.__tbAdmin = canAdmin;
    table.rowActions = canAdmin ? (row, idx, currentRow) => {
      const live = () => currentRow?.() ?? row;
      const b = document.createElement('tf-button');
      b.setAttribute('variant', 'secondary');
      b.setAttribute('size', 'sm');
      b.setAttribute('icon', 'history');
      b.textContent = T('position.move');
      b.dataset.act = 'move';
      if (row._moved) {
        b.setAttribute('disabled', '');
        b.title = T('position.moved_blocked');
      }
      b.addEventListener('click', (e) => {
        e.stopPropagation();
        if (!live()._moved) ctx.go({ kind: 'move', partition: live()._partition });
      });
      return b;
    } : null;
  }
  setRowsIfChanged(table, tableRows);
  const count = host.querySelector('[data-role="count"]');
  patchHtml(count, '<tf-chip size="sm" variant="outline" status="neutral"></tf-chip>');
  setAttr(count.firstElementChild, 'label', fmtCount(rows.length));
  const total = rows.reduce((s, r) => s + r.waiting, 0);
  patchHtml(host.querySelector('[data-role="footer"]'), [
    `<span>${T('position.footer_waiting', { count: `<b>${escapeHtml(fmtCount(total))}</b>`, n: total })}</span>`,
    moved.size ? `<span>${escapeHtml(T('position.footer_moved'))}</span>` : '',
  ].join(''));
}

// ---------------------------------------------------------------------------
// Ustawienia
// ---------------------------------------------------------------------------

function settingsHtml(view) {
  const { data, notice } = view;
  const mode = data.detail.commitMode;
  const cfg = data.topicDetail?.topic || null;
  // Both what the way of confirming means and why it is not changed here.
  const confirm = [`
    <div class="tb-vrow">
      <div class="tb-vr-label">${escapeHtml(T('consumer.settings.commit_label'))}</div>
      <div>
        <div class="tb-vr-value">${escapeHtml(commitModeLabel(mode))}</div>
        <div class="tb-vr-hint">${escapeHtml(commitModeHint(mode))}</div>
        <div class="tb-vr-lock">${sprite('lock')}<span>${escapeHtml(T('consumer.settings.commit_lock'))}</span></div>
      </div>
    </div>`];
  const other = [
    valueRow(T('consumer.settings.topic_label'), view.topic, T('consumer.settings.topic_lock'), true),
    valueRow(T('consumer.settings.name_label'), view.group, T('consumer.settings.name_lock'), true),
  ];
  if (cfg) {
    const attempts = Number(cfg.maxDeliveryAttempts) || 0;
    other.push(`
      <div class="tb-vrow">
        <div class="tb-vr-label">${escapeHtml(T('consumer.settings.retry_label'))}</div>
        <div>
          <div class="tb-vr-value">${escapeHtml(T('consumer.settings.retry_value', { count: fmtCount(attempts), n: attempts, pause: fmtDuration(cfg.retryBackoffMs) }))}</div>
          <div class="tb-vr-hint tb-vr-link"><span>${escapeHtml(T('consumer.settings.retry_hint', { topic: view.topic }))}</span>
            <tf-button variant="ghost" size="sm" icon="settings" data-go="topic-settings">${escapeHtml(T('consumer.settings.retry_link'))}</tf-button></div>
        </div>
      </div>`);
  }
  return `
    ${noticeHtml(notice)}
    ${settingsCard('confirm', 'check', T('consumer.settings.confirm_title'), confirm, false)}
    ${settingsCard('other', 'settings', T('consumer.settings.other_title'), other, false)}`;
}

