// =============================================================================
// File: modules/org-structure/visibility-tab.test.js
// Description: The Widoczność tab against a stubbed transport. What has to
//   hold: a member sees their own inspection and no person picker, an
//   administrator gets the picker and choosing somebody asks the server about
//   THAT person in both directions, every area row names its verdict and the
//   rule the server gave, the viewers list starts with the person, and a
//   failure says so instead of drawing an empty card.
// =============================================================================

import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { I18n, sleep, cleanBody } from '../../lib/actions/_test-setup.js';

process.on('unhandledRejection', () => {});

const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
const { mountVisibilityTab, refreshVisibilityTab, unmountVisibilityTab } = await import('./visibility-tab.js');

const vt = (key, params) => I18n.t(`org_structure.visibility.${key}`, params);

const person = (id, name) => ({ user_id: id, display_name: name });

const seesFor = (id, name) => ({
  user: person(id, name),
  manager: person('u-boss', 'Magdalena Kamińska'),
  subtree: [person('u-1', 'Paweł'), person('u-2', 'Marek'), person('u-3', 'Piotr'), person('u-4', 'Ewa'), person('u-5', 'Jan')],
  direct: [person('u-1', 'Paweł'), person('u-2', 'Marek')],
  rows: [
    { area: 'structure', verdict: 'all', rule: 'every_member' },
    { area: 'utilization', verdict: 'subtree', rule: 'primary_manager' },
    { area: 'absence_dates', verdict: 'subtree', rule: 'primary_manager' },
    { area: 'absence_reasons', verdict: 'direct', rule: 'primary_manager' },
    { area: 'position_history', verdict: 'own', rule: 'owner' },
    { area: 'everyone_else', verdict: 'none', rule: 'none' },
  ],
});

const whoFor = (id, name) => ({
  subject: person(id, name),
  viewers: [
    { user_id: 'u-adm', display_name: 'Admin', rule: 'administrator', kinds: ['absence_reason', 'absence_dates', 'position_history'] },
    { user_id: id, display_name: name, rule: 'owner', kinds: ['absence_reason'] },
    { user_id: 'u-anna', display_name: 'Anna', rule: 'primary_manager', kinds: ['absence_reason', 'absence_dates', 'time_utilization'] },
  ],
});

const view = {
  assignments: [
    { subject: { kind: 'user', id: 'u-anna' }, display_name: 'Anna Kowalska' },
    { subject: { kind: 'user', id: 'u-marek' }, display_name: 'Marek Nowak' },
  ],
};

const calls = [];
let failing = false;

beforeEach(() => {
  unmountVisibilityTab();
  calls.length = 0;
  failing = false;
  ApiBinary.one = (kind, payload) => {
    calls.push({ kind, payload });
    if (failing) return Promise.reject(new Error('boom'));
    if (kind === 'orgVisibilityRequest') return Promise.resolve(seesFor(payload.userId ?? 'u-me', payload.userId ? 'Wybrana' : 'Ja'));
    if (kind === 'orgWhoSeesRequest') return Promise.resolve(whoFor(payload.subjectUserId ?? 'u-me', payload.subjectUserId ? 'Wybrana' : 'Ja'));
    return Promise.reject(new Error(`unexpected request ${kind}`));
  };
});

async function mount(permissions) {
  cleanBody();
  document.body.innerHTML = '<div id="host"></div>';
  await mountVisibilityTab(document.getElementById('host'), { view, myPermissions: permissions });
  await sleep(0);
}

const text = (el) => el.textContent.replace(/\s+/g, ' ').trim();

test('a member sees their own inspection and no person picker', async () => {
  await mount([]);
  assert.equal(document.getElementById('org-vis-person'), null);
  assert.equal(text(document.querySelector('.org-vis-self')), vt('self_only'));
  assert.deepEqual(calls.map((c) => [c.kind, c.payload]), [['orgVisibilityRequest', {}], ['orgWhoSeesRequest', {}]]);
});

