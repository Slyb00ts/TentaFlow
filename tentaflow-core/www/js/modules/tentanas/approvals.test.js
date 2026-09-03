// =============================================================================
// File: modules/tentanas/approvals.test.js
// Description: The four-eyes queue of the Tasks tab (plan-02 §5.10) against a
// fake screen: the pending list with its operation labels, the caller's own
// request offering no approve button, a second admin's approval and rejection
// sending ApprovalDecideRequest (with the APPROVER's sudo password on the
// approval only), the fleet switch, and what a parked red-path answer does to
// `followResponse`. Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, click, confirmWindow, window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

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
  // why the switch is off and what that means for protected snapshots.
  assert.equal(body.querySelector('#nas-approvals-card').hidden, true);
  const note = body.querySelector('#nas-approvals-settings').textContent;
  assert.match(note, /domyślnie wyłączone/);
  assert.match(note, /Zdjęcie ochrony snapshotu zawsze wymaga drugiego administratora/);

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

test('reportParked names the operation and the resource it would have touched', async () => {
  const win = reportParked(pending({ operation: 'share_delete', subject: 'projekty' }));
  await flush();
  assert.match(win.textContent, /Usunięcie udostępnienia z danymi/);
  assert.match(win.textContent, /projekty/);
  win.remove();
  document.body.innerHTML = '';
});
