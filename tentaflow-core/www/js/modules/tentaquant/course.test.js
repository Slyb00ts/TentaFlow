// =============================================================================
// File: modules/tentaquant/course.test.js
// Description: The Kurs tab (Q11). The decisions — which text, which kata is
// next, what a verdict says, who the ranking lists — are pure and pinned
// without a DOM; the view itself is driven through a fake screen whose `tq`
// answers what Core's `Kata*` handlers answer, so a wrong field name between
// the wire and the markup fails here.
// =============================================================================

import { window, I18n } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  courseState, drawCourse, entryKata, gradeView, groupedKatas, isOpen, kataSummary, kataTitle,
  nextKata, percentDone, pickText, rankingRows,
} = await import('./course.js');

const kata = (over = {}) => ({
  kataId: '01-superposition-h',
  groupId: 'basics',
  position: 1,
  points: 60,
  tier: 'T0',
  titles: { pl: 'Superpozycja: bramka H', en: 'Superposition: the H gate' },
  summaries: { pl: 'jeden kubit', en: 'one qubit' },
  status: 'open',
  attempts: 0,
  bestScore: null,
  pointsEarned: 0,
  ...over,
});

const LIST = {
  groups: [
    { groupId: 'entanglement', position: 2, titles: { pl: 'Splątanie', en: 'Entanglement' }, kataCount: 1, passedCount: 0, unlocked: false },
    { groupId: 'basics', position: 1, titles: { pl: 'Podstawy', en: 'Basics' }, kataCount: 2, passedCount: 1, unlocked: true },
  ],
  katas: [
    kata({ kataId: '03-locked', groupId: 'entanglement', position: 3, status: 'locked' }),
    kata({ kataId: '02-x-gate', position: 2, status: 'open' }),
    kata({ kataId: '01-superposition-h', position: 1, status: 'passed', pointsEarned: 60, attempts: 2 }),
  ],
  passedCount: 1,
  totalCount: 3,
  points: 60,
  maxPoints: 200,
};

// ---- pure ------------------------------------------------------------------

test('a text map is read in the language of the dashboard, then English, then anything', () => {
  assert.equal(pickText({ pl: 'Kurs', en: 'Course' }), 'Kurs');
  assert.equal(pickText({ en: 'Course' }), 'Course', 'a missing language falls back to English');
  assert.equal(pickText({ ja: 'コース' }), 'コース', 'and then to whatever the map has');
  assert.equal(pickText(null), '');
  assert.equal(kataTitle(kata()), 'Superpozycja: bramka H');
  assert.equal(kataSummary(kata()), 'jeden kubit');
});

test('katas are grouped in course order whatever order the answer came in', () => {
  const sections = groupedKatas(LIST);
  assert.deepEqual(sections.map((s) => s.group.groupId), ['basics', 'entanglement']);
  assert.deepEqual(sections[0].katas.map((k) => k.kataId), ['01-superposition-h', '02-x-gate']);
  assert.deepEqual(sections[1].katas.map((k) => k.kataId), ['03-locked']);
});

test('the course opens on the first kata that is open and not passed yet', () => {
  assert.equal(entryKata(LIST.katas).kataId, '02-x-gate');
  const all = LIST.katas.map((k) => ({ ...k, status: 'passed' }));
  assert.equal(entryKata(all).kataId, '01-superposition-h', 'a finished course opens on its first kata');
  assert.equal(entryKata([]), null);
});

test('the next kata skips what is passed and what is locked, and wraps around', () => {
  assert.equal(nextKata(LIST.katas, '01-superposition-h').kataId, '02-x-gate');
  assert.equal(nextKata(LIST.katas, '02-x-gate'), null, 'only a locked kata is left');
  const wrapped = [
    kata({ kataId: 'a', position: 1, status: 'open' }),
    kata({ kataId: 'b', position: 2, status: 'passed' }),
  ];
  assert.equal(nextKata(wrapped, 'b').kataId, 'a');
  assert.equal(isOpen(kata({ status: 'locked' })), false);
  assert.equal(isOpen(kata({ status: 'attempted' })), true);
});

test('progress is a rounded share of the katas passed', () => {
  assert.equal(percentDone(LIST), 33);
  assert.equal(percentDone({ passedCount: 0, totalCount: 0 }), 0);
});

