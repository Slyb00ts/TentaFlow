// =============================================================================
// File: modules/tentanas/access-log.test.js
// Description: The "Dziennik dostępu" card of the Tasks tab (n15, plan-02
// §5.10) against a fake screen: the four filters narrowing the request the
// node gets, the state lines that name what is audited and — crucially — what
// is NOT (the SMB Direct path of an audited share, an NFS export whose events
// live in the host's audit log), the card hiding itself when nothing audits,
// and the forwarding dialog of §5.9 sending both targets. Runs under
// happy-dom.
// =============================================================================

import { fakeScreen, flush, click, confirmWindow, typeInto, window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { accessLogCardHtml, wireAccessLog } = await import('./access-log.js');

const event = (over = {}) => ({
  eventId: 1,
  at: '2026-09-03 12:12:03',
  share: 'projekty',
  user: 'anna',
  client: '10.10.0.24',
  operation: 'openat',
  result: 'ok',
  target: 'dane.xlsx',
  detail: '',
  ...over,
});

const answer = (over = {}) => ({
  events: [
    event(),
    event({ eventId: 2, operation: 'unlinkat', result: 'fail', target: 'raport.xlsx', detail: 'NT_STATUS_ACCESS_DENIED' }),
  ],
  total: 2,
  shares: ['archiwum', 'projekty'],
  users: ['anna', 'jan'],
  operations: ['openat', 'unlinkat'],
  audit: {
    auditedShares: ['projekty'],
    auditedExports: [],
    unauditedSmbDirect: [],
    retentionDays: 30,
    collectorState: 'ok',
    detail: '',
    collectedAt: '2026-09-03 12:13:00',
    eventCount: 2,
  },
  forward: { enabled: false, syslogTarget: '', webhookUrl: '', includeAccess: false, pending: 0, lastSentAt: null, lastError: '' },
  ...over,
});

function mount(admin = true) {
  document.body.innerHTML = '';
  const body = document.createElement('div');
  body.innerHTML = accessLogCardHtml(admin);
  document.body.appendChild(body);
  return body;
}

const latestWindow = () => [...document.querySelectorAll('tf-window')].at(-1);

test('the log renders one row per event and marks a refusal as one', async () => {
  const screen = fakeScreen({ tentaNasAccessLogRequest: answer() });
  const body = mount();
  const view = wireAccessLog(screen, body);
  await view.refresh();
  await flush();

  const table = body.querySelector('#nas-access-table');
  assert.equal(table.rows.length, 2);
  assert.equal(body.querySelector('#nas-access-count').getAttribute('label'), '2');
  assert.match(table.rows[0].user, /anna/);
  assert.match(table.rows[0].user, /10\.10\.0\.24/, 'the client address rides with the user');
  assert.match(table.rows[0].operation, /openat/);
  assert.match(table.rows[0].result, /status="ok"/);
  assert.match(table.rows[1].result, /status="err"/, 'a refusal is not painted like a success');
  assert.match(table.rows[1].result, /NT_STATUS_ACCESS_DENIED/, 'and it carries the reason smbd gave');
  assert.match(body.querySelector('#nas-access-hint').textContent, /30 dni/);
  assert.match(body.querySelector('#nas-access-state').textContent, /Audytowane udostępnienia SMB: projekty/);
  screen.dispose();
});

test('each filter narrows the request the node gets, and only that one', async () => {
  const screen = fakeScreen({ tentaNasAccessLogRequest: answer() });
  const body = mount();
  const view = wireAccessLog(screen, body);
  await view.refresh();
  await flush();

  const pick = async (id, value) => {
    body.querySelector(`#nas-access-${id}`).dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value } }));
    await flush();
  };
  await pick('share', 'projekty');
  assert.deepEqual(screen.calls.at(-1).payload, { share: 'projekty', user: '', operation: '', result: '' });
  await pick('user', 'anna');
  assert.deepEqual(screen.calls.at(-1).payload, { share: 'projekty', user: 'anna', operation: '', result: '' });
  await pick('operation', 'unlinkat');
  await pick('result', 'fail');
  assert.deepEqual(screen.calls.at(-1).payload, { share: 'projekty', user: 'anna', operation: 'unlinkat', result: 'fail' });

  // "Any" clears exactly one filter and leaves the other three.
  await pick('user', '__any');
  assert.deepEqual(screen.calls.at(-1).payload, { share: 'projekty', user: '', operation: 'unlinkat', result: 'fail' });

  // The result filter carries the node's two verdicts and nothing else, so
  // 'ok' is a value it can send and anything else is not offered.
  await pick('result', 'ok');
  assert.equal(screen.calls.at(-1).payload.result, 'ok');
  screen.dispose();
});

test('a filter value that aged out of the log stops filtering instead of returning nothing', async () => {
  let res = answer();
  const screen = fakeScreen({ tentaNasAccessLogRequest: () => res });
  const body = mount();
  const view = wireAccessLog(screen, body);
  await view.refresh();
  await flush();
  body.querySelector('#nas-access-share').dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'archiwum' } }));
  await flush();
  assert.equal(screen.calls.at(-1).payload.share, 'archiwum');

  // The node's retention dropped every `archiwum` row: the next answer no
  // longer offers that share.
  res = answer({ shares: ['projekty'] });
  await view.refresh();
  await flush();
  assert.equal(body.querySelector('#nas-access-share').value, '__any');
  await view.refresh();
  assert.equal(screen.calls.at(-1).payload.share, '', 'and the request stopped carrying it');
  screen.dispose();
});

