import './tentabus/_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { severityOf, severityForAction } = await import('./audit.js');

test('a stored severity decides the chip: warning is warn, error and critical are err, info is info', () => {
  assert.equal(severityOf({ action: 'sla.calls.rejected', severity: 'warning' }), 'warn');
  assert.equal(severityOf({ action: 'sla.concurrency.rejected', severity: 'warning' }), 'warn');
  assert.equal(severityOf({ action: 'sla.breach.raised', severity: 'warning' }), 'warn');
  assert.equal(severityOf({ action: 'sla.breach.resolved', severity: 'info' }), 'info');
  assert.equal(severityOf({ action: 'service.update', severity: 'error' }), 'err');
  assert.equal(severityOf({ action: 'service.update', severity: 'Critical' }), 'err');
});

test('a row without a stored severity keeps the name-derived chip', () => {
  for (const [action, chip] of [['login.failed', 'err'], ['sla.calls.rejected', 'err'], ['rate.limit', 'warn'], ['user.create', 'ok'], ['config.change', 'info']]) {
    assert.equal(severityOf({ action }), chip);
    assert.equal(severityOf({ action, severity: '' }), chip);
    assert.equal(severityOf({ action, severity: null }), chip);
    assert.equal(severityOf({ action, severity: 'something-new' }), chip, 'an unknown stored value falls back to the name');
    assert.equal(severityForAction(action), chip);
  }
});
