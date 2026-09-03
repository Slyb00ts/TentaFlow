// =============================================================================
// File: modules/tentanas/share-users.test.js
// Description: The share-users dialog against a fake screen: the form
// validator (name pattern, taken names, minimum password length, repeat
// mismatch), the save gate on a mismatch, the ShareUserSet payload through
// sudo with the password kept off the console and out of the DOM, the
// password-only edit form and the delete confirmation. Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, click, typeInto, window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { openShareUsersDialog, validateUserForm, shareUserNameValid, PASSWORD_MIN } = await import('./share-users.js');

const users = [
  { name: 'anna', description: 'Anna K.', createdAt: '2026-08-01T10:00:00Z', shares: ['dokumenty'] },
  { name: 'backup', description: '', createdAt: null, shares: [] },
];

/** Captures everything written to the console while `fn` runs so a leaked secret is caught. */
async function withConsoleCapture(fn) {
  const lines = [];
  const original = {};
  for (const m of ['log', 'info', 'warn', 'error', 'debug']) {
    original[m] = console[m];
    console[m] = (...args) => lines.push(args.map((a) => (typeof a === 'string' ? a : JSON.stringify(a))).join(' '));
  }
  try { await fn(); } finally { Object.assign(console, original); }
  return lines;
}

test('user names follow the POSIX pattern and the validator reports each field', () => {
  assert.ok(shareUserNameValid('anna'));
  assert.ok(shareUserNameValid('_svc-backup1'));
  assert.ok(!shareUserNameValid('Anna'));
  assert.ok(!shareUserNameValid('1abc'));
  assert.ok(!shareUserNameValid(''));

  assert.deepEqual(Object.keys(validateUserForm({ name: 'nowy', password: 'correcthorse', repeat: 'correcthorse', existing: ['anna'] })), []);
  const bad = validateUserForm({ name: 'anna', password: 'short', repeat: 'shorter', existing: ['anna'] });
  assert.match(bad.name, /zajęta|istnieje/i);
  assert.match(bad.password, new RegExp(String(PASSWORD_MIN)));
  assert.ok(!bad.repeat, 'a too-short password is reported once, on the password field');
  assert.ok(validateUserForm({ name: 'nowy', password: 'correcthorse', repeat: 'correcthorsE', existing: [] }).repeat, 'a mismatch is reported on the repeat field');
  assert.equal(PASSWORD_MIN, 8);

  // Editing keeps the name and only checks the password when one is typed.
  assert.deepEqual(Object.keys(validateUserForm({ name: 'anna', password: '', repeat: '', existing: ['anna'], editing: true })), []);
  assert.deepEqual(Object.keys(validateUserForm({ name: 'anna', password: 'abc', repeat: '', existing: ['anna'], editing: true })), ['password']);
});

test('the list shows users with their shares and only admins get the actions', async () => {
  const screen = fakeScreen({}, { admin: false });
  const win = openShareUsersDialog(screen, { users });
  await flush();
  const table = win.querySelector('#nas-su-table');
  assert.equal(table.rows.length, 2);
  assert.equal(table.rows[0]._user.name, 'anna');
  assert.match(table.rows[0].name, /anna/);
  assert.match(table.rows[0].name, /Anna K\./);
  assert.match(String(table.rows[0].shares), /dokumenty/);
  assert.ok(win.querySelector('[data-act="add"]').hasAttribute('disabled'));
  screen.dispose();
});

