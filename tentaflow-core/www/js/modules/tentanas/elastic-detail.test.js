// =============================================================================
// Plik: modules/tentanas/elastic-detail.test.js
// Opis: Rzeczywisty detal Elastic z kontrolowanym transportem, pomiarami null i guardami.
// Przykład: node --test --import ./js/_test-register.js js/modules/tentanas/elastic-detail.test.js
// =============================================================================

import { fakeScreen, flush, click, I18n } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { drawElasticDetail, elasticCapacity, elasticCardHtml, elasticState } from './elastic-detail.js';
import { fmtOptionalBytes, jobCanCancel } from './format.js';

const GiB = 1024 ** 3;
const disk = { diskId: 'serial-1', name: 'd1', device: '/dev/vdb', role: 'data', kind: 'hdd', filesystem: 'xfs', mountpoint: '/mnt/tentanas-branches/media/data/d1', sizeBytes: 32 * GiB, usedBytes: 0, freeBytes: 31 * GiB, mounted: true, devicePresent: true, health: 'unknown' };
function array(overrides = {}) {
  return { name: 'media', kind: 'elastic-array', state: 'active', stateDetail: '', enabled: true, filesystem: 'xfs', unionPath: '/mnt/media', createPolicy: 'mfs', dataDisks: [disk], parityDisks: [{ ...disk, diskId: 'serial-2', name: 'p1', mountpoint: '/mnt/tentanas-branches/media/parity/1' }], cacheDisks: [], usableBytes: 31 * GiB, usedBytes: 0, protection: { status: 'unknown', faultTolerance: null, movedUnsyncedBytes: null, protectedAsOf: '2026-09-07 12:00:00' }, snapraid: { parityErrors: null, configPath: '/etc/tentanas/snapraid-media.conf' }, updatedAt: '2026-09-07 12:01:00', ...overrides };
}

async function mount(value, fixtures = {}, options) {
  const screen = fakeScreen({ tentaNasElasticArrayGetRequest: { array: value }, ...fixtures }, options);
  screen.array = 'media';
  screen.openArray = (name) => { screen.array = name; };
  const body = document.createElement('div');
  document.body.appendChild(body);
  await drawElasticDetail(screen, body);
  await flush();
  return { screen, body };
}

test('null nie staje się zerem, zaś zmierzone zero pozostaje 0 B', () => {
  assert.equal(fmtOptionalBytes(null), '—');
  assert.equal(fmtOptionalBytes(undefined), '—');
  assert.equal(fmtOptionalBytes(NaN), '—');
  assert.equal(fmtOptionalBytes(0), '0 B');
  assert.equal(elasticCapacity(array({ usableBytes: null })).free, null);
  assert.equal(elasticCapacity(array()).free, 31 * GiB);
  assert.match(elasticCardHtml(array({ usedBytes: null })), /nas-unmeasured/);
  assert.doesNotMatch(elasticCardHtml(array({ usedBytes: null })), /width:0%/);
});

test('pending oznacza oczekiwanie na montowanie, nie tworzenie ani działający job, we wszystkich locale', async () => {
  const locales = [
    ['pl', 'Oczekuje na montowanie', 'W toku', 'Nie zmierzono', 'w toku'],
    ['en', 'Awaiting mount', 'In progress', 'Not measured', 'running'],
    ['de', 'Wartet auf Einhängen', 'In Bearbeitung', 'Nicht gemessen', 'läuft'],
    ['es', 'Pendiente de montaje', 'En curso', 'Sin medir', 'en curso'],
    ['fr', 'En attente de montage', 'En cours', 'Non mesuré', 'en cours'],
  ];
  try {
    for (const [language, pending, creating, unknown, runningJob] of locales) {
      await I18n.setLanguage(language);
      const value = array({ state: 'pending' });
      assert.deepEqual(elasticState(value), { label: pending, tone: 'warn' });
      assert.equal(elasticState(array({ state: 'creating' })).label, creating);
      assert.equal(elasticState(array({ state: 'unknown' })).label, unknown);
      assert.equal(I18n.t('tentanas.jobs.status_running'), runningJob);
      const card = document.createElement('div');
      card.innerHTML = elasticCardHtml(value);
      assert.equal(card.querySelector('tf-chip[dot]').getAttribute('label'), pending);
      const { screen, body } = await mount(value);
      try {
        assert.equal(body.querySelector('.grid-2 tf-chip[dot]').getAttribute('label'), pending);
        assert.ok(body.querySelector('[data-act="restore"]'));
        assert.equal(screen.jobLogs.length, 0);
        assert.deepEqual(screen.calls.map((call) => call.kind), ['tentaNasElasticArrayGetRequest']);
      } finally { screen.dispose(); }
    }
  } finally { await I18n.setLanguage('pl'); }
});

