// ============ File: tf-kanban.summary.test.js — Authoritative aggregate counts with paged cards ============

import { window } from '../sdk-runtime/_dom-test-harness.js';
import test, { after } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

globalThis.fetch = async (url) => new Response(readFileSync(new URL(`../..${url}`, import.meta.url)), { headers: { 'Content-Type': 'text/css' } });
globalThis.Document = window.Document;
globalThis.CSS = window.CSS;
await import('./tf-kanban.js');
after(async () => window.happyDOM.close());

test('column counts and WIP warnings use the current aggregate while placeholders use loaded cards', async () => {
  const board = document.createElement('tf-kanban');
  document.body.appendChild(board);
  board.columns = [{ id: 'todo', label: 'To do', count: 78, limit: 10 }, { id: 'done', label: 'Done', count: 0 }];
  board.cards = [{ id: 'loaded', column: 'todo', title: 'First page task' }];
  const columns = board.shadowRoot.querySelectorAll('.col');
  assert.equal(columns[0].querySelector('.cnt').textContent, '78/10');
  assert.ok(columns[0].classList.contains('over-limit'));
  assert.equal(columns[0].querySelector('.col-empty').hidden, true);
  assert.match(columns[0].getAttribute('aria-label'), /78 cards/);
  board.columns = [{ id: 'todo', label: 'To do', count: 2 }, { id: 'done', label: 'Done', count: 9 }];
  assert.equal(board.shadowRoot.querySelectorAll('.cnt')[0].textContent, '2');
  assert.equal(board.shadowRoot.querySelectorAll('.cnt')[1].textContent, '9');
  assert.equal(board.shadowRoot.querySelectorAll('.col-empty')[1].hidden, false);
  board.columns = [{ id: 'todo', label: 'To do' }];
  assert.equal(board.shadowRoot.querySelector('.cnt').textContent, '1');
  await window.happyDOM.waitUntilComplete();
  board.remove();
});
