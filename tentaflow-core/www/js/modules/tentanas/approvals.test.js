// =============================================================================
// File: modules/tentanas/approvals.test.js
// Description: The four-eyes queue of the Tasks tab (plan-02 §5.10) against a
// fake screen: the pending list with its operation labels, the caller's own
// request offering no approve button, a second admin's approval and rejection
// sending ApprovalDecideRequest (with the APPROVER's sudo password on the
// approval only), the fleet switch, and what a parked red-path answer does to
// `followResponse`. Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, click, confirmWindow, window, WWW_ROOT } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const { approvalsCardHtml, wireApprovals, operationLabel, reportParked } = await import('./approvals.js');
const { followResponse } = await import('./dialogs.js');

const inAnHour = () => new Date(Date.now() + 3600_000).toISOString();

const pending = (over = {}) => ({
  requestId: 'r-1',
  operation: 'pool_destroy',
  subject: 'tank',
  detail: 'niszczy pulę tank i wszystkie jej datasety',
  status: 'pending',
  requestedBy: 'u-anna',
  requestedAt: new Date(Date.now() - 60_000).toISOString(),
  expiresAt: inAnHour(),
  decidedBy: null,
  decidedAt: null,
  decisionNote: '',
  decisionJobId: null,
  isOwnRequest: false,
  ...over,
});

const settings = (over = {}) => ({ enabled: true, ttlHours: 24, adminCount: 2, byDefault: true, ...over });

function mount(admin = true) {
  const body = document.createElement('div');
  body.innerHTML = approvalsCardHtml(admin);
  document.body.appendChild(body);
  return body;
}

/** The confirm dialog the decision opens, then its confirm action. */
async function confirmDecision(note = '') {
  await flush();
  const win = [...document.querySelectorAll('tf-window')].pop();
  assert.ok(win, 'the decision dialog is open');
  if (note) win.querySelector('#nas-approval-note').value = note;
  confirmWindow(win);
  await flush();
  return win;
}

test('the pending list names the operation, who asked and when it expires', async () => {
  const screen = fakeScreen({ tentaNasApprovalsListRequest: { approvals: [pending()], settings: settings() } });
  const body = mount();
  const { refresh } = wireApprovals(screen, body);
  await refresh();
  await flush();

  const table = body.querySelector('#nas-approvals-table');
  assert.equal(table.rows.length, 1);
  assert.match(table.rows[0].operation, /Zniszczenie puli/);
  assert.match(table.rows[0].operation, /niszczy pulę tank/);
  assert.match(table.rows[0].requested, /u-anna/);
  assert.match(table.rows[0].status, /label="czeka"/);
  assert.equal(body.querySelector('#nas-approvals-count').getAttribute('label'), '1');
  assert.equal(body.querySelector('#nas-approvals-card').hidden, false);
  assert.match(body.querySelector('#nas-approvals-settings').textContent, /2 administratorzy/);

  assert.equal(operationLabel('snapshot_release'), 'Zdjęcie ochrony snapshotu');
  assert.equal(operationLabel('nonsense'), 'Operacja');
  screen.dispose();
});

// Wave-4 critic minor 12: a config import from an export the fleet has no
// node name for arrives with no subject (the node drops the id at the read
// boundary) — the row says nothing rather than a blank or an id.
test('a config import with no node name shows no subject, not an id', async () => {
  const screen = fakeScreen({
    tentaNasApprovalsListRequest: { approvals: [pending({ operation: 'config_import', subject: '' })], settings: settings() },
  });
  const body = mount();
  const { refresh } = wireApprovals(screen, body);
  await refresh();
  await flush();
  const row = body.querySelector('#nas-approvals-table').rows[0];
  assert.equal(row.subject, '—');
  assert.match(row.operation, /Import konfiguracji/);
  screen.dispose();
});

// The owner's rule (format.js `jobAuthor`): a machine id is never shown as a
// name. `requestedBy`/`decidedBy` used to be printed raw, so an account the
// server could not resolve (deleted, or one that exists only on the node that
// forwarded the request) showed its UUID in the open. Both columns must route
// through `jobAuthor` — the UUID becomes "nieznane konto" with the id moved to
// a tooltip, and a system author (the scheduler) reads as its translated name
// with no tooltip at all.
test('an unresolved account reads "nieznane konto" with the id in a tooltip; the scheduler is not a UUID', async () => {
  const uuid = '3fa85f64-5717-4562-b3fc-2c963f66afa6';
  const screen = fakeScreen({
    tentaNasApprovalsListRequest: {
      approvals: [pending({ requestId: 'r-uuid', requestedBy: uuid, status: 'approved', decidedBy: 'scheduler' })],
      settings: settings(),
    },
  });
  const body = mount();
  const { refresh } = wireApprovals(screen, body);
  await refresh();
  await flush();

  const table = body.querySelector('#nas-approvals-table');
  // The raw id lives ONLY in a title= tooltip, never as visible text.
  const wrap = document.createElement('div');
  wrap.innerHTML = table.rows[0].requested;
  assert.doesNotMatch(wrap.textContent, new RegExp(uuid), 'the id is not visible text');
  assert.match(wrap.textContent, /nieznane konto/);
  assert.equal(wrap.querySelector('.tf-table__cell-sub').getAttribute('title'), uuid);
  // The scheduler is a system author, not an unresolved account: it is
  // translated and gets no tooltip.
  assert.match(table.rows[0].status, /harmonogram/);
  assert.doesNotMatch(table.rows[0].status, /title="scheduler"/);
  screen.dispose();
});