test('every area is a row with its verdict and the rule the server gave', async () => {
  await mount([]);
  const rows = [...document.querySelectorAll('#org-vis-sees tbody tr')];
  assert.deepEqual(rows.map((r) => r.querySelector('b').textContent), [
    vt('area_structure'), vt('area_utilization'), vt('area_absence_dates'), vt('area_absence_reasons'),
    vt('area_position_history'), vt('area_everyone_else'),
  ]);
  const cells = (row) => [...row.querySelectorAll('td')].map(text);
  assert.equal(cells(rows[1])[1], vt('verdict_subtree', { count: 5 }));
  assert.equal(cells(rows[3])[1], vt('verdict_direct', { count: 2 }));
  assert.equal(cells(rows[4])[1], vt('verdict_own'));
  assert.equal(cells(rows[5])[1], vt('verdict_none'));
  assert.equal(cells(rows[1])[2], vt('rule_primary_manager'));
  assert.equal(cells(rows[5])[2], vt('rule_none'));
  assert.equal(rows[1].querySelector('.org-vis-verdict').className.includes('org-vis-yes'), true);
  assert.equal(rows[4].querySelector('.org-vis-verdict').className.includes('org-vis-part'), true);
  assert.equal(rows[5].querySelector('.org-vis-verdict').className.includes('org-vis-no'), true);
});

test('the people a verdict reaches are named, the long list cut to a count', async () => {
  await mount([]);
  const rows = [...document.querySelectorAll('#org-vis-sees tbody tr')];
  assert.equal(text(rows[1].querySelector('.org-vis-sub')), vt('people_more', { names: 'Paweł, Marek, Piotr, Ewa', count: 5 }));
  assert.equal(text(rows[3].querySelector('.org-vis-sub')), 'Paweł, Marek');
  assert.equal(text(rows[0].querySelector('.org-vis-sub')), vt('area_structure_sub'));
});

test('the chips name the manager and the size of the subtree', async () => {
  await mount([]);
  const chips = [...document.querySelectorAll('#org-vis-chips tf-chip')].map(text);
  assert.deepEqual(chips, [vt('chip_manager', { name: 'Magdalena Kamińska' }), vt('chip_subtree', { count: 5 })]);
});

test('the viewers of the data list the person first and say what each may see', async () => {
  await mount([]);
  const rows = [...document.querySelectorAll('.org-vis-who-row')];
  assert.deepEqual(rows.map((r) => text(r.querySelector('.org-vis-who-name'))), ['Ja', 'Anna', 'Admin']);
  assert.equal(text(rows[0].querySelector('.org-vis-who-kinds')), vt('viewer_self'));
  assert.equal(text(rows[1].querySelector('.org-vis-who-kinds')),
    [vt('kind_absence_reason'), vt('kind_absence_dates'), vt('kind_time_utilization')].join(', '));
  assert.equal(text(rows[2].querySelector('.org-vis-rule')), vt('rule_administrator'));
  assert.match(text(document.getElementById('org-vis-who-card')), new RegExp(vt('who_note')));
});

test('an administrator picks a person from the structure and both questions are asked about them', async () => {
  await mount(['org.admin']);
  const picker = document.getElementById('org-vis-person');
  assert.ok(picker);
  assert.deepEqual([...picker.querySelectorAll('option')].map((o) => [o.value, o.textContent]), [
    ['', vt('pick_me')], ['u-anna', 'Anna Kowalska'], ['u-marek', 'Marek Nowak'],
  ]);
  calls.length = 0;
  picker.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'u-marek' } }));
  await sleep(0);
  assert.deepEqual(calls.map((c) => [c.kind, c.payload]), [
    ['orgVisibilityRequest', { userId: 'u-marek' }], ['orgWhoSeesRequest', { subjectUserId: 'u-marek' }],
  ]);
  assert.match(text(document.querySelector('#org-vis-sees h3')), /Wybrana/);
  assert.match(text(document.querySelector('#org-vis-who-card h3')), /Wybrana/);

  // Back to "me" asks without a person.
  calls.length = 0;
  picker.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: '' } }));
  await sleep(0);
  assert.deepEqual(calls.map((c) => c.payload), [{}, {}]);
});

test('a refresh reads the answers again for the person on screen', async () => {
  await mount(['org.admin']);
  calls.length = 0;
  refreshVisibilityTab({ view });
  await sleep(0);
  assert.deepEqual(calls.map((c) => c.kind), ['orgVisibilityRequest', 'orgWhoSeesRequest']);
});

test('a failed read says so and leaves no half-drawn card as the answer', async () => {
  failing = true;
  await mount([]);
  const alert = document.getElementById('org-vis-error');
  assert.equal(alert.hidden, false);
  assert.equal(alert.getAttribute('message'), vt('load_failed', { message: 'boom' }));
});