test('a verdict says what happened in the words of the wire code', () => {
  const passed = gradeView({ outcome: 'passed', reason: '', metric: 'tvd', value: 0.0123, threshold: 0.05, shots: 1024, durationMs: 3, counts: {} }, 120);
  assert.equal(passed.tone, 'ok');
  assert.match(passed.title, /\+120 pkt/);
  assert.match(passed.lines[0], /TVD od ideału 0,012 \(próg 0,05\)/);
  assert.match(passed.lines[1], /1024 strzały w 3 ms|1\s?024 strzały w 3 ms/);

  const again = gradeView({ outcome: 'passed', metric: 'fidelity', value: 1, threshold: 1, shots: 0, durationMs: 0 }, 0);
  assert.match(again.title, /ponownie/);

  const failed = gradeView({ outcome: 'failed', reason: 'qubit_count', metric: 'fidelity', value: null, threshold: 1, expectedQubits: 1, gotQubits: 2, shots: 0 });
  assert.equal(failed.tone, 'warn');
  assert.match(failed.lines[0], /2 kubity.*1 kubitu/);
  assert.equal(failed.lines.length, 1, 'a refusal that stopped before measuring reports no number');

  const invalid = gradeView({ outcome: 'invalid', diagnostic: { kind: 'syntax', message: 'unexpected token', line: 4 } });
  assert.equal(invalid.tone, 'bad');
  assert.match(invalid.lines[0], /linia 4: unexpected token/);
  assert.equal(gradeView(null), null);
});

test('a sub-millisecond verdict says so instead of printing zero', () => {
  const view = gradeView({ outcome: 'passed', metric: 'tvd', value: 0, threshold: 0.05, shots: 8, durationMs: 0 }, 0);
  assert.match(view.lines[1], /<1 ms/);
});

test('the ranking adds the caller after a gap only when they are not on the list', () => {
  const entry = (id, position, over = {}) => ({ userId: id, position, displayName: id, katasPassed: 1, points: 60, isMe: false, ...over });
  const top = [entry('a', 1), entry('b', 2)];
  assert.deepEqual(rankingRows({ entries: top, me: entry('b', 2, { isMe: true }) }).map((r) => r.gap), [false, false]);
  const rows = rankingRows({ entries: top, me: entry('z', 7, { isMe: true }) });
  assert.deepEqual(rows.map((r) => [r.entry.userId, r.gap]), [['a', false], ['b', false], ['z', true]]);
  assert.deepEqual(rankingRows({ entries: [], me: null }), []);
});

// ---- the view --------------------------------------------------------------

const TASK = { task: { pl: 'Przygotuj `|+⟩`.', en: 'Prepare `|+⟩`.' }, starterCode: 'OPENQASM 3.0;\nqubit[1] q;\n' };

const RANKING = {
  enabled: true,
  total: 2,
  entries: [
    { position: 1, userId: 'u2', displayName: 'Karol Wiśniewski', katasPassed: 2, points: 120, isMe: false },
    { position: 2, userId: 'u1', displayName: 'Anna Kowalska', katasPassed: 1, points: 60, isMe: true },
  ],
  me: { position: 2, userId: 'u1', displayName: 'Anna Kowalska', katasPassed: 1, points: 60, isMe: true },
};

function fakeScreen({ list = LIST, ranking = RANKING, permissions = ['quant.read', 'quant.run'], submit } = {}) {
  const root = window.document.createElement('div');
  root.className = 'tq-root';
  window.document.body.appendChild(root);
  const screen = {
    root,
    tab: 'course',
    instanceId: 'tentaquant-0a1b2c3d',
    disposed: false,
    lab: { myPermissions: permissions },
    course: courseState(),
    requests: [],
    locations: 0,
    setLocation() { this.locations += 1; },
    async tq(kind, payload = {}) {
      this.requests.push([kind, payload]);
      if (kind === 'tentaQuantKataListRequest') return list;
      if (kind === 'tentaQuantKataRankingRequest') return structuredClone(ranking);
      if (kind === 'tentaQuantKataGetRequest') {
        return { kata: list.katas.find((k) => k.kataId === payload.kataId), ...TASK };
      }
      if (kind === 'tentaQuantKataSubmitRequest') return submit(payload);
      throw new Error(`unexpected ${kind}`);
    },
  };
  const host = window.document.createElement('div');
  root.appendChild(host);
  return { screen, host };
}

const cleanup = () => { window.document.body.innerHTML = ''; };
const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