test('the author of a request gets no approve button, only the reason why', async () => {
  const screen = fakeScreen({
    tentaNasApprovalsListRequest: { approvals: [pending({ isOwnRequest: true })], settings: settings() },
  });
  const body = mount();
  const { refresh } = wireApprovals(screen, body);
  await refresh();
  await flush();

  const table = body.querySelector('#nas-approvals-table');
  const actions = table.rowActions(table.rows[0]);
  assert.equal(actions.querySelectorAll('tf-button').length, 0, 'nothing to click on your own request');
  assert.match(actions.textContent, /Twoje zgłoszenie/);
  screen.dispose();
});

test('etykiety Elastic opisują operację zatwierdzenia', () => {
  assert.equal(operationLabel('elastic_create'), 'Tworzenie Elastic Array');
  assert.equal(operationLabel('elastic_restore'), 'Przywracanie montowania Elastic Array');
  // Uzbrojenie harmonogramu jest osobną operacją od pojedynczego przebiegu —
  // bez tej pozycji etykieta degraduje się do ogólnego „Operacja", które nie
  // mówi zatwierdzającemu nic o tym, na co się zgadza.
  assert.equal(operationLabel('elastic_schedule'), 'Uzbrojenie harmonogramu Elastic');
  assert.notEqual(operationLabel('elastic_schedule'), operationLabel('nonsense'));
});

// EVERY operation the node can park has a label, checked against the Rust
// constants rather than against a list written here.
//
// The hand-written test above named three operations and passed while
// `elastic_fix` (a repair that overwrites blocks), `elastic_add_disk` (a disk
// gets formatted) and `elastic_destroy` (an array stops serving) all rendered
// as the generic „Operacja" — the three where the approving admin most needs
// to be told what they are agreeing to. A list of constants cannot be kept in
// step by hand, so the source is the list.
test('every operation the node can park has its own label', () => {
  const source = readFileSync(join(WWW_ROOT, '..', 'src/tentanas/approvals.rs'), 'utf8');
  const declared = [...source.matchAll(/pub const OP_[A-Z_]+: &str = "([a-z_]+)"/g)].map((m) => m[1]);
  assert.ok(declared.length >= 13, `parsed ${declared.length} operations: ${declared}`);
  const generic = operationLabel('nonsense');
  for (const op of declared) {
    // Disk replacement is WITHDRAWN: the node refuses the request before
    // anything is parked, so it is the one operation with no label, and this
    // assertion is what makes that a decision rather than an omission.
    if (op === 'elastic_replace_disk') {
      assert.equal(operationLabel(op), generic, 'a withdrawn operation needs no label');
      continue;
    }
    assert.notEqual(operationLabel(op), generic, `${op} degrades to the generic label`);
    assert.ok(!operationLabel(op).startsWith('approvals.'), `${op} has no string in the bundle`);
  }
});

for (const change of ['node', 'surface', 'sudo']) {
  test(`zatwierdzenie Elastic nie wysyła starego requestId po zmianie ${change}`, async () => {
    const screen = fakeScreen({ tentaNasApprovalsListRequest: { approvals: [pending({ operation: 'elastic_create' })], settings: settings() } });
    const body = mount();
    const { refresh } = wireApprovals(screen, body);
    await refresh();
    await flush();
    const table = body.querySelector('#nas-approvals-table');
    let releaseSudo;
    if (change === 'sudo') screen.withSudo = async (fn, title, isCurrent) => {
      assert.equal(typeof isCurrent, 'function');
      await new Promise((resolve) => { releaseSudo = resolve; });
      return isCurrent() ? fn(null) : null;
    };
    click(table.rowActions(table.rows[0]).querySelector('tf-button'));
    await flush();
    if (change === 'node') screen.currentNode = () => ({ nodeId: 'other' });
    if (change === 'surface') body.innerHTML = approvalsCardHtml(true);
    await confirmDecision();
    if (change === 'sudo') {
      assert.equal(typeof releaseSudo, 'function');
      screen.currentNode = () => ({ nodeId: 'other' });
      releaseSudo();
      await flush();
    }
    assert.equal(screen.calls.filter((call) => call.kind === 'tentaNasApprovalDecideRequest').length, 0);
    screen.dispose();
  });
}

