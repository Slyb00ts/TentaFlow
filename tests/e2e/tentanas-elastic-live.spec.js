// =============================================================================
// Plik: tentanas-elastic-live.spec.js
// Opis: Prawdziwe logowanie i odczyt API Elastic w wskazanej VM, bez mutacji dysków.
// Przykład: TENTANAS_LIVE_PHASE=inspect z konfiguracją tentanas-elastic-live.
// =============================================================================

const { test, expect } = require('@playwright/test');
const { readFileSync } = require('node:fs');

function required(name) {
  const value = process.env[name];
  if (!value) throw new Error(`Brak wymaganej zmiennej ${name}`);
  return value;
}

async function fillSecret(locator, value) {
  try {
    await locator.fill(value);
  } catch {
    throw new Error('Nie udało się wypełnić pola sekretu; szczegóły pominięto');
  }
}

function diskContract() {
  const manifest = JSON.parse(readFileSync(required('TENTANAS_LIVE_MANIFEST'), 'utf8'));
  if (manifest.schema !== 1 || !/^[0-9a-f-]{36}$/.test(manifest.uuid || '')) {
    throw new Error('Wymagany manifest VM przekazany przez PM');
  }
  const roles = ['os', 'data1', 'data2', 'parity', 'cache', 'spare'];
  if (Object.keys(manifest.disks || {}).sort().join() !== [...roles].sort().join()) {
    throw new Error('Kontrakt wymaga wszystkich sześciu fizycznych ról');
  }
  const disks = roles.map((role) => {
    const disk = manifest.disks[role];
    if (typeof disk.serial !== 'string' || !disk.serial
        || !Number.isSafeInteger(disk.bytes) || disk.bytes <= 0) {
      throw new Error('Niepełna tożsamość lub rozmiar w kontrakcie dysków');
    }
    return { role, serial: disk.serial, bytes: disk.bytes };
  });
  if (new Set(disks.map((disk) => disk.serial)).size !== roles.length) {
    throw new Error('Seriale w kontrakcie nie są unikalne');
  }
  if (disks.filter((disk) => ['data1', 'data2', 'parity'].includes(disk.role))
    .some((disk) => disk.bytes <= 20 * 1024 ** 3)) {
    throw new Error('Profil E2 wymaga danych i parity większych niż minfreespace 20 GiB');
  }
  return { uuid: manifest.uuid, disks };
}

async function login(page) {
  const password = required('TENTANAS_LIVE_PASSWORD');
  const rotation = required('TENTANAS_LIVE_ROTATION');
  if (!['required', 'none'].includes(rotation)) throw new Error('Rotation musi być required albo none');
  const nextPassword = rotation === 'required' ? required('TENTANAS_LIVE_NEW_PASSWORD') : null;
  if (nextPassword && ([...nextPassword].length < 12 || nextPassword === password)) {
    throw new Error('Nowe hasło musi spełniać wymagania formularza');
  }
  await page.goto('/');
  await page.locator('#login-username input').fill(process.env.TENTANAS_LIVE_USERNAME || 'admin');
  await fillSecret(page.locator('#login-password input'), password);
  await page.locator('#login-submit').click();
  if (rotation === 'required') {
    await expect(page.locator('#login-new-password input')).toBeVisible();
    await fillSecret(page.locator('#login-password input'), password);
    await fillSecret(page.locator('#login-new-password input'), nextPassword);
    await fillSecret(page.locator('#login-confirm-password input'), nextPassword);
    await page.locator('#login-submit').click();
  }
  await expect(page.locator('#login-form')).toHaveCount(0, { timeout: 30000 });
}

test('rzeczywisty login → katalog → gotowy NAS → capabilities i dyski', async ({ page }, testInfo) => {
  if (required('TENTANAS_LIVE_PHASE') !== 'inspect') {
    throw new Error('Ten odbiór obsługuje wyłącznie inspect; create nie jest zaimplementowane');
  }
  const expectedNodeId = required('TENTANAS_LIVE_NODE_ID');
  const contract = diskContract();
  if (!/^[0-9a-f]{64}$/.test(expectedNodeId)) throw new Error('Wymagany dokładny nodeId z kontraktu PM');
  const errors = [];
  page.on('pageerror', () => errors.push('pageerror'));
  page.on('console', (message) => {
    if (message.type() === 'error') errors.push('console.error');
  });
  await login(page);
  const observed = await page.evaluate(async ({ expectedNodeId }) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const catalog = await ApiBinary.one('addonCatalogListRequest');
    const nodes = await ApiBinary.one('tentaNasNodesListRequest', {});
    if (nodes.localNodeId !== expectedNodeId) throw new Error('Inny lokalny węzeł niż wskazany przez PM');
    const node = nodes.nodes.find((entry) => entry.nodeId === expectedNodeId);
    if (!node || node.instanceStatus !== 'ready') {
      throw new Error('PM musi najpierw przygotować włączoną instancję NAS; test niczego nie instaluje');
    }
    const capabilities = await ApiBinary.one('tentaNasElasticCapabilitiesRequest', {});
    const inventory = await ApiBinary.one('tentaNasDisksListRequest', {});
    return {
      catalogVariant: catalog.variant,
      packageCount: catalog.packages?.length,
      nodeId: nodes.localNodeId,
      instanceStatus: node.instanceStatus,
      capabilities,
      disks: inventory.disks,
      inventoryVariant: inventory.variant,
    };
  }, { expectedNodeId });
  expect(observed.catalogVariant).toBe('AddonCatalogListResponse');
  expect(observed.packageCount).toBeGreaterThan(0);
  expect(observed.nodeId).toBe(expectedNodeId);
  expect(observed.instanceStatus).toBe('ready');
  expect(observed.capabilities.variant).toBe('TentaNasElasticCapabilitiesResponse');
  expect(observed.capabilities.capabilities.mergerfs).toBe(true);
  expect(observed.capabilities.capabilities.snapraid).toBe(true);
  expect([...observed.capabilities.capabilities.filesystems].sort()).toEqual(['ext4', 'xfs']);
  expect(observed.inventoryVariant).toBe('TentaNasDisksListResponse');
  expect(Array.isArray(observed.disks)).toBe(true);
  expect(observed.disks.map((disk) => ({ serial: disk.serial, bytes: disk.sizeBytes }))
    .sort((a, b) => a.serial.localeCompare(b.serial)))
    .toEqual(contract.disks.map(({ serial, bytes }) => ({ serial, bytes }))
      .sort((a, b) => a.serial.localeCompare(b.serial)));
  await testInfo.attach('elastic-readonly-observation.json', {
    body: JSON.stringify({ contract, observed }, null, 2), contentType: 'application/json',
  });
  await page.screenshot({ path: testInfo.outputPath('authenticated-page.png'), fullPage: true });
  expect(errors, 'Liczba błędów konsoli; treści nie są utrwalane, aby nie zapisać sekretów').toEqual([]);
});
