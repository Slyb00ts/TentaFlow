// ===== File: lib/flow-executions.test.js — the Flows execution history: correlation column and the way to the timeline =====

import { window } from '../modules/tentabus/_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { executionsTableHtml } = await import('./flow-executions.js');

const CORR = '0193c7e2-5b1a-7c3e-9a40-2f6d8e1b7a55';

function table(executions) {
  const host = document.createElement('div');
  host.innerHTML = executionsTableHtml(executions);
  return host;
}

test('a run with a correlation shows it shortened with the whole id in its title, and links to its timeline', () => {
  const host = table([{ id: 'e91c04aa', status: 'completed', startedAtEpoch: 1, completedAtEpoch: 2, correlationId: CORR }]);
  const headers = [...host.querySelectorAll('th')].map((th) => th.textContent.trim());
  assert.deepEqual(headers.slice(0, 5), ['ID', 'Status', 'Korelacja', 'Start', 'Koniec']);
  const cell = host.querySelector('.exec-correlation code');
  assert.equal(cell.textContent, '0193c7e2…7a55');
  assert.equal(cell.getAttribute('title'), CORR);
  const link = host.querySelector('.exec-timeline-link');
  assert.equal(link.getAttribute('href'), `#/events?correlation=${CORR}`);
  assert.match(link.textContent, /Oś czasu/);
});

test('a run without a correlation shows a dash and no link', () => {
  const host = table([{ id: '4d0a9e13', status: 'failed', startedAtEpoch: 1, completedAtEpoch: null, correlationId: null }]);
  assert.equal(host.querySelector('.exec-correlation').textContent, '—');
  assert.equal(host.querySelector('.exec-timeline-link'), null);
  assert.match(host.querySelector('tbody tr').lastElementChild.textContent, /brak/);
});

test('an answer from a peer without the field renders like a run without a correlation', () => {
  const host = table([{ id: 'x', status: 'completed', startedAtEpoch: 1, completedAtEpoch: 2 }]);
  assert.equal(host.querySelector('.exec-timeline-link'), null);
});
