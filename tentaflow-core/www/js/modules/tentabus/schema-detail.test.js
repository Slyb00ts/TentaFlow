// =============================================================================
// File: modules/tentabus/schema-detail.test.js
// Description: A message pattern's page (U5, T08b): the pattern's own
// description read from its text, the state of each version (the one topics
// check with, older, withdrawn), the header (format, version, state, who and
// when, topics, compatibility), the actions an administrator gets and why
// one is disabled, the reader's line, a withdrawn pattern's warning with no
// new version and no compatibility change, the text of a version read-only
// with "Kopiuj" and "Pobierz", and the loading / missing / error states.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const {
  schemaDescription, displayText, versionState, downloadName, editorLanguage, headerLine, withdrawnText, versionRows,
  drawSchemaDetail, shareShownText, hl7ProfileView,
} = await import('./schema-detail.js');

const norm = (s) => String(s).replace(/[  ]/g, ' ').replace(/\s+/g, ' ').trim();
const tick = () => new Promise((r) => setTimeout(r, 20));
const DAY = 86_400_000;
const ADDED = new Date(2026, 8, 20, 10, 0, 0).getTime();

const TEXT = JSON.stringify({ description: 'Wizyta musi mieć pacjenta i termin.', type: 'object', required: ['pacjent'], properties: { pacjent: { type: 'string' } } }, null, 2);
const wizyta = {
  subject: 'wizyta', schemaType: 'json_schema', compatibility: 'backward', latestVersion: 3, deprecatedAtMs: null,
  usedByTopics: ['wizyty'], createdAtMs: ADDED, createdByLabel: 'Anna Kowalska',
};
const versions = [
  { subject: 'wizyta', version: 1, createdAtMs: ADDED - 20 * DAY, createdByLabel: 'Anna Kowalska', deprecatedAtMs: ADDED },
  { subject: 'wizyta', version: 2, createdAtMs: ADDED - 10 * DAY, createdByLabel: 'Anna Kowalska', deprecatedAtMs: null },
  { subject: 'wizyta', version: 3, createdAtMs: ADDED, createdByLabel: null, deprecatedAtMs: null },
];

test('the pattern\'s description comes from its own text', () => {
  assert.equal(schemaDescription('json_schema', TEXT), 'Wizyta musi mieć pacjenta i termin.');
  assert.equal(schemaDescription('avro', JSON.stringify({ type: 'record', name: 'R', doc: 'Odczyt urządzenia.', fields: [] })), 'Odczyt urządzenia.');
  assert.equal(schemaDescription('json_schema', '{"type":"object"}'), '');
  assert.equal(schemaDescription('json_schema', '{broken'), '');
  assert.equal(schemaDescription('protobuf', 'message A {}'), '');
});

test('a pattern written in JSON is shown laid out; any other text as stored', () => {
  assert.equal(displayText('json_schema', '{"type":"object","required":["a"]}'), '{\n  "type": "object",\n  "required": [\n    "a"\n  ]\n}');
  assert.equal(displayText('json_schema', '{broken'), '{broken');
  assert.equal(displayText('protobuf', 'message A {}'), 'message A {}');
});

test('file names, editor languages and version states', () => {
  assert.equal(downloadName('wizyta', 5, 'json_schema'), 'wizyta-v5.json');
  assert.equal(downloadName('e-recepta', 2, 'thrift'), 'e-recepta-v2.thrift');
  assert.equal(downloadName('x', 1, 'unknown'), 'x-v1.txt');
  assert.equal(editorLanguage('json_schema'), 'json');
  assert.equal(editorLanguage('protobuf'), 'plain');
  assert.equal(versionState(versions[2], { subjectDeprecated: false, effective: 3 }), 'current');
  assert.equal(versionState(versions[1], { subjectDeprecated: false, effective: 3 }), 'older');
  assert.equal(versionState(versions[0], { subjectDeprecated: false, effective: 3 }), 'deprecated');
  assert.equal(versionState(versions[2], { subjectDeprecated: true, effective: 3 }), 'deprecated', 'a withdrawn pattern has only withdrawn versions');
});

