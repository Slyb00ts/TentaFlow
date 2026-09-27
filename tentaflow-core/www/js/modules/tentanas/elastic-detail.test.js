// =============================================================================
// Plik: modules/tentanas/elastic-detail.test.js
// Opis: Rzeczywisty detal Elastic z kontrolowanym transportem, pomiarami null i guardami.
// Przykład: node --test --import ./js/_test-register.js js/modules/tentanas/elastic-detail.test.js
// =============================================================================

import { fakeScreen, flush, click, confirmWindow, typeInto, I18n } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { drawElasticDetail, elasticCapacity, elasticCardSkeletonHtml, paintElasticCard, elasticState } from './elastic-detail.js';
import { fmtOptionalBytes, jobCanCancel } from './format.js';

// M2 (critic-round2-wave1-2026-09-22.md): the n05 array card is now a
// skeleton + a painter, the same split as a ZFS pool card. This helper
// exercises both together, exactly as `pools.js` does on first build, so the
// tests below that only care about the rendered markup do not have to.
function renderedElasticCard(array) {
  const box = document.createElement('div');
  box.innerHTML = elasticCardSkeletonHtml(array);
  const card = box.firstElementChild;
  paintElasticCard(card, array);
  return card;
}

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

// B5: the create policy is the node's real mount option (`mfs`), but in
// mergerfs' shorthand; the pane words it in the reader's language.
test('the union\'s create policy reads in words, not as the mergerfs code', async () => {
  const { screen, body } = await mount(array());
  try {
    assert.equal(body.querySelector('[data-f="create-policy"]').textContent, 'najwięcej wolnego miejsca');
  } finally {
    screen.dispose?.();
    body.remove();
  }
});

// n05 M9: the card names the bytes waiting on the cache outside parity (the
// node's figure, only when non-zero) as a warning chip in its head, and the
// mover as n11 states it — and a later paint patches the chip in place.
test('the n05 card carries the cache-without-parity chip and the mover row from the node\'s figures', () => {
  const cache = { ...disk, diskId: 'serial-c', name: 'c1', diskName: 'nvme0n1', role: 'cache' };
  const withCache = (bytes) => array({ cacheDisks: [cache], protection: { status: 'protected', faultTolerance: 1, cacheUnprotectedBytes: bytes, protectedAsOf: '2026-09-07 12:00:00' }, mover: {} });
  const card = renderedElasticCard(withCache(18 * GiB));
  const chip = card.querySelector('[data-slot="cache-waiting"] tf-chip');
  assert.ok(chip, 'the chip is there');
  assert.equal(chip.getAttribute('label'), `${fmtOptionalBytes(18 * GiB)} na cache bez parity`);
  assert.equal(chip.getAttribute('status'), 'warn');
  assert.equal(card.querySelector('[data-f="mover"]').textContent, 'bez ograniczeń — automatycznie');
  paintElasticCard(card, withCache(20 * GiB));
  assert.ok(card.querySelector('[data-slot="cache-waiting"] tf-chip') === chip, 'patched in place');
  paintElasticCard(card, withCache(0));
  assert.equal(card.querySelector('[data-slot="cache-waiting"] tf-chip'), null, 'nothing waiting, no chip');
  assert.equal(renderedElasticCard(array()).querySelector('[data-slot="cache-waiting"] tf-chip'), null, 'no cache, no chip');
});