test('the card says which paths are NOT audited and hides itself when nothing is', async () => {
  const gap = answer({
    audit: {
      ...answer().audit,
      auditedShares: ['projekty'],
      auditedExports: ['backups'],
      unauditedSmbDirect: ['projekty'],
    },
  });
  const screen = fakeScreen({ tentaNasAccessLogRequest: gap });
  const body = mount();
  const view = wireAccessLog(screen, body);
  await view.refresh();
  await flush();
  const state = body.querySelector('#nas-access-state').textContent;
  assert.match(state, /SMB Direct \(RDMA\) nie jest audytowana: projekty/);
  assert.match(state, /watchami auditd: backups/);
  assert.match(state, /dziennika audytu systemu/, 'an audited export says where its events actually go');
  assert.equal(body.querySelector('#nas-access-card').hidden, false);
  screen.dispose();

  // Nothing audited and nothing ever collected: the card keeps out of the way.
  const quiet = fakeScreen({
    tentaNasAccessLogRequest: answer({
      events: [], total: 0,
      audit: { ...answer().audit, auditedShares: [], auditedExports: [], eventCount: 0 },
    }),
  });
  const body2 = mount();
  const view2 = wireAccessLog(quiet, body2);
  await view2.refresh();
  await flush();
  assert.equal(body2.querySelector('#nas-access-card').hidden, true);
  assert.match(body2.querySelector('#nas-access-state').textContent, /Żadne udostępnienie SMB nie ma włączonego audytu/);
  quiet.dispose();
});

test('a collector that cannot read the journal says so instead of showing an empty log', async () => {
  const screen = fakeScreen({
    tentaNasAccessLogRequest: answer({
      events: [], total: 0,
      audit: {
        ...answer().audit,
        collectorState: 'unavailable',
        detail: 'journalctl is not installed',
      },
    }),
  });
  const body = mount();
  const view = wireAccessLog(screen, body);
  await view.refresh();
  await flush();
  assert.match(body.querySelector('#nas-access-state').textContent, /Zbieranie wpisów nie działa: journalctl is not installed/);
  screen.dispose();
});

test('a page shorter than the match count says how much it is not showing', async () => {
  const screen = fakeScreen({ tentaNasAccessLogRequest: answer({ total: 4213 }) });
  const body = mount();
  const view = wireAccessLog(screen, body);
  await view.refresh();
  await flush();
  assert.match(body.querySelector('#nas-access-state').textContent, /Pokazano 2 z 4213/);
  screen.dispose();
});

test('the forwarding dialog sends both targets and reports what the node refused', async () => {
  let sent = null;
  const screen = fakeScreen({
    tentaNasAccessLogRequest: answer(),
    tentaNasAlertForwardSetRequest: (p) => {
      sent = p;
      if (p.syslogTarget === 'siem.local') throw new Error("'siem.local' is not a syslog target — expected host:port");
      return answer({ forward: { enabled: true, syslogTarget: p.syslogTarget, webhookUrl: p.webhookUrl, includeAccess: p.includeAccess, pending: 3, lastSentAt: null, lastError: '' } });
    },
  });
  const body = mount();
  const view = wireAccessLog(screen, body);
  await view.refresh();
  await flush();

  click(body.querySelector('#nas-access-card [data-act="forward"]'));
  await flush();
  const dialog = latestWindow();
  dialog.querySelector('#nas-forward-enabled').checked = true;
  typeInto(dialog.querySelector('#nas-forward-syslog'), 'siem.local');
  confirmWindow(dialog);
  await flush();
  await flush();
  // The node refused the target, and the dialog stays open with its reason.
  assert.equal(sent.syslogTarget, 'siem.local');
  assert.ok(dialog.isConnected, 'a refused target keeps the dialog open');
  assert.match(dialog.querySelector('#nas-forward-error').textContent, /expected host:port/);

  typeInto(dialog.querySelector('#nas-forward-syslog'), 'siem.local:514');
  typeInto(dialog.querySelector('#nas-forward-webhook'), 'https://siem.local/hooks/tentanas');
  dialog.querySelector('#nas-forward-include').checked = true;
  confirmWindow(dialog);
  await flush();
  await flush();
  assert.deepEqual(sent, {
    enabled: true,
    syslogTarget: 'siem.local:514',
    webhookUrl: 'https://siem.local/hooks/tentanas',
    includeAccess: true,
  });
  // The saved answer repaints the card without a second request.
  assert.match(body.querySelector('#nas-access-state').textContent, /Przekazywanie włączone: siem\.local:514, https:\/\/siem\.local\/hooks\/tentanas \(w kolejce: 3\)/);
  screen.dispose();
});

test('a viewer sees the log but not the forwarding button', async () => {
  const screen = fakeScreen({ tentaNasAccessLogRequest: answer() }, { admin: false });
  const body = mount(false);
  const view = wireAccessLog(screen, body);
  await view.refresh();
  await flush();
  assert.equal(body.querySelector('#nas-access-card [data-act="forward"]'), null);
  assert.equal(body.querySelector('#nas-access-table').rows.length, 2);
  screen.dispose();
});