test('the line under the name: when, by whom, and who checks with it', () => {
  assert.equal(headerLine(wizyta), 'dodany 20.09.2026 · Anna Kowalska · używa go topik wizyty');
  assert.equal(headerLine({ ...wizyta, createdByLabel: null, usedByTopics: [] }), 'dodany 20.09.2026 · żaden topik go nie używa');
  assert.equal(headerLine({ ...wizyta, usedByTopics: ['wizyty', 'kolejka'] }), 'dodany 20.09.2026 · Anna Kowalska · używają go topiki kolejka i wizyty');
  assert.match(withdrawnText({ ...wizyta, deprecatedAtMs: 1 }, 3, true), /Topik wizyty nadal sprawdza wiadomości według ostatniej wersji \(3\), dopóki nie wybierzesz innego wzoru w jego ustawieniach/);
  assert.match(withdrawnText({ ...wizyta, usedByTopics: [], deprecatedAtMs: 1 }, 3, true), /możesz go usunąć/);
  // A reader is not asked to do what only an administrator can.
  const reader = withdrawnText({ ...wizyta, deprecatedAtMs: 1 }, 3, false);
  assert.match(reader, /według ostatniej wersji \(3\), dopóki administrator nie wybierze w nim innego wzoru\.$/);
  assert.doesNotMatch(reader, /wybierzesz/);
  assert.doesNotMatch(withdrawnText({ ...wizyta, usedByTopics: [], deprecatedAtMs: 1 }, 3, false), /możesz go usunąć/);
});

test('versions newest first: "Wycofaj" only on an active version for an administrator, "Pokaż" on the others', () => {
  const rows = versionRows({ info: wizyta, versions, shownVersion: 3, canAdmin: true });
  assert.deepEqual(rows.map((r) => r._version), [3, 2, 1]);
  assert.deepEqual(rows.map((r) => [r._canWithdraw, r._shown]), [[true, true], [true, false], [false, false]]);
  assert.match(rows[0].state, /aktualna/);
  assert.match(rows[1].state, /starsza/);
  assert.match(rows[2].state, /wycofana/);
  assert.match(rows[1].version, /Wersja 2.*10\.09\.2026 · Anna Kowalska/);
  assert.equal(versionRows({ info: wizyta, versions, shownVersion: 3, canAdmin: false }).some((r) => r._canWithdraw), false);
  assert.equal(versionRows({ info: { ...wizyta, deprecatedAtMs: 1 }, versions, shownVersion: 3, canAdmin: true }).some((r) => r._canWithdraw), false);
});

function mount(view) {
  const body = document.createElement('div');
  document.body.appendChild(body);
  const moves = [];
  const full = {
    name: 'wizyta', info: wizyta, subjectsLoaded: true, versions, shown: { version: 3, text: TEXT, error: null },
    error: null, errorKind: null, canAdmin: true, notice: null, instanceLabel: 'Produkcja', ...view,
  };
  drawSchemaDetail(body, { view: () => full, go: (a) => moves.push(a) });
  return { body, moves, view: full };
}

test('an administrator\'s page: header, the text of the version topics check with, versions, compatibility', () => {
  const { body, moves } = mount({});
  assert.equal(body.querySelector('.tb-title').textContent, 'wizyta');
  assert.deepEqual([...body.querySelectorAll('[data-role="chips"] tf-chip')].map((c) => c.getAttribute('label')), ['JSON Schema', 'wersja 3', 'w użyciu']);
  assert.equal(body.querySelector('[data-role="desc"]').textContent, 'dodany 20.09.2026 · Anna Kowalska · używa go topik wizyty');
  assert.equal(body.querySelector('[data-role="badges"] tf-chip').getAttribute('label'), 'zgodność: nowe programy przeczytają stare wiadomości');
  assert.equal(body.querySelector('[data-role="text-title"]').textContent, 'Wersja 3 — tekst wzoru');
  assert.equal(body.querySelector('[data-role="about"]').textContent, 'Wizyta musi mieć pacjenta i termin.');
  const editor = body.querySelector('tf-code-editor');
  assert.ok(editor.hasAttribute('readonly'));
  assert.equal(editor.getAttribute('language'), 'json');
  assert.equal(editor.value, TEXT);
  const del = body.querySelector('[data-role="delete"]');
  assert.ok(del.hasAttribute('disabled'), 'a topic uses it');
  assert.equal(del.getAttribute('title'), 'Nie można usunąć: używa go topik wizyty. Usuniesz go, gdy w ustawieniach tego topiku wybierzesz inny wzór.');
  assert.equal(norm(body.querySelector('[data-role="delete-note"]').textContent), del.getAttribute('title'));
  assert.equal(body.querySelector('[data-role="new-version"]').hasAttribute('disabled'), false);
  assert.equal(body.querySelector('[data-role="versions-count"]').getAttribute('label'), '3');
  assert.equal(body.querySelector('[data-role="versions"]').rows.length, 3);
  const compat = body.querySelector('[data-role="compat-card"]');
  assert.equal(compat.querySelector('.tb-vr-label').textContent, 'Co jest sprawdzane');
  assert.equal(compat.querySelector('.tb-vr-value').textContent, 'nowe programy przeczytają stare wiadomości');
  assert.equal(compat.querySelector('.tb-vr-hint').textContent, 'Nowa wersja nie może wymagać niczego, czego stare wiadomości nie mają.');
  body.querySelector('[data-role="new-version"]').click();
  body.querySelector('[data-role="compat"]').click();
  body.querySelector('[data-role="withdraw"]').click();
  del.click();
  assert.deepEqual(moves.map((m) => m.kind), ['new-version', 'compat', 'withdraw'], 'a disabled button does nothing');
});

