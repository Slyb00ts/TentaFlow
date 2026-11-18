import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  NAMES_SHOWN, usageWarning, revokeConfirmMessage,
} from './service-account-usage.js';

// Echoes the key and variables so the wording under test is deterministic.
const t = (key, vars) => (vars ? `${key}|${JSON.stringify(vars)}` : key);

test('no flow reading under the key means no warning', () => {
  assert.equal(usageWarning([], t), null);
  assert.equal(usageWarning(undefined, t), null);
});

test('the warning counts the flows and names them', () => {
  const flows = [{ flowId: 'f1', name: 'Orders in', status: 'active' }, { flowId: 'f2', name: 'Lab results', status: 'active' }];
  assert.equal(
    usageWarning(flows, t),
    'access_keys.revoke_service_account_warning|{"count":2,"names":"Orders in, Lab results"}',
  );
});

test('a long list is cut to the first names and a remainder count', () => {
  const flows = Array.from({ length: NAMES_SHOWN + 3 }, (_, i) => ({ flowId: `f${i}`, name: `Flow ${i}` }));
  const { names, count } = JSON.parse(usageWarning(flows, t).split('|')[1]);
  assert.equal(count, NAMES_SHOWN + 3);
  assert.equal(names.split(', ').length, NAMES_SHOWN + 1);
  assert.ok(names.endsWith(', +3'), names);
});

test('a nameless flow is shown by its id rather than dropped from the list', () => {
  const warning = usageWarning([{ flowId: 'f-9', name: '' }], t);
  assert.ok(warning.includes('f-9'), warning);
});

test('the confirmation asks the base question first and the warning after it', async () => {
  const asked = [];
  const message = await revokeConfirmMessage('k-1', {
    baseMessage: 'Revoke?',
    loadUsage: async (uid) => { asked.push(uid); return [{ flowId: 'f1', name: 'Orders in' }]; },
    t,
  });
  assert.deepEqual(asked, ['k-1']);
  const [first, second] = message.split('\n\n');
  assert.equal(first, 'Revoke?');
  assert.ok(second.startsWith('access_keys.revoke_service_account_warning'), second);
});

test('a key no flow uses is confirmed with the base question alone', async () => {
  const message = await revokeConfirmMessage('k-1', { baseMessage: 'Revoke?', loadUsage: async () => [], t });
  assert.equal(message, 'Revoke?');
});

test('a failed usage lookup still asks, and says the check did not happen', async () => {
  const message = await revokeConfirmMessage('k-1', {
    baseMessage: 'Revoke?',
    loadUsage: async () => { throw new Error('requires Admin session'); },
    t,
  });
  assert.equal(message, 'Revoke?\n\naccess_keys.revoke_usage_unknown');
});

test('a flow name is passed through untouched; escaping is the renderer\'s job and the text stays plain', () => {
  const hostile = '<script>alert(1)</script>';
  const warning = usageWarning([{ flowId: 'f1', name: hostile }], t);
  assert.ok(warning.includes(hostile), warning);
});