test('a password mismatch blocks the save and the payload never carries the repeat or reaches the console', async () => {
  const created = { name: 'nowy', description: 'Nowy', createdAt: '2026-09-01T00:00:00Z', shares: [] };
  const screen = fakeScreen({ tentaNasShareUserSetRequest: { users: [...users, created] } });
  let published = null;
  const win = openShareUsersDialog(screen, { users, onChange: (list) => { published = list; } });
  await flush();

  click(win.querySelector('[data-act="add"]'));
  await flush();
  const save = win.querySelector('[data-act="form-save"]');
  const pass = win.querySelector('#nas-su-pass');
  const repeat = win.querySelector('#nas-su-repeat');
  assert.equal(pass.getAttribute('type'), 'password');
  assert.equal(repeat.getAttribute('type'), 'password');
  assert.ok(save.hasAttribute('disabled'));

  typeInto(win.querySelector('#nas-su-name'), 'nowy');
  typeInto(win.querySelector('#nas-su-desc'), 'Nowy');
  typeInto(pass, 'correcthorse');
  typeInto(repeat, 'correcthorsE');
  assert.ok(save.hasAttribute('disabled'), 'a mismatch keeps the save disabled');
  assert.ok(repeat.hasAttribute('error'));
  click(save);
  await flush();
  assert.equal(screen.calls.length, 0, 'nothing is sent while the passwords differ');

  const logs = await withConsoleCapture(async () => {
    typeInto(repeat, 'correcthorse');
    assert.ok(!save.hasAttribute('disabled'));
    click(save);
    await flush();
    await flush();
  });

  assert.equal(screen.calls.length, 1);
  assert.deepEqual(screen.calls[0], {
    kind: 'tentaNasShareUserSetRequest',
    payload: { name: 'nowy', description: 'Nowy', password: 'correcthorse', sudoPassword: 'hunter2' },
  });
  assert.ok(!logs.some((l) => l.includes('correcthorse')), 'the password never reaches the console');
  assert.ok(!win.innerHTML.includes('correcthorse'), 'the password is not echoed into the markup');
  assert.equal(published.length, 3, 'the caller gets the refreshed list');
  assert.ok(win.querySelector('#nas-su-table'), 'the dialog is back on the list');
  assert.equal(win.querySelector('#nas-su-table').rows.length, 3);
  screen.dispose();
});

test('setting a password for an existing user keeps the name read-only and omits an empty description change', async () => {
  const screen = fakeScreen({ tentaNasShareUserSetRequest: { users } });
  const win = openShareUsersDialog(screen, { users });
  await flush();
  const table = win.querySelector('#nas-su-table');
  const actions = table.rowActions(table.rows[1]);
  click(actions.querySelector('[data-act="password"]'));
  await flush();

  const name = win.querySelector('#nas-su-name');
  assert.equal(name.value, 'backup');
  assert.ok(name.hasAttribute('readonly'));
  typeInto(win.querySelector('#nas-su-pass'), 'tooshort');
  typeInto(win.querySelector('#nas-su-repeat'), 'tooshort');
  assert.ok(!win.querySelector('[data-act="form-save"]').hasAttribute('disabled'), 'exactly eight characters is the minimum');
  typeInto(win.querySelector('#nas-su-pass'), 'seven77');
  typeInto(win.querySelector('#nas-su-repeat'), 'seven77');
  assert.ok(win.querySelector('[data-act="form-save"]').hasAttribute('disabled'), 'seven characters is too short');
  typeInto(win.querySelector('#nas-su-pass'), 'longenough');
  typeInto(win.querySelector('#nas-su-repeat'), 'longenough');
  click(win.querySelector('[data-act="form-save"]'));
  await flush();
  await flush();
  assert.deepEqual(screen.calls[0].payload, { name: 'backup', description: '', password: 'longenough', sudoPassword: 'hunter2' });
  screen.dispose();
});

test('a refused sudo prompt leaves the form open with nothing sent', async () => {
  const screen = fakeScreen({ tentaNasShareUserSetRequest: { users } }, { sudo: null });
  const win = openShareUsersDialog(screen, { users });
  await flush();
  click(win.querySelector('[data-act="add"]'));
  await flush();
  typeInto(win.querySelector('#nas-su-name'), 'nowy');
  typeInto(win.querySelector('#nas-su-pass'), 'correcthorse');
  typeInto(win.querySelector('#nas-su-repeat'), 'correcthorse');
  click(win.querySelector('[data-act="form-save"]'));
  await flush();
  await flush();
  assert.equal(screen.calls.length, 0);
  assert.ok(win.querySelector('#nas-su-pass'), 'the form stays open');
  assert.ok(!win.querySelector('[data-act="form-save"]').hasAttribute('disabled'));
  screen.dispose();
});

test('deleting a user asks for confirmation and sends ShareUserDelete through sudo', async () => {
  const screen = fakeScreen({ tentaNasShareUserDeleteRequest: { users: [users[0]] } });
  const win = openShareUsersDialog(screen, { users });
  await flush();
  const table = win.querySelector('#nas-su-table');
  click(table.rowActions(table.rows[1]).querySelector('[data-act="delete"]'));
  await flush();

  const confirm = [...document.querySelectorAll('tf-window')].find((w) => w !== win);
  assert.ok(confirm, 'a confirmation window opened');
  confirm.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
  await new Promise((r) => setTimeout(r, 300));
  await flush();

  assert.deepEqual(screen.calls, [{ kind: 'tentaNasShareUserDeleteRequest', payload: { name: 'backup', sudoPassword: 'hunter2' } }]);
  assert.equal(win.querySelector('#nas-su-table').rows.length, 1);
  screen.dispose();
});
