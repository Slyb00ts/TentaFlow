// =============================================================================
// File: modules/tentanas/schedule-editor.test.js
// Description: The schedule editor round-trips a NasSchedule: the fields
// show the incoming cadence, time, weekday and day, only the rows that apply
// to the cadence are visible, edits read back clamped into the wire shape,
// and confirm hands `{ enabled, schedule }` to onSave. The snapshot schedule
// editor sends the same shape plus the retention counts and the §5.10
// protection period, which it refuses to save when an enabled DAILY-or-coarser
// tier keeps less history than the protection. Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, typeInto, confirmWindow, window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { openScheduleEditor, normalizeSchedule, readScheduleFields } = await import('./schedule-editor.js');
const { openSnapshotScheduleEditor } = await import('./snapshots.js');

const field = (win, prefix, id) => win.querySelector(`#${prefix}-${id}`);
const selectValue = (sel, value) => {
  sel.value = value;
  sel.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value } }));
};

test('normalizeSchedule clamps every field into the wire range', () => {
  assert.deepEqual(normalizeSchedule({ every: 'weekly', hour: 3, minute: 30, weekday: 6, day: 1 }), { every: 'weekly', hour: 3, minute: 30, weekday: 6, day: 1 });
  assert.deepEqual(normalizeSchedule({ every: 'never', hour: 99, minute: -5, weekday: 9, day: 31 }), { every: 'daily', hour: 23, minute: 0, weekday: 6, day: 28 });
  assert.deepEqual(normalizeSchedule(null), { every: 'daily', hour: 0, minute: 0, weekday: 0, day: 1 });
});

test('a weekly schedule renders into the fields and confirms back unchanged', async () => {
  const schedule = { every: 'weekly', hour: 3, minute: 30, weekday: 6, day: 1 };
  let saved = null;
  const win = openScheduleEditor({ title: 'Scrub', schedule, enabled: true, onSave: (v) => { saved = v; } });
  await flush();
  assert.equal(field(win, 'nas-sched', 'every').value, 'weekly');
  assert.equal(field(win, 'nas-sched', 'hour').value, '3');
  assert.equal(field(win, 'nas-sched', 'minute').value, '30');
  assert.equal(field(win, 'nas-sched', 'weekday').value, '6');
  assert.equal(win.querySelector('[data-sched-part="time"]').hidden, false);
  assert.equal(win.querySelector('[data-sched-part="weekday"]').hidden, false);
  assert.equal(win.querySelector('[data-sched-part="day"]').hidden, true);
  assert.match(win.querySelector('[data-sched-part="preview"]').textContent, /03:30/);

  assert.deepEqual(readScheduleFields(win, 'nas-sched'), schedule);
  confirmWindow(win);
  await flush();
  assert.deepEqual(saved, { enabled: true, schedule });
  await new Promise((r) => setTimeout(r, 300));
  assert.equal(document.querySelector('tf-window'), null, 'window closed after save');
});

test('edits to cadence, day and the enabled switch come back in the saved shape', async () => {
  let saved = null;
  const win = openScheduleEditor({ title: 'Scrub', schedule: { every: 'weekly', hour: 2, minute: 0, weekday: 0, day: 1 }, allowed: ['weekly', 'monthly'], onSave: (v) => { saved = v; } });
  await flush();
  const every = field(win, 'nas-sched', 'every');
  assert.deepEqual([...every.querySelectorAll('option')].map((o) => o.value), ['weekly', 'monthly'], 'only the allowed cadences');

  selectValue(every, 'monthly');
  assert.equal(win.querySelector('[data-sched-part="weekday"]').hidden, true);
  assert.equal(win.querySelector('[data-sched-part="day"]').hidden, false);
  typeInto(field(win, 'nas-sched', 'day'), '31');
  typeInto(field(win, 'nas-sched', 'hour'), '4');
  typeInto(field(win, 'nas-sched', 'minute'), '15');
  win.querySelector('#nas-sched-enabled').checked = false;

  confirmWindow(win);
  await flush();
  assert.deepEqual(saved, { enabled: false, schedule: { every: 'monthly', hour: 4, minute: 15, weekday: 0, day: 28 } });
  win.remove();
  document.body.innerHTML = '';
});

test('a failing save keeps the window open with the error', async () => {
  const win = openScheduleEditor({ title: 'Scrub', schedule: { every: 'daily', hour: 1, minute: 0, weekday: 0, day: 1 }, onSave: () => { throw new Error('scheduler offline'); } });
  await flush();
  confirmWindow(win);
  await flush();
  await flush();
  const err = win.querySelector('#nas-sched-error');
  assert.equal(err.hidden, false);
  assert.match(err.textContent, /scheduler offline/);
  assert.ok(!win.querySelector('[data-action="confirm"]').hasAttribute('disabled'), 'retry possible');
  win.remove();
  document.body.innerHTML = '';
});

