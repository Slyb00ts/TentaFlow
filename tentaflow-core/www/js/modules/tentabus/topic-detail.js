// ===== File: modules/tentabus/topic-detail.js — a topic's page: title, section menu and one section at a time =====
//
// The page lives under the Topiki tab (PROJEKT-SZCZEGOLOW.md): "← Wszystkie
// topiki", the topic's name with what it carries and checks, "Podgląd
// wiadomości" on the right, then a vertical section menu (tf-tabs
// orientation="vertical"; a "Sekcja: …" list on a phone) beside exactly one
// section: Stan, Ustawienia, Partycje i kopie. Sections are read views; every
// change goes through a window (topic-settings.js, partitions.js) and comes
// back as a "Zapisano …" note over the section it changed.
//
// Rights come from `TopicDetailResponse.access`: without administration the
// page shows no change buttons and says who can change the topic
// (`adminLabels`); without read access the sections that show messages and
// their numbers are unavailable with the reason, and so is the preview.
//
// Drawn once per topic and state, then painted in place on every poll. The
// shell passes the moves in as `ctx.go`.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { patchHtml, setAttr, setText, setRowsIfChanged } from '/js/lib/dom-patch.js';
import { T, fmtCount, fmtBytes, contentTypeLabel } from '/js/modules/tentabus/format.js';
import { TOPIC_SECTIONS } from '/js/modules/tentabus/routes.js';
import { topicSubline } from '/js/modules/tentabus/topics.js';
import { loadErrorHtml } from '/js/modules/tentabus/overview.js';
import { paintStateSection } from '/js/modules/tentabus/topic-state.js';
import { settingsHtml, whoCanChange } from '/js/modules/tentabus/topic-settings.js';
import { partitionRows, transferBlocker, copyChipHtml, rangeText, unavailableText } from '/js/modules/tentabus/partitions.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-select.js';
import '/js/components/tf-button.js';
import '/js/components/tf-table.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-spinner.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

const SECTION_ICONS = { state: 'gauge', settings: 'settings', partitions: 'layers' };
/** Sections that show messages or their numbers: closed to a reader without read access. */
const READ_SECTIONS = new Set(['state', 'partitions']);

/**
 * Loads a topic's page data so that only the newest answer lands: a poll
 * that left before a save must not paint the old values under "Zapisano".
 * `fetch(instanceId, name)` asks the server; `context()` is the open page's
 * `{ instanceId, name }`; `apply({ detail } | { error })` takes the answer.
 * An answer for a page that is no longer open, or overtaken by a later
 * request, is dropped.
 */
export function topicDetailLoader({ fetch, context, apply }) {
  let latest = 0;
  return async function load(name) {
    const turn = ++latest;
    const { instanceId } = context();
    const stillWanted = () => {
      const now = context();
      return turn === latest && now.instanceId === instanceId && now.name === name;
    };
    let outcome;
    try {
      outcome = { detail: await fetch(instanceId, name) };
    } catch (error) {
      outcome = { error };
    }
    if (stillWanted()) apply(outcome);
  };
}

/** Whether a section can be opened with these rights. */
export function sectionOpen(section, access) {
  return !READ_SECTIONS.has(section) || Boolean(access?.canRead);
}

/**
 * The section a page shows: the asked one when it can be opened, else the
 * first that can (a reader without read access lands on Ustawienia).
 */
export function effectiveSection(section, access) {
  if (TOPIC_SECTIONS.includes(section) && sectionOpen(section, access)) return section;
  return TOPIC_SECTIONS.find((s) => sectionOpen(s, access)) || 'settings';
}