test('the tab opens on the first open kata and draws the progress, groups and ranking', async () => {
  const { screen, host } = fakeScreen();
  await drawCourse(screen, host);
  assert.equal(screen.course.kataId, '02-x-gate');
  assert.ok(screen.locations > 0, 'the route follows the kata');
  assert.equal(host.querySelectorAll('tf-stat-card').length, 3);
  assert.equal(host.querySelectorAll('.kata-group').length, 2);
  assert.equal(host.querySelectorAll('.kata-row').length, 3);
  assert.equal(host.querySelectorAll('.kata-row.locked').length, 1);
  assert.equal(host.querySelector('.kata-row.cur-row').dataset.kata, '02-x-gate');
  assert.match(host.querySelector('.kata-head h3').textContent, /Kata 2/);
  assert.equal(host.querySelector('#tq-kata-source').value, TASK.starterCode);
  assert.equal(host.querySelectorAll('.leader-row').length, 2);
  assert.equal(host.querySelector('.leader-row.hl .me').textContent, 'to Ty');
  cleanup();
});

test('a locked kata is not a link and clicking another one loads it', async () => {
  const { screen, host } = fakeScreen();
  await drawCourse(screen, host);
  host.querySelector('.kata-row.locked').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  await tick();
  assert.equal(screen.course.kataId, '02-x-gate');

  host.querySelector('.kata-row[data-kata="01-superposition-h"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  await tick();
  assert.equal(screen.course.kataId, '01-superposition-h');
  assert.deepEqual(screen.requests.filter(([kind]) => kind === 'tentaQuantKataGetRequest').map(([, p]) => p.kataId), ['02-x-gate', '01-superposition-h']);
  cleanup();
});

test('a person without quant.run reads the task but cannot submit', async () => {
  const { screen, host } = fakeScreen({ permissions: ['quant.read'] });
  await drawCourse(screen, host);
  assert.ok(host.querySelector('[data-act="check"]').hasAttribute('disabled'));
  assert.match(host.querySelector('.kata-actions .hint').textContent, /quant\.run/);
  cleanup();
});

test('submitting sends the typed source, shows the verdict and keeps the draft', async () => {
  const sent = [];
  const { screen, host } = fakeScreen({
    submit: (payload) => {
      sent.push(payload);
      return {
        kata: kata({ kataId: '02-x-gate' }),
        grade: {
          outcome: 'passed', reason: '', metric: 'fidelity', value: 1, threshold: 1,
          expectedQubits: 1, gotQubits: 1, shots: 0, counts: {}, durationMs: 0, diagnostic: null,
        },
        pointsAwarded: 60,
      };
    },
  });
  await drawCourse(screen, host);
  host.querySelector('#tq-kata-source').value = 'OPENQASM 3.0;\nqubit[1] q;\nx q[0];\n';
  host.querySelector('[data-act="check"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  await tick();
  await tick();

  assert.deepEqual(sent, [{ kataId: '02-x-gate', qasm3: 'OPENQASM 3.0;\nqubit[1] q;\nx q[0];\n' }]);
  assert.match(host.querySelector('.check-result.ok .cr-title').textContent, /\+60 pkt/);
  assert.equal(host.querySelector('#tq-kata-source').value, 'OPENQASM 3.0;\nqubit[1] q;\nx q[0];\n', 'the redraw keeps what was typed');
  const lists = screen.requests.filter(([kind]) => kind === 'tentaQuantKataListRequest').length;
  assert.equal(lists, 2, 'the totals and the unlocked groups are re-read from Core');

  host.querySelector('[data-act="reset"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(host.querySelector('#tq-kata-source').value, TASK.starterCode);
  cleanup();
});

test('a switched-off ranking is said so and takes the position card with it', async () => {
  const { screen, host } = fakeScreen({ ranking: { enabled: false, total: 0, entries: [], me: null } });
  await drawCourse(screen, host);
  assert.equal(host.querySelectorAll('tf-stat-card').length, 2);
  assert.equal(host.querySelectorAll('.leader-row').length, 0);
  assert.match(host.querySelector('tf-empty-state').getAttribute('title'), /wyłączony/);
  cleanup();
});

test('a failed list is an alert, and an answer that arrives after the tab changed paints nothing', async () => {
  const failing = fakeScreen();
  failing.screen.tq = async () => { throw new Error('boom'); };
  await drawCourse(failing.screen, failing.host);
  assert.equal(failing.host.querySelector('tf-alert').getAttribute('message'), 'boom');

  const late = fakeScreen();
  const pending = drawCourse(late.screen, late.host);
  late.screen.tab = 'dashboard';
  await pending;
  assert.equal(late.host.querySelector('.kata-group'), null);
  cleanup();
});

test('the course strings exist in the language the dashboard is set to', async () => {
  await I18n.setLanguage('en');
  const view = gradeView({ outcome: 'failed', reason: 'mismatch', metric: 'fidelity', value: 0.5, threshold: 1, shots: 0 });
  assert.match(view.title, /Not yet/);
  assert.match(view.lines[1], /State fidelity 0\.5 \(required 1\)/);
  await I18n.setLanguage('pl');
});
