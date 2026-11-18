import '../tentabus/_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const www = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..');
const sheet = (path) => readFileSync(join(www, path), 'utf8');
const hostFetch = globalThis.fetch;
globalThis.fetch = (url, init) => {
  const href = String(url);
  if (href.startsWith('/css/')) return Promise.resolve(new Response(sheet(href), { headers: { 'Content-Type': 'text/css' } }));
  return hostFetch(url, init);
};
if (typeof globalThis.CSSStyleSheet !== 'function' && window.CSSStyleSheet) globalThis.CSSStyleSheet = window.CSSStyleSheet;
if (typeof globalThis.Document !== 'function' && window.Document) globalThis.Document = window.Document;

await import('../../components/tf-table.js');
await import('../../components/tf-chip.js');
const { setRowsIfChanged } = await import('../../lib/dom-patch.js');
const { slaCellHtml, paintSlaCells } = await import('./flow-cell.js');
const { slaView } = await import('./model.js');

const policies = new Map([['p1', { id: 'p1', spec: { name: 'Standard' } }]]);
const flow = (state, inFlight) => ({ id: 'f1', name: 'Orders', slaPolicyId: 'p1', slaBrief: { state, inFlight, maxConcurrency: 4, windowUsed: 50, windowLimit: 60, governedElsewhere: false } });
const VOLATILE = new Set(['slaBrief', 'busConsumer']);
const rowOf = (f) => ({ id: f.id, name: f.name, slaHtml: slaCellHtml(f.id, slaView(f, policies)) });

test('the flow-list poll path keeps the cell nodes: unchanged rows are not reassigned, values are painted in place', async () => {
  document.body.innerHTML = '';
  const table = document.createElement('tf-table');
  table.innerHTML = '<tf-column key="name" label="N"></tf-column><tf-column key="slaHtml" renderer="html" label="SLA"></tf-column>';
  document.body.append(table);

  const poll = (f) => {
    const changed = setRowsIfChanged(table, [rowOf(f)], VOLATILE);
    paintSlaCells(table.shadowRoot, new Map([[f.id, slaView(f, policies)]]));
    return changed;
  };
  assert.equal(poll(flow('ok', 1)), true);
  const box = table.shadowRoot.querySelector('[data-sla-flow="f1"]');
  assert.ok(box, 'the cell is drawn in the table');
  const chip = box.querySelector('[data-sla-chip]');
  const detail = box.querySelector('[data-sla-detail]');
  assert.equal(chip.getAttribute('label'), 'OK');
  assert.match(detail.textContent, /1\/4/);

  assert.equal(poll(flow('warn', 3)), false, 'only volatile fields moved, so the table keeps its rows');
  assert.equal(table.shadowRoot.querySelector('[data-sla-flow="f1"]'), box);
  assert.equal(box.querySelector('[data-sla-chip]'), chip);
  assert.equal(chip.getAttribute('status'), 'warn');
  assert.match(detail.textContent, /50\/60/);
});