function pageHtml(name) {
  return `
    <div class="tb-detail">
      <div class="tb-back"><tf-button variant="ghost" icon="chevron-left" data-go="back">${escapeHtml(T('detail.back'))}</tf-button></div>
      <div class="tb-title-row">
        <div class="tb-title-main">
          <h1 class="tb-title mono">${escapeHtml(name)}</h1>
          <div class="tb-title-desc" data-role="desc"></div>
        </div>
        <div class="tb-title-actions">
          <tf-button variant="secondary" icon="eye" data-go="preview" data-role="preview">${escapeHtml(T('detail.preview'))}</tf-button>
          <div class="tb-title-note" data-role="preview-note" hidden></div>
        </div>
      </div>
      <tf-select class="tb-section-pick" data-role="pick" prefix="${escapeAttr(T('detail.section_prefix'))}" aria-label="${escapeAttr(T('detail.section_prefix'))}"></tf-select>
      <div class="tb-drill">
        <tf-tabs class="tb-section-nav" orientation="vertical" data-role="menu" aria-label="${escapeAttr(T('detail.sections'))}">
          ${TOPIC_SECTIONS.map((s) => `<tf-tab id="${s}" icon="${SECTION_ICONS[s]}">${escapeHtml(T(`detail.section.${s}`))}</tf-tab>`).join('')}
        </tf-tabs>
        <div class="tb-section">
          ${TOPIC_SECTIONS.map((s) => `<div data-section="${s}" hidden></div>`).join('')}
        </div>
      </div>
    </div>`;
}

function loadingHtml() {
  return `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('shell.loading'))}</div>`;
}

function missingHtml(name) {
  return `
    <div class="tb-back"><tf-button variant="ghost" icon="chevron-left" data-go="back">${escapeHtml(T('detail.back'))}</tf-button></div>
    <div class="section-card">
      <tf-empty-state badge icon="share" title="${escapeAttr(T('detail.missing_title', { name }))}" message="${escapeAttr(T('detail.missing_text'))}">
        <tf-button variant="primary" icon="share" data-go="back">${escapeHtml(T('detail.back'))}</tf-button>
      </tf-empty-state>
    </div>`;
}

/**
 * Draws or repaints the page from `ctx.view()` = `{ name, detail, error,
 * errorKind, section, stats, subjects, capabilities, nodes, replicaTopics,
 * replicaLags, lagSeries, notice, justMoved, instanceLabel, nowMs }`.
 * `ctx.go(action)`: `{ kind: 'back' | 'preview' | 'delete' | 'retry' }`,
 * `{ kind: 'section', section }`, `{ kind: 'change', card }`,
 * `{ kind: 'group', group }`, `{ kind: 'dlq' }`, `{ kind: 'transfer', partition }`.
 */