test('the snapshot schedule editor round-trips an existing NasSchedule with its retention', async () => {
  const existing = {
    scheduleId: 'sched-42', dataset: 'tank/home', enabled: true, recursive: false,
    schedule: { every: 'monthly', hour: 23, minute: 45, weekday: 0, day: 15 },
    keepFrequent: 0, keepHourly: 24, keepDaily: 7, keepWeekly: 4, keepMonthly: 12,
    protectDays: 1,
  };
  let saved = null;
  const screen = fakeScreen({ tentaNasSnapshotScheduleSetRequest: (p) => { saved = p; return { schedule: { ...p } }; } });
  const win = openSnapshotScheduleEditor(screen, { schedule: existing, datasets: [{ name: 'tank/home' }, { name: 'tank/media' }], onDone: () => {} });
  await flush();
  assert.equal(win.querySelector('#nas-ss-dataset').value, 'tank/home');
  assert.ok(win.querySelector('#nas-ss-dataset').hasAttribute('disabled'), 'dataset is fixed when editing');
  assert.equal(field(win, 'nas-ss', 'every').value, 'monthly');
  assert.equal(field(win, 'nas-ss', 'day').value, '15');
  assert.equal(win.querySelector('#nas-ss-keepDaily').value, '7');
  assert.equal(win.querySelector('#nas-ss-recursive').checked, false);
  assert.match(win.querySelector('#nas-ss-preview').textContent, /24/);

  confirmWindow(win);
  await flush();
  await flush();
  assert.deepEqual(saved, {
    scheduleId: 'sched-42', dataset: 'tank/home', enabled: true, recursive: false,
    schedule: { every: 'monthly', hour: 23, minute: 45, weekday: 0, day: 15 },
    keepFrequent: 0, keepHourly: 24, keepDaily: 7, keepWeekly: 4, keepMonthly: 12,
    protectDays: 1,
  });
  screen.dispose();
});

test('a new snapshot schedule starts hourly on the given dataset with an empty scheduleId', async () => {
  let saved = null;
  const screen = fakeScreen({ tentaNasSnapshotScheduleSetRequest: (p) => { saved = p; return { schedule: p }; } });
  const win = openSnapshotScheduleEditor(screen, { datasets: [{ name: 'tank/home' }, { name: 'tank/media' }], dataset: 'tank/media', onDone: () => {} });
  await flush();
  assert.equal(win.querySelector('#nas-ss-dataset').value, 'tank/media');
  assert.equal(field(win, 'nas-ss', 'every').value, '1h');
  assert.equal(win.querySelector('.sched-fields [data-sched-part="time"]').hidden, true, 'sub-daily cadence has no clock time');
  typeInto(win.querySelector('#nas-ss-keepHourly'), '48');
  confirmWindow(win);
  await flush();
  await flush();
  assert.equal(saved.scheduleId, '');
  assert.equal(saved.dataset, 'tank/media');
  assert.equal(saved.recursive, true);
  assert.deepEqual(saved.schedule, { every: '1h', hour: 0, minute: 0, weekday: 0, day: 1 });
  assert.equal(saved.keepHourly, 48);
  assert.equal(saved.protectDays, 0, 'a new schedule protects nothing until asked to');
  screen.dispose();
});

test('the schedule editor refuses a COARSE retention shorter than the protection it hands out', async () => {
  let saved = null;
  const screen = fakeScreen({ tentaNasSnapshotScheduleSetRequest: (p) => { saved = p; return { schedule: p }; } });
  const win = openSnapshotScheduleEditor(screen, { datasets: [{ name: 'tank/home' }], dataset: 'tank/home', onDone: () => {} });
  await flush();
  // The defaults keep 24 hourly (one day), 7 daily, 4 weekly and 3 monthly.
  typeInto(win.querySelector('#nas-ss-protectDays'), '30');
  confirmWindow(win);
  await flush();
  await flush();
  assert.equal(saved, null, 'nothing is sent while the retention cannot hold the protection');
  const err = win.querySelector('#nas-ss-error');
  assert.equal(err.hidden, false);
  assert.match(err.textContent, /dzienny/, 'the hourly tier holds nothing, so the daily one is blamed');
  assert.doesNotMatch(err.textContent, /godzinowy/);
  assert.match(err.textContent, /30 dni/);

  // 30 daily covers the window, and the complaint moves on to the next COARSE
  // tier that is still too short — 4 weekly is 28 days.
  typeInto(win.querySelector('#nas-ss-keepDaily'), '30');
  confirmWindow(win);
  await flush();
  await flush();
  assert.equal(saved, null);
  assert.match(err.textContent, /tygodniowy/);

  // Disabling that tier makes the schedule coherent; the fine tiers were never
  // part of the rule and keep their counts.
  typeInto(win.querySelector('#nas-ss-keepWeekly'), '0');
  confirmWindow(win);
  await flush();
  await flush();
  assert.equal(saved.protectDays, 30);
  assert.equal(saved.keepDaily, 30);
  assert.equal(saved.keepHourly, 24, 'the fine tier is untouched by the protection rule');
  screen.dispose();
});

test('a protection with no coarse tier enabled saves, and the editor says it holds nothing', async () => {
  let saved = null;
  const screen = fakeScreen({ tentaNasSnapshotScheduleSetRequest: (p) => { saved = p; return { schedule: p }; } });
  const win = openSnapshotScheduleEditor(screen, { datasets: [{ name: 'tank/home' }], dataset: 'tank/home', onDone: () => {} });
  await flush();
  const note = win.querySelector('#nas-ss-protect-note');
  assert.equal(note.hidden, true, 'nothing to warn about while protection is off');
  typeInto(win.querySelector('#nas-ss-keepDaily'), '0');
  typeInto(win.querySelector('#nas-ss-keepWeekly'), '0');
  typeInto(win.querySelector('#nas-ss-keepMonthly'), '0');
  typeInto(win.querySelector('#nas-ss-protectDays'), '30');
  assert.equal(note.hidden, false);
  assert.match(note.textContent, /nie zatrzyma żadnego snapshotu/);
  confirmWindow(win);
  await flush();
  await flush();
  assert.equal(saved.protectDays, 30, 'a legal schedule is still saved');
  assert.equal(saved.keepDaily, 0);
  screen.dispose();
});
