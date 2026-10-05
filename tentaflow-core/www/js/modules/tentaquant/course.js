// ===== File: modules/tentaquant/course.js — Q11, the Kurs tab of one laboratory =====
//
// The course is a fixed list of katas — short tasks with an automatic verdict —
// in groups that open one after another (plan §12.3). Core grades every answer
// (`Kata::Submit`, deterministic, native simulator), so what this view holds is
// presentation: the list as the wire ordered it, the one kata being worked on,
// the last verdict and the ranking.
//
// What the mockup shows and this does NOT build: the streak (it needs the
// per-day history of `kata_progress`, which the wire does not carry), hints,
// "Uruchom w studio" and "Zapisz jako notatnik" (they hand a kata over to a
// project, which a course taken by anyone — project or not — does not have),
// the ranking period selector, and the katas past the tenth: the screen draws
// exactly the katas `Kata::List` answers with, never a locked placeholder for
// one that does not exist.
//
// The pure helpers at the top hold the decisions — which text, which kata is
// next, what a verdict says — and are unit-tested without a DOM. Everything
// that touches the DOM goes through `drawCourse`, which owns the staleness
// guard: a response that arrives after the tab, the lab or the kata changed is
// dropped instead of painted over the new view.

import { I18n } from '/js/i18n.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { T, sprite, errMessage, has, initials, editorLabels, mimeLabels } from '/js/modules/tentaquant/format.js';
import { countsBundle } from '/js/modules/tentaquant/quantum-view.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-code-editor.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-mime-output.js';
import '/js/components/tf-progress-bar.js';
import '/js/components/tf-stat-card.js';

export const OUTCOME_PASSED = 'passed';
export const OUTCOME_INVALID = 'invalid';