test('a version row leads to its text and to withdrawing it', () => {
  const { body, moves } = mount({});
  const table = body.querySelector('[data-role="versions"]');
  table.rowActions(table.rows[1], 1).querySelector('[data-act="show-version"]').click();
  table.rowActions(table.rows[1], 1).querySelector('[data-act="withdraw-version"]').click();
  assert.equal(table.rowActions(table.rows[0], 0).querySelector('[data-act="show-version"]'), null, 'the shown version has no "Pokaż"');
  assert.equal(table.rowActions(table.rows[2], 2).querySelector('[data-act="withdraw-version"]'), null, 'a withdrawn version is not withdrawn again');
  assert.deepEqual(moves, [{ kind: 'show-version', version: 2 }, { kind: 'withdraw-version', version: 2 }]);
});

test('an older version on the page says which one topics check with; a text that failed offers another try', () => {
  const older = mount({ shown: { version: 2, text: '{"type":"object"}', error: null } });
  assert.equal(older.body.querySelector('[data-role="text-title"]').textContent, 'Wersja 2 — tekst wzoru');
  assert.equal(older.body.querySelector('[data-role="text-note"]').textContent, 'Topiki sprawdzają wiadomości według wersji 3, nie tej.');
  assert.equal(older.body.querySelector('[data-role="about"]').hidden, true);
  const failed = mount({ shown: { version: 2, text: null, error: new Error('x') } });
  assert.equal(failed.body.querySelector('tf-code-editor').hidden, true);
  assert.ok(failed.body.querySelector('[data-role="copy"]').hasAttribute('disabled'));
  assert.match(failed.body.querySelector('[data-role="text-state"]').textContent, /Nie udało się wczytać tekstu wersji 2/);
  failed.body.querySelector('[data-go="retry-text"]').click();
  assert.deepEqual(failed.moves, [{ kind: 'retry-text' }]);
});

test('a withdrawn pattern: the warning, nothing new to add, no compatibility change', () => {
  const { body } = mount({ info: { ...wizyta, deprecatedAtMs: ADDED } });
  const alert = body.querySelector('[data-role="warning"] tf-alert');
  assert.equal(alert.getAttribute('title'), 'Wzór wycofany');
  assert.match(alert.getAttribute('message'), /Topik wizyty nadal sprawdza wiadomości według ostatniej wersji \(3\)/);
  assert.equal(body.querySelector('[data-role="chips"] [data-role="state"]').getAttribute('label'), 'wycofany');
  assert.equal(body.querySelector('[data-role="new-version"]').getAttribute('title'), 'Wzór jest wycofany — nowych wersji się nie dodaje.');
  assert.ok(body.querySelector('[data-role="withdraw"]').hasAttribute('disabled'));
  assert.equal(body.querySelector('[data-role="compat"]'), null);
  assert.match(body.querySelector('[data-role="compat-card"]').textContent, /zgodności się nie zmienia/);
  assert.match(body.querySelector('[data-role="versions-sub"]').textContent, /wycofane są wszystkie jego wersje/);
});