test('creating i needs_attention pochodzą z rzeczywistego kontraktu backendu', async (t) => {
  for (const state of ['creating', 'needs_attention']) await t.test(state, async () => {
    const { screen, body } = await mount(array({ state }));
    assert.equal(elasticState(array({ state })).label, state === 'creating' ? 'W toku' : 'Wymaga uwagi');
    assert.equal(elasticState(array({ state })).tone, state === 'creating' ? 'warn' : 'err');
    assert.equal(Boolean(body.querySelector('[data-act="restore"]')), state === 'needs_attention');
    screen.dispose();
  });
});

test('detal ma prawdziwe role, ścieżki, null ochrony oraz działający link dysku i powrót', async () => {
  const { screen, body } = await mount(array());
  assert.match(body.textContent, /\/mnt\/media/);
  assert.match(body.textContent, /\/dev\/vdb/);
  assert.match(body.textContent, /Nowe dane poza sync—/);
  assert.match(body.textContent, /Błędy parity—/);
  assert.equal(body.querySelectorAll('.kpi tf-stat-card').length, 3);
  assert.equal(body.querySelector('[data-act="restore"]'), null);
  for (const act of ['sync', 'scrub', 'fix', 'destroy', 'mover', 'add-disk']) assert.equal(body.querySelector(`[data-act="${act}"]`), null);
  click(body.querySelector('[data-act="disk"]'));
  assert.deepEqual(screen.openedDisks, ['serial-1']);
  click(body.querySelector('[data-act="back"]'));
  assert.equal(screen.array, null);
  screen.dispose();
});

test('zero parity nie ogłasza ochrony, a unprotected z parity nie twierdzi że go brak', async () => {
  const first = await mount(array({ parityDisks: [], protection: { status: 'protected', faultTolerance: 0 } }));
  assert.match(first.body.textContent, /Bez ochrony parity/);
  first.screen.dispose();
  const second = await mount(array({ protection: { status: 'unprotected' } }));
  assert.match(second.body.textContent, /Dane niechronione/);
  second.screen.dispose();
});

for (const initialState of ['needs_attention', 'pending']) test(`restore ${initialState} wysyła raz, śledzi job i odświeża Get`, async () => {
  let reads = 0;
  const { screen, body } = await mount(array(), {
    tentaNasElasticArrayGetRequest: () => {
      reads++;
      const active = reads > 2;
      return { array: array({ state: active ? 'active' : initialState,
        dataDisks: [{ ...disk, mounted: active, devicePresent: true }],
        parityDisks: [{ ...disk, diskId: 'serial-2', name: 'p1', mounted: active, devicePresent: true,
          mountpoint: '/mnt/tentanas-branches/media/parity/1' }] }) };
    },
    tentaNasElasticArrayRestoreRequest: { job: { jobId: 'restore-1', kind: 'elastic_restore', status: 'running' } },
  });
  const button = body.querySelector('[data-act="restore"]');
  assert.ok(button, `Restore dostępny dla ${initialState}`);
  click(button); click(button);
  await flush(); await flush();
  assert.equal(screen.calls.filter((c) => c.kind === 'tentaNasElasticArrayRestoreRequest').length, 1);
  assert.equal(screen.calls.find((c) => c.kind === 'tentaNasElasticArrayRestoreRequest').payload.name, 'media');
  assert.equal(screen.jobLogs[0].jobId, 'restore-1');
  screen.jobLogs[0].onFinish({ status: 'succeeded' });
  await flush();
  assert.ok(reads >= 3);
  assert.equal(body.querySelector('[data-act="restore"]'), null);
  assert.equal(jobCanCancel({ kind: 'elastic_create' }), false);
  assert.equal(jobCanCancel({ kind: 'elastic_restore' }), false);
  assert.equal(jobCanCancel({ kind: 'pool_scrub' }), true);
  screen.dispose();
});

