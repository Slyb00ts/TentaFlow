// =============================================================================
// Plik: modules/tentanas/pool-wizard-elastic.test.js
// Opis: Rzeczywisty kreator i komponenty DOM z kontrolowanym transportem NAS.
// =============================================================================
import { fakeScreen, flush, click, typeInto, window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { openPoolWizard, elasticNameValid } = await import('./pool-wizard.js');
const GB = 1024 ** 3;
const disks = [32, 64, 64, 64].map((n, i) => ({ diskId: `id${i}`, name: `sd${i}`, serial: `serial${i}`, sizeBytes: n * GB, health: 'ok', model: 'Disk', kind: 'hdd' }));
const job = { jobId: 'j1', kind: 'elastic_create', status: 'succeeded', log: [], progressPct: 100 };
function preview(p) {
  return { plan: { usableBytes: 96 * GB, rawBytes: 160 * GB, parityBytes: 64 * GB, cacheBytes: 0, faultTolerance: p.parityDiskIds.length, refusals: [], warnings: [], unionPath: `/mnt/${p.name}`, wipedDevices: [...p.dataDiskIds, ...p.parityDiskIds].map((id) => '/dev/' + disks.find((d) => d.diskId === id).name), stepsPreview: 'mkfs; mount' } };
}
const next = (win) => click(win.querySelector('[data-wizard-next]'));
const back = (win) => click(win.querySelector('[data-wizard-back]'));
const change = (el, value) => el.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value } }));
function check(win, id, role = 'disks', checked = true) {
  const cb = win.querySelector(`#nas-pw-${role} [data-disk="${id}"] tf-checkbox`);
  cb.checked = checked;
  cb.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { checked } }));
}
function setup(fixtures = {}, options = {}) {
  const screen = fakeScreen({ tentaNasElasticCapabilitiesRequest: { capabilities: { mergerfs: true, snapraid: true, filesystems: ['xfs', 'ext4'], detail: '' }, freeDisks: disks }, tentaNasElasticArrayPlanRequest: preview, tentaNasElasticArrayCreateRequest: { job }, tentaNasJobGetRequest: { job }, ...fixtures });
  screen.nodeId = 'node-orion'; screen.tab = 'pools';
  const win = openPoolWizard(screen, options);
  return { screen, win };
}
async function toParity(win) {
  await flush();
  change(win.querySelector('#nas-pw-kind'), 'elastic'); next(win);
  check(win, 'id0'); check(win, 'id1'); next(win);
  typeInto(win.querySelector('#nas-pw-name'), 'media');
}
async function toSummary(win) {
  await toParity(win);
  check(win, 'id2', 'parity');
  click(win.querySelector('[data-pw-preview]')); await flush(); await flush();
  next(win); typeInto(win.querySelector('#nas-pw-confirm'), 'media');
}
test('nazwa Elastic odpowiada walidatorowi helpera, nie regułom ZFS', () => {
  for (const name of ['1array', 'Media-1', 'a.b:c', 'tentanas-data', 'a'.repeat(64)]) assert.ok(elasticNameValid(name), name);
  for (const name of ['', '-x', 'a/b', 'a b', 'ą', 'a'.repeat(65), 'tentanas', 'tentanas-branches']) assert.equal(elasticNameValid(name), false, name);
});
test('cztery kroki wysyłają dokładny preview i oddzielne typed confirm; job nie powiela callbacku', async () => {
  const received = [];
  const { screen, win } = setup({}, { onCreated: (r) => received.push(r) });
  await toSummary(win);
  assert.equal(win.querySelectorAll('.loss-list li').length, 3);
  assert.ok(win.querySelector('#nas-pw-summary').shadowRoot.textContent.includes('/mnt/media'));
  next(win); next(win); await flush(); await flush();
  const calls = screen.calls.filter((c) => c.kind === 'tentaNasElasticArrayCreateRequest');
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].payload, { name: 'media', filesystem: 'xfs', dataDiskIds: ['id0', 'id1'], parityDiskIds: ['id2'], confirmName: 'media', sudoPassword: 'hunter2' });
  assert.deepEqual(screen.calls.find((c) => c.kind === 'tentaNasElasticArrayPlanRequest').payload.cacheDiskIds, []);
  assert.equal(received.length, 0);
  win.dispatchEvent(new window.CustomEvent('close-request'));
  win.dispatchEvent(new window.CustomEvent('close-request'));
  assert.deepEqual(received, [{ kind: 'elastic', name: 'media', outcome: 'job', jobId: 'j1' }]);
  screen.dispose();
});
test('zero parity działa bez SnapRAID; wybór FS jest rzeczywistym segmented', async () => {
  const { screen, win } = setup({ tentaNasElasticCapabilitiesRequest: { capabilities: { mergerfs: true, snapraid: false, filesystems: ['ext4'] }, freeDisks: disks } });
  await toParity(win);
  assert.ok(win.querySelector('#nas-pw-parity tf-checkbox').hasAttribute('disabled'));
  click(win.querySelector('[data-pw-preview]')); await flush(); next(win);
  typeInto(win.querySelector('#nas-pw-confirm'), 'media'); next(win); await flush();
  assert.deepEqual(screen.calls.find((c) => c.kind === 'tentaNasElasticArrayCreateRequest').payload.parityDiskIds, []);
  assert.equal(screen.calls.find((c) => c.kind === 'tentaNasElasticArrayCreateRequest').payload.filesystem, 'ext4');
  screen.dispose();
});
test('parity za małe i trzecie niedostępne, dane nie są automatycznie przepinane', async () => {
  const { screen, win } = setup(); await toParity(win);
  check(win, 'id2', 'parity'); check(win, 'id3', 'parity');
  back(win); check(win, 'id0', 'disks', false); next(win);
  assert.ok(win.querySelector('#nas-pw-parity [data-disk="id0"] tf-checkbox').hasAttribute('disabled'));
  assert.equal(win.querySelectorAll('#nas-pw-parity .disk-cell.checked').length, 2);
  assert.equal(screen.calls.filter((c) => c.kind.includes('Create')).length, 0);
  screen.dispose();
});
test('trzeci wystarczająco duży parity jest odmawiany niezależnie od rozmiaru', async () => {
  const fifth = { ...disks[3], diskId: 'id4', name: 'sd4', serial: 'serial4' };
  const { screen, win } = setup({ tentaNasElasticCapabilitiesRequest: { capabilities: { mergerfs: true, snapraid: true, filesystems: ['xfs'] }, freeDisks: [...disks, fifth] } });
  await toParity(win); check(win, 'id2', 'parity'); check(win, 'id3', 'parity');
  const cb = win.querySelector('#nas-pw-parity [data-disk="id4"] tf-checkbox');
  assert.ok(cb.hasAttribute('disabled'));
  check(win, 'id4', 'parity');
  click(win.querySelector('[data-pw-preview]')); await flush();
  assert.deepEqual(screen.calls.find((c) => c.kind === 'tentaNasElasticArrayPlanRequest').payload.parityDiskIds, ['id2', 'id3']);
  screen.dispose();
});
test('stary preview po zmianie nazwy nie aktywuje podsumowania', async () => {
  let resolve;
  const { screen, win } = setup({ tentaNasElasticArrayPlanRequest: (p) => new Promise((r) => { resolve = () => r(preview(p)); }) });
  await toParity(win); click(win.querySelector('[data-pw-preview]'));
  typeInto(win.querySelector('#nas-pw-name'), 'other'); resolve(); await flush();
  assert.ok(win.querySelector('[data-wizard-next]').hasAttribute('disabled'));
  assert.equal(win.querySelector('#nas-pw-preview').textContent, '');
  screen.dispose();
});
for (const reply of [{}, { plan: { refusals: [] } }, { plan: { ...preview({ name: 'media', dataDiskIds: ['id0'], parityDiskIds: [] }).plan, refusals: [{ detail: 'disk_in_use' }] } }]) {
  test('niepełny lub odmowny plan nie uruchamia create: ' + JSON.stringify(reply), async () => {
    const { screen, win } = setup({ tentaNasElasticArrayPlanRequest: reply }); await toParity(win);
    click(win.querySelector('[data-pw-preview]')); await flush(); next(win);
    assert.ok(win.querySelector('[data-wizard-next]').hasAttribute('disabled'));
    assert.equal(screen.calls.some((c) => c.kind.includes('Create')), false); screen.dispose();
  });
}
for (const outcome of ['approval', 'unknown']) {
  test(outcome + ' nie jest sukcesem ani możliwością ponowienia create', async () => {
    const { screen, win } = setup({ tentaNasElasticArrayCreateRequest: outcome === 'approval' ? { approval: { requestId: 'a1' } } : () => { throw new Error('network'); } });
    await toSummary(win); next(win); await flush(); next(win); await flush();
    assert.equal(screen.calls.filter((c) => c.kind === 'tentaNasElasticArrayCreateRequest').length, 1);
    assert.equal(screen.calls.some((c) => c.kind === 'tentaNasJobGetRequest'), false);
    assert.equal(win.querySelector('.result-box.ok'), null); screen.dispose();
  });
}
test('busy przed oczekiwaniem sudo; anulowanie odblokowuje bez wysyłki', async () => {
  const { screen, win } = setup(); await toSummary(win);
  let resolve; let prompts = 0;
  screen.withSudo = () => { prompts++; return new Promise((r) => { resolve = r; }); };
  next(win); next(win); assert.equal(prompts, 1);
  assert.ok(win.querySelector('[data-wizard-next]').hasAttribute('disabled'));
  assert.ok(win.querySelector('#nas-pw-confirm').hasAttribute('disabled'));
  resolve(null); await flush();
  assert.equal(screen.calls.some((c) => c.kind.includes('Create')), false);
  assert.equal(win.querySelector('[data-wizard-next]').hasAttribute('disabled'), false); screen.dispose();
});
test('zamknięcie podczas Create odświeża raz listę; późna odpowiedź nie nawiguje', async () => {
  let resolve; const outcomes = [];
  const { screen, win } = setup({ tentaNasElasticArrayCreateRequest: () => new Promise((r) => { resolve = r; }) }, { onCreated: (r) => outcomes.push(r) });
  await toSummary(win); next(win);
  win.dispatchEvent(new window.CustomEvent('close-request'));
  assert.deepEqual(outcomes, [{ kind: 'elastic', name: 'media', outcome: 'unknown' }]);
  resolve({ job }); await flush();
  assert.equal(outcomes.length, 1);
  assert.equal(screen.calls.some((c) => c.kind === 'tentaNasJobGetRequest'), false);
  screen.dispose();
});
for (const stale of ['node', 'tab', 'window', 'disposed']) {
  test('opóźnione sudo po zmianie ' + stale + ' nie wysyła create', async () => {
    const { screen, win } = setup(); await toSummary(win);
    let send; let resolve;
    screen.withSudo = (fn, title, guard) => new Promise((r) => { send = () => { assert.equal(guard(), false); return fn('secret'); }; resolve = r; });
    next(win);
    if (stale === 'node') screen.nodeId = 'other';
    if (stale === 'tab') screen.tab = 'jobs';
    if (stale === 'window') win.remove();
    if (stale === 'disposed') screen.disposed = true;
    resolve(await send()); await flush();
    assert.equal(screen.calls.some((c) => c.kind.includes('Create')), false); screen.dispose();
  });
}
test('niezgodne typed confirm nigdy nie wysyła Create ani nie kopiuje nazwy', async () => {
  const { screen, win } = setup(); await toSummary(win);
  for (const value of ['', 'medi', ' media ', 'other']) {
    typeInto(win.querySelector('#nas-pw-confirm'), value); next(win);
    assert.ok(win.querySelector('[data-wizard-next]').hasAttribute('disabled'));
  }
  assert.equal(screen.calls.some((c) => c.kind.includes('Create')), false); screen.dispose();
});
test('zmiana FS i powrót do danych unieważnia plan i potwierdzenie', async () => {
  const { screen, win } = setup(); await toSummary(win); back(win); back(win);
  const fs = win.querySelector('#nas-pw-filesystem');
  assert.equal(fs.tagName, 'TF-SEGMENTED'); change(fs, 'ext4'); next(win);
  assert.ok(win.querySelector('[data-wizard-next]').hasAttribute('disabled'));
  click(win.querySelector('[data-pw-preview]')); await flush(); next(win);
  assert.equal(win.querySelector('#nas-pw-confirm').value, '');
  assert.equal(screen.calls.filter((c) => c.kind === 'tentaNasElasticArrayPlanRequest').at(-1).payload.filesystem, 'ext4');
  screen.dispose();
});
for (const stale of ['node', 'tab', 'window']) {
  test('spóźniony plan po zmianie ' + stale + ' nie renderuje nowego widoku', async () => {
    let resolve;
    const { screen, win } = setup({ tentaNasElasticArrayPlanRequest: (p) => new Promise((r) => { resolve = () => r(preview(p)); }) });
    await toParity(win); click(win.querySelector('[data-pw-preview]'));
    if (stale === 'node') screen.nodeId = 'other';
    if (stale === 'tab') screen.tab = 'jobs';
    if (stale === 'window') win.remove();
    const content = win.innerHTML; resolve(); await flush();
    assert.equal(win.innerHTML, content);
    assert.equal(screen.calls.some((c) => c.kind.includes('Create')), false); screen.dispose();
  });
}
test('brak mergerfs lub niepełna odpowiedź capabilities nie odblokowują Elastic', async () => {
  for (const reply of [{}, { capabilities: { mergerfs: false, filesystems: ['xfs'], detail: 'missing mergerfs' }, freeDisks: disks }]) {
    const { screen, win } = setup({ tentaNasElasticCapabilitiesRequest: reply }); await flush();
    assert.ok(win.querySelector('tf-choice-card[value="elastic"]').hasAttribute('disabled'));
    change(win.querySelector('#nas-pw-kind'), 'elastic'); next(win);
    assert.equal(win.querySelector('#nas-pw-filesystem'), null); screen.dispose();
  }
});
test('spóźnione capabilities po zmianie węzła nie odblokowują karty', async () => {
  let resolve;
  const { screen, win } = setup({ tentaNasElasticCapabilitiesRequest: () => new Promise((r) => { resolve = r; }) });
  screen.nodeId = 'different';
  resolve({ capabilities: { mergerfs: true, snapraid: true, filesystems: ['xfs'] }, freeDisks: disks }); await flush();
  assert.ok(win.querySelector('tf-choice-card[value="elastic"]').hasAttribute('disabled'));
  screen.dispose();
});
test('dwa parity trafiają do jednego Create, trzeci nie; ext4 zachowany', async () => {
  const { screen, win } = setup(); await flush();
  change(win.querySelector('#nas-pw-kind'), 'elastic'); next(win);
  check(win, 'id0'); check(win, 'id1');
  change(win.querySelector('#nas-pw-filesystem'), 'ext4'); next(win);
  check(win, 'id2', 'parity'); check(win, 'id3', 'parity');
  typeInto(win.querySelector('#nas-pw-name'), 'media'); click(win.querySelector('[data-pw-preview]')); await flush(); next(win);
  typeInto(win.querySelector('#nas-pw-confirm'), 'media'); next(win); await flush();
  const create = screen.calls.find((c) => c.kind === 'tentaNasElasticArrayCreateRequest');
  assert.deepEqual(create.payload.parityDiskIds, ['id2', 'id3']);
  assert.equal(create.payload.filesystem, 'ext4');
  screen.dispose();
});
test('nieadministrator nie przechodzi kreatora ani nie wysyła Create', async () => {
  const { screen, win } = setup(); screen.isAdmin = false; await flush();
  change(win.querySelector('#nas-pw-kind'), 'elastic'); next(win);
  assert.ok(win.querySelector('#nas-pw-kind'));
  assert.ok(win.querySelector('[data-wizard-next]').hasAttribute('disabled'));
  assert.equal(screen.calls.some((c) => c.kind.includes('Create')), false); screen.dispose();
});
test('zawijany renderer podsumowania pokazuje serial jako tekst, nie HTML', async () => {
  const serial = '<img src=x onerror=alert(1)>';
  const { screen, win } = setup({ tentaNasElasticCapabilitiesRequest: {
    capabilities: { mergerfs: true, snapraid: true, filesystems: ['xfs', 'ext4'] },
    freeDisks: disks.map((disk, index) => index === 0 ? { ...disk, serial } : disk),
  } });
  await toSummary(win);
  const shadow = win.querySelector('#nas-pw-summary').shadowRoot;
  assert.ok(shadow.textContent.includes(serial));
  assert.equal(shadow.querySelector('img'), null);
  screen.dispose();
});
