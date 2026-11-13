// =============================================================================
// File: modules/events-correlation.test.js
// Description: The correlation id in the Events screen (TB-02.4): the visible
// "Korelacja" input is wired to the same server-side filter as the
// `?correlation=` deep link, a run's inspector shows its correlation section
// (copy, the triggering bus record with the TentaBus deep link, or the
// "not available" note, "show everything with this correlation"), and every
// label exists in all five locales.
// =============================================================================

import { window } from './tentabus/_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;
if (typeof globalThis.CSS === 'undefined' && window.CSS) globalThis.CSS = window.CSS;
if (typeof globalThis.matchMedia !== 'function') {
  globalThis.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
}
if (typeof window.matchMedia !== 'function') window.matchMedia = globalThis.matchMedia;

const { ApiBinary } = await import('../protocol/api-binary-shim.js');
const { default: EventsScreen } = await import('./events.js');

const flush = async () => { for (let i = 0; i < 6; i += 1) await new Promise((r) => setTimeout(r, 0)); };
const CORR = '0193c7e2-5b1a-7c3e-9a40-2f6d8e1b7a55';

function wireRow(seq, kind, payload, over = {}) {
  return {
    runId: 'run-1', seq, atMs: 1_000 + seq, kind, origin: 'bus', actorKind: 'system',
    correlationId: CORR, payloadJson: JSON.stringify({ kind, ...payload }), ...over,
  };
}

const requests = [];
function stubBrowse(rows) {
  ApiBinary.one = async (kind, payload) => {
    requests.push({ kind, payload });
    if (kind !== 'eventsBrowseRequest') throw new Error(`unexpected request ${kind}`);
    return { rows, nextCursor: null, scopedToSelf: false };
  };
}

async function mount(params, rows = []) {
  requests.length = 0;
  stubBrowse(rows);
  document.body.innerHTML = EventsScreen.render(params);
  await EventsScreen.mount(params);
  await flush();
}

test('the visible filter starts from the deep link and the first request carries it', async () => {
  await mount({ correlation: CORR });
  assert.equal(document.getElementById('events-correlation-input').value, CORR);
  assert.equal(requests[0].payload.correlationId, CORR);
  assert.equal(document.getElementById('events-correlation').hidden, false);
  EventsScreen.unmount();
});

test('typing a correlation reloads with it, clearing the chip removes it from the input and the request', async () => {
  await mount(null);
  assert.equal(requests[0].payload.correlationId, null);
  const input = document.getElementById('events-correlation-input');
  input.value = `  ${CORR}  `;
  input.dispatchEvent(new window.CustomEvent('change', { detail: { value: input.value } }));
  await flush();
  assert.equal(requests.at(-1).payload.correlationId, CORR, 'trimmed, sent to the server filter');
  assert.equal(document.getElementById('events-correlation').hidden, false);

  document.getElementById('events-correlation').dispatchEvent(new window.CustomEvent('remove'));
  await flush();
  assert.equal(requests.at(-1).payload.correlationId, null);
  assert.equal(input.value, '');
  EventsScreen.unmount();
});

test('a blank input is no filter', async () => {
  await mount({ correlation: CORR });
  const input = document.getElementById('events-correlation-input');
  input.value = '   ';
  input.dispatchEvent(new window.CustomEvent('change', { detail: { value: input.value } }));
  await flush();
  assert.equal(requests.at(-1).payload.correlationId, null);
  EventsScreen.unmount();
});

async function inspect(rows) {
  await mount(null, rows);
  const row = document.querySelector('.ev-row');
  assert.ok(row, 'the ledger rendered a row');
  row.click();
  await flush();
  return document.getElementById('events-inspector');
}

test('a bus run shows its correlation, the triggering record and the way into TentaBus', async () => {
  const inspector = await inspect([
    wireRow(1, 'request_started', { bus_source: { instance_id: 'tentabus-8b000003', topic: 'zgloszenia.nowe', partition: 0, offset: 18402 } }),
    wireRow(2, 'step_start', { step: 'bus_publish' }),
  ]);
  const section = inspector.querySelector('#events-correlation-section');
  assert.ok(section);
  assert.equal(section.querySelector('.ev-corr-chip').textContent, CORR);
  const link = section.querySelector('#events-open-bus');
  assert.equal(link.getAttribute('href'), `#/tentabus?instance=tentabus-8b000003&topic=zgloszenia.nowe&corr=${CORR}&partition=0&offset=18402`);
  assert.match(section.textContent, /Partycja \/ numer/);
  assert.match(section.textContent, /0 \/ 18402/);
  assert.equal(section.textContent.includes('niedostępne'), false);

  const before = requests.length;
  assert.equal(requests.at(-1).payload.correlationId, null, 'the page started unfiltered');
  inspector.querySelector('#events-show-correlation').click();
  await flush();
  assert.equal(requests.length, before + 1, 'the click reloads the page');
  assert.equal(requests.at(-1).payload.correlationId, CORR);
  assert.equal(document.getElementById('events-correlation-input').value, CORR);
  EventsScreen.unmount();
});

test('a bus run recorded without coordinates says they are not available', async () => {
  const inspector = await inspect([wireRow(1, 'request_started', {})]);
  const section = inspector.querySelector('#events-correlation-section');
  assert.ok(section);
  assert.equal(section.querySelector('#events-open-bus'), null);
  assert.match(section.textContent, /Współrzędne magistrali niedostępne/);
  EventsScreen.unmount();
});

test('a bus run whose opening row is not loaded says so instead of "not available"', async () => {
  const inspector = await inspect([wireRow(2, 'step_start', { step: 'bus_publish' }), wireRow(3, 'step_end', { step: 'bus_publish' })]);
  const section = inspector.querySelector('#events-correlation-section');
  assert.match(section.textContent, /nie są wczytane/);
  assert.doesNotMatch(section.textContent, /niedostępne/);
  EventsScreen.unmount();
});

test('a run from any other origin has no coordinates block at all', async () => {
  const inspector = await inspect([wireRow(1, 'request_started', {}, { origin: 'chat' })]);
  const section = inspector.querySelector('#events-correlation-section');
  assert.ok(section.querySelector('.ev-corr-chip'));
  assert.equal(section.querySelector('#events-open-bus'), null);
  assert.equal(section.querySelector('.ev-corr-note'), null);
  assert.equal(section.querySelector('dl'), null);
  EventsScreen.unmount();
});

test('a run with no correlation has no such section', async () => {
  const inspector = await inspect([wireRow(1, 'request_started', {}, { correlationId: null })]);
  assert.equal(inspector.querySelector('#events-correlation-section'), null);
  EventsScreen.unmount();
});

test('every label of the correlation feature exists in all five locales', () => {
  const keys = ['corr_coordinates_not_loaded', 'correlation_label', 'correlation_placeholder', 'corr_copy', 'corr_copied', 'corr_copy_failed', 'corr_topic',
    'corr_partition_number', 'corr_open_bus', 'corr_no_coordinates', 'corr_show_all'];
  for (const language of ['pl', 'en', 'de', 'es', 'fr']) {
    const bundle = JSON.parse(readFileSync(new URL(`../../i18n/${language}.json`, import.meta.url), 'utf8'));
    for (const key of keys) assert.equal(typeof bundle.events[key], 'string', `${language}: events.${key}`);
    for (const key of ['col_exec_correlation', 'exec_timeline', 'exec_no_timeline']) {
      assert.equal(typeof bundle.flows[key], 'string', `${language}: flows.${key}`);
    }
  }
});