test('right after withdrawing, the note of it stands alone; "Pokaż" is a word, not only an eye', () => {
  const { body } = mount({ info: { ...wizyta, deprecatedAtMs: ADDED }, notice: { tone: 'success', withdrawn: true, title: 'Wycofano wzór wizyta', text: 'x' } });
  assert.equal(body.querySelectorAll('tf-alert').length, 1);
  assert.equal(body.querySelector('[data-role="notice"] tf-alert').getAttribute('title'), 'Wycofano wzór wizyta');
  const table = body.querySelector('[data-role="versions"]');
  const show = table.rowActions(table.rows[1], 1).querySelector('[data-act="show-version"]');
  assert.equal(show.textContent, 'Pokaż');
  assert.equal(show.getAttribute('aria-label'), 'Pokaż wersję 2');
});

test('every version withdrawn but the pattern not: the newest still checks and the page says so', () => {
  const all = versions.map((v) => ({ ...v, deprecatedAtMs: ADDED }));
  const { body } = mount({ versions: all });
  assert.equal(body.querySelector('[data-role="versions-sub"]').textContent, 'Wszystkie wersje są wycofane — topiki sprawdzają wiadomości według ostatniej, wersji 3.');
});

test('an unused pattern can be deleted; a reader gets no buttons and the reason', () => {
  const unused = mount({ info: { ...wizyta, usedByTopics: [] } });
  assert.equal(unused.body.querySelector('[data-role="delete"]').hasAttribute('disabled'), false);
  unused.body.querySelector('[data-role="delete"]').click();
  assert.deepEqual(unused.moves, [{ kind: 'delete' }]);
  const reader = mount({ canAdmin: false });
  assert.equal(reader.body.querySelector('[data-role="actions"] tf-button'), null);
  assert.equal(norm(reader.body.querySelector('[data-role="actions"]').textContent), 'Wzory dodaje, zmienia, wycofuje i usuwa administrator instancji.');
  assert.equal(reader.body.querySelector('[data-role="compat"]'), null);
  assert.equal(reader.body.querySelector('[data-role="versions"]').rows.some((r) => r._canWithdraw), false);
  assert.equal(reader.body.querySelector('[data-role="copy"]').hasAttribute('disabled'), false, 'reading and copying are for everyone');
});

test('loading, missing and failed states', () => {
  assert.ok(mount({ info: null, subjectsLoaded: false }).body.querySelector('tf-spinner'));
  const missing = mount({ info: null, subjectsLoaded: true });
  assert.equal(missing.body.querySelector('tf-empty-state').getAttribute('title'), 'Wzoru wizyta już nie ma');
  missing.body.querySelector('tf-empty-state [data-go="back"]').click();
  assert.deepEqual(missing.moves, [{ kind: 'back' }]);
  const failed = mount({ versions: null, error: new Error('timed out'), errorKind: 'timeout' });
  assert.equal(failed.body.querySelector('tf-empty-state').getAttribute('title'), 'Nie udało się wczytać wzoru');
  failed.body.querySelector('[data-go="retry"]').click();
  assert.deepEqual(failed.moves, [{ kind: 'retry' }]);
});

test('"Kopiuj" writes the shown text to the clipboard; "Pobierz" saves it under the version\'s name', async () => {
  const copied = [];
  Object.defineProperty(globalThis.navigator, 'clipboard', { configurable: true, value: { writeText: async (t) => { copied.push(t); } } });
  const { view } = mount({});
  await shareShownText('copy', view);
  assert.deepEqual(copied, [TEXT]);

  const saved = [];
  const created = [];
  const originalCreate = URL.createObjectURL;
  const originalRevoke = URL.revokeObjectURL;
  URL.createObjectURL = (blob) => { created.push(blob); return 'blob:tb-test'; };
  URL.revokeObjectURL = () => {};
  const originalClick = window.HTMLAnchorElement.prototype.click;
  window.HTMLAnchorElement.prototype.click = function click() { saved.push({ href: this.href, download: this.download, attached: this.isConnected }); };
  try {
    await shareShownText('download', view);
    assert.deepEqual(saved, [{ href: 'blob:tb-test', download: 'wizyta-v3.json', attached: true }]);
    assert.equal(await created[0].text(), TEXT);
    assert.match(created[0].type, /^application\/schema\+json/);
    assert.equal(document.querySelectorAll('a[download]').length, 0, 'the link does not stay in the page');
  } finally {
    URL.createObjectURL = originalCreate;
    URL.revokeObjectURL = originalRevoke;
    window.HTMLAnchorElement.prototype.click = originalClick;
  }
  await tick();
});

