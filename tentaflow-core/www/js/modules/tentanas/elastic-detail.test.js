// =============================================================================
// Plik: modules/tentanas/elastic-detail.test.js
// Opis: Rzeczywisty detal Elastic z kontrolowanym transportem, pomiarami null i guardami.
// Przykład: node --test --import ./js/_test-register.js js/modules/tentanas/elastic-detail.test.js
// =============================================================================

import { fakeScreen, flush, click, confirmWindow, typeInto, I18n } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { drawElasticDetail, elasticCapacity, elasticCardHtml, elasticState } from './elastic-detail.js';
import { fmtOptionalBytes, jobCanCancel } from './format.js';

const GiB = 1024 ** 3;
// `name` is the SLOT and `device` the by-uuid path the node mounts — the
// production shapes (`db.rs` `elastic_arrays`). `diskName` is the kernel name
// the node looked up in its inventory, and the only one the screen may show.
const BY_UUID = '/dev/disk/by-uuid/2a7f1c30-9e64-4b8d-a5f2-71c3e806d914';
const disk = { diskId: 'serial-1', name: 'd1', diskName: 'vdb', device: BY_UUID, role: 'data', kind: 'hdd', filesystem: 'xfs', mountpoint: '/mnt/tentanas-branches/media/data/d1', sizeBytes: 32 * GiB, usedBytes: 0, freeBytes: 31 * GiB, mounted: true, devicePresent: true, health: 'unknown' };
function array(overrides = {}) {
  return { name: 'media', kind: 'elastic-array', state: 'active', stateDetail: '', enabled: true, parityRunAvailable: true, filesystem: 'xfs', unionPath: '/mnt/media', createPolicy: 'mfs', dataDisks: [disk], parityDisks: [{ ...disk, diskId: 'serial-2', name: 'parity1', diskName: 'vdc', index: 1, mountpoint: '/mnt/tentanas-branches/media/parity/1' }], cacheDisks: [], usableBytes: 31 * GiB, usedBytes: 0, protection: { status: 'unknown', faultTolerance: null, movedUnsyncedBytes: null, protectedAsOf: '2026-09-07 12:00:00' }, snapraid: { parityErrors: null, configPath: '/etc/tentanas/snapraid-media.conf' }, updatedAt: '2026-09-07 12:01:00', ...overrides };
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
  // n11 names each disk by its kernel name. The slot and the filesystem UUID
  // are keys, not names: the UUID path is the name's tooltip, never text.
  const cells = [...body.querySelectorAll('.disk-cell[data-disk]')];
  assert.deepEqual(cells.map((c) => c.querySelector('.dc-name').textContent.trim()), ['vdb', 'vdc']);
  assert.doesNotMatch(body.textContent, /by-uuid|2a7f1c30/);
  assert.equal(cells[0].querySelector('.dc-name [title]').getAttribute('title'), `${BY_UUID} · /mnt/tentanas-branches/media/data/d1`);
  assert.equal(cells[0].querySelector('.disk-kind').textContent, 'HDD');
  assert.match(body.textContent, /Nowe dane poza sync—/);
  assert.match(body.textContent, /Błędy parity—/);
  assert.equal(body.querySelectorAll('.kpi tf-stat-card').length, 4);
  assert.match(body.querySelector('.kpi').textContent, /Cache/);
  assert.equal(body.querySelector('[data-act="restore"]'), null);
  for (const act of ['sync', 'scrub']) assert.equal(body.querySelector(`.nas-snapraid > .section-card-head [data-act="${act}"]`).hasAttribute('disabled'), false);
  // A HEALTHY array offers no repair: `snapraid fix` writes the disk back from
  // parity, so a button here would be one click from overwriting good data.
  // Growing it and dissolving it are offered, because both are always legal on
  // an array that is simply serving.
  assert.equal(body.querySelector('[data-act="fix"]'), null);
  for (const act of ['destroy', 'add-disk']) assert.ok(body.querySelector(`[data-act="${act}"]`), act);
  assert.equal(body.querySelector('[data-act="add-disk"]').hasAttribute('disabled'), false);
  // The mover panel always renders. This fixture has no cache disk, so the run
  // must be REFUSED with a reason rather than quietly hidden.
  assert.ok(body.querySelector('[data-act="mover"]').hasAttribute('disabled'));
  assert.match(body.querySelector('.nas-mover').textContent, /nie ma czego przenosić/);
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
  assert.equal(jobCanCancel({ kind: 'elastic_sync' }), false);
  assert.equal(jobCanCancel({ kind: 'elastic_scrub' }), false);
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

for (const action of ['sync', 'scrub']) test(`${action} wysyła dokładnie raz z hasłem, blokuje obie akcje i odczytuje historię po terminalnym jobie`, async () => {
  const kind = `tentaNasElasticArray${action === 'sync' ? 'Sync' : 'Scrub'}Request`;
  const value = array();
  let release;
  const { screen, body } = await mount(value, { [kind]: () => new Promise((resolve) => { release = resolve; }) });
  click(body.querySelector(`[data-act="${action}"]`));
  click(body.querySelector('[data-act="sync"]'));
  click(body.querySelector('[data-act="scrub"]'));
  assert.deepEqual(screen.calls.filter((call) => call.kind === kind).map((call) => call.payload), [{ name: 'media', sudoPassword: 'hunter2' }]);
  for (const act of ['sync', 'scrub']) assert.ok(body.querySelector(`[data-act="${act}"]`).hasAttribute('disabled'));
  release({ job: { jobId: `job-${action}`, kind: `elastic_${action}`, status: 'running' } });
  await flush(); await flush();
  assert.equal(screen.jobLogs[0].jobId, `job-${action}`);
  assert.match(body.textContent, /Przyjęto zadanie SnapRAID/);
  value.snapraid.history = [{ kind: action, jobId: `job-${action}`, startedAt: '2026-09-08T12:00:00Z', finishedAt: '2026-09-08T12:01:00Z', outcome: 'ok', errorsFile: 0, errorsIo: 0, errorsData: 0, totalBlocks: 100, checkedBlocks: action === 'scrub' ? 68 : null, exitCode: 0 }];
  await screen.jobLogs[0].onFinish({ jobId: `job-${action}`, status: 'succeeded' });
  assert.equal(body.querySelector('.nas-snapraid-history tf-chip').getAttribute('label'), 'Sukces');
  assert.match(body.querySelector('.nas-snapraid-history').textContent, /0 \/ 0 \/ 0/);
  assert.equal(body.querySelector('[data-act="sync"]').hasAttribute('disabled'), false);
  assert.equal(body.querySelector('.kpi tf-stat-card:nth-child(2)').getAttribute('value'), 'Nie zmierzono');
  click(body.querySelector('[data-act="history-job"]'));
  assert.equal(screen.jobLogs[1].jobId, `job-${action}`);
  screen.dispose();
});

// `parityRunAvailable` is the node's OWN admission rule for a Sync and a full
// Scrub, and the surface repeats it rather than recomputing it from `state`:
// the array states that refuse one are not a list this file can keep (see the
// test below, where a state that looks unavailable admits it).
test('maintenance wymaga administratora, parity i dopuszczenia przebiegu przez węzeł', async (t) => {
  for (const [label, value, options] of [
    ['reader', array(), { admin: false }], ['zero parity', array({ parityDisks: [] })],
    ['disabled', array({ enabled: false })], ['pending', array({ state: 'pending', parityRunAvailable: false })],
    ['creating', array({ state: 'creating', parityRunAvailable: false })],
    ['nierozwiązany mover', array({ state: 'active', parityRunAvailable: false, unresolvedOperation: true })],
    ['unknown', array({ state: 'unknown', parityRunAvailable: false })], ['running', array({ snapraid: { history: [{ kind: 'sync', outcome: 'running' }] } })],
  ]) await t.test(label, async () => {
    const { screen, body } = await mount(value, {}, options);
    for (const action of ['sync', 'scrub']) {
      const button = body.querySelector(`[data-act="${action}"]`);
      assert.ok(button.hasAttribute('disabled'));
      button.dispatchEvent(new Event('click'));
    }
    await flush();
    assert.deepEqual(screen.calls.map((call) => call.kind), ['tentaNasElasticArrayGetRequest']);
    screen.dispose();
  });
});

test('maintenance: anulowane sudo oraz zmiana node, powierzchni i świeżego stanu podczas sudo nie wysyłają mutacji', async (t) => {
  const cancelled = await mount(array(), {}, { sudo: null });
  click(cancelled.body.querySelector('[data-act="sync"]'));
  await flush();
  assert.equal(cancelled.body.querySelector('[data-act="sync"]').hasAttribute('disabled'), false);
  assert.equal(cancelled.screen.calls.length, 1);
  cancelled.screen.dispose();
  for (const mode of ['node', 'surface', 'state', 'admin']) await t.test(mode, async () => {
    const value = array();
    const { screen, body } = await mount(value);
    let release;
    screen.withSudo = async (fn) => { await new Promise((resolve) => { release = resolve; }); return fn('secret-test'); };
    click(body.querySelector('[data-act="scrub"]'));
    if (mode === 'node') screen.currentNode = () => ({ nodeId: 'other' });
    if (mode === 'surface') body.replaceChildren();
    // The fresh-state check is the node's admission rule, not the array's
    // state string: a Sync is still legal on an array that needs attention.
    if (mode === 'state') value.parityRunAvailable = false;
    if (mode === 'admin') screen.isAdmin = false;
    release();
    await flush();
    assert.equal(screen.calls.filter((call) => call.kind.endsWith('ScrubRequest')).length, 0);
    screen.dispose();
  });
});

test('maintenance: approval i nieznana odpowiedź nie dają joba ani automatycznego retry po refresh', async (t) => {
  for (const mode of ['approval', 'lost', 'malformed']) await t.test(mode, async () => {
    const { screen, body } = await mount(array(), { tentaNasElasticArraySyncRequest: () => {
      if (mode === 'lost') throw new Error('secret must not render');
      return mode === 'approval' ? { approval: { requestId: 'approval-sync' } } : {};
    } });
    click(body.querySelector('[data-act="sync"]'));
    await flush(); await flush();
    assert.match(body.textContent, mode === 'approval' ? /drugiego administratora/ : /Wynik żądania jest nieznany/);
    assert.doesNotMatch(body.textContent, /secret must/);
    assert.equal(screen.jobLogs.length, 0);
    click(body.querySelector('[data-act="refresh"]'));
    await flush();
    assert.ok(body.querySelector('[data-act="sync"]').hasAttribute('disabled'));
    assert.ok(body.querySelector('[data-act="scrub"]').hasAttribute('disabled'));
    assert.equal(screen.calls.filter((call) => call.kind.endsWith('SyncRequest')).length, 1);
    screen.dispose();
  });
});

test('odmowa dirty pozostaje niewykonanym scrub bez zer i nie zmienia ostatniego sukcesu ani ochrony', async () => {
  const value = array();
  const { screen, body } = await mount(value, { tentaNasElasticArrayScrubRequest: { job: { jobId: 'refused', status: 'running' } } });
  click(body.querySelector('[data-act="scrub"]'));
  await flush(); await flush();
  value.snapraid.history = [{ kind: 'scrub', outcome: 'refused', jobId: 'refused', detail: '<img src=x onerror=alert(1)>', startedAt: '2026-09-08T12:00:00Z', finishedAt: '2026-09-08T12:00:01Z', exitCode: null }];
  await screen.jobLogs[0].onFinish({ jobId: 'refused', status: 'failed' });
  const history = body.querySelector('.nas-snapraid-history');
  assert.equal(history.querySelector('tf-chip').getAttribute('label'), 'Nie wykonano');
  assert.match(history.textContent, /— \/ — \/ —/);
  assert.match(history.textContent, /Kod procesu—/);
  assert.equal(history.querySelector('img'), null);
  assert.equal(body.querySelector('[data-act="sync"]').hasAttribute('disabled'), false);
  assert.match(body.querySelector('.nas-snapraid > .stat-rows').textContent, /Ostatni zakończony scrub—/);
  assert.equal(value.protection.protectedAsOf, '2026-09-07 12:00:00');
  assert.equal(body.querySelector('.kpi tf-stat-card:nth-child(2)').getAttribute('value'), 'Nie zmierzono');
  screen.dispose();
});

test('po niepotwierdzonym finale joba albo nieudanym świeżym Get maintenance pozostaje zablokowane', async () => {
  let failRead = false;
  const { screen, body } = await mount(array(), {
    tentaNasElasticArrayGetRequest: () => { if (failRead) throw new Error('offline'); return { array: array() }; },
    tentaNasElasticArraySyncRequest: { job: { jobId: 'sync', status: 'running' } },
  });
  click(body.querySelector('[data-act="sync"]'));
  await flush(); await flush();
  await screen.jobLogs[0].onFinish({ status: 'running' });
  assert.ok(body.querySelector('[data-act="sync"]').hasAttribute('disabled'));
  failRead = true;
  await screen.jobLogs[0].onFinish({ jobId: 'sync', status: 'succeeded' });
  assert.equal(body.querySelector('[data-act="sync"]'), null);
  failRead = false;
  click(body.querySelector('[data-act="refresh"]'));
  await flush();
  assert.ok(body.querySelector('[data-act="sync"]').hasAttribute('disabled'));
  screen.dispose();
});

test('historia zachowuje rozwinięte szczegóły przy odświeżaniu i nie odblokowuje obcego joba', async () => {
  const value = array({ snapraid: { history: [{ kind: 'sync', jobId: 'sync', operationId: 'op', outcome: 'running', startedAt: '2026-09-08T12:00:00Z' }] } });
  const { screen, body } = await mount(value);
  const details = body.querySelector('details');
  assert.equal(details.open, false);
  details.open = true;
  details.dispatchEvent(new Event('toggle'));
  click(body.querySelector('[data-act="refresh"]'));
  await flush();
  assert.equal(body.querySelector('details').open, true);
  click(body.querySelector('[data-act="history-job"]'));
  await screen.jobLogs[0].onFinish({ jobId: 'other', status: 'succeeded' });
  assert.ok(body.querySelector('[data-act="sync"]').hasAttribute('disabled'));
  body.querySelector('details').open = false;
  body.querySelector('details').dispatchEvent(new Event('toggle'));
  click(body.querySelector('[data-act="refresh"]'));
  await flush();
  assert.equal(body.querySelector('details').open, false);
  screen.dispose();
});

test('zamknięte przyczyny odmowy i etykiety approval są tłumaczone w pięciu locale', async () => {
  try {
    for (const language of ['pl', 'en', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      const reasons = ['no_parity', 'precondition_failed', 'unsynced_changes', 'empty_parity'];
      const { screen, body } = await mount(array({ snapraid: { history: reasons.map((detail, index) => ({ kind: 'scrub', outcome: 'refused', operationId: String(index), detail })) } }));
      const history = body.querySelector('.nas-snapraid-history');
      for (const reason of reasons) assert.ok(!history.textContent.includes(reason));
      assert.doesNotMatch(history.textContent, /tentanas\./);
      for (const operation of ['elastic_sync', 'elastic_scrub']) {
        assert.ok(!I18n.t(`tentanas.approvals.op_${operation}`).startsWith('tentanas.'));
        assert.ok(!I18n.t(`tentanas.jobs.kind_${operation}`).startsWith('tentanas.'));
      }
      screen.dispose();
    }
  } finally { await I18n.setLanguage('pl'); }
});

test('Restore nie obchodzi rozpoczętej maintenance, lecz samo Refused nie blokuje odtworzenia montowania', async () => {
  for (const kind of ['sync', 'scrub']) for (const outcome of ['running', 'failed', 'needs_attention', 'refused', 'interrupted']) {
    const { screen, body } = await mount(array({ state: 'pending', snapraid: { history: [{ kind, outcome }] } }));
    assert.equal(Boolean(body.querySelector('[data-act="restore"]')), ['refused', 'interrupted'].includes(outcome), `${kind}/${outcome}`);
    screen.dispose();
  }
});

// A Sync or a Scrub a reboot cut off is INTERRUPTED: the node closes it and
// marks parity out of date, and the next Sync is what settles it. It is not a
// failure, and it is no evidence for a repair — `snapraid fix` over a disk in
// use reverts what users changed since the last Sync.
test('przerwany sync nie jest błędem ani powodem naprawy', async () => {
  for (const kind of ['sync', 'scrub']) {
    const { screen, body } = await mount(array({ state: 'needs_attention', snapraid: { history: [{ kind, outcome: 'interrupted' }] } }));
    const chip = body.querySelector('.nas-snapraid-history tf-chip');
    assert.equal(chip.getAttribute('status'), 'warn', kind);
    assert.equal(chip.getAttribute('label'), 'Przerwany: dokończy go następny Sync', kind);
    const repair = body.querySelector('[data-act="fix"]');
    assert.ok(!repair || repair.hasAttribute('disabled'), `${kind}: no repair offered`);
    screen.dispose();
  }
});

const cacheDisk = { ...disk, diskId: 'serial-3', name: 'c1', kind: 'nvme', mountpoint: '/mnt/tentanas-branches/media/cache/c1' };
const moverArray = (overrides = {}, mover = {}) => array({
  cacheDisks: [cacheDisk],
  mover: { enabled: true, schedule: null, minAgeSecs: 7200, cacheMinFreePct: 20, coupledSync: true, lastRun: null, history: [], ...mover },
  ...overrides,
});
const moverRow = (panel, label) => [...panel.querySelectorAll('.sr')].find((r) => r.textContent.startsWith(label));

test('panel movera pokazuje reguły i sprzężony sync, a niezmierzone liczniki nie stają się zerem', async () => {
  const { screen, body } = await mount(moverArray({}, {
    lastRun: {
      startedAt: '2026-09-08T12:00:00Z', finishedAt: '2026-09-08T12:06:00Z', outcome: 'partial',
      movedBytes: 42 * GiB, movedFiles: 7, skippedBytes: 0, skippedFiles: 0, countsKnown: false,
      detail: '', coupledSync: { kind: 'sync', outcome: 'ok' },
    },
    history: [{ startedAt: '2026-09-08T11:00:00Z', finishedAt: '2026-09-08T11:02:00Z', movedBytes: 12 * GiB }],
  }));
  const panel = body.querySelector('.nas-mover');
  // The cache rule is shown as a FILL level; the setting is the minimum free %.
  assert.match(panel.textContent, /LUB cache > 80%/);
  assert.match(moverRow(panel, 'Pliki otwarte').textContent, /pomijane/);
  assert.match(moverRow(panel, 'Ostatni przebieg').textContent, /42 GiB/);
  assert.match(moverRow(panel, 'Ostatni przebieg').textContent, /sync OK/);
  assert.match(moverRow(panel, 'Przeniesiono').textContent, /42 GiB · 7/);
  // countsKnown:false — the skipped half was never walked and must not read 0.
  const skipped = moverRow(panel, 'Pominięto');
  assert.match(skipped.textContent, /nie zmierzono/);
  assert.doesNotMatch(skipped.textContent, /0/);
  assert.match(panel.querySelector('.mover-hist').textContent, /12 GiB/);
  assert.match(panel.querySelector('.nas-mover-parity-note').textContent, /dopiero po najbliższym sync/);
  assert.equal(body.querySelector('[data-act="mover"]').hasAttribute('disabled'), false);
  screen.dispose();
});

// Files changing under a sync are what a writable share does while it is
// synced: neither the mover's last run nor the SnapRAID history may read that
// as a failure, and the admin is told the next sync covers those files.
test('sync, który zastał zmieniające się pliki, jest częściowy, a nie nieudany', async () => {
  const { screen, body } = await mount(moverArray({
    snapraid: { history: [{ kind: 'sync', outcome: 'partial', errorsFile: 3, errorsIo: 0, errorsData: 0, exitCode: 1, detail: 'pliki zmieniały się podczas Sync; parity obejmie je następny Sync' }] },
  }, {
    lastRun: {
      startedAt: '2026-09-08T12:00:00Z', finishedAt: '2026-09-08T12:06:00Z', outcome: 'partial',
      movedBytes: 2 * GiB, movedFiles: 3, skippedBytes: 0, skippedFiles: 0, countsKnown: true,
      detail: '', coupledSync: { kind: 'sync', outcome: 'partial' },
    },
  }));
  const panel = body.querySelector('.nas-mover');
  const last = moverRow(panel, 'Ostatni przebieg').textContent;
  assert.match(last, /sync częściowy: zmienione pliki obejmie następny sync/);
  assert.doesNotMatch(last, /nieudany/);
  const chip = body.querySelector('.nas-snapraid-history tf-chip');
  assert.equal(chip.getAttribute('status'), 'warn');
  assert.equal(chip.getAttribute('label'), 'Częściowy: pliki zmieniły się w trakcie');
  screen.dispose();
});

test('sekcja przenoszenia nazywa się tym, co robi, i tłumaczy regułę własnymi progami macierzy', async () => {
  const { screen, body } = await mount(moverArray());
  const panel = body.querySelector('.nas-mover');
  // The name alone ("Mover") told nobody what the section does; the summary
  // says it, and the wire name stays inside, where the job history needs it.
  assert.match(panel.querySelector('summary .title').textContent, /Zaawansowane: przenoszenie z cache na dyski/);
  assert.doesNotMatch(panel.querySelector('summary').textContent, /mover/i);
  assert.match(panel.textContent, /jako „Mover”/);
  const explain = panel.querySelector('.nas-mover-explain');
  assert.match(explain.textContent, /Nowe pliki trafiają najpierw na dysk cache/);
  assert.match(explain.textContent, /Węzeł sam przenosi/);
  // 7200 s and cacheMinFreePct 20 — the same numbers as the rules row, and the
  // cache half is the FILL level, not the free one.
  assert.match(explain.textContent, /starszy niż 2 h/);
  assert.match(explain.textContent, /cache przekroczy 80%/);
  screen.dispose();
});

test('mover bez ustawionych progów wyjaśnia zasadę bez wymyślania liczb', async () => {
  const { screen, body } = await mount(moverArray({}, { minAgeSecs: null, cacheMinFreePct: null }));
  const explain = body.querySelector('.nas-mover .nas-mover-explain');
  assert.match(explain.textContent, /progi nie są jeszcze znane/);
  assert.doesNotMatch(explain.textContent, /%/);
  screen.dispose();
});

test('bez przebiegu i bez ustawień mover pokazuje kreski, nie zera', async () => {
  const { screen, body } = await mount(array({ cacheDisks: [cacheDisk] }));
  const panel = body.querySelector('.nas-mover');
  for (const label of ['Reguły', 'Ostatni przebieg', 'Przeniesiono', 'Pominięto']) {
    assert.match(moverRow(panel, label).textContent, /—/, label);
    assert.doesNotMatch(moverRow(panel, label).textContent, /0 B/, label);
  }
  assert.match(panel.querySelector('.mover-hist').textContent, /Brak zarejestrowanych przebiegów/);
  screen.dispose();
});

test('mover: nieadministrator i macierz bez cache nie mogą uruchomić przebiegu', async (t) => {
  for (const [label, value, options] of [
    ['reader', moverArray(), { admin: false }],
    ['bez cache', array()],
    ['pending', moverArray({ state: 'pending' })],
    ['wyłączona', moverArray({ enabled: false })],
    ['sync w toku', moverArray({ snapraid: { history: [{ kind: 'sync', outcome: 'running' }] } })],
  ]) await t.test(label, async () => {
    const { screen, body } = await mount(value, {}, options);
    const button = body.querySelector('[data-act="mover"]');
    assert.ok(button.hasAttribute('disabled'));
    button.dispatchEvent(new Event('click'));
    await flush();
    assert.deepEqual(screen.calls.map((call) => call.kind), ['tentaNasElasticArrayGetRequest']);
    screen.dispose();
  });
});

test('mover wysyła dokładnie jedno żądanie z nazwą macierzy i śledzi job', async () => {
  const { screen, body } = await mount(moverArray(), {
    tentaNasElasticArrayMoverRequest: { job: { jobId: 'mover-1', kind: 'elastic_mover', status: 'running' } },
  });
  const button = body.querySelector('[data-act="mover"]');
  click(button); click(button);
  await flush(); await flush();
  assert.deepEqual(
    screen.calls.filter((c) => c.kind === 'tentaNasElasticArrayMoverRequest').map((c) => c.payload),
    [{ name: 'media', sudoPassword: 'hunter2' }],
  );
  assert.equal(screen.jobLogs[0].jobId, 'mover-1');
  assert.match(body.textContent, /Przyjęto zadanie przenoszenia z cache/);
  assert.equal(jobCanCancel({ kind: 'elastic_mover' }), false);
  screen.dispose();
});

test('mover: anulowane sudo nie wysyła nic, a approval i utracona odpowiedź nie udają joba', async (t) => {
  const cancelled = await mount(moverArray(), {}, { sudo: null });
  click(cancelled.body.querySelector('[data-act="mover"]'));
  await flush();
  assert.equal(cancelled.screen.calls.length, 1);
  assert.equal(cancelled.body.querySelector('[data-act="mover"]').hasAttribute('disabled'), false);
  cancelled.screen.dispose();
  for (const mode of ['approval', 'lost']) await t.test(mode, async () => {
    const { screen, body } = await mount(moverArray(), {
      tentaNasElasticArrayMoverRequest: () => {
        if (mode === 'lost') throw new Error('secret must not render');
        return { approval: { requestId: 'approval-mover' } };
      },
    });
    click(body.querySelector('[data-act="mover"]'));
    await flush(); await flush();
    assert.match(body.textContent, mode === 'approval' ? /drugiego administratora/ : /Wynik żądania jest nieznany/);
    assert.doesNotMatch(body.textContent, /secret must/);
    assert.equal(screen.jobLogs.length, 0);
    assert.ok(body.querySelector('[data-act="mover"]').hasAttribute('disabled'));
    screen.dispose();
  });
});

test('nierozwiązana operacja blokuje mover i mówi dlaczego, zamiast zgłaszać błąd wewnętrzny', async () => {
  const { screen, body } = await mount(moverArray({ unresolvedOperation: true }));
  const button = body.querySelector('[data-act="mover"]');
  assert.ok(button.hasAttribute('disabled'));
  assert.match(body.querySelector('.nas-mover').textContent, /niepotwierdzoną operację/);
  button.dispatchEvent(new Event('click'));
  await flush();
  assert.deepEqual(screen.calls.map((c) => c.kind), ['tentaNasElasticArrayGetRequest']);
  screen.dispose();
});

// When every unresolved operation is a mover run, the next mover run is what
// settles it — and once the node stops retrying on its own, an admin's run is
// the only way on. The button is offered, on an array that needs attention too.
test('mover rozstrzygający wcześniejsze przebiegi jest dostępny mimo nierozwiązanej operacji', async () => {
  for (const state of ['active', 'needs_attention']) {
    const { screen, body } = await mount(moverArray({ state, unresolvedOperation: true, moverSettlesUnresolved: true }));
    assert.equal(body.querySelector('[data-act="mover"]').hasAttribute('disabled'), false, state);
    screen.dispose();
  }
  const { screen, body } = await mount(moverArray({ state: 'needs_attention', unresolvedOperation: true }));
  assert.ok(body.querySelector('[data-act="mover"]').hasAttribute('disabled'));
  screen.dispose();
});

test('nieskonfigurowany mover nie przedstawia domyślnych liczb jako ustawień', async () => {
  const unset = await mount(moverArray());
  const panel = unset.body.querySelector('.nas-mover');
  assert.match(moverRow(panel, 'Okno przenoszenia').textContent, /bez ograniczeń — automatycznie/);
  assert.match(moverRow(panel, 'Reguły').textContent, /domyślne:/);
  unset.screen.dispose();
  const set = await mount(moverArray({}, { configured: true, schedule: { every: '1h' } }));
  const configured = set.body.querySelector('.nas-mover');
  assert.match(moverRow(configured, 'Okno przenoszenia').textContent, /tylko co 1 h/);
  assert.doesNotMatch(moverRow(configured, 'Reguły').textContent, /domyślne:/);
  assert.match(moverRow(configured, 'Reguły').textContent, /LUB cache > 80%/);
  set.screen.dispose();
});

// The cadence and the rules are stored as separate rows and either can stand
// alone, so the panel must not answer one question with the other's fact: a
// config import that wrote only a cadence leaves the rules still defaults.
test('kadencja i reguły movera to dwa osobne fakty na karcie', async () => {
  const scheduledOnly = await mount(moverArray({}, { schedule: { every: '6h' }, configured: false }));
  const panel = scheduledOnly.body.querySelector('.nas-mover');
  assert.match(moverRow(panel, 'Okno przenoszenia').textContent, /co 6 h/);
  assert.match(moverRow(panel, 'Reguły').textContent, /domyślne:/, 'kadencja nie czyni reguł decyzją');
  scheduledOnly.screen.dispose();

  const ruledOnly = await mount(moverArray({}, { schedule: null, configured: true, minAgeSecs: 1800, cacheMinFreePct: 30 }));
  const ruled = ruledOnly.body.querySelector('.nas-mover');
  assert.match(moverRow(ruled, 'Okno przenoszenia').textContent, /bez ograniczeń — automatycznie/);
  assert.doesNotMatch(moverRow(ruled, 'Reguły').textContent, /domyślne:/);
  assert.match(moverRow(ruled, 'Reguły').textContent, /LUB cache > 70%/);
  ruledOnly.screen.dispose();
});

// A schedule the admin switched off stays saved — that is what the editor
// promises — so it must not render like a live one.
test('wyłączona kadencja SnapRAID mówi, że jest wyłączona', async () => {
  const { screen, body } = await mount(moverArray({
    snapraid: {
      syncSchedule: { every: 'daily', hour: 3, minute: 0, weekday: 0, day: 1 }, syncScheduleEnabled: false,
      scrubSchedule: { every: 'weekly', hour: 4, minute: 0, weekday: 0, day: 1 }, scrubScheduleEnabled: true,
    },
  }));
  const panel = body.querySelector('.nas-snapraid');
  assert.match(moverRow(panel, 'Harmonogram sync').textContent, /wyłączony/);
  assert.doesNotMatch(moverRow(panel, 'Harmonogram scrub').textContent, /wyłączony/);
  assert.match(moverRow(panel, 'Harmonogram scrub').textContent, /co tydzień/);
  screen.dispose();
});

// n15's dialog is ONE form, so it is one request: an admin who changes the
// cadence and the age together cannot end up with half of it saved.
test('okno movera zapisuje kadencję i reguły w jednym żądaniu', async () => {
  const { screen, body } = await mount(
    moverArray({}, {
      schedule: { every: '1h', hour: 0, minute: 0, weekday: 0, day: 1 },
      configured: true, minAgeSecs: 7200, cacheMinFreePct: 20, coupledSync: true,
    }),
    { tentaNasElasticMoverScheduleSetRequest: { ok: true } },
  );
  // The header button and the pill BOTH carry this action. The pill is the one
  // the panel documents as the way in, so it is the one under test — clicking
  // whichever happens to come first is how a dead pill went unnoticed.
  assert.equal(body.querySelectorAll('[data-act="mover-schedule"]').length, 2, 'przycisk i pigułka');
  click(body.querySelector('.sched-pill[data-act="mover-schedule"]'));
  await flush();
  const win = document.querySelector('tf-window.nas-mover-schedule');
  assert.ok(win, 'okno movera otwarte');
  // The window may be as fine as fifteen minutes or as coarse as a night.
  assert.deepEqual(
    [...win.querySelectorAll('#nas-mover-every option')].map((o) => o.value),
    ['15m', '30m', '1h', '6h', 'daily'],
  );
  // The switch is the RESTRICTION, and the dialog says that moving stays
  // automatic while it is off.
  assert.match(win.textContent, /Przenoś tylko w oknie harmonogramu/);
  assert.match(win.textContent, /pliki są przenoszone automatycznie/);
  win.querySelector('#nas-mover-coupled').checked = false;
  confirmWindow(win);
  await flush();
  await flush();
  const sent = screen.calls.find((c) => c.kind === 'tentaNasElasticMoverScheduleSetRequest');
  assert.ok(sent, 'harmonogram zapisany');
  assert.equal(sent.payload.name, 'media');
  assert.equal(sent.payload.enabled, true);
  assert.equal(sent.payload.schedule.every, '1h');
  assert.equal(sent.payload.minAgeSecs, 7200);
  assert.equal(sent.payload.cacheMinFreePct, 20);
  assert.equal(sent.payload.coupledSync, false);
  win.remove();
  screen.dispose();
});

test('pasek historii nie zaprzecza ostatniemu przebiegowi', async () => {
  const run = {
    startedAt: '2026-09-08T12:00:00Z', finishedAt: '2026-09-08T12:06:00Z', outcome: 'ok',
    movedBytes: 42 * GiB, movedFiles: 7, skippedBytes: 0, skippedFiles: 0, countsKnown: true,
    detail: '', coupledSync: { kind: 'sync', outcome: 'ok' },
  };
  const { screen, body } = await mount(moverArray({}, { lastRun: run, history: [run] }));
  const strip = body.querySelector('.mover-hist');
  assert.match(strip.textContent, /42 GiB/);
  assert.doesNotMatch(strip.textContent, /Brak zarejestrowanych/);
  screen.dispose();
});

test('włączone okno ogranicza przenoszenie i mówi o tym; zapisane, lecz wyłączone niczego nie ogranicza', async () => {
  const daily = { every: 'daily', hour: 3, minute: 0, weekday: 0, day: 1 };
  const restricted = await mount(moverArray({}, { enabled: true, schedule: daily }));
  const panel = restricted.body.querySelector('.nas-mover');
  assert.match(panel.textContent, /ograniczone do okna harmonogramu/);
  assert.match(moverRow(panel, 'Okno przenoszenia').textContent, /^Okno przenoszenia\s*tylko /);
  assert.equal(restricted.body.querySelector('[data-act="mover"]').hasAttribute('disabled'), false, 'ręczny przebieg pozostaje dostępny');
  restricted.screen.dispose();

  const saved = await mount(moverArray({}, { enabled: false, schedule: daily }));
  const off = saved.body.querySelector('.nas-mover');
  assert.doesNotMatch(off.textContent, /ograniczone do okna harmonogramu/);
  assert.match(moverRow(off, 'Okno przenoszenia').textContent, /bez ograniczeń \(zapisane okno .* jest wyłączone\)/);
  saved.screen.dispose();
});

// The owner's decision (2026-09-17): moving off the cache is automatic, so the
// main view asks the admin to operate nothing and names no process. What it
// shows is one fact about the DATA; everything a run needs is folded away.
const mainViewText = (body) => {
  const copy = body.cloneNode(true);
  copy.querySelectorAll('details[data-section="mover"]').forEach((details) => {
    [...details.children].forEach((child) => { if (child.tagName !== 'SUMMARY') child.remove(); });
  });
  return copy.textContent;
};

test('widok główny nie mówi „Mover” i pokazuje jedną linię o danych czekających na cache', async () => {
  const { screen, body } = await mount(moverArray({ protection: { status: 'window_open', cacheUnprotectedBytes: 18 * GiB, movedUnsyncedBytes: 0 } }));
  try {
  assert.doesNotMatch(mainViewText(body), /mover/i);
  const details = body.querySelector('details[data-section="mover"]');
  assert.equal(details.open, false, 'konfiguracja i ręczny przebieg są zwinięte');
  assert.ok(details.querySelector('[data-act="mover"]'), 'ręczny przebieg jest w sekcji zaawansowanej');
  assert.ok(details.querySelector('.mover-hist'), 'historia pozostaje osiągalna');
  const lines = body.querySelectorAll('.nas-cache-pending .sr');
  assert.equal(lines.length, 1);
  assert.equal(lines[0].querySelector('.k').textContent, 'Na dysku cache, jeszcze bez ochrony');
  assert.equal(lines[0].querySelector('.v').textContent, '18 GiB');
  // Nothing else in the main view is a control for moving files.
  for (const act of ['mover', 'mover-schedule']) {
    assert.ok([...body.querySelectorAll(`[data-act="${act}"]`)].every((el) => details.contains(el)), act);
  }
  } finally { screen.dispose(); }
});

test('niezerowa ilość danych czekających na cache wygląda jak zwykła wartość, nie jak błąd', async () => {
  const render = async (bytes) => {
    const { screen, body } = await mount(moverArray({ protection: { status: 'window_open', cacheUnprotectedBytes: bytes } }));
    try {
      const line = body.querySelector('.nas-cache-pending');
      return {
        text: line.querySelector('.v').textContent,
        valueClass: line.querySelector('.v').className,
        rowClass: line.querySelector('.sr').className,
        alarming: Boolean(line.querySelector('tf-alert, tf-chip, .num-err, .num-warn, .err, .warn')),
        alerts: body.querySelectorAll('tf-alert').length,
      };
    } finally { screen.dispose(); }
  };
  const zero = await render(0);
  const pending = await render(18 * GiB);
  assert.equal(zero.text, '0 B');
  assert.equal(pending.text, '18 GiB');
  assert.equal(pending.valueClass, zero.valueClass, 'ta sama klasa co dla zera');
  assert.equal(pending.rowClass, zero.rowClass);
  assert.equal(pending.alarming, false);
  assert.equal(pending.alerts, 0);
  // Unmeasured stays unmeasured rather than becoming a confident zero.
  assert.equal((await render(null)).text, '—');
});

test('macierz bez cache nie pokazuje linii o danych na cache', async () => {
  const { screen, body } = await mount(array());
  try {
    assert.ok(body.querySelector('.nas-cache-pending') === null);
    assert.doesNotMatch(mainViewText(body), /mover/i);
  } finally { screen.dispose(); }
});

test('zmieniające się liczby nie przebudowują panelu, a rozwinięta sekcja przenoszenia zostaje rozwinięta', async () => {
  let reads = 0;
  const value = () => {
    reads += 1;
    return { array: moverArray({
      usedBytes: reads * GiB,
      cacheUsedBytes: reads * 2 * GiB,
      updatedAt: `2026-09-07 12:0${reads}:00`,
      protection: { status: 'window_open', cacheUnprotectedBytes: reads * 3 * GiB, movedUnsyncedBytes: reads * GiB },
      cacheDisks: [{ ...cacheDisk, usedBytes: reads * 4 * GiB }],
    }) };
  };
  const { screen, body } = await mount(null, { tentaNasElasticArrayGetRequest: value });
  try {
  const pending = body.querySelector('.nas-cache-pending');
  const heading = body.querySelector('.nas-elastic-heading');
  assert.equal(pending.querySelector('.v').textContent, fmtOptionalBytes(3 * GiB));
  click(body.querySelector('[data-act="refresh"]'));
  await flush(); await flush();
  // `ok(===)`, not `equal`: a failing `equal` on two DOM nodes makes the
  // assertion inspect the whole document for its diff and never returns.
  assert.ok(body.querySelector('.nas-cache-pending') === pending, 'ten sam węzeł, nie przebudowa');
  assert.ok(body.querySelector('.nas-elastic-heading') === heading, 'nagłówek nie jest przebudowany');
  assert.equal(pending.querySelector('.v').textContent, fmtOptionalBytes(6 * GiB));
  assert.equal(body.querySelector('tf-stat-card[data-fig="capacity"]').getAttribute('value'), fmtOptionalBytes(2 * GiB));
  assert.equal(body.querySelector('tf-stat-card[data-fig="cache"]').getAttribute('value'), fmtOptionalBytes(4 * GiB));
  assert.ok(body.querySelector('.disk-cell[data-disk="serial-3"] [data-fig="disk-usage"]').textContent.startsWith(`${fmtOptionalBytes(8 * GiB)} /`));

  const details = body.querySelector('details[data-section="mover"]');
  details.open = true;
  details.dispatchEvent(new window.Event('toggle'));
  click(body.querySelector('[data-act="refresh"]'));
  await flush(); await flush();
  assert.equal(body.querySelector('details[data-section="mover"]').open, true, 'rozwinięcie przeżywa odświeżenie');
  } finally { screen.dispose(); }
});

test('wyłączony sprzężony sync ostrzega, że przeniesione pliki zostają poza parity', async () => {
  const { screen, body } = await mount(moverArray({}, { coupledSync: false }));
  const explain = body.querySelector('.nas-mover .nas-mover-parity-note').textContent;
  assert.match(explain, /pozostaną poza parity/);
  assert.doesNotMatch(explain, /sprzężony krok/);
  screen.dispose();
});

test('etykiety movera są tłumaczone w pięciu locale', async () => {
  try {
    for (const language of ['pl', 'en', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      assert.ok(!I18n.t('tentanas.approvals.op_elastic_mover').startsWith('tentanas.'));
      assert.ok(!I18n.t('tentanas.jobs.kind_elastic_mover').startsWith('tentanas.'));
      const { screen, body } = await mount(moverArray({}, { enabled: false }));
      assert.doesNotMatch(body.querySelector('.nas-mover').textContent, /tentanas\./);
      screen.dispose();
    }
  } finally { await I18n.setLanguage('pl'); }
});

test('Restore ponownie sprawdza historię maintenance po oczekiwaniu na sudo', async () => {
  const value = array({ state: 'pending' });
  const { screen, body } = await mount(value);
  let release;
  screen.withSudo = async (fn) => { await new Promise((resolve) => { release = resolve; }); return fn('test-only'); };
  click(body.querySelector('[data-act="restore"]'));
  value.snapraid.history = [{ kind: 'sync', outcome: 'needs_attention' }];
  release();
  await flush();
  assert.equal(screen.calls.filter((call) => call.kind.endsWith('RestoreRequest')).length, 0);
  assert.equal(body.querySelector('[data-act="restore"]'), null);
  screen.dispose();
});

// The whole pane is one template, rebuilt on every 5 s poll — which threw away
// the KPI tiles, the disk cells and any <details> the admin had opened.
test('an unchanged poll leaves the Elastic pane standing', async () => {
  const screen = fakeScreen({ tentaNasElasticArrayGetRequest: { array: array() } });
  screen.array = 'media';
  screen.openArray = (name) => { screen.array = name; };
  const scheduled = [];
  screen.later = (fn) => { scheduled.push(fn); };
  const body = document.createElement('div');
  document.body.appendChild(body);
  await drawElasticDetail(screen, body);
  await flush();
  const view = body.querySelector('.nas-elastic-detail');
  const tiles = [...view.querySelectorAll('tf-stat-card')];
  const cells = [...view.querySelectorAll('.disk-cell')];
  assert.ok(tiles.length, 'the KPI row is painted');
  assert.ok(cells.length, 'the disks are painted');

  assert.ok(scheduled.length, 'the pane armed its poll');
  await scheduled[0]();
  await flush();
  [...view.querySelectorAll('tf-stat-card')].forEach((el, i) => assert.equal(el === tiles[i], true, `tile ${i} survives the poll`));
  [...view.querySelectorAll('.disk-cell')].forEach((el, i) => assert.equal(el === cells[i], true, `disk cell ${i} survives the poll`));
  screen.dispose();
});

// =============================================================================
// Repair, grow, dissolve (§5.3 lifecycle)
// =============================================================================

/// The repair control appears on EVIDENCE and nowhere else.
///
/// `snapraid fix -d <disk>` writes the named disk back from the parity
/// checkpoint, so on an array that reports nothing wrong the button's only
/// possible effect is overwriting healthy data — and on an array with no parity
/// at all the node can only refuse it. Both are checked here against the same
/// four kinds of evidence `tentanas::elastic::repair_evidence` accepts.
// The array a repair is actually reached for: a scrub that ended without
// success. THAT is a parity fault; `unresolvedOperation` is not, because it is
// equally true for a mover that stopped part-way and for an add-disk that
// failed, and parity cannot repair either of those.
const snapraidWith = (history, parityErrors = null) => ({
  parityErrors, configPath: '/etc/tentanas/snapraid-media.conf', history,
});
// A scrub that MARKED blocks: the errors it counted are what a repair writes
// back. Measured on rig11: `-e fix` with no such scrub behind it writes
// nothing at all and still reports "Everything OK", so the count is part of
// the evidence and not decoration.
const failedScrub = { kind: 'scrub', outcome: 'needs_attention', errors: 7, startedAt: '2026-09-08 01:00:00', finishedAt: '2026-09-08 01:10:00' };
const okFix = { kind: 'fix', outcome: 'ok', startedAt: '2026-09-08 02:00:00', finishedAt: '2026-09-08 02:30:00' };

// THE WAY OUT of a parity run that ended badly, and of a repair that repaired
// nothing: the Sync and the full Scrub stay startable on the array such a run
// left behind. Without this the screen read `state` plus `unresolvedOperation`
// and disabled both buttons on exactly the array that needs them — the repair
// dialog says "run a Sync", and the Sync was the one thing refused (W5/W6 of
// the fourth review). And no replacement is offered here either, whatever the
// array reports, because the feature is withdrawn.
test('nieudany scrub i naprawa bez zapisu zostawiają Sync i Scrub do uruchomienia', async () => {
  const nothingRepaired = { kind: 'fix', outcome: 'nothing_repaired', errors: 391, startedAt: '2026-09-08 03:00:00', finishedAt: '2026-09-08 03:04:00' };
  for (const history of [[failedScrub], [nothingRepaired, failedScrub], [{ ...failedScrub, kind: 'sync', outcome: 'failed' }]]) {
    const { screen, body } = await mount(array({ state: 'needs_attention', snapraid: snapraidWith(history) }), {
      tentaNasElasticArraySyncRequest: { job: { jobId: 'job-sync', status: 'running' } },
    });
    for (const act of ['sync', 'scrub']) {
      assert.equal(body.querySelector(`[data-act="${act}"]`).hasAttribute('disabled'), false, `${act}: ${history[0].outcome}`);
    }
    assert.equal(body.querySelector('[data-act="replace-disk"]'), null, 'wymiana dysku jest wycofana');
    click(body.querySelector('[data-act="sync"]'));
    await flush();
    assert.deepEqual(
      screen.calls.filter((c) => c.kind === 'tentaNasElasticArraySyncRequest').map((c) => c.payload),
      [{ name: 'media', sudoPassword: 'hunter2' }],
    );
    screen.dispose();
  }
  // A nothing-repaired repair is reported as itself, so the detail an admin
  // reads matches what the array will let them do next.
  const { screen, body } = await mount(array({ state: 'needs_attention', snapraid: snapraidWith([nothingRepaired, failedScrub]) }));
  assert.equal(body.querySelector('.nas-snapraid-history tf-chip').getAttribute('label'), 'Nic nie naprawiono');
  screen.dispose();
});

test('naprawa pojawia się tylko przy dowodzie awarii parity i nigdy bez parity', async () => {
  const cases = [
    [{}, false, 'zdrowa macierz nie oferuje naprawy'],
    [{ parityDisks: [] }, false, 'bez parity nie ma z czego odbudować'],
    [{ snapraid: snapraidWith([failedScrub]) }, true, 'scrub, który zaznaczył błędy, to dowód'],
    // NOT evidence: a scrub that counted nothing, a failed sync, and the
    // parity-error figure of the reporting window — none of them marks a block
    // in the content file, and a repair only writes marked blocks.
    [{ snapraid: snapraidWith([{ ...failedScrub, errors: 0 }]) }, false, 'scrub bez błędów nic nie zaznaczył'],
    [{ snapraid: snapraidWith([{ ...failedScrub, kind: 'sync' }]) }, false, 'nieudany sync nic nie zaznacza'],
    [{ snapraid: snapraidWith([], 3) }, false, 'liczba błędów w oknie to nie zaznaczone bloki'],
    [{ state: 'needs_attention', snapraid: snapraidWith([failedScrub]) }, true, 'naprawa startuje z needs_attention'],
    // Newest first, so a repair that already succeeded settled the run below
    // it — the same rule the node applies in SQL over the same rows.
    [{ snapraid: snapraidWith([okFix, failedScrub]) }, false, 'udana naprawa rozwiązała ten przebieg'],
    // NOT parity faults: a stuck mover and a failed add both set
    // `unresolvedOperation`, and offering to overwrite a data disk from parity
    // over either of them would be a repair for the wrong problem.
    [{ unresolvedOperation: true }, false, 'nierozwiązana operacja to nie awaria parity'],
    [{ dataDisks: [{ ...disk, health: 'critical' }] }, false, 'sam SMART nie jest awarią parity'],
    // A dead data disk is the one case a repair CANNOT run on: it writes that
    // very disk. The screen must say what to do first, not offer the button.
    [{ dataDisks: [{ ...disk, devicePresent: false }], snapraid: snapraidWith([failedScrub]) }, false, 'brak dysku blokuje naprawę'],
    [{ dataDisks: [{ ...disk, mounted: false }], snapraid: snapraidWith([failedScrub]) }, false, 'niezamontowany dysk blokuje naprawę'],
    [{ enabled: false, snapraid: snapraidWith([failedScrub]) }, false, 'wyłączona macierz nic nie uruchamia'],
  ];
  for (const [overrides, offered, why] of cases) {
    const { screen, body } = await mount(array(overrides));
    assert.equal(Boolean(body.querySelector('[data-act="fix"]')), offered, why);
    screen.dispose();
  }
  // And when it is blocked the reason is the disk, in the admin's words —
  // never the helper's `precondition_failed` after a job has been started.
  const blocked = await mount(array({ dataDisks: [{ ...disk, devicePresent: false }], snapraid: snapraidWith([failedScrub]) }));
  assert.match(blocked.body.querySelector('.nas-snapraid').textContent, /nie widać na tym węźle/);
  blocked.screen.dispose();

  // And a reader never sees any of the three, whatever the array reports.
  const { screen, body } = await mount(array({ snapraid: snapraidWith([failedScrub]) }), {}, { admin: false });
  assert.equal(body.querySelector('[data-act="fix"]'), null);
  assert.equal(body.querySelector('[data-act="add-disk"]'), null);
  assert.equal(body.querySelector('[data-act="destroy"]'), null);
  screen.dispose();
});

/// The repair retypes the DISK, sends exactly one request naming that disk, and
/// follows the job. Retyping the ARRAY name must not arm it: the mistake this
/// dialog exists to stop is repairing the wrong disk of the right array.
test('naprawa wymaga przepisania nazwy dysku i wysyła dokładnie jedno żądanie', async () => {
  const { screen, body } = await mount(array({ snapraid: snapraidWith([failedScrub]) }), {
    tentaNasElasticArrayFixRequest: { job: { jobId: 'job-fix', status: 'running' } },
  });
  click(body.querySelector('[data-act="fix"]'));
  await flush();
  const win = document.querySelector('tf-window');
  assert.ok(win, 'okno naprawy jest otwarte');
  // WHAT THE DIALOG PROMISES has to be what `-e fix` does, measured on rig11:
  // it writes back only the blocks the last Scrub marked, in files unchanged
  // since the last Sync. It said the opposite — that it would overwrite the
  // named disk from parity and restore files from the last sync — which is the
  // unfiltered `fix` this app no longer runs, and an admin who believed it
  // would expect deleted files back.
  assert.match(win.textContent, /bloki, które ostatni Scrub oznaczył/);
  assert.match(win.textContent, /nie przywraca brakujących plików/);
  assert.match(win.textContent, /Nic nie naprawiono/, 'i co zrobić, gdy nic nie zaznaczono');
  assert.doesNotMatch(win.textContent, /Odtwarza pliki z ostatniego udanego sync/);
  const confirm = win.querySelector('[data-action="confirm"]');
  assert.ok(confirm.hasAttribute('disabled'), 'przycisk startuje zablokowany');
  typeInto(win.querySelector('#nas-retype'), 'media');
  assert.ok(confirm.hasAttribute('disabled'), 'nazwa macierzy nie uzbraja naprawy dysku');
  // The admin retypes the name on the cell. The slot is not it.
  typeInto(win.querySelector('#nas-retype'), 'd1');
  assert.ok(confirm.hasAttribute('disabled'), 'the slot key does not arm the repair');
  assert.match(win.textContent, /Przepisz nazwę dysku, aby potwierdzić: vdb/);
  typeInto(win.querySelector('#nas-retype'), 'vdb');
  assert.equal(confirm.hasAttribute('disabled'), false);
  confirmWindow(win);
  await flush();
  const sent = screen.calls.filter((c) => c.kind === 'tentaNasElasticArrayFixRequest');
  assert.equal(sent.length, 1);
  assert.deepEqual(sent[0].payload, { name: 'media', disk: 'd1', confirmDisk: 'd1', sudoPassword: 'hunter2' });
  assert.deepEqual(screen.jobLogs.map((j) => j.jobId), ['job-fix']);
  screen.dispose();
});

/// Growing the array: one request, the array name retyped, and the picked disk
/// carried by id. A disk larger than the array's parity stays VISIBLE with the
/// reason and cannot be picked — snapraid accepts such a disk and refuses only
/// once the data has outgrown the parity, so this warning is the only one that
/// arrives in time.
test('dodanie dysku przepisuje nazwę macierzy, odmawia dysku większego niż parity i wysyła raz', async () => {
  const free = [
    { diskId: 'free-1', name: 'sdc', sizeBytes: 16 * GiB, serial: 'S1', kind: 'hdd', health: 'ok' },
    { diskId: 'free-2', name: 'sdd', sizeBytes: 64 * GiB, serial: 'S2', kind: 'hdd', health: 'ok' },
    { diskId: 'free-3', name: 'sde', sizeBytes: 16 * GiB, serial: 'S3', kind: 'hdd', health: 'critical' },
  ];
  const { screen, body } = await mount(array(), {
    tentaNasElasticCapabilitiesRequest: { capabilities: { mergerfs: true, snapraid: true, filesystems: ['xfs'] }, freeDisks: free },
    tentaNasElasticArrayAddDiskRequest: { job: { jobId: 'job-add', status: 'running' } },
  });
  click(body.querySelector('[data-act="add-disk"]'));
  await flush();
  const win = document.querySelector('tf-window');
  assert.ok(win, 'okno dodania dysku jest otwarte');
  const cells = [...win.querySelectorAll('#nas-add-disk .disk-cell')];
  // The critical disk is not offered at all; the oversized one is offered and
  // refused with its reason.
  assert.deepEqual(cells.map((c) => c.dataset.disk), ['free-1', 'free-2']);
  const oversized = cells.find((c) => c.dataset.disk === 'free-2');
  assert.ok(oversized.classList.contains('disabled'));
  assert.match(oversized.textContent, /parity/);
  click(oversized);
  assert.equal(oversized.classList.contains('checked'), false, 'dysku większego niż parity nie da się wybrać');
  const good = cells.find((c) => c.dataset.disk === 'free-1');
  click(good);
  assert.ok(good.classList.contains('checked'));
  typeInto(win.querySelector('#nas-retype'), 'media');
  confirmWindow(win);
  await flush();
  const sent = screen.calls.filter((c) => c.kind === 'tentaNasElasticArrayAddDiskRequest');
  assert.equal(sent.length, 1);
  assert.deepEqual(sent[0].payload, { name: 'media', diskId: 'free-1', confirmName: 'media', sudoPassword: 'hunter2' });
  assert.deepEqual(screen.jobLogs.map((j) => j.jobId), ['job-add']);
  screen.dispose();
});

/// A dissolve says what it does NOT do, because that is the shape of the
/// operation: nothing is formatted, so the disks keep their filesystems and the
/// array import takes the array back. Without the retype nothing is sent.
test('rozwiązanie obiecuje zachowanie danych, wymaga przepisania nazwy i wysyła raz', async () => {
  const { screen, body } = await mount(array(), {
    tentaNasElasticArrayDestroyRequest: { job: { jobId: 'job-kill', status: 'running' } },
  });
  const row = body.querySelector('.danger-zone [data-act="destroy"]');
  assert.ok(row, 'strefa zagrożenia ma akcję rozwiązania');
  click(row);
  await flush();
  const win = document.querySelector('tf-window');
  const confirm = win.querySelector('[data-action="confirm"]');
  assert.ok(confirm.hasAttribute('disabled'));
  assert.match(win.textContent, /zachowuje system plików/);
  assert.match(win.textContent, /Import macierzy przywraca/);
  assert.match(win.textContent, /nie formatuje niczego/, 'dialog mówi wprost, że nic nie formatuje');
  // Nothing was sent while the retype was empty.
  assert.equal(screen.calls.filter((c) => c.kind === 'tentaNasElasticArrayDestroyRequest').length, 0);
  typeInto(win.querySelector('#nas-retype'), 'medi');
  assert.ok(confirm.hasAttribute('disabled'), 'częściowa nazwa nie uzbraja');
  typeInto(win.querySelector('#nas-retype'), 'media');
  confirmWindow(win);
  await flush();
  const sent = screen.calls.filter((c) => c.kind === 'tentaNasElasticArrayDestroyRequest');
  assert.equal(sent.length, 1);
  assert.deepEqual(sent[0].payload, { name: 'media', confirmName: 'media', sudoPassword: 'hunter2' });
  assert.deepEqual(screen.jobLogs.map((j) => j.jobId), ['job-kill']);
  screen.dispose();
});

/// A member the helper found NO device for (`devicePresent: false`) arrives
/// with no live name — that is what a failed or pulled disk looks like. The
/// cell says the disk is missing and which member it was; every sentence names
/// it by its part in the array; and no repair is offered on it, because
/// "brak dysku" is nothing an admin can retype to confirm overwriting.
test('członek bez nazwy z inwentarza jest „brakiem dysku” z rolą w macierzy, bez slotu i bez naprawy', async () => {
  const lost = { ...disk, diskId: 'serial-9', name: 'd2', diskName: '', devicePresent: false, mounted: false, mountpoint: '/mnt/tentanas-branches/media/data/d2' };
  const { screen, body } = await mount(array({ dataDisks: [disk, lost], snapraid: snapraidWith([failedScrub]) }), {
    tentaNasElasticArrayDestroyRequest: { job: { jobId: 'job-kill', status: 'running' } },
  });
  const cell = body.querySelector('.disk-cell[data-branch="d2"]');
  assert.equal(cell.querySelector('.dc-name').textContent.trim(), 'brak dysku');
  assert.match(cell.textContent, /dysk danych 2/);
  assert.doesNotMatch(cell.textContent, /\bd2\b/, 'the slot key is not printed');
  assert.equal(Boolean(cell.querySelector('[data-act="fix"]')), false, 'no repair on a disk nobody can name');
  // An absent data disk blocks the repair of the whole array, and the
  // reason names the member by its part, not by a slot or an id.
  assert.equal(body.querySelector('[data-act="fix"]'), null, 'no repair while a data disk is absent');
  assert.match(body.textContent, /dysk danych 2/);
  assert.equal(cell.querySelector('[data-act="disk"]').getAttribute('title'), 'Szczegóły dysku dysk danych 2');

  click(body.querySelector('.danger-zone [data-act="destroy"]'));
  await flush();
  const win = document.querySelector('tf-window');
  assert.match(win.textContent, /: vdb, dysk danych 2/, 'the dissolve list names disks, not slots');
  win.remove();
  screen.dispose();
});

/// The same cell said red "brak dysku" and "Obecny: tak" at once whenever no
/// name reached it — right after a core start, before the inventory's first
/// pass. Absence is the helper's `devicePresent === false` and nothing else;
/// a present member without a live name is named by its part in the array,
/// and a remembered name appears only on its own line, marked as last-known,
/// never in the name's place.
test('obecny członek bez nazwy nie jest „brakiem dysku”, a ostatnia znana nazwa jest oznaczona', async () => {
  const unnamed = { ...disk, diskId: 'serial-8', name: 'd2', diskName: '', diskLastName: 'sdq', devicePresent: true, mountpoint: '/mnt/tentanas-branches/media/data/d2' };
  const gone = { ...disk, diskId: 'serial-9', name: 'd3', diskName: '', diskLastName: 'sdr', devicePresent: false, mounted: false, mountpoint: '/mnt/tentanas-branches/media/data/d3' };
  const { screen, body } = await mount(array({ dataDisks: [disk, unnamed, gone] }));
  const present = body.querySelector('.disk-cell[data-branch="d2"]');
  assert.equal(present.querySelector('.dc-name').textContent.trim(), 'dysk danych 2');
  assert.doesNotMatch(present.textContent, /brak dysku/, 'present is not missing');
  assert.equal(Boolean(present.querySelector('.dc-name .num-err')), false, 'no error styling on a present member');
  assert.match(present.textContent, /Obecny: Tak/);
  assert.equal(present.querySelector('[data-role="last-seen"]').textContent.trim(), 'ostatnio widziany jako sdq');
  assert.notEqual(present.querySelector('.dc-name').textContent.trim(), 'sdq', 'the remembered name is never the name');
  const missing = body.querySelector('.disk-cell[data-branch="d3"]');
  assert.equal(missing.querySelector('.dc-name').textContent.trim(), 'brak dysku');
  assert.equal(Boolean(missing.querySelector('.dc-name .num-err')), true);
  assert.equal(missing.querySelector('[data-role="last-seen"]').textContent.trim(), 'ostatnio widziany jako sdr');
  // A live name needs no "last seen" line.
  assert.equal(body.querySelector('.disk-cell[data-branch="d1"] [data-role="last-seen"]'), null);
  screen.dispose();
});

/// Every sentence the three dialogs and the three gates put on screen exists in
/// all five bundles. `i18n-parity` proves the KEYS are there; this proves the
/// screen asks for the ones it means, in the language the admin picked.
test('etykiety naprawy, dodania dysku i rozwiązania są tłumaczone w pięciu locale', async () => {
  const locales = [
    ['pl', 'Napraw z parity', 'Dodaj dysk danych', 'Rozwiąż macierz'],
    ['en', 'Repair from parity', 'Add data disk', 'Dissolve the array'],
    ['de', 'Aus Parität reparieren', 'Datenträger hinzufügen', 'Array auflösen'],
    ['es', 'Reparar desde la paridad', 'Añadir disco de datos', 'Disolver la matriz'],
    ['fr', 'Réparer depuis la parité', 'Ajouter un disque de données', 'Dissoudre la matrice'],
  ];
  try {
    for (const [language, repair, add, dissolve] of locales) {
      await I18n.setLanguage(language);
      const { screen, body } = await mount(array({ snapraid: snapraidWith([failedScrub]) }));
      assert.equal(body.querySelector('[data-act="fix"]').textContent.trim(), repair, language);
      assert.equal(body.querySelector('[data-act="add-disk"]').textContent.trim(), add, language);
      assert.equal(body.querySelector('.danger-zone [data-act="destroy"]').textContent.trim(), dissolve, language);
      screen.dispose();
    }
  } finally {
    await I18n.setLanguage('pl');
  }
});

const folders = [
  { name: 'filmy', path: '/mnt/media/filmy', cachePolicy: 'yes', shareId: 'sh-1', shareLabel: 'Filmy' },
  { name: 'foto', path: '/mnt/media/foto', cachePolicy: 'only', shareId: '', shareLabel: '' },
  { name: 'backup', path: '/mnt/media/backup', cachePolicy: 'no', shareId: '', shareLabel: '' },
];

test('tabela Foldery rozdziela nieznaną listę od macierzy bez folderów', async (t) => {
  await t.test('nieodczytana unia bez zapisanych polityk', async () => {
    const { screen, body } = await mount(array({ folders: [], foldersKnown: false }));
    const card = body.querySelector('.nas-folders');
    assert.ok(card, 'karta Foldery istnieje');
    assert.equal(card.querySelector('.nas-folder-rows'), null, 'nie ma wiersza, bo nic nie wiemy');
    assert.match(card.textContent, /Nie można odczytać listy folderów/);
    assert.doesNotMatch(card.textContent, /nie ma jeszcze folderów/);
    screen.dispose();
  });
  await t.test('odczytana unia, która naprawdę nie ma folderów', async () => {
    const { screen, body } = await mount(array({ folders: [], foldersKnown: true }));
    const card = body.querySelector('.nas-folders');
    assert.match(card.textContent, /nie ma jeszcze folderów/);
    assert.doesNotMatch(card.textContent, /Nie można odczytać listy folderów/);
    screen.dispose();
  });
  await t.test('nieodczytana unia, ale z zapisanym przypięciem', async () => {
    const { screen, body } = await mount(array({ folders: [folders[1]], foldersKnown: false }));
    const card = body.querySelector('.nas-folders');
    assert.equal(card.querySelectorAll('.nas-folder-rows .fr[data-folder]').length, 1);
    assert.match(card.textContent, /tylko te z zapisaną polityką/);
    screen.dispose();
  });
  // Brak pola na drucie to też „nie zmierzono”, nie „brak folderów”.
  const { screen, body } = await mount(array({ folders: [] }));
  assert.match(body.querySelector('.nas-folders').textContent, /Nie można odczytać listy folderów/);
  screen.dispose();
});

test('wiersz folderu pokazuje udział, politykę i ostrzeżenie o bajtach poza parity', async () => {
  const { screen, body } = await mount(array({ folders, foldersKnown: true }));
  const rows = [...body.querySelectorAll('.nas-folder-rows .fr[data-folder]')];
  assert.deepEqual(rows.map((r) => r.dataset.folder), ['filmy', 'foto', 'backup']);
  assert.match(rows[0].textContent, /Filmy/);
  assert.match(rows[0].textContent, /\/mnt\/media\/filmy/);
  assert.match(rows[0].textContent, /Tak \(domyślnie\)/);
  assert.match(rows[1].textContent, /Tylko cache/);
  assert.match(rows[2].textContent, /brak/, 'folder bez udziału mówi „brak”, nie zostaje pusty');
  // Przypięty folder jest oznaczony i karta mówi, co to znaczy dla parity.
  assert.equal(rows[1].querySelector('tf-chip').getAttribute('label'), 'poza parity');
  assert.equal(rows[0].querySelector('tf-chip'), null);
  assert.match(body.querySelector('.nas-folders-pinned').textContent, /pozostają poza parity/);
  screen.dispose();
});

test('kolumna Cache jest kontrolką tylko dla admina', async () => {
  const { screen, body } = await mount(array({ folders, foldersKnown: true }), {}, { admin: false });
  assert.equal(body.querySelector('.nas-folders [data-act="folder-cache"]'), null);
  assert.match(body.querySelector('.nas-folders .fr[data-folder="foto"]').textContent, /Tylko cache/);
  screen.dispose();
});

test('dialog polityki cache ostrzega przy „tylko cache” i zapisuje wybór jednym żądaniem', async () => {
  let sent = null;
  const { screen, body } = await mount(array({ folders, foldersKnown: true }), {
    tentaNasElasticFolderCacheSetRequest: (payload) => { sent = payload; return { array: array({ folders, foldersKnown: true }) }; },
  });
  click(body.querySelector('.nas-folders .fr[data-folder="filmy"] [data-act="folder-cache"]'));
  await flush();
  const win = document.querySelector('tf-window.nas-folder-cache');
  assert.ok(win, 'okno się otwiera');
  const select = win.querySelector('#nas-folder-policy');
  assert.equal(select.value, 'yes');
  // Ostrzeżenie pojawia się tam, gdzie admin wybiera — nie tylko w komentarzu.
  assert.equal(win.querySelector('#nas-folder-policy-warn').hidden, true);
  select.value = 'only';
  select.dispatchEvent(new window.CustomEvent('change', { detail: { value: 'only' }, bubbles: true }));
  const warn = win.querySelector('#nas-folder-policy-warn');
  assert.equal(warn.hidden, false);
  assert.match(warn.textContent, /pozostaną poza parity tak długo, jak trwa przypięcie/);
  assert.match(win.querySelector('#nas-folder-policy-sub').textContent, /nigdy nie zabiera plików z cache/);

  confirmWindow(win);
  await flush();
  await flush();
  assert.deepEqual(sent, { name: 'media', folder: 'filmy', cachePolicy: 'only' });
  // Zapis odświeża detal, więc tabela pokazuje stan po zapisie, a nie draft.
  assert.equal(screen.calls.filter((c) => c.kind === 'tentaNasElasticArrayGetRequest').length, 2);
  screen.dispose();
});

test('odmowa zapisu polityki zostaje w oknie i nic nie zmienia', async () => {
  const { screen, body } = await mount(array({ folders, foldersKnown: true }), {
    tentaNasElasticFolderCacheSetRequest: () => { throw new Error('Ten folder nie istnieje w tej macierzy'); },
  });
  click(body.querySelector('.nas-folders .fr[data-folder="foto"] [data-act="folder-cache"]'));
  await flush();
  const win = document.querySelector('tf-window.nas-folder-cache');
  assert.equal(win.querySelector('#nas-folder-policy').value, 'only');
  confirmWindow(win);
  await flush();
  await flush();
  const err = win.querySelector('#nas-folder-error');
  assert.equal(err.hidden, false);
  assert.match(err.textContent, /Ten folder nie istnieje w tej macierzy/);
  assert.ok(win.isConnected, 'okno zostaje otwarte po odmowie');
  assert.equal(screen.calls.filter((c) => c.kind === 'tentaNasElasticArrayGetRequest').length, 1);
  screen.dispose();
});
