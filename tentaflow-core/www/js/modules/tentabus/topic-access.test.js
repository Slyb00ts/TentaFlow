// =============================================================================
// File: modules/tentabus/topic-access.test.js
// Description: A topic's Dostęp section (U6, T09): one row per subject with
// its name, kind and group size, rights read as the server decides them (a
// "zabroniono" wins, an old every-right entry counts); the AclSet requests of
// "Nadaj", "Zmień" and "Usuń" (one per right, the old entry cleared first);
// "Co się stanie" in the mockups' words; the directory picker that lets only
// the newest answer land and offers only subjects without an entry; the
// keys card for the server administrator (rights to messages and patterns,
// last use, "Wydaj klucz", "Zmień", "Unieważnij") and for a topic
// administrator who is not one (message rights only, no buttons, who issues
// keys); the key windows: the exact scope ids, the secret shown once with the
// address and the key's consumer group.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const {
  subjectRows, rightOf, currentRights, subjectTableRow, buildAclRequests, buildAclRemoveRequests, grantImpact, changeImpact, removeFacts,
  keyRows, keyTableRow, lastUsedText, buildKeyCreateRequest, buildKeyRightsRequests, recordsUrl, issueImpact, keyRightsImpact,
  directoryLoader, pickable, pickLabel, accessCount, paintAccessSection,
  openAccessGrant, openAccessChange, openAccessRemove, openKeyIssue, openKeyRights, openKeyRevoke,
} = await import('./topic-access.js');
const { busSchemaScopeId, topicScopeId } = await import('../access-keys-scopes.js');

const norm = (s) => String(s).replace(/[  ]/g, ' ').replace(/\s+/g, ' ').trim();
// The words of a cell's markup, its title and second line apart.
const words = (html) => { const d = document.createElement('div'); d.innerHTML = html; return norm([...d.childNodes].map((n) => n.textContent).join(' ')); };
const tick = (ms = 20) => new Promise((r) => setTimeout(r, ms));
const closeAll = () => document.querySelectorAll('tf-window').forEach((w) => w.remove());
const pick = (el, value) => {
  el.value = value;
  el.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value } }));
};
const check = (el, on) => {
  if (on) el.setAttribute('checked', '');
  else el.removeAttribute('checked');
  el.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { checked: on } }));
};

const INSTANCE = 'tentabus-a1b2c3d4';
const ORG = 'org-default';
const TOPIC = 'wyniki-badan';
const NOW = new Date(2026, 8, 29, 12, 0, 0).getTime();
const where = { instanceId: INSTANCE, orgId: ORG, topic: TOPIC };

const ACL = [
  { subjectType: 'user', subjectId: 'u-anna', accessLevel: 'allow', action: 'admin', subjectLabel: 'Anna Kowalska', memberCount: null },
  { subjectType: 'group', subjectId: 'g-ksiegowosc', accessLevel: 'allow', action: 'read', subjectLabel: 'Księgowość', memberCount: 4 },
  { subjectType: 'group', subjectId: 'g-ksiegowosc', accessLevel: 'deny', action: 'write', subjectLabel: 'Księgowość', memberCount: 4 },
  { subjectType: 'user', subjectId: 'u-piotr', accessLevel: 'deny', action: '*', subjectLabel: 'Piotr Zieliński', memberCount: null },
  { subjectType: 'addon', subjectId: 'asystent', accessLevel: 'allow', action: 'read', subjectLabel: 'Asystent lekarza', memberCount: null },
  { subjectType: 'user', subjectId: 'asystent-stary', accessLevel: 'deny', action: 'read', subjectLabel: null, memberCount: null },
  { subjectType: 'api_key', subjectId: 'k-lis', accessLevel: 'allow', action: 'write', subjectLabel: 'System laboratoryjny (LIS)', memberCount: null },
  { subjectType: 'api_key', subjectId: 'k-portal', accessLevel: 'allow', action: 'read', subjectLabel: 'Portal wyników', memberCount: null },
];
const schemaId = busSchemaScopeId(INSTANCE, ORG);
const KEYS = [
  { keyId: 'k-lis', name: 'System laboratoryjny (LIS)', lastUsedAtEpoch: (NOW - 20_000) / 1000, scopes: [
    { resourceType: 'topic', resourceId: topicScopeId(INSTANCE, ORG, TOPIC), accessLevel: 'allow', action: 'write' },
    { resourceType: 'bus_schema_registry', resourceId: schemaId, accessLevel: 'allow', action: 'read' },
    { resourceType: 'bus_schema_registry', resourceId: schemaId, accessLevel: 'allow', action: 'write' },
    { resourceType: 'model', resourceId: 'bielik', accessLevel: 'allow', action: '*' },
  ] },
  { keyId: 'k-portal', name: 'Portal wyników', lastUsedAtEpoch: null, scopes: [
    { resourceType: 'topic', resourceId: topicScopeId(INSTANCE, ORG, TOPIC), accessLevel: 'allow', action: 'read' },
  ] },
  { keyId: 'k-schemas', name: 'Aplikacja pacjenta', lastUsedAtEpoch: new Date(2026, 8, 28, 20, 11).getTime() / 1000, scopes: [
    { resourceType: 'bus_schema_registry', resourceId: schemaId, accessLevel: 'allow', action: 'read' },
  ] },
  { keyId: 'k-other', name: 'Inna instancja', lastUsedAtEpoch: null, scopes: [
    { resourceType: 'bus_schema_registry', resourceId: busSchemaScopeId('tentabus-ffffffff', ORG), accessLevel: 'allow', action: 'read' },
  ] },
];