test('null nie staje się zerem, zaś zmierzone zero pozostaje 0 B', () => {
  assert.equal(fmtOptionalBytes(null), '—');
  assert.equal(fmtOptionalBytes(undefined), '—');
  assert.equal(fmtOptionalBytes(NaN), '—');
  assert.equal(fmtOptionalBytes(0), '0 B');
  assert.equal(elasticCapacity(array({ usableBytes: null })).free, null);
  assert.equal(elasticCapacity(array()).free, 31 * GiB);
  assert.match(renderedElasticCard(array({ usedBytes: null })).outerHTML, /nas-unmeasured/);
  assert.doesNotMatch(renderedElasticCard(array({ usedBytes: null })).outerHTML, /width:0%/);
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
      const card = renderedElasticCard(value);
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
  // n11: the shell's breadcrumb ("… › Pule › media") is the way back; the
  // pane has no second one.
  assert.equal(body.querySelector('[data-act="back"]'), null);
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

test('zmiana noda podczas sudo nie wysyła starej mutacji; spóźniony Get nie odmalowuje powierzchni', async () => {
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

// Wave 6: the array state detail arrives as codes beside the node's English.
// The n05 card and the n11 pane read the reader's language, name members by
// kernel name or number (never the slot), and keep the English as the
// tooltip; a poll that brings a new state patches the same line.
test('the array state is worded from its codes on the card and the pane, the English only in the tooltip', async () => {
  const english = 'data disk #1, data disk sdh are present and not mounted yet — the next reconcile mounts them and then the union';
  const pending = array({
    state: 'pending',
    stateDetail: english,
    stateReasons: [{ code: 'branches_mountable', params: { data: '#1,sdh' } }],
  });
  const card = renderedElasticCard(pending);
  const reason = card.querySelector('.pc-reason');
  assert.equal(reason.textContent, 'Obecne i jeszcze niezamontowane: dysk danych nr 1, dysk danych sdh. Następne uzgodnienie zamontuje je, a potem unię.');
  assert.equal(reason.getAttribute('title'), english);

  let current = pending;
  const { screen, body, poll } = await mountPolled(() => current);
  try {
    const line = body.querySelector('[data-f="state-detail"]');
    assert.match(line.textContent, /^Obecne i jeszcze niezamontowane: dysk danych nr 1, dysk danych sdh\./);
    assert.equal(line.getAttribute('title'), english);
    current = array({
      state: 'error',
      stateDetail: 'data disk sdh is not on this node — the union stays down',
      stateReasons: [{ code: 'branches_gone', params: { data: 'sdh', union: 'down' } }],
    });
    await poll();
    assert.equal(body.querySelector('[data-f="state-detail"]'), line, 'the same line, patched');
    assert.match(line.textContent, /^Brak na tym węźle: dysk danych sdh\. Unia pozostaje odmontowana/);
    // A sentence the node stored without codes is said generically, in the
    // reader's language, with the sentence as its tooltip.
    current = array({ state: 'disabled', enabled: false, stateDetail: 'switched off by the import: the cache disk is not on this node' });
    await poll();
    assert.equal(line.textContent, 'Węzeł opisał stan tej macierzy tylko własnymi słowami — szczegóły w podpowiedzi.');
    assert.equal(line.getAttribute('title'), 'switched off by the import: the cache disk is not on this node');
  } finally {
    screen.dispose();
  }
});

// Wave-6 critic MINOR 4: a sentence the array's row STORES reaches the
// screen coded and worded; the stored error is only the (id-filtered)
// tooltip, and an older uncoded row goes through the id filter too.
test('a stored array sentence is worded from its code, and an uncoded one never shows an id', () => {
  const wwn = 'mkfs.xfs failed on /dev/disk/by-id/wwn-0x5000c500a1b2c3d4';
  const failed = renderedElasticCard(array({
    state: 'needs_attention', stateDetail: wwn,
    stateReasons: [{ code: 'operation_failed', params: { operation: 'create' } }],
  })).querySelector('.pc-reason');
  assert.equal(failed.textContent, 'Operacja „Tworzenie Elastic Array” nie powiodła się — szczegóły w podpowiedzi.');
  assert.doesNotMatch(failed.getAttribute('title'), /wwn-0x5000/);
  assert.match(failed.getAttribute('title'), /^mkfs\.xfs failed on /);
  const lost = renderedElasticCard(array({
    state: 'needs_attention', stateDetail: 'Utracono nadzór core; stan zadania nie dowodzi zakończenia I/O',
    stateReasons: [{ code: 'supervision_lost', params: {} }],
  })).querySelector('.pc-reason');
  assert.match(lost.textContent, /^Węzeł stracił nadzór nad operacją tej macierzy/);
  const unknownOp = renderedElasticCard(array({ state: 'needs_attention', stateDetail: 'x', stateReasons: [{ code: 'operation_failed', params: { operation: 'replace_disk' } }] })).querySelector('.pc-reason');
  assert.equal(unknownOp.textContent, 'Operacja na tej macierzy nie powiodła się — szczegóły w podpowiedzi.');
  const old = renderedElasticCard(array({ state: 'needs_attention', stateDetail: wwn })).querySelector('.pc-reason');
  assert.equal(old.textContent, 'Węzeł opisał stan tej macierzy tylko własnymi słowami — szczegóły w podpowiedzi.', 'an uncoded stored sentence is said generically');
  assert.doesNotMatch(old.getAttribute('title'), /wwn-0x5000/, 'and its tooltip goes through the id filter');
  assert.match(old.getAttribute('title'), /^mkfs\.xfs failed on /);
});

test('odpowiedź obcej macierzy i HTML w diagnostyce są bezpiecznie odrzucane/renderowane', async () => {
  const wrong = await mount(array({ name: 'other' }));
  assert.ok(wrong.body.querySelector('tf-alert'));
  assert.equal(wrong.body.querySelector('.kpi'), null);
  wrong.screen.dispose();
  const safe = await mount(array({ stateDetail: '<img src=x onerror=alert(1)>' }));
  assert.equal(safe.body.querySelector('img'), null);
  // Uncoded, so said generically; the markup is the tooltip's TEXT.
  assert.match(safe.body.querySelector('[data-f="state-detail"]').getAttribute('title'), /<img/);
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
test('maintenance wymaga administratora, parity i dopuszczenia przebiegu przez node', async (t) => {
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
      const reasons = ['no_parity', 'precondition_failed', 'unsynced_changes', 'empty_parity',
        // The helper's own admission gate (0.14.0).
        'operation_pending', 'attention_other', 'fault_unacknowledged', 'attention_add_disk'];
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
  assert.match(explain.textContent, /Node sam przenosi/);
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
  assert.equal(lines[0].querySelector('.k').textContent, 'Na cache bez parity');
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
  assert.ok(body.querySelector('.nas-cache-pending') === pending, 'ten sam node, nie przebudowa');
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
    const { screen, body } = await mount(array({ state: 'needs_attention', syncFaultId: 'fault-1', snapraid: snapraidWith(history) }), {
      tentaNasElasticArraySyncRequest: { job: { jobId: 'job-sync', status: 'running' } },
    });
    for (const act of ['sync', 'scrub']) {
      assert.equal(body.querySelector(`[data-act="${act}"]`).hasAttribute('disabled'), false, `${act}: ${history[0].outcome}`);
    }
    assert.equal(body.querySelector('[data-act="replace-disk"]'), null, 'wymiana dysku jest wycofana');
    click(body.querySelector('[data-act="sync"]'));
    await flush();
    // A scrub that marked blocks nothing has repaired is a fault a Sync pays
    // for: the Sync goes through the confirm that names the cost, and only
    // that confirm acknowledges it. A failed Sync is not such a fault.
    const fault = history[history.length - 1].kind === 'scrub';
    if (fault) {
      const win = document.querySelector('tf-window');
      assert.ok(win, `${history[0].outcome}: the confirm opens`);
      // Critic wave 5, MINOR 3: the confirm says what was never measured.
      assert.match(win.querySelector('.explain-box').textContent, /Dwóch przypadków nie zmierzono: pliku, którego scrub nie mógł odczytać, a który też się zmienił, oraz dysku, który zacznie zawodzić w trakcie Sync/);
      typeInto(win.querySelector('#retype-input'), 'media');
      confirmWindow(win);
      await flush();
    }
    assert.deepEqual(
      screen.calls.filter((c) => c.kind === 'tentaNasElasticArraySyncRequest').map((c) => c.payload),
      [fault ? { name: 'media', acknowledgeParityFault: 'fault-1', sudoPassword: 'hunter2' } : { name: 'media', sudoPassword: 'hunter2' }],
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
  assert.match(blocked.body.querySelector('.nas-snapraid').textContent, /nie widać na tym nodzie/);
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
  // The loss line names the disk as the cell does, never by its branch
  // mountpoint, which ends in the internal slot name.
  assert.match(win.querySelector('.loss-list').textContent, /vdb — oznaczone bloki/);
  assert.doesNotMatch(win.textContent, /tentanas-branches/);
  const confirm = win.querySelector('[data-action="confirm"]');
  assert.ok(confirm.hasAttribute('disabled'), 'przycisk startuje zablokowany');
  typeInto(win.querySelector('#retype-input'), 'media');
  assert.ok(confirm.hasAttribute('disabled'), 'nazwa macierzy nie uzbraja naprawy dysku');
  // The admin retypes the name on the cell. The slot is not it.
  typeInto(win.querySelector('#retype-input'), 'd1');
  assert.ok(confirm.hasAttribute('disabled'), 'the slot key does not arm the repair');
  assert.match(win.textContent, /Przepisz nazwę dysku, aby potwierdzić: vdb/);
  typeInto(win.querySelector('#retype-input'), 'vdb');
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
  typeInto(win.querySelector('#retype-input'), 'media');
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
  typeInto(win.querySelector('#retype-input'), 'medi');
  assert.ok(confirm.hasAttribute('disabled'), 'częściowa nazwa nie uzbraja');
  typeInto(win.querySelector('#retype-input'), 'media');
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

// =============================================================================
// Stable skeleton, keyed patching (owner's rule: a poll patches what changed
// and never rebuilds a subtree; buttons and open sections survive it)
// =============================================================================

// Mounts the detail with its poll captured instead of timed, so a test runs
// the NEXT poll itself, exactly when it has changed what the node answers.
async function mountPolled(read) {
  const screen = fakeScreen({ tentaNasElasticArrayGetRequest: () => ({ array: read() }) });
  screen.array = 'media';
  screen.openArray = (name) => { screen.array = name; };
  const scheduled = [];
  screen.later = (fn) => { scheduled.push(fn); };
  const body = document.createElement('div');
  document.body.appendChild(body);
  await drawElasticDetail(screen, body);
  await flush();
  const poll = async () => {
    const next = scheduled.shift();
    assert.ok(next, 'the pane armed its next poll');
    await next();
    await flush();
  };
  return { screen, body, poll };
}

// Every node a poll must leave standing, collected by a stable selector.
const standing = (body) => ({
  kpi: body.querySelector('.kpi'),
  tiles: [...body.querySelectorAll('.kpi tf-stat-card')],
  disks: body.querySelector('.disk-cells').closest('.section-card'),
  folders: body.querySelector('.nas-folders'),
  state: body.querySelector('.grid-2 > .section-card:first-child'),
  snapraid: body.querySelector('.nas-snapraid'),
  history: body.querySelector('.nas-snapraid-history'),
  mover: body.querySelector('details[data-section="mover"]'),
  danger: body.querySelector('.danger-zone'),
  cell: body.querySelector('.disk-cell[data-branch="d1"]'),
  fix: body.querySelector('.disk-cell[data-branch="d1"] [data-act="fix"]'),
  buttons: ['refresh', 'sync', 'scrub', 'mover', 'mover-schedule', 'add-disk', 'destroy'].map((act) => body.querySelector(`[data-act="${act}"]`)),
  run: body.querySelector('.nas-snapraid-history li'),
});

function assertSameNodes(before, after) {
  for (const key of Object.keys(before)) {
    if (Array.isArray(before[key])) {
      assert.equal(after[key].length, before[key].length, key);
      // `ok(===)`, never `equal` on DOM nodes: a failing `equal` diffs the
      // whole document and does not return.
      before[key].forEach((node, i) => assert.ok(node && after[key][i] === node, `${key}[${i}] is the same node`));
    } else {
      assert.ok(before[key] && after[key] === before[key], `${key} is the same node`);
    }
  }
}

test('a poll that changes the mover, the history and the cache figure patches values and keeps every node', async () => {
  const lastRun = { startedAt: '2026-09-08T14:00:00Z', finishedAt: '2026-09-08T14:06:00Z', outcome: 'ok', movedBytes: 42 * GiB, movedFiles: 7, skippedBytes: 0, skippedFiles: 0, countsKnown: true, coupledSync: { kind: 'sync', outcome: 'ok' } };
  let second = false;
  // A scrub that counted errors keeps a repair on offer across both polls, so
  // the repair button is one of the nodes that has to survive.
  const read = () => moverArray({
    protection: { status: 'window_open', cacheUnprotectedBytes: (second ? 20 : 18) * GiB, movedUnsyncedBytes: 0, protectedAsOf: '2026-09-07 12:00:00' },
    snapraid: snapraidWith(second
      ? [{ kind: 'sync', outcome: 'ok', jobId: 'sync-2', startedAt: '2026-09-08 14:06:00', finishedAt: '2026-09-08 14:07:00' }, { ...failedScrub, outcome: 'failed' }]
      : [failedScrub]),
  }, second ? { lastRun, history: [lastRun] } : {});
  const { screen, body, poll } = await mountPolled(read);
  try {
    const before = standing(body);
    assert.ok(before.fix, 'the fixture offers a repair');
    before.mover.open = true;
    before.mover.dispatchEvent(new window.Event('toggle'));
    const details = before.run.querySelector('details');
    details.open = true;
    details.dispatchEvent(new window.Event('toggle'));
    assert.match(moverRow(before.mover, 'Ostatni przebieg').textContent, /—/);
    assert.equal(before.run.querySelector('tf-chip').getAttribute('label'), 'Wymaga uwagi');

    second = true;
    await poll();
    assertSameNodes(before, { ...standing(body), run: body.querySelectorAll('.nas-snapraid-history li')[1] });
    // The values moved.
    assert.match(moverRow(before.mover, 'Ostatni przebieg').textContent, /42 GiB/);
    assert.match(before.mover.querySelector('.mover-hist').textContent, /42 GiB/);
    assert.equal(before.run.querySelector('tf-chip').getAttribute('label'), 'Błąd');
    assert.equal(body.querySelectorAll('.nas-snapraid-history li').length, 2, 'the new run is added');
    assert.equal(body.querySelector('.nas-cache-pending .v').textContent, '20 GiB');
    assert.equal(before.tiles[1].getAttribute('value'), '20 GiB');
    // And what the admin opened is still open.
    assert.equal(before.mover.open, true);
    assert.equal(details.open, true);

    // The busy flag of a pending request is also a patch, not a rebuild.
    // The Scrub is what is started here: over the scrub's unrepaired errors
    // the Sync goes through its confirm first (F1), which is no request yet.
    let release;
    screen.withSudo = async (fn) => { await new Promise((resolve) => { release = resolve; }); return fn('x'); };
    click(body.querySelector('[data-act="scrub"]'));
    assert.ok(before.buttons[1].hasAttribute('disabled'), 'sync is disabled while a request is pending');
    assertSameNodes(before, { ...standing(body), run: body.querySelectorAll('.nas-snapraid-history li')[1] });
    release();
    await flush();
  } finally { screen.dispose(); }
});

test('a disk added to the array adds only its own cell', async () => {
  const added = { ...disk, diskId: 'serial-7', name: 'd2', diskName: 'vdd', mountpoint: '/mnt/tentanas-branches/media/data/d2' };
  let grown = false;
  const { screen, body, poll } = await mountPolled(() => moverArray({ dataDisks: grown ? [disk, added] : [disk] }));
  try {
    const cells = [...body.querySelectorAll('.disk-cell')];
    const hosts = [...body.querySelectorAll('.disk-cells')];
    const section = body.querySelector('.disk-cells').closest('.section-card');
    assert.equal(cells.length, 3, 'd1, parity and cache');
    grown = true;
    await poll();
    const after = [...body.querySelectorAll('.disk-cell')];
    assert.equal(after.length, 4);
    for (const cell of cells) assert.ok(after.includes(cell), `${cell.dataset.branch} is the same node`);
    const fresh = after.filter((cell) => !cells.includes(cell));
    assert.equal(fresh.length, 1);
    assert.equal(fresh[0].dataset.branch, 'd2');
    assert.equal(fresh[0].querySelector('.dc-name').textContent.trim(), 'vdd');
    [...body.querySelectorAll('.disk-cells')].forEach((host, i) => assert.ok(host === hosts[i], `cell host ${i} is the same node`));
    assert.ok(body.querySelector('.disk-cells').closest('.section-card') === section);
  } finally { screen.dispose(); }
});

// n11: KPI "Ochrona" reads "18 GiB na cache" in the warning colour, and the
// SnapRAID card repeats the figure beside the parity it is outside of.
test('bajty na cache poza parity prowadzą kafel Ochrona i mają wiersz na karcie SnapRAID', async () => {
  let bytes = 18 * GiB;
  const { screen, body, poll } = await mountPolled(() => moverArray({ protection: { status: 'window_open', cacheUnprotectedBytes: bytes, movedUnsyncedBytes: 0 } }));
  try {
    const tile = body.querySelector('.kpi tf-stat-card:nth-child(2)');
    assert.equal(tile.getAttribute('value'), '18 GiB');
    assert.equal(tile.getAttribute('accent'), 'warning');
    assert.equal(tile.getAttribute('delta-type'), 'warn');
    assert.equal(tile.getAttribute('delta'), 'Na cache bez parity');
    const rowOf = () => moverRow(body.querySelector('.nas-snapraid > .stat-rows'), 'Na cache bez parity');
    assert.ok(rowOf(), 'the SnapRAID card has the cache row');
    assert.equal(rowOf().querySelector('.v').textContent, '18 GiB');
    assert.ok(rowOf().querySelector('.v').classList.contains('num-warn'));
    // Drained: the tile says the checkpoint's state again, on the same node.
    bytes = 0;
    await poll();
    assert.ok(body.querySelector('.kpi tf-stat-card:nth-child(2)') === tile);
    assert.equal(tile.getAttribute('value'), 'Dane poza checkpointem');
    assert.equal(tile.hasAttribute('accent'), false);
    assert.equal(tile.hasAttribute('delta-type'), false);
    assert.equal(rowOf().querySelector('.v').textContent, '0 B');
    assert.equal(rowOf().querySelector('.v').classList.contains('num-warn'), false);
  } finally { screen.dispose(); }
  // No cache, no row; no parity, no figure in the tile (every byte is outside).
  const plain = await mount(array());
  try {
    assert.equal(moverRow(plain.body.querySelector('.nas-snapraid > .stat-rows'), 'Na dysku cache'), undefined);
  } finally { plain.screen.dispose(); }
  const bare = await mount(moverArray({ parityDisks: [], protection: { status: 'unprotected', cacheUnprotectedBytes: 18 * GiB } }));
  try {
    const tile = bare.body.querySelector('.kpi tf-stat-card:nth-child(2)');
    assert.equal(tile.getAttribute('value'), 'Bez ochrony parity');
    assert.equal(tile.hasAttribute('accent'), false);
  } finally { bare.screen.dispose(); }
});

test('błędy parity mówią, z jakiego okna są liczone', async () => {
  const { screen, body } = await mount(array({ snapraid: { ...snapraidWith([], 0), parityErrorsWindowDays: 30 } }));
  try {
    const r = moverRow(body.querySelector('.nas-snapraid > .stat-rows'), 'Błędy parity');
    assert.equal(r.querySelector('.k').textContent, 'Błędy parity (30 dni)');
    assert.equal(r.querySelector('.v').textContent, '0');
  } finally { screen.dispose(); }
});

// m26 / backlog 2026-09-21: the array detail drew a second breadcrumb bar
// ("Pule › media") under the shell's "TentaNas › helios". n11 has one bar,
// so the view hands its tail to the shell exactly as the ZFS pool detail does
// — before the first read, so a failed read still has the way back.
test('the array detail names its tail in the shell breadcrumb and draws no bar of its own', async () => {
  const screen = fakeScreen({ tentaNasElasticArrayGetRequest: () => { throw new Error('offline'); } });
  screen.array = 'media';
  screen.nodeId = 'node-orion';
  screen.openArray = (name) => { screen.array = name; };
  screen.crumbTails = [];
  screen.setCrumbTail = (tail) => { screen.crumbTails.push(tail); };
  try {
    const body = document.createElement('div');
    document.body.appendChild(body);
    await drawElasticDetail(screen, body);
    await flush();
    assert.equal(body.querySelectorAll('tf-breadcrumb, .nas-crumbs').length, 0, 'no second bar in the pane');
    assert.deepEqual(screen.crumbTails, [[{ label: 'Pule', act: 'pools', query: 'node=node-orion&tab=pools' }, { label: 'media' }]]);
  } finally {
    screen.dispose();
  }
});

// ----- recovery out of needs_attention (helper 0.14.0) ----------------------

// F1: a Sync over an unrepaired Scrub or Repair fault costs the files the
// Scrub could not read (measured on rig11). The button opens a confirm that
// names that cost, and ONLY that confirm sends the acknowledgement — the node
// and its helper refuse the Sync without it. Without a fault: no confirm, no
// flag.
test('Sync przy nienaprawionym błędzie wymaga potwierdzenia kosztu, a tylko ono wysyła zgodę', async () => {
  const failedFix = { kind: 'fix', outcome: 'failed', startedAt: '2026-09-08 02:00:00', finishedAt: '2026-09-08 02:10:00' };
  for (const [label, overrides] of [
    ['helper records the cause', { state: 'needs_attention', attention: 'scrub_failed', syncNeedsAcknowledgement: true, syncFaultId: 'fault-1' }],
    ['scrub marked blocks', { state: 'needs_attention', syncFaultId: 'fault-1', snapraid: snapraidWith([failedScrub]) }],
    ['repair failed', { state: 'needs_attention', syncFaultId: 'fault-1', snapraid: snapraidWith([failedFix]) }],
  ]) {
    const { screen, body } = await mount(array(overrides), {
      tentaNasElasticArraySyncRequest: { job: { jobId: 'job-sync', status: 'running' } },
    });
    const syncs = () => screen.calls.filter((c) => c.kind === 'tentaNasElasticArraySyncRequest');
    assert.match(body.querySelector('.nas-snapraid').textContent, /wymaga potwierdzenia/, label);
    click(body.querySelector('[data-act="sync"]'));
    await flush();
    assert.equal(syncs().length, 0, `${label}: nothing is sent before the confirm`);
    const win = document.querySelector('tf-window');
    assert.ok(win, label);
    assert.match(win.textContent, /nie da się już odtworzyć z parity/, `${label}: the confirm names the cost`);
    assert.match(win.textContent, /Oznaczone bloki niezmienionych plików oraz niezmienione pliki, których scrub nie mógł odczytać, pozostaną do naprawienia/, label);
    const confirm = win.querySelector('[data-action="confirm"]');
    typeInto(win.querySelector('#retype-input'), 'medi');
    assert.ok(confirm.hasAttribute('disabled'), `${label}: armed only by the array name`);
    typeInto(win.querySelector('#retype-input'), 'media');
    confirmWindow(win);
    await flush();
    // The confirm names THE fault it showed (M1): the id stays internal.
    assert.deepEqual(syncs().map((c) => c.payload), [{ name: 'media', acknowledgeParityFault: 'fault-1', sudoPassword: 'hunter2' }], label);
    assert.doesNotMatch(body.innerHTML, /fault-1/, `${label}: the id is never shown`);
    assert.deepEqual(screen.jobLogs.map((j) => j.jobId), ['job-sync'], label);
    screen.dispose();
  }
  // A second click while the confirm is open opens no second one.
  {
    const { screen, body } = await mount(array({ state: 'needs_attention', attention: 'scrub_failed', syncNeedsAcknowledgement: true, syncFaultId: 'fault-1' }));
    click(body.querySelector('[data-act="sync"]'));
    click(body.querySelector('[data-act="sync"]'));
    await flush();
    assert.equal(document.querySelectorAll('tf-window').length, 1);
    screen.dispose();
  }
  // M2: the fault ends with its cure — a clean scrub after the Sync, or for
  // file errors alone the Sync that drops them. Then no confirm is asked.
  const okSync = { kind: 'sync', outcome: 'ok', startedAt: '2026-09-09 01:00:00', finishedAt: '2026-09-09 01:10:00' };
  const okScrub = { kind: 'scrub', outcome: 'ok', errors: 0, startedAt: '2026-09-10 01:00:00', finishedAt: '2026-09-10 01:10:00' };
  for (const history of [[okScrub, okSync, failedScrub], [okSync, { ...failedScrub, errorsData: 0 }]]) {
    const { screen, body } = await mount(array({ snapraid: snapraidWith(history) }), {
      tentaNasElasticArraySyncRequest: { job: { jobId: 'job-sync', status: 'running' } },
    });
    assert.equal(body.querySelector('[data-act="fix"]'), null, 'no repair is left to offer');
    click(body.querySelector('[data-act="sync"]'));
    await flush();
    assert.equal(document.querySelector('tf-window'), null);
    screen.dispose();
  }
  // ...while a Sync alone does not end DATA errors: their marks stay repairable.
  const stillMarked = await mount(array({ snapraid: snapraidWith([okSync, { ...failedScrub, errorsData: 7 }]) }));
  assert.ok(stillMarked.body.querySelector('[data-act="fix"]'));
  stillMarked.screen.dispose();
  // A healthy array and a failed Sync: no confirm and no acknowledgement.
  for (const overrides of [{}, { state: 'needs_attention', attention: 'sync_failed', snapraid: snapraidWith([{ ...failedScrub, kind: 'sync', errors: null }]) }]) {
    const { screen, body } = await mount(array(overrides), {
      tentaNasElasticArraySyncRequest: { job: { jobId: 'job-sync', status: 'running' } },
    });
    click(body.querySelector('[data-act="sync"]'));
    await flush();
    assert.equal(document.querySelector('tf-window'), null);
    assert.deepEqual(screen.calls.filter((c) => c.kind === 'tentaNasElasticArraySyncRequest').map((c) => c.payload),
      [{ name: 'media', sudoPassword: 'hunter2' }]);
    screen.dispose();
  }
});

// F2: on a parity fault the next steps are listed in the order that costs
// least — the repair while a scrub's marks wait for it, a scrub, and the Sync
// last — and the helper's recorded cause is a sentence, never a code.
test('przy błędzie parity kolejne kroki to naprawa, scrub, Sync, a przyczyna jest zdaniem', async () => {
  const { screen, body } = await mount(array({ state: 'needs_attention', attention: 'scrub_failed', syncNeedsAcknowledgement: true, stateDetail: 'narzędzie zgłosiło błędy', snapraid: snapraidWith([failedScrub]) }));
  const steps = [...body.querySelectorAll('.nas-next-steps li')].map((li) => li.textContent);
  assert.equal(steps.length, 3);
  assert.match(steps[0], /Napraw z parity/);
  assert.match(steps[1], /scrub/);
  assert.match(steps[2], /Sync/);
  const detail = body.querySelector('[data-f="state-detail"]').textContent;
  assert.match(detail, /Ostatni scrub zgłosił błędy/);
  assert.doesNotMatch(body.textContent, /scrub_failed|narzędzie zgłosiło/);
  screen.dispose();
  // Without marks to write back there is no repair to list.
  const bare = await mount(array({ state: 'needs_attention', attention: 'scrub_failed', syncNeedsAcknowledgement: true }));
  assert.deepEqual([...bare.body.querySelectorAll('.nas-next-steps li')].length, 2);
  bare.screen.dispose();
  // A healthy array lists nothing.
  const healthy = await mount(array());
  assert.equal(healthy.body.querySelector('.nas-next-steps'), null);
  healthy.screen.dispose();
});

const pendingAddDisk = (overrides = {}) => ({ diskId: 'wwn-0x5000c500a1b2c3d4', diskName: 'sdh', diskLastName: '', step: 'mount', inUnion: false, undoPossible: true, ...overrides });

// Minor 1 of the release review: an array the helper holds for a cause only
// a Restore addresses — a journal an older helper left wedged, which the
// helper settles on that Restore — offers the Restore even with a failed
// parity run in the history.
test('przyczyna, którą rozwiązuje tylko odtworzenie, udostępnia Odtwórz montowania mimo nieudanego przebiegu', async () => {
  const wedged = await mount(array({ state: 'needs_attention', attention: 'other', parityRunAvailable: false, snapraid: snapraidWith([failedScrub]) }));
  assert.ok(wedged.body.querySelector('[data-act="restore"]'));
  wedged.screen.dispose();
  const fault = await mount(array({ state: 'needs_attention', attention: 'scrub_failed', snapraid: snapraidWith([failedScrub]) }));
  assert.equal(fault.body.querySelector('[data-act="restore"]'), null, 'a parity fault is not a Restore\'s to settle');
  fault.screen.dispose();
});

// F3: an add that stopped part-way is FINISHED from the array screen, on an
// array that needs attention too: "Dokończ dodawanie dysku sdh" sends the
// pinned disk, with no picker. The undo appears only while the node says the
// disk never joined the share. No id reaches the screen.
test('niedokończone dodanie dysku: „Dokończ dodawanie dysku” wysyła przypięty dysk bez wybierania, „Wycofaj” tylko przed dołączeniem', async () => {
  const { screen, body } = await mount(array({ state: 'needs_attention', attention: 'add_disk', unresolvedOperation: true, parityRunAvailable: false, pendingAddDisk: pendingAddDisk() }), {
    tentaNasElasticArrayAddDiskRequest: { job: { jobId: 'job-resume', status: 'running' } },
    tentaNasElasticArrayAddDiskAbortRequest: { job: { jobId: 'job-undo', status: 'running' } },
  });
  assert.equal(body.querySelector('[data-act="add-disk"]'), null, 'no other disk is offered');
  const resume = body.querySelector('[data-act="add-disk-resume"]');
  assert.ok(resume);
  assert.equal(resume.getAttribute('label'), 'Dokończ dodawanie dysku sdh');
  assert.equal(resume.hasAttribute('disabled'), false, 'finishing is enabled on an array that needs attention');
  assert.match(body.textContent, /Dodawanie dysku sdh nie zostało dokończone\. Zatrzymało się przed zamontowaniem dysku\./);
  assert.doesNotMatch(body.innerHTML, /wwn-0x5000c500a1b2c3d4/, 'no id on the screen');
  assert.match(body.querySelector('[data-f="state-detail"]').textContent, /Dodawanie dysku do macierzy nie zostało dokończone/);
  click(resume);
  await flush();
  const win = document.querySelector('tf-window');
  assert.ok(win);
  assert.equal(win.querySelector('#nas-add-disk'), null, 'the picker stays closed');
  typeInto(win.querySelector('#retype-input'), 'media');
  confirmWindow(win);
  await flush();
  assert.equal(screen.calls.filter((c) => c.kind === 'tentaNasElasticCapabilitiesRequest').length, 0, 'no free-disk list is asked for');
  assert.deepEqual(screen.calls.filter((c) => c.kind === 'tentaNasElasticArrayAddDiskRequest').map((c) => c.payload),
    [{ name: 'media', diskId: 'wwn-0x5000c500a1b2c3d4', confirmName: 'media', sudoPassword: 'hunter2' }]);
  // The undo: its own confirm, the pinned disk, the abort request.
  click(body.querySelector('[data-act="add-disk-undo"]'));
  await flush();
  const undo = [...document.querySelectorAll('tf-window')].pop();
  assert.match(undo.textContent, /wyczyści wyłącznie system plików, który nadało mu to dodawanie/);
  typeInto(undo.querySelector('#retype-input'), 'media');
  confirmWindow(undo);
  await flush();
  assert.deepEqual(screen.calls.filter((c) => c.kind === 'tentaNasElasticArrayAddDiskAbortRequest').map((c) => c.payload),
    [{ name: 'media', diskId: 'wwn-0x5000c500a1b2c3d4', confirmName: 'media', sudoPassword: 'hunter2' }]);
  assert.deepEqual(screen.jobLogs.map((j) => j.jobId), ['job-resume', 'job-undo']);
  screen.dispose();

  // Once the disk may have joined the share there is no undo, and the screen
  // says the add can only be finished.
  for (const pending of [pendingAddDisk({ step: 'sync', inUnion: true, undoPossible: false }), pendingAddDisk({ undoPossible: false, inUnion: null })]) {
    const joined = await mount(array({ state: 'needs_attention', attention: 'add_disk', unresolvedOperation: true, pendingAddDisk: pending }));
    assert.ok(joined.body.querySelector('[data-act="add-disk-resume"]'));
    assert.equal(joined.body.querySelector('[data-act="add-disk-undo"]'), null);
    assert.match(joined.body.textContent, /można tylko dokończyć/);
    joined.screen.dispose();
  }
  // A disk with no live name is "nowy dysk", never its id.
  const unnamed = await mount(array({ state: 'needs_attention', attention: 'add_disk', pendingAddDisk: pendingAddDisk({ diskName: '', diskLastName: 'sdq' }) }));
  assert.equal(unnamed.body.querySelector('[data-act="add-disk-resume"]').getAttribute('label'), 'Dokończ dodawanie dysku nowy dysk (ostatnio widziany jako sdq)');
  unnamed.screen.dispose();
  const nameless = await mount(array({ state: 'needs_attention', attention: 'add_disk', pendingAddDisk: pendingAddDisk({ diskName: '', diskLastName: '' }) }));
  assert.equal(nameless.body.querySelector('[data-act="add-disk-resume"]').getAttribute('label'), 'Dokończ dodawanie dysku nowy dysk');
  nameless.screen.dispose();
  // A repair is not offered over an unfinished add, whatever the history says.
  const held = await mount(array({ state: 'needs_attention', attention: 'add_disk', pendingAddDisk: pendingAddDisk(), snapraid: snapraidWith([failedScrub]) }));
  assert.equal(held.body.querySelector('[data-act="fix"]'), null);
  held.screen.dispose();
  // And a reader sees neither control.
  const reader = await mount(array({ state: 'needs_attention', attention: 'add_disk', pendingAddDisk: pendingAddDisk() }), {}, { admin: false });
  assert.equal(reader.body.querySelector('[data-act="add-disk-resume"]'), null);
  assert.equal(reader.body.querySelector('[data-act="add-disk-undo"]'), null);
  reader.screen.dispose();
});

// The poll rule (owner's hard rule): a pending add that only changes its
// step patches text; the buttons stay the same nodes.
test('zmiana kroku niedokończonego dodania nie przebudowuje przycisków', async () => {
  const value = array({ state: 'needs_attention', attention: 'add_disk', pendingAddDisk: pendingAddDisk() });
  let current = value;
  const { screen, body } = await mount(value, { tentaNasElasticArrayGetRequest: () => ({ array: current }) });
  const resume = body.querySelector('[data-act="add-disk-resume"]');
  const undo = body.querySelector('[data-act="add-disk-undo"]');
  current = array({ state: 'needs_attention', attention: 'add_disk', pendingAddDisk: pendingAddDisk({ step: 'join' }) });
  click(body.querySelector('[data-act="refresh"]'));
  await flush();
  assert.equal(body.querySelector('[data-act="add-disk-resume"]'), resume);
  assert.equal(body.querySelector('[data-act="add-disk-undo"]'), undo);
  assert.match(body.textContent, /przed dołączeniem dysku do udziału/);
  screen.dispose();
});

test('przyczyny uwagi, kroki dodawania i potwierdzenie Sync są tłumaczone w pięciu locale', async () => {
  try {
    for (const language of ['pl', 'en', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      for (const attention of ['sync_failed', 'scrub_failed', 'fix_failed', 'add_disk', 'other']) {
        const { screen, body } = await mount(array({ state: 'needs_attention', attention, syncNeedsAcknowledgement: attention !== 'sync_failed', pendingAddDisk: attention === 'add_disk' ? pendingAddDisk() : null }));
        assert.doesNotMatch(body.textContent, /tentanas\.|scrub_failed|sync_failed|fix_failed|add_disk\b|undefined/, `${language}/${attention}`);
        screen.dispose();
      }
      for (const code of ['operation_pending', 'attention_other', 'fault_unacknowledged', 'attention_add_disk', 'attention_parity_fault', 'disk_claimed', 'add_joined', 'precondition_failed', 'unsynced_changes', 'empty_parity', 'no_parity']) {
        assert.ok(!I18n.t(`tentanas.refusal.elastic_${code}`).startsWith('tentanas.'), `${language}/${code}`);
      }
      assert.ok(!I18n.t('tentanas.jobs.kind_elastic_add_disk_abort').startsWith('tentanas.'), language);
      assert.ok(!I18n.t('tentanas.approvals.op_elastic_add_disk_abort').startsWith('tentanas.'), language);
    }
  } finally { await I18n.setLanguage('pl'); }
});

// n11 "Użycie": the node's last bounded walk of each folder. A poll that
// brings a new figure writes it into the SAME cell, and a folder with no
// figure says "—" with the reason as its tooltip, never a zero.
test('the folder usage column shows the measured size, patches it in place and words a missing one', async () => {
  const measuredAt = new Date(Date.now() - 3 * 3600 * 1000).toISOString();
  let usage = { usedBytes: 2 * 1024 ** 4, usedMeasuredAt: measuredAt, usedReasons: [] };
  const read = () => array({
    foldersKnown: true,
    folders: [
      { ...folders[0], ...usage },
      { ...folders[1], usedBytes: null, usedReasons: [{ code: 'folder_usage_unreadable', params: {} }] },
      { ...folders[2] },
    ],
  });
  const { screen, body, poll } = await mountPolled(read);
  try {
    const cell = (name) => body.querySelector(`.nas-folders .fr[data-folder="${name}"] [data-f="folder-used"]`);
    const filmy = cell('filmy');
    assert.equal(filmy.textContent, '2.0 TiB');
    assert.match(filmy.getAttribute('title'), /^Zmierzono 3 h temu/);
    assert.equal(cell('foto').textContent, '—');
    assert.match(cell('foto').getAttribute('title'), /nie dało się odczytać/);
    // A node too old to send the field: no figure, no reason, no zero.
    assert.equal(cell('backup').textContent, '—');
    assert.equal(cell('backup').getAttribute('title'), 'Nie zmierzono');
    assert.match(body.querySelector('.nas-folders .fr-head').textContent, /Użycie/);

    usage = { usedBytes: 3 * 1024 ** 4, usedMeasuredAt: new Date().toISOString(), usedReasons: [] };
    await poll();
    assert.ok(cell('filmy') === filmy, 'the cell is the same node');
    assert.equal(filmy.textContent, '3.0 TiB');
    assert.match(filmy.getAttribute('title'), /^Zmierzono 0 s temu|^Zmierzono \d+ s temu/);
  } finally {
    screen.dispose?.();
    body.remove();
  }
});