/// The screen's course state: which kata is open, what the person typed in
/// each one and the last verdict. Drafts live here, not in the editor, so a
/// redraw after a verdict (the list and the totals changed) cannot lose them.
export function courseState(patch = {}) {
  return { kataId: null, drafts: {}, grade: null, awarded: 0, ...patch };
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// The text of a `{language: text}` map in the language of the dashboard, then
/// English, then whatever the map has: a language the course was not translated
/// into must still read as words.
export function pickText(map) {
  if (!map || typeof map !== 'object') return '';
  const language = I18n.getLanguage();
  return map[language] || map.en || Object.values(map).find(Boolean) || '';
}

export const kataTitle = (kata) => pickText(kata?.titles);
export const kataSummary = (kata) => pickText(kata?.summaries);

/// The groups with their katas in course order, each kata under the group it
/// names. A kata whose group the answer does not list is dropped: it would have
/// nowhere to be drawn.
export function groupedKatas(list) {
  const katas = (list?.katas || []).slice().sort((a, b) => a.position - b.position);
  return (list?.groups || [])
    .slice()
    .sort((a, b) => a.position - b.position)
    .map((group) => ({ group, katas: katas.filter((k) => k.groupId === group.groupId) }));
}

/// A kata the person may work on: not behind a group that is still closed.
export const isOpen = (kata) => Boolean(kata) && kata.status !== 'locked';

/// The kata to land on: the first open one not passed yet, or — once the whole
/// course is passed — the first one, so the view never opens on nothing.
export function entryKata(katas) {
  const list = (katas || []).slice().sort((a, b) => a.position - b.position);
  return list.find((k) => isOpen(k) && k.status !== 'passed') || list[0] || null;
}

/// The kata after `kataId` that is open and not passed, wrapping to the first
/// such one; null when nothing is left to do.
export function nextKata(katas, kataId) {
  const list = (katas || []).slice().sort((a, b) => a.position - b.position);
  const current = list.findIndex((k) => k.kataId === kataId);
  const ahead = list.slice(current + 1).concat(list.slice(0, Math.max(current, 0)));
  return ahead.find((k) => k.kataId !== kataId && isOpen(k) && k.status !== 'passed') || null;
}

export function percentDone(list) {
  const total = Number(list?.totalCount) || 0;
  return total > 0 ? Math.round(((Number(list?.passedCount) || 0) / total) * 100) : 0;
}

const fmtNumber = (value, digits = 3) => Number(value).toLocaleString(I18n.getLanguage(), {
  minimumFractionDigits: 0,
  maximumFractionDigits: digits,
});

/// What a verdict says, as three parts the view lays out: a tone, a headline
/// and the detail lines under it. `awarded` is what THIS attempt paid; the
/// reason text is chosen by the wire's code, never by parsing a sentence.
export function gradeView(grade, awarded = 0) {
  if (!grade) return null;
  if (grade.outcome === OUTCOME_PASSED) {
    return {
      tone: 'ok',
      title: awarded > 0 ? T('course.result_passed', { n: awarded }) : T('course.result_passed_again'),
      lines: measuredLines(grade),
    };
  }
  if (grade.outcome === OUTCOME_INVALID) {
    const diagnostic = grade.diagnostic || {};
    const where = diagnostic.line
      ? T('course.diagnostic_at', { line: diagnostic.line, message: diagnostic.message || '' })
      : (diagnostic.message || '');
    return { tone: 'bad', title: T('course.result_invalid'), lines: where ? [where] : [] };
  }
  const reason = grade.reason
    ? T(`course.reason_${grade.reason}`, { got: grade.gotQubits, expected: grade.expectedQubits })
    : '';
  return {
    tone: 'warn',
    title: T('course.result_failed'),
    lines: [reason, ...measuredLines(grade)].filter(Boolean),
  };
}

/// The measured number against its bound, then — for a distribution — how many
/// shots took how long. A refusal that stopped before measuring has no number
/// and says nothing about one.
function measuredLines(grade) {
  const lines = [];
  if (grade.value !== null && grade.value !== undefined) {
    lines.push(T(`course.metric_${grade.metric}`, {
      value: fmtNumber(grade.value),
      threshold: fmtNumber(grade.threshold),
    }));
  }
  if (Number(grade.shots) > 0) {
    lines.push(T('course.run_info', {
      shots: Number(grade.shots),
      ms: Number(grade.durationMs) > 0 ? String(grade.durationMs) : '<1',
    }));
  }
  return lines;
}

/// The ranking rows to draw: the top list as answered, then — when the caller
/// is ranked but not on it — their own line after a gap, so nobody has to count
/// to find themselves.
export function rankingRows(ranking) {
  const entries = ranking?.entries || [];
  const rows = entries.map((entry) => ({ entry, gap: false }));
  const me = ranking?.me;
  if (me && !entries.some((entry) => entry.userId === me.userId)) rows.push({ entry: me, gap: true });
  return rows;
}

// ---------------------------------------------------------------------------
// Markup
// ---------------------------------------------------------------------------

const STATE_ICON = { passed: 'check-circle', attempted: 'play', open: 'play', locked: 'lock' };

function kpiHtml(list, ranking) {
  const total = Number(list.totalCount) || 0;
  const cards = [
    `<tf-stat-card label="${escapeAttr(T('course.kpi_progress'))}" icon="catalog"
      value="${escapeAttr(T('course.progress_value', { done: Number(list.passedCount) || 0, total }))}"
      delta="${escapeAttr(T('course.kpi_progress_delta', { percent: percentDone(list) }))}" delta-type="neutral"></tf-stat-card>`,
    `<tf-stat-card label="${escapeAttr(T('course.kpi_points'))}" icon="star"
      value="${Number(list.points) || 0}"
      delta="${escapeAttr(T('course.kpi_points_delta', { max: Number(list.maxPoints) || 0 }))}" delta-type="neutral"></tf-stat-card>`,
  ];
  // The position card exists only while a ranking does: a switched-off ranking
  // has no position to report, and an empty card would read as "last".
  if (ranking && ranking.enabled) {
    const me = ranking.me;
    cards.push(`<tf-stat-card label="${escapeAttr(T('course.kpi_position'))}" icon="crown"
      value="${escapeAttr(me ? T('course.position_value', { position: me.position, total: ranking.total }) : '—')}"
      delta="${escapeAttr(me ? T('course.kpi_position_delta', { n: ranking.total }) : T('course.kpi_position_none'))}" delta-type="neutral"></tf-stat-card>`);
  }
  return `<div class="tq-kpi">${cards.join('')}</div>`;
}

function kataRowHtml(kata, currentId) {
  const locked = kata.status === 'locked';
  const earned = Number(kata.pointsEarned) || 0;
  return `
    <div class="kata-row${kata.kataId === currentId ? ' cur-row' : ''}${locked ? ' locked' : ''}" data-kata="${escapeAttr(kata.kataId)}"
      ${locked ? 'aria-disabled="true"' : 'role="button" tabindex="0"'}
      aria-label="${escapeAttr(`${kataTitle(kata)} — ${T(`course.status_${kata.status}`)}`)}">
      <span class="ks ${kata.status}">${sprite(STATE_ICON[kata.status] || 'play')}</span>
      <div class="km">
        <div class="kn">${escapeHtml(String(kata.position).padStart(2, '0'))} · ${escapeHtml(kataTitle(kata))}</div>
        <div class="kd">${escapeHtml(kataSummary(kata))}</div>
      </div>
      <span class="tier ${kata.tier.toLowerCase()}">${escapeHtml(kata.tier)}</span>
      <span class="pts${earned > 0 ? ' got' : ''}">${escapeHtml(earned > 0 ? `+${earned}` : String(kata.points))}</span>
    </div>`;
}

function groupHtml({ group, katas }, previousTitle, currentId) {
  const done = Number(group.passedCount) || 0;
  const count = Number(group.kataCount) || katas.length;
  const complete = count > 0 && done === count;
  const chip = !group.unlocked
    ? `<tf-chip status="neutral" label="${escapeAttr(T('course.group_locked', { group: previousTitle }))}"></tf-chip>`
    : complete
      ? `<tf-chip status="ok" label="${escapeAttr(T('course.group_complete'))}"></tf-chip>`
      : `<tf-chip status="info" label="${escapeAttr(T('course.group_current'))}"></tf-chip>`;
  return `
    <div class="kata-group">
      <div class="kata-group-head">
        <span class="kg-title">${escapeHtml(`${group.position} · ${pickText(group.titles)}`)}</span>
        <span class="kg-count">${escapeHtml(T('course.group_count', { done, total: count }))}</span>
        <tf-progress-bar size="sm" tone="${complete ? 'success' : 'accent'}" value="${count ? Math.round((done / count) * 100) : 0}"></tf-progress-bar>
        ${chip}
      </div>
      ${katas.map((kata) => kataRowHtml(kata, currentId)).join('')}
    </div>`;
}

function verdictHtml(view, grade) {
  if (!view) return '';
  const icon = view.tone === 'ok' ? 'check-circle' : view.tone === 'bad' ? 'alert' : 'info';
  return `
    <div class="check-result ${view.tone}" role="status">
      <div class="cr-ico">${sprite(icon)}</div>
      <div class="cr-body">
        <div class="cr-title">${escapeHtml(view.title)}</div>
        ${view.lines.map((line) => `<div class="cr-sub">${escapeHtml(line)}</div>`).join('')}
        ${Number(grade.shots) > 0 ? '<tf-mime-output class="cr-hist" id="tq-kata-counts"></tf-mime-output>' : ''}
        <div class="cr-next" id="tq-kata-next"></div>
      </div>
    </div>`;
}

function rankingHtml(ranking, error) {
  const head = `
    <div class="section-card-head">
      <div class="title">${sprite('crown')} ${escapeHtml(T('course.ranking_title'))}</div>
    </div>`;
  if (error) {
    return `<div class="section-card">${head}<tf-alert tone="danger" title="${escapeAttr(T('course.ranking_load_failed'))}" message="${escapeAttr(error)}"></tf-alert></div>`;
  }
  if (!ranking.enabled) {
    return `<div class="section-card">${head}<tf-empty-state icon="crown" title="${escapeAttr(T('course.ranking_off'))}" message="${escapeAttr(T('course.ranking_off_sub'))}"></tf-empty-state></div>`;
  }
  const rows = rankingRows(ranking);
  if (!rows.length) {
    return `<div class="section-card">${head}<tf-empty-state icon="crown" title="${escapeAttr(T('course.ranking_empty'))}" message="${escapeAttr(T('course.ranking_empty_sub'))}"></tf-empty-state></div>`;
  }
  const lines = rows.map(({ entry, gap }) => `
    ${gap ? '<div class="leader-gap" aria-hidden="true">⋯</div>' : ''}
    <div class="leader-row${entry.isMe ? ' hl' : ''}">
      <span class="pos">${entry.position}</span>
      <span class="who"><span class="av">${escapeHtml(initials(entry.displayName))}</span>${escapeHtml(entry.displayName)}${entry.isMe ? `<span class="me">${escapeHtml(T('course.ranking_you'))}</span>` : ''}</span>
      <span class="pct">${escapeHtml(T('course.progress_value', { done: entry.katasPassed, total: ranking.catalogTotal }))}</span>
      <span class="pts-col">${escapeHtml(fmtNumber(entry.points, 0))}</span>
    </div>`).join('');
  return `
    <div class="section-card">
      ${head}
      <div class="leader-head"><span>${escapeHtml(T('course.ranking_sub', { n: (ranking.entries || []).length, total: ranking.total }))}</span></div>
      <div class="leader">${lines}</div>
    </div>`;
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// Loads the course and the ranking and draws the tab into `host`. The kata
/// being worked on is `screen.course.kataId`; the view moves it when the person
/// picks another one and tells the screen so the route follows.
export async function drawCourse(screen, host) {
  const state = screen.course;
  // A later draw (a kata pick, a verdict) supersedes this one; the counter is
  // what lets the older one notice and stop before it paints.
  const draw = (screen.courseDraw = (screen.courseDraw || 0) + 1);
  const instanceId = screen.instanceId;
  const stale = () => screen.disposed || screen.courseDraw !== draw
    || screen.instanceId !== instanceId || screen.tab !== 'course' || !host.isConnected;

  host.innerHTML = `<div class="tq-loading">${escapeHtml(I18n.t('common.loading'))}</div>`;
  let list;
  try {
    list = await screen.tq('tentaQuantKataListRequest');
  } catch (e) {
    if (stale()) return;
    host.innerHTML = `<tf-alert tone="danger" title="${escapeAttr(T('course.load_failed'))}" message="${escapeAttr(errMessage(e))}"></tf-alert>`;
    return;
  }
  if (stale()) return;

  let ranking = null;
  let rankingError = '';
  try {
    ranking = await screen.tq('tentaQuantKataRankingRequest');
    ranking.catalogTotal = Number(list.totalCount) || 0;
  } catch (e) {
    rankingError = errMessage(e);
  }
  if (stale()) return;

  const katas = list.katas || [];
  const wanted = katas.find((k) => k.kataId === state.kataId);
  const current = isOpen(wanted) ? wanted : entryKata(katas);
  if (!current || !isOpen(current)) {
    host.innerHTML = `<tf-empty-state icon="catalog" title="${escapeAttr(T('course.load_failed'))}"></tf-empty-state>`;
    return;
  }
  if (state.kataId !== current.kataId) {
    state.kataId = current.kataId;
    state.grade = null;
    screen.setLocation();
  }

  let detail;
  try {
    detail = await screen.tq('tentaQuantKataGetRequest', { kataId: current.kataId });
  } catch (e) {
    if (stale()) return;
    host.innerHTML = `${kpiHtml(list, ranking)}<tf-alert tone="danger" title="${escapeAttr(T('course.kata_load_failed'))}" message="${escapeAttr(errMessage(e))}"></tf-alert>`;
    return;
  }
  if (stale()) return;

  paint(screen, host, { list, ranking, rankingError, detail, stale });
}

function paint(screen, host, { list, ranking, rankingError, detail, stale }) {
  const state = screen.course;
  const kata = detail.kata;
  const sections = groupedKatas(list);
  const view = gradeView(state.grade, state.awarded);
  const language = I18n.getLanguage();
  const task = detail.task?.[language] || detail.task?.en || Object.values(detail.task || {})[0] || '';
  const group = sections.find((s) => s.group.groupId === kata.groupId)?.group;
  // Reading the course needs `quant.read`; having an answer graded needs
  // `quant.run`, and a button the server would refuse is not offered.
  const canRun = has(screen.lab?.myPermissions, 'quant.run');

  host.innerHTML = `
    ${kpiHtml(list, ranking)}
    <div class="kata-layout">
      <div class="kata-groups">
        ${sections.map((section, index) => groupHtml(
          section,
          index > 0 ? pickText(sections[index - 1].group.titles) : '',
          kata.kataId,
        )).join('')}
      </div>
      <div class="kata-main">
        <div class="section-card">
          <div class="kata-head">
            <div>
              <h3>${sprite('catalog')}${escapeHtml(T('course.kata_heading', { n: kata.position, title: kataTitle(kata) }))}</h3>
              <div class="section-sub">${escapeHtml(T('course.kata_meta', { group: group ? pickText(group.titles) : '', n: kata.position, total: list.totalCount }))}</div>
            </div>
            <div class="kh-meta">
              <span class="tier ${kata.tier.toLowerCase()}">${escapeHtml(kata.tier)}</span>
              <tf-chip label="${escapeAttr(T('course.points_value', { n: kata.points }))}"></tf-chip>
              ${kata.attempts > 0 ? `<tf-chip status="info" label="${escapeAttr(T('course.attempts', { n: kata.attempts }))}"></tf-chip>` : ''}
            </div>
          </div>
          <div class="readme">
            <h3>${escapeHtml(T('course.task_title'))}</h3>
            <tf-mime-output id="tq-kata-task"></tf-mime-output>
          </div>
          <div class="section-card-head tq-kata-code-head">
            <div class="title">${sprite('code')} ${escapeHtml(T('course.code_title'))}</div>
            <span class="hint">${escapeHtml(T('course.code_hint'))}</span>
          </div>
          <tf-code-editor id="tq-kata-source" language="plain" aria-label="${escapeAttr(T('course.code_title'))}"></tf-code-editor>
          <div class="kata-actions">
            <tf-button variant="primary" icon="check" data-act="check" ${canRun ? '' : 'disabled'}>${escapeHtml(T('course.action_check'))}</tf-button>
            <tf-button variant="ghost" icon="refresh" data-act="reset">${escapeHtml(T('course.action_reset'))}</tf-button>
            ${canRun ? '' : `<span class="hint">${escapeHtml(T('course.run_required'))}</span>`}
          </div>
        </div>
        ${verdictHtml(view, state.grade || {})}
        ${rankingHtml(ranking || {}, rankingError)}
      </div>
    </div>`;

  const taskView = host.querySelector('#tq-kata-task');
  taskView.labels = mimeLabels();
  taskView.bundle = { 'text/markdown': task };

  const editor = host.querySelector('#tq-kata-source');
  editor.labels = editorLabels();
  editor.value = state.drafts[kata.kataId] ?? detail.starterCode ?? '';
  editor.addEventListener('change', () => { state.drafts[kata.kataId] = editor.value; });

  if (state.grade && Number(state.grade.shots) > 0) {
    const counts = host.querySelector('#tq-kata-counts');
    counts.labels = mimeLabels();
    counts.bundle = countsBundle(state.grade.counts, state.grade.shots);
  }
  const next = nextKata(list.katas, kata.kataId);
  const nextHost = host.querySelector('#tq-kata-next');
  if (nextHost && state.grade?.outcome === OUTCOME_PASSED && next) {
    nextHost.innerHTML = `<tf-button variant="primary" size="sm" icon="chevron-right" data-act="next">${escapeHtml(T('course.action_next', { title: kataTitle(next) }))}</tf-button>`;
    nextHost.querySelector('[data-act="next"]').addEventListener('click', () => pick(screen, host, next.kataId));
  }

  host.querySelectorAll('.kata-row[data-kata]:not(.locked)').forEach((row) => {
    const go = () => pick(screen, host, row.dataset.kata);
    row.addEventListener('click', go);
    row.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); go(); }
    });
  });
  host.querySelector('[data-act="reset"]').addEventListener('click', () => {
    delete state.drafts[kata.kataId];
    editor.value = detail.starterCode ?? '';
  });
  const check = host.querySelector('[data-act="check"]');
  check.addEventListener('click', () => submit(screen, host, kata.kataId, editor, check, stale));
}

/// Opens another kata. The previous verdict belongs to the previous kata, so it
/// goes with it.
function pick(screen, host, kataId) {
  const state = screen.course;
  if (state.kataId === kataId && !state.grade) return;
  state.kataId = kataId;
  state.grade = null;
  state.awarded = 0;
  screen.setLocation();
  drawCourse(screen, host);
}

async function submit(screen, host, kataId, editor, button, stale) {
  const state = screen.course;
  const source = editor.value;
  state.drafts[kataId] = source;
  button.setAttribute('disabled', '');
  try {
    const res = await screen.tq('tentaQuantKataSubmitRequest', { kataId, qasm3: source });
    if (stale()) return;
    state.grade = res.grade;
    state.awarded = Number(res.pointsAwarded) || 0;
    // The totals, the unlocked groups and the ranking all moved: redraw from
    // the server's view of them rather than patching the local copy.
    await drawCourse(screen, host);
  } catch (e) {
    if (stale()) return;
    button.removeAttribute('disabled');
    toast(`${T('course.submit_failed')}: ${errMessage(e)}`, 'error');
  }
}