export function drawTopicDetail(body, ctx) {
  const view = ctx.view();
  let mode = 'page';
  if (!view.detail) {
    if (!view.error) mode = 'loading';
    else mode = /\bbus\.topic_not_found\b/.test(String(view.error?.message || '')) ? 'missing' : `error:${view.errorKind}`;
  }
  const sig = `${view.name}|${mode}`;
  if (body.__tbDetail !== sig) {
    body.__tbDetail = sig;
    body.__tbTables = null;
    if (mode === 'loading') patchHtml(body, loadingHtml());
    else if (mode === 'missing') patchHtml(body, missingHtml(view.name));
    else if (mode.startsWith('error:')) patchHtml(body, `<div class="tb-back"><tf-button variant="ghost" icon="chevron-left" data-go="back">${escapeHtml(T('detail.back'))}</tf-button></div>${loadErrorHtml({ kind: view.errorKind, instanceLabel: view.instanceLabel, titleKey: 'detail.error_title' })}`);
    else {
      patchHtml(body, pageHtml(view.name));
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
  else if (d.go === 'change') ctx.go({ kind: 'change', card: d.card });
  else if (d.go === 'group') ctx.go({ kind: 'group', group: d.group });
  else ctx.go({ kind: d.go });
}

function paintPage(body, view, ctx) {
  const { detail } = view;
  const topic = detail.topic;
  const access = detail.access || { canRead: false, canWrite: false, canAdmin: false };
  const section = effectiveSection(view.section, access);

  setText(body.querySelector('[data-role="desc"]'), topicSubline({ contentLabel: contentTypeLabel(topic.contentType), schemaId: topic.schemaId || '' }));
  const preview = body.querySelector('[data-role="preview"]');
  setAttr(preview, 'disabled', !access.canRead);
  const note = body.querySelector('[data-role="preview-note"]');
  note.hidden = access.canRead;
  setText(note, access.canRead ? '' : T('detail.preview_no_read', { name: topic.name }));

  const menu = body.querySelector('[data-role="menu"]');
  for (const s of TOPIC_SECTIONS) {
    const tab = menu.querySelector(`tf-tab#${s}`);
    setAttr(tab, 'disabled', !sectionOpen(s, access));
    setAttr(tab, 'count', s === 'partitions' ? fmtCount(topic.partitions) : null);
  }
  if (menu.getAttribute('value') !== section) menu.value = section;
  const pick = body.querySelector('[data-role="pick"]');
  const options = TOPIC_SECTIONS.filter((s) => sectionOpen(s, access));
  const optSig = options.join('|');
  if (pick.__tbSig !== optSig) {
    pick.__tbSig = optSig;
    pick.setOptions(options.map((s) => ({ value: s, label: T(`detail.section.${s}`) })), section);
  } else if (pick.value !== section) {
    pick.value = section;
  }

  for (const s of TOPIC_SECTIONS) {
    const host = body.querySelector(`[data-section="${s}"]`);
    host.hidden = s !== section;
  }
  const host = body.querySelector(`[data-section="${section}"]`);
  const sectionView = {
    ...view,
    topic,
    partitions: detail.partitions || [],
    access,
    adminLabels: detail.adminLabels || [],
    notice: view.notice?.section === section ? view.notice : null,
  };
  if (section === 'state') paintStateSection(host, sectionView);
  else if (section === 'settings') patchHtml(host, settingsHtml(sectionView));
  else paintPartitionsSection(host, sectionView, ctx);
}

function partitionsSkeleton() {
  return `
    <div data-role="notice"></div>
    <div data-role="who"></div>
    <div data-role="blocked"></div>
    <div class="section-card">
      <div class="section-card-head"><div class="title">${sprite('layers')} ${escapeHtml(T('detail.section.partitions'))} <span data-role="count"></span></div></div>
      <div class="section-sub">${escapeHtml(T('partitions.explain'))}</div>
      <tf-table data-role="table">
        <tf-column key="partition" label="${escapeAttr(T('partitions.col_partition'))}" renderer="html"></tf-column>
        <tf-column key="leader" label="${escapeAttr(T('partitions.col_leader'))}" renderer="html"></tf-column>
        <tf-column key="copies" label="${escapeAttr(T('partitions.col_copies'))}" renderer="html" fill></tf-column>
        <tf-column key="range" label="${escapeAttr(T('partitions.col_range'))}" renderer="html"></tf-column>
      </tf-table>
      <div class="tb-table-footer" data-role="legend"></div>
    </div>`;
}

/** The partitions table of a topic, painted in place. */
function paintPartitionsSection(host, view, ctx) {
  if (host.__tbPartitions !== 'built') {
    host.__tbPartitions = 'built';
    patchHtml(host, partitionsSkeleton());
  }
  const { topic, partitions, nodes, replicaTopics, access, justMoved, notice } = view;
  const canAdmin = Boolean(access.canAdmin);
  patchHtml(host.querySelector('[data-role="notice"]'), notice
    ? `<tf-alert tone="${escapeAttr(notice.tone || 'success')}" title="${escapeAttr(notice.title)}" message="${escapeAttr(notice.text || '')}"></tf-alert>`
    : '');
  patchHtml(host.querySelector('[data-role="who"]'), canAdmin ? '' : `<div class="tb-who-can">${sprite('lock')}<span>${escapeHtml(whoCanChange(view.adminLabels))}</span></div>`);
  const replicaPartitions = (replicaTopics || []).find((r) => r.topic === topic.name)?.partitions || null;
  const rows = partitionRows({ detailPartitions: partitions, replicaPartitions: replicaPartitions || [], nodes });
  const moved = justMoved || new Set();
  const table = host.querySelector('[data-role="table"]');
  const blockers = rows.map((r) => (replicaPartitions == null ? T('partitions.blocked_loading') : transferBlocker(r.replica, nodes, moved.has(r.partition))));
  // A disabled button shows no tooltip (and a phone has no hover), so the
  // reason is text on the page: once when every partition shares it, else
  // under the partition it holds back.
  const shared = canAdmin && blockers.length > 0 && blockers.every((b) => b && b === blockers[0]) ? blockers[0] : null;
  patchHtml(host.querySelector('[data-role="blocked"]'), shared ? `<div class="tb-who-can">${sprite('lock')}<span>${escapeHtml(shared)}</span></div>` : '');
  const tableRows = rows.map((r, i) => {
    const blocker = blockers[i];
    const subs = [
      r.sizeBytes > 0 ? `<div class="tf-table__cell-sub">${escapeHtml(T('partitions.size', { size: fmtBytes(r.sizeBytes) }))}</div>` : '',
      moved.has(r.partition) ? `<div class="tf-table__cell-sub">${escapeHtml(T('partitions.just_changed'))}</div>` : '',
      canAdmin && blocker && !shared && !moved.has(r.partition) ? `<div class="tf-table__cell-sub">${escapeHtml(blocker)}</div>` : '',
      r.unavailable ? `<div class="tf-table__cell-sub"><span class="tf-chip tf-chip--outline err">${escapeHtml(unavailableText(r.unavailable))}</span></div>` : '',
    ].join('');
    return {
      partition: `<span class="tf-table__cell-title">${escapeHtml(T('partitions.name', { n: fmtCount(r.partition) }))}</span>${subs}`,
      leader: r.leader ? `<span class="tf-table__cell--mono">${escapeHtml(r.leader)}</span>` : '—',
      copies: r.copies.length ? r.copies.map(copyChipHtml).join(' ') : '—',
      range: escapeHtml(rangeText(r.range)),
      _key: String(r.partition),
      _partition: r.partition,
      _blocker: blocker,
    };
  });
  table.rowActionsKey = (row) => `${row._partition}|${row._blocker}|${canAdmin}`;
  if (table.__tbAdmin !== canAdmin) {
    table.__tbAdmin = canAdmin;
    table.rowActions = canAdmin ? (row, idx, currentRow) => {
      const live = () => currentRow?.() ?? row;
      const b = document.createElement('tf-button');
      b.setAttribute('variant', 'secondary');
      b.setAttribute('size', 'sm');
      b.setAttribute('icon', 'branch');
      b.textContent = T('partitions.transfer_button');
      b.dataset.act = 'transfer';
      if (row._blocker) {
        b.setAttribute('disabled', '');
        b.title = row._blocker;
      }
      b.addEventListener('click', (e) => {
        e.stopPropagation();
        if (!live()._blocker) ctx.go({ kind: 'transfer', partition: live()._partition });
      });
      return b;
    } : null;
  }
  setRowsIfChanged(table, tableRows);
  const count = host.querySelector('[data-role="count"]');
  patchHtml(count, '<tf-chip size="sm" variant="outline" status="neutral"></tf-chip>');
  setAttr(count.firstElementChild, 'label', fmtCount(rows.length));
  patchHtml(host.querySelector('[data-role="legend"]'), [
    `<span>${copyChipHtml({ state: 'ok', label: T('partitions.legend_ok_chip') })} ${escapeHtml(T('partitions.legend_ok'))}</span>`,
    `<span>${copyChipHtml({ state: 'lag', label: T('partitions.legend_lag_chip') })} ${escapeHtml(T('partitions.legend_lag'))}</span>`,
    moved.size ? `<span>${escapeHtml(T('partitions.legend_just_changed'))}</span>` : '',
  ].join(''));
}