test('a second admin approves: the decision carries the approver sudo password and the list comes back', async () => {
  let sent = null;
  const after = { approvals: [pending({ status: 'approved', decidedBy: 'u-piotr', decisionJobId: 'job-9' })], settings: settings() };
  let executed = 0;
  const screen = fakeScreen({
    tentaNasApprovalsListRequest: { approvals: [pending()], settings: settings() },
    tentaNasApprovalDecideRequest: (p) => { sent = p; return after; },
  });
  const body = mount();
  const { refresh } = wireApprovals(screen, body, { onExecuted: () => { executed += 1; } });
  await refresh();
  await flush();

  const table = body.querySelector('#nas-approvals-table');
  const approve = table.rowActions(table.rows[0]).querySelector('tf-button');
  assert.equal(approve.textContent, 'Zatwierdź');
  click(approve);
  await confirmDecision('pula wycofana z produkcji');
  await flush();

  assert.deepEqual(sent, {
    requestId: 'r-1',
    approve: true,
    note: 'pula wycofana z produkcji',
    sudoPassword: 'hunter2',
  });
  assert.equal(executed, 1, 'the tab reloads the jobs the approval started');
  assert.match(table.rows[0].status, /label="zatwierdzona"/);
  assert.match(table.rows[0].status, /u-piotr/);
  screen.dispose();
});

test('a rejection sends approve:false and never asks for a password', async () => {
  let sent = null;
  const screen = fakeScreen({
    tentaNasApprovalsListRequest: { approvals: [pending()], settings: settings() },
    tentaNasApprovalDecideRequest: (p) => { sent = p; return { approvals: [], settings: settings() }; },
  });
  const body = mount();
  const { refresh } = wireApprovals(screen, body);
  await refresh();
  await flush();

  const table = body.querySelector('#nas-approvals-table');
  const reject = [...table.rowActions(table.rows[0]).querySelectorAll('tf-button')][1];
  assert.equal(reject.textContent, 'Odrzuć');
  click(reject);
  await confirmDecision('pula jest w użyciu');
  await flush();

  assert.equal(sent.approve, false);
  assert.equal(sent.note, 'pula jest w użyciu');
  assert.equal(sent.sudoPassword, undefined, 'rejecting runs nothing on the node');
  assert.equal(table.rows.length, 0);
  screen.dispose();
});

test('cancelling the decision dialog sends nothing', async () => {
  let sent = null;
  const screen = fakeScreen({
    tentaNasApprovalsListRequest: { approvals: [pending()], settings: settings() },
    tentaNasApprovalDecideRequest: (p) => { sent = p; return { approvals: [], settings: settings() }; },
  });
  const body = mount();
  const { refresh } = wireApprovals(screen, body);
  await refresh();
  await flush();

  const table = body.querySelector('#nas-approvals-table');
  click(table.rowActions(table.rows[0]).querySelector('tf-button'));
  await flush();
  const win = [...document.querySelectorAll('tf-window')].pop();
  win.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'cancel' }, cancelable: true }));
  await flush();
  assert.equal(sent, null);
  screen.dispose();
});

test('the fleet switch shows where its value came from and saves the new one', async () => {
  let sent = null;
  const screen = fakeScreen({
    tentaNasApprovalsListRequest: { approvals: [], settings: settings({ enabled: false, adminCount: 1, byDefault: true }) },
    tentaNasApprovalSettingsSetRequest: (p) => { sent = p; return { approvals: [], settings: settings({ enabled: true, adminCount: 1, byDefault: false }) }; },
  });
  const body = mount();
  const { refresh } = wireApprovals(screen, body);
  await refresh();
  await flush();

  // One admin, nothing waiting: the card keeps out of the way, and it says
  // why the switch is off and what that means for protected snapshots — the
  // owner's 2026-09-03 ruling, so the note names the red path the release
  // takes instead of promising a protection nobody could ever lift.
  assert.equal(body.querySelector('#nas-approvals-card').hidden, true);
  const note = body.querySelector('#nas-approvals-settings').textContent;
  assert.match(note, /domyślnie wyłączone/);
  assert.match(note, /idzie zwykłą czerwoną ścieżką/);
  assert.match(note, /Gdy pojawi się drugi administrator/);

  const toggle = body.querySelector('#nas-approvals-enabled');
  assert.equal(toggle.checked, false);
  assert.equal(body.querySelector('#nas-approvals-ttl').value, '24');
  toggle.checked = true;
  toggle.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { checked: true } }));
  await flush();
  assert.deepEqual(sent, { enabled: true, ttlHours: 24 }, 'the switch resends the TTL it is showing');
  assert.equal(body.querySelector('#nas-approvals-card').hidden, false, 'switched on, the queue is visible even when empty');

  // …and the TTL field resends the switch, so neither control resets the other.
  const ttl = body.querySelector('#nas-approvals-ttl');
  ttl.value = '6';
  ttl.dispatchEvent(new window.CustomEvent('change', { bubbles: true }));
  await flush();
  assert.deepEqual(sent, { enabled: true, ttlHours: 6 });
  screen.dispose();
});