// ---------------------------------------------------------------------------
// The subjects' table
// ---------------------------------------------------------------------------

test('one row per subject: groups, people, addons; keys are not here; an old every-right entry counts for all three', () => {
  const rows = subjectRows(ACL);
  assert.deepEqual(rows.map((r) => `${r.subjectType}:${r.subjectId}`), ['group:g-ksiegowosc', 'user:u-anna', 'user:u-piotr', 'user:asystent-stary', 'addon:asystent']);
  const ksiegowosc = rows[0];
  assert.deepEqual(currentRights(ksiegowosc), { read: 'allow', write: 'deny', admin: '' });
  const piotr = rows[2];
  assert.equal(piotr.wildcard, 'deny');
  assert.deepEqual(['read', 'write', 'admin'].map((a) => rightOf(piotr, a)), ['deny', 'deny', 'deny']);
  // A deny anywhere wins over the right's own allow.
  assert.equal(rightOf({ rights: { read: 'allow', write: null, admin: null }, wildcard: 'deny' }, 'read'), 'deny');
});

test('a row says who in plain words: the name, the kind, a group\'s size; an unknown id is said to be unknown', () => {
  const rows = subjectRows(ACL);
  const cell = (r) => words(subjectTableRow(r).who);
  assert.equal(cell(rows[0]), 'Księgowość Grupa · 4 osoby');
  assert.equal(cell(subjectRows([{ subjectType: 'group', subjectId: 'g', accessLevel: 'allow', action: 'read', subjectLabel: 'Lekarze', memberCount: 12 }])[0]), 'Lekarze Grupa · 12 osób');
  assert.equal(cell(subjectRows([{ subjectType: 'group', subjectId: 'g', accessLevel: 'allow', action: 'read', subjectLabel: 'Dyżur', memberCount: 1 }])[0]), 'Dyżur Grupa · 1 osoba');
  assert.equal(cell(rows[1]), 'Anna Kowalska Użytkownik');
  assert.equal(cell(rows[4]), 'Asystent lekarza Addon');
  assert.equal(cell(rows[3]), 'Nieznany podmiot Użytkownik · identyfikator asystent-stary', 'an old user row naming an addon');
  const row = subjectTableRow(rows[0]);
  assert.match(row.read, /tf-chip--outline ok">pozwolono</);
  assert.match(row.write, /tf-chip--outline err">zabroniono</);
  assert.match(row.admin, />—</);
});

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

test('Nadaj: one AclSet per right that is set; nothing for a right left unset', () => {
  assert.deepEqual(buildAclRequests(INSTANCE, TOPIC, { subjectType: 'group', subjectId: 'g-rej' }, null, { read: 'allow', write: '', admin: 'deny' }), [
    { instanceId: INSTANCE, topic: TOPIC, subjectType: 'group', subjectId: 'g-rej', accessLevel: 'allow', action: 'read' },
    { instanceId: INSTANCE, topic: TOPIC, subjectType: 'group', subjectId: 'g-rej', accessLevel: 'deny', action: 'admin' },
  ]);
});

test('Zmień: only what changes — a deny back to unset clears that right alone; an old every-right entry is cleared first', () => {
  const [ksiegowosc, , piotr] = subjectRows(ACL);
  assert.deepEqual(buildAclRequests(INSTANCE, TOPIC, ksiegowosc, ksiegowosc, { read: 'allow', write: '', admin: '' }), [
    { instanceId: INSTANCE, topic: TOPIC, subjectType: 'group', subjectId: 'g-ksiegowosc', accessLevel: 'clear', action: 'write' },
  ]);
  assert.deepEqual(buildAclRequests(INSTANCE, TOPIC, piotr, piotr, { read: 'deny', write: 'deny', admin: '' }).map((r) => `${r.accessLevel}:${r.action}`), ['clear:*', 'deny:read', 'deny:write']);
  assert.deepEqual(buildAclRemoveRequests(INSTANCE, TOPIC, ksiegowosc).map((r) => `${r.accessLevel}:${r.action}`), ['clear:read', 'clear:write']);
  assert.deepEqual(buildAclRemoveRequests(INSTANCE, TOPIC, piotr).map((r) => `${r.accessLevel}:${r.action}`), ['clear:*']);
});

test('"Co się stanie" in the mockups\' words: a group grant, a changed right, a removed entry', () => {
  assert.equal(
    grantImpact({ subject: { subjectType: 'group', label: 'Rejestracja', memberCount: 5 }, next: { read: 'allow', write: '', admin: '' }, topic: TOPIC }).join(' '),
    '5 osób z grupy Rejestracja będzie mogło czytać wiadomości w topiku wyniki-badan. O zapisie i administracji zdecyduje rola w organizacji.',
  );
  assert.equal(
    grantImpact({ subject: { subjectType: 'group', label: 'Dyżur', memberCount: 2 }, next: { read: 'allow', write: 'allow', admin: '' }, topic: TOPIC })[0],
    '2 osoby z grupy Dyżur będą mogły czytać wiadomości i wysyłać wiadomości w topiku wyniki-badan.',
  );
  assert.deepEqual(
    grantImpact({ subject: { subjectType: 'user', label: 'Tomasz Nowak' }, next: { read: 'allow', write: 'deny', admin: 'deny' }, topic: TOPIC }),
    [
      'Użytkownik „Tomasz Nowak” dostanie w topiku wyniki-badan: czytanie.',
      'Zakaz dla „Tomasz Nowak”: zapis i administracja — wygrywa z rolą w organizacji i z pozwoleniem z grupy.',
    ],
  );
  assert.equal(
    changeImpact({ name: 'Księgowość', current: { read: 'allow', write: 'deny', admin: '' }, next: { read: 'allow', write: '', admin: '' } }).join(' '),
    'Zapis dla „Księgowość” zmieni się z „zabroniono” na „nie ustawiono”. Zdecyduje o nim rola w organizacji. Pozostałe prawa bez zmian.',
  );
  assert.equal(
    changeImpact({ name: 'Anna Kowalska', current: { read: '', write: '', admin: 'allow' }, next: { read: '', write: '', admin: '' } }).join(' '),
    'Administracja dla „Anna Kowalska” zmieni się z „pozwolono” na „nie ustawiono”. Zdecyduje o niej rola w organizacji. Pozostałe prawa bez zmian.',
  );
  const [ksiegowosc, anna] = subjectRows(ACL);
  assert.deepEqual(removeFacts(ksiegowosc), { lead: 'Księgowość straci wpis w tym topiku: czytanie (pozwolono), zapis (zabroniono).', liftsDeny: true });
  assert.deepEqual(removeFacts(anna), { lead: 'Anna Kowalska straci wpis w tym topiku: administracja (pozwolono).', liftsDeny: false });
});

// ---------------------------------------------------------------------------
// The directory picker
// ---------------------------------------------------------------------------

test('the directory: only the newest question lands, and nothing once the window is gone', async () => {
  const pending = [];
  const applied = [];
  let alive = true;
  const load = directoryLoader({
    fetch: (q) => new Promise((resolve) => pending.push({ q, resolve })),
    apply: (o) => applied.push(o),
    alive: () => alive,
  });
  const first = load('user', '');
  const second = load('group', '');
  pending[1].resolve({ entries: [{ subjectType: 'group', subjectId: 'g', label: 'Lekarze' }], truncated: true });
  pending[0].resolve({ entries: [{ subjectType: 'user', subjectId: 'u', label: 'Tomasz' }] });
  await Promise.all([first, second]);
  assert.deepEqual(applied, [{ kind: 'group', query: '', entries: [{ subjectType: 'group', subjectId: 'g', label: 'Lekarze' }], truncated: true }], 'the slower answer to "Użytkownik" does not overwrite "Grupa"');
  alive = false;
  const third = load('addon', '');
  pending[2].resolve({ entries: [] });
  await third;
  assert.equal(applied.length, 1);
  const rows = subjectRows(ACL);
  assert.deepEqual(pickable([{ subjectType: 'group', subjectId: 'g-ksiegowosc', label: 'Księgowość' }, { subjectType: 'group', subjectId: 'g-rej', label: 'Rejestracja' }], rows).map((e) => e.subjectId), ['g-rej']);
  assert.equal(pickLabel({ subjectType: 'group', label: 'Rejestracja', memberCount: 5 }), 'Rejestracja (5 osób)');
});

test('"Nadaj dostęp": groups without an entry, a stale answer ignored, the rights, then one request per right', async () => {
  closeAll();
  const asked = [];
  const answers = [];
  const sent = [];
  const saved = [];
  const win = openAccessGrant({
    instanceId: INSTANCE,
    topic: TOPIC,
    rows: subjectRows(ACL),
    directory: (q) => { asked.push(q); return new Promise((resolve) => answers.push(resolve)); },
    setAcl: async (r) => { sent.push(r); },
    describeError: String,
    onSaved: (n) => saved.push(n),
  });
  assert.deepEqual(asked, [{ kind: 'group', query: '' }], 'opens on groups');
  assert.match(win.querySelector('[data-role="impact"]').textContent, /Wybierz, komu nadać dostęp/);
  assert.ok(win.querySelector('[data-act="save"]').hasAttribute('disabled'));
  // Switch to users and back before the first answer lands.
  pick(win.querySelector('[data-role="kind"]'), 'user');
  pick(win.querySelector('[data-role="kind"]'), 'group');
  assert.equal(asked.length, 3);
  answers[2]({ entries: [
    { subjectType: 'group', subjectId: 'g-ksiegowosc', label: 'Księgowość', memberCount: 4 },
    { subjectType: 'group', subjectId: 'g-rej', label: 'Rejestracja', memberCount: 5 },
    { subjectType: 'group', subjectId: 'g-piel', label: 'Pielęgniarki', memberCount: 9 },
  ], truncated: false });
  answers[1]({ entries: [{ subjectType: 'user', subjectId: 'u-tomasz', label: 'Tomasz Nowak' }] });
  answers[0]({ entries: [] });
  await tick();
  const select = win.querySelector('[data-role="subject"]');
  assert.equal(select.getAttribute('label'), 'Która grupa');
  assert.deepEqual([...select.querySelectorAll('select option')].map((o) => o.textContent), ['Rejestracja (5 osób)', 'Pielęgniarki (9 osób)'], 'Księgowość already has an entry; the late user list never lands');
  assert.equal(win.querySelector('[data-role="pick-note"]').textContent, 'Na liście są grupy, które nie mają jeszcze wpisu.');
  assert.equal(norm(win.querySelector('[data-role="impact"]').textContent),
    'Co się stanie po zapisaniu: 5 osób z grupy Rejestracja będzie mogło czytać wiadomości w topiku wyniki-badan. O zapisie i administracji zdecyduje rola w organizacji.');
  pick(win.querySelector('tf-segmented[data-right="read"]'), '');
  assert.match(win.querySelector('[data-role="impact"]').textContent, /Ustaw co najmniej jedno prawo/);
  assert.ok(win.querySelector('[data-act="save"]').hasAttribute('disabled'), 'nothing set, nothing to grant');
  pick(win.querySelector('tf-segmented[data-right="read"]'), 'allow');
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual(sent, [{ instanceId: INSTANCE, topic: TOPIC, subjectType: 'group', subjectId: 'g-rej', accessLevel: 'allow', action: 'read' }]);
  assert.equal(saved[0].title, 'Nadano dostęp');
  assert.match(saved[0].text, /^5 osób z grupy Rejestracja/);
});

test('"Nadaj dostęp" to an addon, and a refusal stays in the window in plain words', async () => {
  closeAll();
  const win = openAccessGrant({
    instanceId: INSTANCE,
    topic: TOPIC,
    rows: [],
    directory: async ({ kind }) => ({ entries: kind === 'addon' ? [{ subjectType: 'addon', subjectId: 'notatki', label: 'Notatki' }] : [] }),
    setAcl: async () => { throw new Error('protocol error PolicyDenied: org Admin role required'); },
    describeError: () => 'Brak uprawnień do tej operacji.',
    onSaved: () => assert.fail('refused'),
  });
  await tick();
  assert.equal(win.querySelector('[data-role="pick-note"]').textContent, 'Każda grupa organizacji ma już wpis w tym topiku (albo organizacja nie ma grup).');
  pick(win.querySelector('[data-role="kind"]'), 'addon');
  await tick();
  assert.equal(win.querySelector('[data-role="subject"]').getAttribute('label'), 'Który addon');
  assert.match(norm(win.querySelector('[data-role="impact"]').textContent), /Addon „Notatki” dostanie w topiku wyniki-badan: czytanie\./);
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.equal(norm(win.querySelector('[data-role="error"]').textContent), 'Brak uprawnień do tej operacji.');
});

test('"Zmień": the subject fixed, the one change described, only its request sent', async () => {
  closeAll();
  const [ksiegowosc] = subjectRows(ACL);
  const sent = [];
  const saved = [];
  const win = openAccessChange(ksiegowosc, { instanceId: INSTANCE, topic: TOPIC, setAcl: async (r) => { sent.push(r); }, describeError: String, onSaved: (n) => saved.push(n) });
  assert.equal(words(win.querySelector('.tb-explain-box').innerHTML), 'Księgowość Grupa · 4 osoby');
  assert.ok(win.querySelector('[data-act="save"]').hasAttribute('disabled'), 'nothing changed yet');
  pick(win.querySelector('tf-segmented[data-right="write"]'), '');
  assert.equal(norm(win.querySelector('[data-role="impact"]').textContent),
    'Co się stanie po zapisaniu: Zapis dla „Księgowość” zmieni się z „zabroniono” na „nie ustawiono”. Zdecyduje o nim rola w organizacji. Pozostałe prawa bez zmian.');
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual(sent, [{ instanceId: INSTANCE, topic: TOPIC, subjectType: 'group', subjectId: 'g-ksiegowosc', accessLevel: 'clear', action: 'write' }]);
  assert.equal(saved[0].title, 'Zapisano');
});

test('"Usuń": the warning when a "zabroniono" goes, none otherwise; every right cleared', async () => {
  closeAll();
  const [ksiegowosc, anna] = subjectRows(ACL);
  const sent = [];
  const win = openAccessRemove(ksiegowosc, { instanceId: INSTANCE, topic: TOPIC, setAcl: async (r) => { sent.push(r); }, describeError: String, onSaved: () => {} });
  assert.equal(norm(win.querySelector('[data-role="lifts-deny"]').textContent), 'Usunięcie „zabroniono” może dać te prawa, jeśli pozwala na nie rola w organizacji.');
  assert.equal(norm(win.querySelector('[data-role="impact"]').textContent), 'Co się stanie po usunięciu: o dostępie do topiku wyniki-badan zdecyduje rola w organizacji.');
  win.querySelector('[data-act="go"]').click();
  await tick();
  assert.deepEqual(sent.map((r) => `${r.accessLevel}:${r.action}`), ['clear:read', 'clear:write']);
  closeAll();
  const plain = openAccessRemove(anna, { instanceId: INSTANCE, topic: TOPIC, setAcl: async () => {}, describeError: String, onSaved: () => {} });
  assert.equal(plain.querySelector('[data-role="lifts-deny"]'), null);
});

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

test('keys for the server administrator: message rights from the topic, pattern rights of THIS instance, last use, other rights counted', () => {
  const rows = keyRows({ aclEntries: ACL, keys: KEYS, instanceId: INSTANCE, orgId: ORG });
  assert.deepEqual(rows.map((r) => r.keyId), ['k-schemas', 'k-portal', 'k-lis'], 'by name; a key with rights to another instance only is not listed');
  const lis = rows.find((r) => r.keyId === 'k-lis');
  assert.deepEqual(lis.rights, { readMessages: false, writeMessages: true, readSchemas: true, writeSchemas: true });
  assert.equal(lis.otherScopes, 1, 'the model right');
  const cells = (r) => { const row = keyTableRow(r, NOW); return [words(row.who), words(row.rights), words(row.used)].join('|'); };
  assert.equal(cells(lis), 'System laboratoryjny (LIS) Klucz API|Wysyła wiadomości Czyta wzory Dodaje wzory|przed chwilą');
  assert.equal(cells(rows.find((r) => r.keyId === 'k-portal')), 'Portal wyników Klucz API|Czyta wiadomości|jeszcze nie');
  assert.equal(cells(rows.find((r) => r.keyId === 'k-schemas')), 'Aplikacja pacjenta Klucz API|Czyta wzory|wczoraj 20:11');
});

test('keys for a topic administrator who is not the server administrator: message rights only, from the topic\'s entries', () => {
  const rows = keyRows({ aclEntries: ACL, keys: null, instanceId: INSTANCE, orgId: ORG });
  assert.deepEqual(rows.map((r) => [r.name, r.rights]), [
    ['Portal wyników', { readMessages: true, writeMessages: false, readSchemas: false, writeSchemas: false }],
    ['System laboratoryjny (LIS)', { readMessages: false, writeMessages: true, readSchemas: false, writeSchemas: false }],
  ]);
  assert.equal(accessCount({ acl: ACL, keys: null }, { instanceId: INSTANCE, orgId: ORG }), 7);
  assert.equal(accessCount({ acl: ACL, keys: KEYS }, { instanceId: INSTANCE, orgId: ORG }), 8);
});

test('a key\'s requests carry the exact scope ids: the topic of this instance and organisation, the patterns of this instance', () => {
  assert.deepEqual(buildKeyCreateRequest(where, { name: '  Portal wyników pacjenta ', rights: { readMessages: true, writeMessages: false, readSchemas: true, writeSchemas: false } }), {
    name: 'Portal wyników pacjenta',
    keyType: 'general',
    scopeResources: [
      { resourceType: 'topic', resourceId: `17\u001ftentabus-a1b2c3d411\u001forg-default12\u001fwyniki-badan`, action: 'read' },
      { resourceType: 'bus_schema_registry', resourceId: `17\u001ftentabus-a1b2c3d411\u001forg-default`, action: 'read' },
    ],
  });
  assert.deepEqual(buildKeyRightsRequests(where, 'k-lis', { readMessages: false, writeMessages: true, readSchemas: true, writeSchemas: true }, { readMessages: true, writeMessages: true, readSchemas: true, writeSchemas: false }), [
    { kind: 'apiKeyScopeSetRequest', payload: { keyUid: 'k-lis', resourceType: 'topic', resourceId: topicScopeId(INSTANCE, ORG, TOPIC), action: 'read', accessLevel: 'allow' } },
    { kind: 'apiKeyScopeClearRequest', payload: { keyUid: 'k-lis', resourceType: 'bus_schema_registry', resourceId: schemaId, action: 'write' } },
  ]);
  assert.equal(recordsUrl('https://tf.local:8090', INSTANCE, 'wyniki badań', ORG), 'https://tf.local:8090/v1/bus/instances/tentabus-a1b2c3d4/topics/wyniki%20bada%C5%84/records?org_id=org-default');
  assert.equal(issueImpact({ name: 'Portal', rights: { readMessages: true, readSchemas: true }, topic: TOPIC })[0], 'Portal dostanie klucz z prawami: czyta wiadomości i czyta wzory (topik wyniki-badan). Klucz zobaczysz tylko raz.');
  assert.deepEqual(keyRightsImpact({ name: 'System laboratoryjny (LIS)', current: { writeMessages: true, readSchemas: true, writeSchemas: true }, next: { writeMessages: true, readSchemas: true, writeSchemas: false } }),
    ['System laboratoryjny (LIS) straci prawa: dodaje wzory.', 'Pozostałe prawa bez zmian; klucz się nie zmienia.']);
  assert.equal(lastUsedText(null, NOW), 'jeszcze nie');
  assert.equal(lastUsedText(NOW - 5000, NOW), 'przed chwilą');
});

function paint({ siteAdmin, keys = KEYS, acl = ACL }) {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const moves = [];
  paintAccessSection(host, {
    topic: { name: TOPIC },
    capabilities: { isSiteAdmin: siteAdmin, orgId: ORG, orgName: 'Przychodnia Zdrowie' },
    instanceId: INSTANCE,
    instanceLabel: 'Produkcja',
    notice: null,
    nowMs: NOW,
    accessData: { acl, aclError: null, keys: siteAdmin ? keys : null, keysError: null },
  }, { go: (a) => moves.push(a) });
  return { host, moves };
}

test('the section for the server administrator: both cards, "Nadaj dostęp", "Wydaj klucz", Zmień / Unieważnij on each key, last use', () => {
  const { host, moves } = paint({ siteAdmin: true });
  const subjects = host.querySelector('[data-role="subjects"]');
  assert.equal(subjects.rows.length, 5);
  assert.deepEqual([...subjects.querySelectorAll('tf-column')].map((c) => c.getAttribute('label')), ['Kto', 'Czytanie', 'Zapis', 'Administracja']);
  const actions = subjects.rowActions(subjects.rows[0], 0);
  assert.deepEqual([...actions.querySelectorAll('tf-button')].map((b) => b.textContent), ['Zmień', 'Usuń']);
  actions.querySelector('[data-act="remove"]').click();
  assert.deepEqual(moves.at(-1), { kind: 'access-remove', subject: 'group:g-ksiegowosc' });
  assert.equal(norm(host.querySelector('[data-role="subjects-foot"]').textContent), '— nie ustawiono tutaj: decyduje rola w organizacji. „Zabroniono” wygrywa z „pozwolono” z grupy.');
  const keysTable = host.querySelector('[data-role="keys"]');
  assert.deepEqual([...keysTable.querySelectorAll('tf-column')].map((c) => c.getAttribute('label')), ['System', 'Prawa', 'Ostatnio użyty']);
  assert.equal(keysTable.rows.length, 3);
  const keyActions = keysTable.rowActions(keysTable.rows[2], 2);
  assert.deepEqual([...keyActions.querySelectorAll('tf-button')].map((b) => b.textContent), ['Zmień', 'Unieważnij']);
  keyActions.querySelector('[data-act="key-revoke"]').click();
  assert.deepEqual(moves.at(-1), { kind: 'key-revoke', keyId: 'k-lis' });
  assert.ok(host.querySelector('[data-go="key-issue"]'));
  assert.ok(host.querySelector('[data-go="access-grant"]'));
  assert.equal(host.querySelector('[data-role="keys-who"]').hidden, true);
  assert.equal(host.querySelector('[data-role="keys-sub"]').textContent, 'Klucz działa w instancji Produkcja i organizacji Przychodnia Zdrowie. Prawa do wzorów wiadomości dotyczą całej instancji, nie tylko tego topiku.');
});

test('the section for a topic administrator who is not the server administrator: keys to read, no key buttons, who issues them', () => {
  const { host } = paint({ siteAdmin: false });
  assert.ok(host.querySelector('[data-go="access-grant"]'), 'entries are theirs to change');
  assert.equal(host.querySelector('[data-go="key-issue"]'), null);
  const keysTable = host.querySelector('[data-role="keys"]');
  assert.deepEqual([...keysTable.querySelectorAll('tf-column')].map((c) => c.getAttribute('label')), ['System', 'Prawa'], 'no last use: the server does not tell them');
  assert.equal(keysTable.rows.length, 2);
  assert.equal(keysTable.rowActions, null);
  assert.equal(host.querySelector('[data-role="keys-who"]').hidden, false);
  assert.equal(norm(host.querySelector('[data-role="keys-who"]').textContent), 'Klucze wydaje administrator serwera.');
});

test('an empty topic says who decides; a key-less topic says so', () => {
  const { host } = paint({ siteAdmin: true, acl: [], keys: [] });
  assert.equal(host.querySelector('[data-role="subjects"]').hidden, true);
  assert.equal(host.querySelector('[data-role="subjects-state"] tf-empty-state').getAttribute('title'), 'Nikt nie ma tu jeszcze wpisu');
  assert.equal(norm(host.querySelector('[data-role="keys-state"]').textContent), 'Żaden system zewnętrzny nie ma jeszcze klucza z prawami do tego topiku.');
});

test('"Wydaj klucz": a name and at least one right, the exact request, then the secret shown once with the address and the key\'s group', async () => {
  closeAll();
  const created = [];
  const issued = [];
  const copied = [];
  const win = openKeyIssue({
    where,
    instanceLabel: 'Produkcja',
    orgName: 'Przychodnia Zdrowie',
    origin: 'https://tf.local:8090',
    create: async (r) => { created.push(r); return { keyId: 'k-new', token: 'sk-0123abcd' }; },
    describeError: String,
    onIssued: (n) => issued.push(n),
  });
  assert.match(win.querySelector('[data-role="impact"]').textContent, /Wpisz nazwę systemu/);
  const name = win.querySelector('[data-role="name"]');
  name.value = 'Portal wyników pacjenta';
  name.dispatchEvent(new Event('input'));
  assert.match(win.querySelector('[data-role="impact"]').textContent, /Zaznacz co najmniej jedno prawo/);
  assert.ok(win.querySelector('[data-act="save"]').hasAttribute('disabled'));
  check(win.querySelector('tf-checkbox[data-key-right="readMessages"]'), true);
  check(win.querySelector('tf-checkbox[data-key-right="readSchemas"]'), true);
  assert.equal(win.querySelector('tf-checkbox[data-key-right="readMessages"]').querySelector('.tf-checkbox-desc').textContent, 'Pobiera wiadomości z topiku wyniki-badan.');
  assert.equal(norm(win.querySelector('[data-role="impact"]').textContent),
    'Co się stanie po zapisaniu: Portal wyników pacjenta dostanie klucz z prawami: czyta wiadomości i czyta wzory (topik wyniki-badan). Klucz zobaczysz tylko raz.');
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual(created, [buildKeyCreateRequest(where, { name: 'Portal wyników pacjenta', rights: { readMessages: true, writeMessages: false, readSchemas: true, writeSchemas: false } })]);
  assert.deepEqual(issued, [{ title: 'Wydano klucz', text: 'Portal wyników pacjenta: czyta wiadomości i czyta wzory.' }]);
  const shown = document.querySelector('tf-window.tb-key-issued');
  assert.ok(shown, 'the secret is shown in its own window');
  assert.equal(shown.querySelector('[data-role="token"]').textContent, 'sk-0123abcd');
  assert.equal(shown.querySelector('[data-role="url"]').textContent, 'https://tf.local:8090/v1/bus/instances/tentabus-a1b2c3d4/topics/wyniki-badan/records?org_id=org-default');
  assert.match(norm(shown.textContent), /Produkcja · Przychodnia Zdrowie/);
  assert.match(norm(shown.textContent), /czyta wiadomości i czyta wzory · topik wyniki-badan/);
  assert.match(norm(shown.textContent), /group=k:k-new \(albo k:k-new\.nazwa/);
  assert.match(norm(shown.textContent), /Skopiuj klucz teraz\. Później nie będzie można go zobaczyć/);
  closeAll();
  const { openKeyIssued } = await import('./topic-access.js');
  const again = openKeyIssued({ where, instanceLabel: 'Produkcja', orgName: 'Przychodnia Zdrowie', origin: 'https://tf.local:8090', name: 'X', rights: { readMessages: true }, keyId: 'k-x', token: 'sk-x', copy: async (t) => { copied.push(t); return true; } });
  again.querySelector('[data-act="copy-key"]').click();
  await tick();
  again.querySelector('[data-act="copy-url"]').click();
  await tick();
  assert.deepEqual(copied, ['sk-x', 'https://tf.local:8090/v1/bus/instances/tentabus-a1b2c3d4/topics/wyniki-badan/records?org_id=org-default']);
  assert.equal(again.querySelector('[data-role="copied"]').textContent, 'Skopiowano adres.');
});

test('"Prawa klucza": one change described, its scope set or cleared; "Unieważnij": at once, other rights named', async () => {
  closeAll();
  const lis = keyRows({ aclEntries: ACL, keys: KEYS, instanceId: INSTANCE, orgId: ORG }).find((r) => r.keyId === 'k-lis');
  const sent = [];
  const win = openKeyRights(lis, { where, scope: async (r) => { sent.push(r); }, describeError: String, onSaved: () => {} });
  check(win.querySelector('tf-checkbox[data-key-right="writeSchemas"]'), false);
  assert.equal(norm(win.querySelector('[data-role="impact"]').textContent), 'Co się stanie po zapisaniu: System laboratoryjny (LIS) straci prawa: dodaje wzory. Pozostałe prawa bez zmian; klucz się nie zmienia.');
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual(sent, [{ kind: 'apiKeyScopeClearRequest', payload: { keyUid: 'k-lis', resourceType: 'bus_schema_registry', resourceId: schemaId, action: 'write' } }]);

  closeAll();
  const revoked = [];
  const confirm = openKeyRevoke(lis, { revoke: async (r) => { revoked.push(r); }, describeError: String, onSaved: () => {} });
  assert.equal(norm(confirm.querySelector('[data-role="impact"]').textContent),
    'Co się stanie po unieważnieniu: Klucz przestanie działać od razu. System laboratoryjny (LIS) straci prawa: wysyła wiadomości, czyta wzory i dodaje wzory. Przestanie działać także 1 inne prawo tego klucza poza tym topikiem.');
  assert.match(norm(confirm.textContent), /Unieważnienie zapisze się w dzienniku audytu/);
  confirm.querySelector('[data-act="go"]').click();
  await tick();
  assert.deepEqual(revoked, [{ keyId: 'k-lis' }]);
});