test('a reader\'s page of a withdrawn pattern gets the reader\'s warning', () => {
  const { body } = mount({ canAdmin: false, info: { ...wizyta, deprecatedAtMs: ADDED } });
  const message = body.querySelector('[data-role="warning"] tf-alert').getAttribute('message');
  assert.match(message, /dopóki administrator nie wybierze w nim innego wzoru/);
  assert.doesNotMatch(message, /wybierzesz/);
});

const PROFILE = JSON.stringify({
  description: 'Profil HL7 v2: wymagane segmenty i pola wyniku badania.',
  required_segments: ['MSH', 'PID'],
  required_fields: ['PID-3', 'PID-5', 'OBX-8', 'ZZZ-1'],
});
const wynik = { ...wizyta, subject: 'wynik-badania', schemaType: 'hl7v2_profile', usedByTopics: ['wyniki-badan'] };

test('an HL7 profile\'s description is its description key', () => {
  assert.equal(schemaDescription('hl7v2_profile', PROFILE), 'Profil HL7 v2: wymagane segmenty i pola wyniku badania.');
  assert.equal(schemaDescription('hl7v2_profile', '{"required_segments":[]}'), '');
});

test('file names and editor languages of an HL7 profile', () => {
  assert.equal(downloadName('wynik', 4, 'hl7v2_profile'), 'wynik-v4.json');
  assert.equal(editorLanguage('hl7v2_profile'), 'json');
  assert.equal(displayText('hl7v2_profile', '{"required_segments":["PID"]}'), '{\n  "required_segments": [\n    "PID"\n  ]\n}');
});

test('a profile spelled out: its segments (listed, then those only a field names) and fields with their names', () => {
  const view = hl7ProfileView(PROFILE);
  assert.deepEqual(view.segments, ['MSH', 'PID', 'OBX', 'ZZZ']);
  assert.deepEqual(view.fields.map((f) => f.address), ['PID-3', 'PID-5', 'OBX-8', 'ZZZ-1']);
  assert.equal(view.fields[0].label, 'Lista identyfikatorów pacjenta');
  assert.equal(view.fields[3].label, '', 'a field outside the dictionary has no name');
  assert.equal(hl7ProfileView('not json'), null);
  assert.equal(hl7ProfileView('[1]'), null);
  assert.deepEqual(hl7ProfileView('{}'), { segments: [], fields: [] });
});

test('an HL7 profile\'s page shows its segments and fields in plain words above the text', () => {
  const { body } = mount({ name: 'wynik-badania', info: wynik, shown: { version: 3, text: PROFILE, error: null } });
  assert.deepEqual([...body.querySelectorAll('[data-role="chips"] tf-chip')].map((c) => c.getAttribute('label')).slice(0, 1), ['profil HL7 v2']);
  assert.equal(body.querySelector('[data-role="about"]').textContent, 'Profil HL7 v2: wymagane segmenty i pola wyniku badania.');
  const profile = body.querySelector('[data-role="profile"]');
  assert.equal(profile.hidden, false);
  assert.deepEqual([...profile.querySelectorAll('[data-role="profile-segments"] tf-chip')].map((c) => c.getAttribute('label')), ['MSH', 'PID', 'OBX', 'ZZZ']);
  assert.match(norm(profile.textContent), /Wymagane segmenty .* Segment to jeden wiersz wiadomości HL7 v2/);
  const rows = body.querySelector('[data-role="profile-fields"]').rows;
  assert.equal(rows.length, 4);
  assert.match(rows[0].field, />PID-3</);
  assert.equal(rows[0].contains, 'Lista identyfikatorów pacjenta');
  assert.equal(body.querySelector('tf-code-editor').getAttribute('language'), 'json');
  // Another format shows no profile.
  const plain = mount({});
  assert.equal(plain.body.querySelector('[data-role="profile"]').hidden, true);
});