test('a viewer sees the queue but cannot decide and has no switch', async () => {
  const screen = fakeScreen(
    { tentaNasApprovalsListRequest: { approvals: [pending()], settings: settings() } },
    { admin: false },
  );
  const body = mount(false);
  const { refresh } = wireApprovals(screen, body);
  await refresh();
  await flush();
  assert.equal(body.querySelector('#nas-approvals-enabled'), null);
  const table = body.querySelector('#nas-approvals-table');
  assert.equal(table.rows.length, 1);
  assert.equal(table.rowActions(table.rows[0]).querySelectorAll('tf-button').length, 0);
  screen.dispose();
});

test('a parked answer reports that nothing ran instead of opening a job log', async () => {
  const screen = fakeScreen({});
  let done = 0;
  followResponse(screen, { approval: pending({ requestId: 'r-7' }) }, () => { done += 1; }, 'nie pokazuj tego');
  await flush();
  assert.equal(screen.jobLogs.length, 0, 'there is no job — nothing executed');
  assert.equal(done, 1, 'the view still refreshes');
  const win = [...document.querySelectorAll('tf-window')].pop();
  assert.match(win.textContent, /Nic jeszcze nie zostało wykonane/);
  assert.match(win.textContent, /Zniszczenie puli/);
  screen.dispose();
});

// The purest form of the defect the owner complained about: the settings line
// is rebuilt from byte-identical markup on every 30 s poll, so it flickers for
// nothing and a selection inside it cannot survive one tick.
test('an unchanged poll leaves the approvals settings line alone', async () => {
  const screen = fakeScreen({ tentaNasApprovalsListRequest: { approvals: [pending()], settings: settings() } });
  const body = mount();
  const { refresh } = wireApprovals(screen, body);
  await refresh();
  await flush();
  const el = body.querySelector('#nas-approvals-settings');
  const span = el.querySelector('.text-3');
  assert.ok(span, 'the line names where the rule comes from');

  await refresh();
  await flush();
  assert.equal(el.querySelector('.text-3') === span, true, 'the settings line is not rewritten every poll');
  screen.dispose();
});

// …and it still has to follow the setting when it really moves.
test('a changed approvals setting is still repainted', async () => {
  let adminCount = 2;
  const screen = fakeScreen({ tentaNasApprovalsListRequest: () => ({ approvals: [pending()], settings: settings({ adminCount }) }) });
  const body = mount();
  const { refresh } = wireApprovals(screen, body);
  await refresh();
  await flush();
  const el = body.querySelector('#nas-approvals-settings');
  assert.doesNotMatch(el.textContent, /Tylko jeden administrator/, 'two admins need no single-admin warning');

  adminCount = 1;
  await refresh();
  await flush();
  assert.match(el.textContent, /Tylko jeden administrator/, 'dropping to one admin is reported');
  screen.dispose();
});

test('reportParked names the operation and the resource it would have touched', async () => {
  const win = reportParked(pending({ operation: 'share_delete', subject: 'projekty' }));
  await flush();
  assert.match(win.textContent, /Usunięcie udostępnienia z danymi/);
  assert.match(win.textContent, /projekty/);
  win.remove();
  document.body.innerHTML = '';
});

// tf-table drops a hide-below outside its breakpoint allowlist, so the
// "expires" column asked to hide at 1000 px never hid at all.
test('every hide-below of the approvals table is a breakpoint tf-table honours', async () => {
  const screen = fakeScreen({ tentaNasApprovalsListRequest: { approvals: [pending()], settings: settings() } });
  try {
    const body = mount();
    const { refresh } = wireApprovals(screen, body);
    await refresh();
    await flush();
    const table = body.querySelector('#nas-approvals-table');
    const wanted = [...table.querySelectorAll('tf-column[hide-below]')].map((c) => c.getAttribute('hide-below'));
    const got = [...table.shadowRoot.querySelectorAll('thead th')].flatMap((th) => [...th.classList]
      .filter((c) => c.startsWith('tf-table__col--hide-below-')).map((c) => c.slice('tf-table__col--hide-below-'.length)));
    assert.deepEqual(got, wanted);
    assert.equal(wanted.length, 2);
    body.remove();
  } finally {
    screen.dispose();
  }
});