test('approval i utracona odpowiedź nie udają sukcesu i nie zezwalają na ponowne wysłanie', async (t) => {
  for (const mode of ['approval', 'unknown']) await t.test(mode, async () => {
    const { screen, body } = await mount(array({ state: 'needs_attention' }), { tentaNasElasticArrayRestoreRequest: () => {
      if (mode === 'unknown') throw new Error('timeout');
      return { approval: { requestId: 'approval-1' } };
    } });
    click(body.querySelector('[data-act="restore"]'));
    await flush(); await flush();
    assert.match(body.textContent, mode === 'approval' ? /drugiego administratora/ : /Wynik żądania jest nieznany/);
    assert.ok(body.querySelector('[data-act="restore"]').hasAttribute('disabled'));
    assert.equal(screen.jobLogs.length, 0);
    click(body.querySelector('[data-act="jobs"]'));
    assert.deepEqual(screen.switchedTabs, ['jobs']);
    screen.dispose();
  });
});

test('anulowanie sudo nie wysyła restore; nieadministrator nie ma tej akcji', async () => {
  const cancelled = await mount(array({ state: 'needs_attention' }), {}, { sudo: null });
  click(cancelled.body.querySelector('[data-act="restore"]'));
  await flush();
  assert.equal(cancelled.screen.calls.filter((c) => c.kind.endsWith('RestoreRequest')).length, 0);
  assert.equal(cancelled.body.querySelector('[data-act="restore"]').hasAttribute('disabled'), false);
  cancelled.screen.dispose();
  const reader = await mount(array({ state: 'needs_attention' }), {}, { admin: false });
  assert.equal(reader.body.querySelector('[data-act="restore"]'), null);
  const disabled = await mount(array({ state: 'pending', enabled: false }));
  assert.equal(disabled.body.querySelector('[data-act="restore"]'), null);
  disabled.screen.dispose();
  reader.screen.dispose();
});

test('zmiana węzła podczas sudo nie wysyła starej mutacji; spóźniony Get nie odmalowuje powierzchni', async () => {
  const { screen, body } = await mount(array({ state: 'needs_attention' }));
  let release;
  screen.withSudo = async (fn, title, isCurrent) => { await new Promise((r) => { release = r; }); return isCurrent() ? fn(undefined) : null; };
  click(body.querySelector('[data-act="restore"]'));
  screen.currentNode = () => ({ nodeId: 'other' });
  release();
  await flush();
  assert.equal(screen.calls.filter((c) => c.kind.endsWith('RestoreRequest')).length, 0);
  screen.dispose();
  const freshBody = document.createElement('div');
  document.body.append(freshBody);
  let releaseGet;
  const stale = fakeScreen({ tentaNasElasticArrayGetRequest: () => new Promise((r) => { releaseGet = r; }) });
  stale.array = 'media';
  const pending = drawElasticDetail(stale, freshBody);
  assert.equal(stale.calls.filter((c) => c.kind.endsWith('GetRequest')).length, 1);
  freshBody.replaceChildren(document.createTextNode('nowy ekran'));
  releaseGet({ array: array() });
  await pending;
  assert.equal(freshBody.textContent, 'nowy ekran');
  freshBody.remove();
  stale.dispose();
});

test('odpowiedź obcej macierzy i HTML w diagnostyce są bezpiecznie odrzucane/renderowane', async () => {
  const wrong = await mount(array({ name: 'other' }));
  assert.ok(wrong.body.querySelector('tf-alert'));
  assert.equal(wrong.body.querySelector('.kpi'), null);
  wrong.screen.dispose();
  const safe = await mount(array({ stateDetail: '<img src=x onerror=alert(1)>' }));
  assert.equal(safe.body.querySelector('img'), null);
  assert.match(safe.body.textContent, /<img/);
  safe.screen.dispose();
});
