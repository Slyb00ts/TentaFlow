// =============================================================================
// File: modules/tentabus/schema-windows.test.js
// Description: The message-pattern windows (U5, T08): names and texts the
// server would refuse are refused here with the reason; the requests carry
// exactly what the server expects (a new pattern's compatibility, a new
// version without one, withdrawing the pattern or one version, deleting);
// the difference of a new JSON Schema version from the newest one in words;
// a refused new version explained in plain words for every sentence the
// server's compatibility check writes (direction included) and the server's
// own text folded away for one it does not know; what topics check with
// after a version is withdrawn; a refused delete names the topics.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const {
  subjectNameProblem, schemaTextProblem, buildRegisterRequest, buildDeleteRequest, jsonSchemaChanges, parseIncompatible,
  incompatibilityReason, refusalHtml, boundTopics, effectiveVersion, versionImpact, versionDeprecateImpact, subjectDeprecateImpact,
  openSchemaAdd, openSchemaVersion, openSchemaCompat, openSchemaDeprecate, openVersionDeprecate, openSchemaDelete,
} = await import('./schema-windows.js');

const norm = (s) => String(s).replace(/[  ]/g, ' ').replace(/\s+/g, ' ').trim();
const tick = () => new Promise((r) => setTimeout(r, 20));
const type = (el, value) => {
  el.value = value;
  el.dispatchEvent(new Event('input'));
};
const pick = (el, value) => {
  el.value = value;
  el.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value } }));
};
const closeAll = () => document.querySelectorAll('tf-window').forEach((w) => w.remove());

const V1 = JSON.stringify({ type: 'object', required: ['pacjent', 'termin'], properties: { pacjent: { type: 'string' }, termin: { type: 'string' } } });
const V2 = JSON.stringify({ type: 'object', required: ['pacjent', 'termin'], properties: { pacjent: { type: 'string' }, termin: { type: 'string' }, gabinet: { type: 'string' } } });
const wizyta = { subject: 'wizyta', schemaType: 'json_schema', compatibility: 'backward', latestVersion: 2, deprecatedAtMs: null, usedByTopics: ['wizyty'] };
const incompatible = (mode, detail) => new Error(`protocol error BadRequest: bus.schema_incompatible: 'wizyta' mode=${mode}: ${detail}`);

test('a new pattern\'s name: empty, characters or length the server refuses, a name already taken', () => {
  assert.equal(subjectNameProblem('  '), 'empty');
  assert.equal(subjectNameProblem('wizyta 2'), 'invalid');
  assert.equal(subjectNameProblem('zażółć'), 'invalid');
  assert.equal(subjectNameProblem('a'.repeat(129)), 'invalid');
  assert.equal(subjectNameProblem('a'.repeat(128)), null);
  assert.equal(subjectNameProblem('wizyta', ['wizyta']), 'taken', 'the server would add a version to it instead');
  assert.equal(subjectNameProblem('skierowanie.v1_b-2', ['wizyta']), null);
});

test('a pattern text: empty, over 256 KB, not JSON for a format written in JSON', () => {
  assert.equal(schemaTextProblem(' ', 'json_schema'), 'empty');
  assert.equal(schemaTextProblem('x'.repeat(256 * 1024 + 1), 'protobuf'), 'too_big');
  assert.equal(schemaTextProblem('{"type": ', 'json_schema'), 'not_json');
  assert.equal(schemaTextProblem('{"type": ', 'avro'), 'not_json');
  assert.equal(schemaTextProblem('syntax = "proto3";', 'protobuf'), null);
  assert.equal(schemaTextProblem(V1, 'json_schema'), null);
});

test('requests: a new pattern carries its compatibility, a new version leaves it to the pattern', () => {
  assert.deepEqual(buildRegisterRequest('i', { subject: 's', schemaType: 'json_schema', schemaText: '{}', compatibility: 'full' }),
    { instanceId: 'i', subject: 's', schemaType: 'json_schema', schemaText: '{}', compatibility: 'full' });
  assert.deepEqual(buildRegisterRequest('i', { subject: 's', schemaType: 'json_schema', schemaText: '{}' }),
    { instanceId: 'i', subject: 's', schemaType: 'json_schema', schemaText: '{}' });
  assert.deepEqual(buildDeleteRequest('i', 's', { deprecateOnly: true }), { instanceId: 'i', subject: 's', deprecateOnly: true });
  assert.deepEqual(buildDeleteRequest('i', 's', { version: 3, deprecateOnly: true }), { instanceId: 'i', subject: 's', deprecateOnly: true, version: 3 });
  assert.deepEqual(buildDeleteRequest('i', 's', { deprecateOnly: false }), { instanceId: 'i', subject: 's', deprecateOnly: false });
});

test('the difference from the newest version, in words', () => {
  assert.equal(jsonSchemaChanges(V1, V2, 1), 'Różnica względem wersji 1: nowe, nieobowiązkowe pole „gabinet”.');
  const required = JSON.stringify({ type: 'object', required: ['pacjent', 'termin', 'lekarz'], properties: { pacjent: { type: 'string' }, lekarz: { type: 'string' }, termin: { type: 'string' } } });
  assert.equal(jsonSchemaChanges(V1, required, 1), 'Różnica względem wersji 1: nowe, wymagane pole „lekarz”.');
  const dropped = JSON.stringify({ type: 'object', required: ['pacjent'], properties: { pacjent: { type: 'string' } } });
  assert.equal(jsonSchemaChanges(V1, dropped, 1), 'Różnica względem wersji 1: usunięte pole „termin”.');
  const optional = JSON.stringify({ type: 'object', required: ['pacjent'], properties: { pacjent: { type: 'string' }, termin: { type: 'string' } } });
  assert.equal(jsonSchemaChanges(V1, optional, 1), 'Różnica względem wersji 1: pole „termin” nie jest już wymagane.');
  assert.equal(jsonSchemaChanges(V1, JSON.stringify(JSON.parse(V1), null, 2), 1), 'Tekst jest taki sam jak wersja 1.');
  const typed = JSON.stringify({ ...JSON.parse(V1), properties: { pacjent: { type: 'integer' }, termin: { type: 'string' } } });
  assert.match(jsonSchemaChanges(V1, typed, 1), /nie dotyczą listy pól/);
  assert.equal(jsonSchemaChanges(V1, '{"type": ', 1), null, 'text still being written says nothing');
});

test('the refusal of a new version is taken apart from the server message', () => {
  assert.deepEqual(parseIncompatible(incompatible('backward', "property 'x' is required by the reader schema but not guaranteed present by the writer schema").message),
    { mode: 'backward', detail: "property 'x' is required by the reader schema but not guaranteed present by the writer schema" });
  assert.equal(parseIncompatible('protocol error BadRequest: bus.invalid_argument: nope'), null);
});

test('every sentence of the compatibility check has plain words, with the direction it failed in', () => {
  const required = "property 'lekarz' is required by the reader schema but not guaranteed present by the writer schema";
  const newRequires = JSON.stringify({ ...JSON.parse(V1), required: ['pacjent', 'termin', 'lekarz'] });
  assert.equal(incompatibilityReason({ mode: 'backward', detail: required, newText: newRequires }).reason, 'nowa wersja wymaga pola „lekarz”, którego stare wiadomości mogą nie mieć');
  assert.equal(incompatibilityReason({ mode: 'full', detail: required, newText: newRequires }).fix, 'Usuń „lekarz” z pól wymaganych albo zmień zgodność wzoru.');
  const dropped = incompatibilityReason({ mode: 'full', detail: required, newText: V1 });
  assert.equal(dropped.reason, 'nowa wersja nie gwarantuje pola „lekarz”, którego wymagają stare programy');
  assert.equal(incompatibilityReason({ mode: 'forward', detail: required, newText: newRequires }).reason, 'nowa wersja nie gwarantuje pola „lekarz”, którego wymagają stare programy');

  const extra = "property 'gabinet' present in the writer schema has no counterpart in the reader schema, which rejects additional properties";
  assert.match(incompatibilityReason({ mode: 'forward', detail: extra, newText: V2 }).reason, /dodaje pole „gabinet”/);
  assert.match(incompatibilityReason({ mode: 'backward', detail: extra, newText: V1 }).reason, /nie zna pola „gabinet”/);
  assert.match(incompatibilityReason({ mode: 'backward', detail: "property 'termin' type is narrowed: writer allows {\"string\"}, reader allows {\"integer\"}", newText: V1 }).reason, /zawęża rodzaj wartości pola „termin”/);
  assert.match(incompatibilityReason({ mode: 'backward', detail: "property 'termin' subschema differs beyond 'type' widening", newText: V1 }).reason, /pole „termin” zmieniło się/);
  assert.match(incompatibilityReason({ mode: 'forward', detail: 'writer schema allows additional properties but reader schema rejects them (additionalProperties: false)', newText: V1 }).reason, /dodatkowe pola/);
  assert.match(incompatibilityReason({ mode: 'full', detail: "root keyword 'additionalProperties' (schema form) changed", newText: V1 }).reason, /dodatkowe pola/);
  assert.match(incompatibilityReason({ mode: 'full', detail: "root keyword 'minProperties' changed and is not covered by the structural properties/required/type/additionalProperties compatibility check", newText: V1 }).reason, /ustawienie „minProperties”/);
  assert.match(incompatibilityReason({ mode: 'full', detail: "definition '#/$defs/Id', reachable from an old-schema property, changed", newText: V1 }).reason, /definicja „\$defs\/Id”/);
  assert.match(incompatibilityReason({ mode: 'full', detail: 'root schema \'type\' must include "object" on both sides for structural compatibility comparison', newText: V1 }).reason, /nie opisuje obiektu/);
  assert.equal(incompatibilityReason({ mode: 'full', detail: 'something new the checker says', newText: V1 }), null);
});

test('the refusal box: known reasons in words, an unknown one with the server\'s text folded away', () => {
  const box = (err) => {
    const el = document.createElement('div');
    el.innerHTML = refusalHtml({ err, title: 'Nie dodano wersji 3.', compatibility: 'backward', schemaType: 'json_schema', newText: V1, describeError: () => 'Odmowa serwera.' });
    return el;
  };
  const known = box(incompatible('backward', "property 'termin' is required by the reader schema but not guaranteed present by the writer schema"));
  assert.equal(norm(known.textContent), 'Nie dodano wersji 3. Ten wzór ma zgodność „nowe programy przeczytają stare wiadomości”, a nowa wersja wymaga pola „termin”, którego stare wiadomości mogą nie mieć. Usuń „termin” z pól wymaganych albo zmień zgodność wzoru.');
  assert.equal(known.querySelector('details'), null);
  const unknown = box(incompatible('full', 'a sentence nobody mapped'));
  assert.match(norm(unknown.textContent), /^Nie dodano wersji 3\. Nowa wersja nie spełnia zgodności „w obie strony” z poprzednią wersją\. Popraw tekst albo zmień zgodność wzoru\./);
  assert.equal(unknown.querySelector('details summary').textContent, 'Szczegóły techniczne');
  assert.equal(unknown.querySelector('details pre').textContent, 'a sentence nobody mapped');
  const invalid = box(new Error("protocol error BadRequest: bus.invalid_argument: schema: invalid schema: /properties: unknown type 'strin'"));
  assert.match(norm(invalid.textContent), /nie jest poprawny wzór w formacie JSON Schema/);
  assert.match(invalid.querySelector('details pre').textContent, /unknown type 'strin'/);
  const withdrawn = box(new Error("protocol error BadRequest: bus.invalid_argument: subject 'wizyta' is deprecated; cannot register a new version"));
  assert.match(norm(withdrawn.textContent), /Wzór został wycofany/);
  assert.match(norm(box(new Error('socket closed')).textContent), /Odmowa serwera\.$/);
});

test('what topics check with: the newest active version, else the newest', () => {
  const v = (version, deprecatedAtMs = null) => ({ version, deprecatedAtMs });
  assert.equal(effectiveVersion(false, [v(1), v(2), v(3)]), 3);
  assert.equal(effectiveVersion(false, [v(1), v(2), v(3, 9)]), 2);
  assert.equal(effectiveVersion(false, [v(1, 9), v(2, 9)]), 2, 'every version withdrawn: the newest still checks');
  assert.equal(effectiveVersion(true, [v(1), v(2, 9)]), 2, 'a withdrawn pattern keeps checking with its newest');
  assert.equal(effectiveVersion(false, []), null);
});

test('"Co się stanie" of a new version, of withdrawing one version and of withdrawing the pattern', () => {
  assert.deepEqual(versionImpact({ nextVersion: 3, latestVersion: 2, compatibility: 'backward', usedByTopics: ['wizyty'] }), [
    'powstanie wersja 3.',
    'Najpierw serwer sprawdzi zgodność „nowe programy przeczytają stare wiadomości” z wersją 2; niezgodnej wersji nie doda.',
    'Topik wizyty zacznie sprawdzać wiadomości według niej.',
  ]);
  assert.deepEqual(versionImpact({ nextVersion: 2, latestVersion: 1, compatibility: 'none', usedByTopics: [] }), ['powstanie wersja 2.', 'Żaden topik jeszcze nie używa tego wzoru.']);
  const v = (version, deprecatedAtMs = null) => ({ version, deprecatedAtMs });
  const three = [v(1), v(2), v(3)];
  assert.deepEqual(versionDeprecateImpact({ version: 3, versions: three, usedByTopics: ['wizyty'] }), ['najnowszą niewycofaną wersją stanie się wersja 2; topik wizyty będzie sprawdzać wiadomości według niej.']);
  assert.deepEqual(versionDeprecateImpact({ version: 1, versions: three, usedByTopics: ['wizyty', 'kolejka'] }), ['topiki kolejka i wizyty nadal sprawdzają wiadomości według wersji 3.']);
  assert.deepEqual(versionDeprecateImpact({ version: 2, versions: [v(1, 5), v(2)], usedByTopics: [] }), ['to ostatnia niewycofana wersja; wycofanie nie wyłącza sprawdzania, więc wzór dalej sprawdza wiadomości według wersji 2.']);
  assert.deepEqual(subjectDeprecateImpact({ usedByTopics: ['wizyty'] }), [
    'nie da się dodać nowej wersji ani wybrać tego wzoru dla topiku; na liście wzorów zobaczysz go jako wycofany.',
    'Topik wizyty nadal będzie sprawdzać wiadomości według ostatniej wersji, dopóki nie wybierzesz innego wzoru w jego ustawieniach.',
  ]);
});

test('a refused delete names the topics that took the pattern', () => {
  assert.deepEqual(boundTopics("protocol error BadRequest: bus.invalid_argument: schema subject 'wizyta' is bound by topics: wizyty, kolejka"), ['wizyty', 'kolejka']);
  assert.equal(boundTopics('bus.schema_not_found'), null);
});

test('"Dodaj wzór": formats from the server, a taken name and a broken text keep it locked, then one request', async () => {
  closeAll();
  const sent = [];
  const added = [];
  const win = openSchemaAdd({
    instanceId: 'tentabus-1a2b3c4d',
    schemaTypes: ['json_schema', 'avro', 'protobuf', 'thrift'],
    existingNames: ['wizyta'],
    register: async (request) => { sent.push(request); return { version: 1, schemaRefId: 7, deduplicated: false }; },
    describeError: () => 'Odmowa serwera.',
    onAdded: (a) => added.push(a),
  });
  assert.equal(win.getAttribute('modal'), '');
  assert.deepEqual([...win.querySelectorAll('tf-choice-card')].map((c) => c.getAttribute('heading')), ['JSON Schema', 'Avro', 'Protobuf', 'Thrift']);
  assert.equal(win.querySelector('[data-role="format"]').getAttribute('value'), 'json_schema');
  assert.equal(win.querySelector('[data-role="compat"]').value, 'backward');
  const save = win.querySelector('[data-act="save"]');
  assert.equal(norm(save.textContent), 'Dodaj wzór');
  const name = win.querySelector('[data-role="name"]');
  type(name, 'wizyta');
  assert.match(name.getAttribute('error'), /już jest/);
  type(win.querySelector('[data-role="text"]'), '{"type": ');
  assert.match(win.querySelector('[data-role="text"]').getAttribute('error'), /To nie jest poprawny JSON/);
  assert.ok(save.hasAttribute('disabled'));
  type(name, 'skierowanie');
  type(win.querySelector('[data-role="text"]'), V1);
  pick(win.querySelector('[data-role="compat"]'), 'full');
  assert.equal(save.hasAttribute('disabled'), false);
  assert.equal(norm(win.querySelector('[data-role="impact"]').textContent), 'Co się stanie po dodaniu: powstanie wzór skierowanie (JSON Schema), wersja 1. Żaden topik go jeszcze nie używa — wybierzesz go w ustawieniach topiku.');
  save.click();
  await tick();
  assert.deepEqual(sent, [{ instanceId: 'tentabus-1a2b3c4d', subject: 'skierowanie', schemaType: 'json_schema', schemaText: V1, compatibility: 'full' }]);
  assert.deepEqual(added, [{ subject: 'skierowanie', schemaType: 'json_schema', version: 1 }]);
});

test('"Dodaj wzór": a file loads into the text; the server\'s refusal stays in the window and clears on the next edit', async () => {
  closeAll();
  const win = openSchemaAdd({
    instanceId: 'i',
    schemaTypes: ['json_schema'],
    existingNames: [],
    register: async () => { throw new Error("protocol error BadRequest: bus.invalid_argument: schema: invalid schema: /: unknown type 'strin'"); },
    describeError: () => 'Odmowa serwera.',
    onAdded: () => assert.fail('nothing was added'),
  });
  type(win.querySelector('[data-role="name"]'), 'skierowanie');
  const file = { size: 20, text: async () => '{"type": "strin"}' };
  win.querySelector('[data-role="file"]').dispatchEvent(new CustomEvent('change', { detail: { files: [file] } }));
  await tick();
  assert.equal(win.querySelector('[data-role="text"]').value, '{"type": "strin"}');
  win.querySelector('[data-act="save"]').click();
  await tick();
  const error = win.querySelector('[data-role="error"]');
  assert.equal(error.hidden, false);
  assert.match(norm(error.textContent), /^Nie dodano wzoru skierowanie\. Serwer nie przyjął tekstu/);
  assert.ok(win.isConnected, 'the window stays open with the refusal');
  type(win.querySelector('[data-role="text"]'), '{"type": "string"}');
  assert.equal(error.hidden, true, 'an edited text is no longer the refused one');
  const big = { size: 256 * 1024 + 1, text: async () => 'x' };
  win.querySelector('[data-role="file"]').dispatchEvent(new CustomEvent('change', { detail: { files: [big] } }));
  await tick();
  assert.match(win.querySelector('[data-role="text"]').getAttribute('error'), /najwyżej 256 KB/);
});

test('"Nowa wersja": starts from the newest text, says the difference, is refused in plain words and keeps the draft', async () => {
  closeAll();
  const drafts = [];
  const win = openSchemaVersion({
    instanceId: 'i',
    subject: wizyta,
    latestText: V1,
    draftText: null,
    register: async () => { throw incompatible('backward', "property 'lekarz' is required by the reader schema but not guaranteed present by the writer schema"); },
    describeError: () => 'Odmowa serwera.',
    onAdded: () => assert.fail('refused'),
    onDraft: (t) => drafts.push(t),
  });
  const text = win.querySelector('[data-role="text"]');
  assert.equal(text.value, V1);
  assert.equal(text.getAttribute('label'), 'Tekst wzoru — wersja 3');
  assert.ok(win.querySelector('[data-act="save"]').hasAttribute('disabled'), 'the same text adds nothing');
  const next = JSON.stringify({ ...JSON.parse(V1), required: ['pacjent', 'termin', 'lekarz'], properties: { ...JSON.parse(V1).properties, lekarz: { type: 'string' } } });
  type(text, next);
  assert.equal(win.querySelector('[data-role="diff"]').textContent, 'Różnica względem wersji 2: nowe, wymagane pole „lekarz”.');
  assert.match(norm(win.querySelector('[data-role="impact"]').textContent), /powstanie wersja 3\. Najpierw serwer sprawdzi zgodność „nowe programy przeczytają stare wiadomości” z wersją 2.*Topik wizyty zacznie sprawdzać/);
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.match(norm(win.querySelector('[data-role="error"]').textContent), /^Nie dodano wersji 3\. Ten wzór ma zgodność „nowe programy przeczytają stare wiadomości”, a nowa wersja wymaga pola „lekarz”/);
  win.close(true);
  await tick();
  await new Promise((r) => setTimeout(r, 300));
  assert.deepEqual(drafts, [next], 'the refused text is handed back for the next attempt');

  closeAll();
  const reopened = openSchemaVersion({ instanceId: 'i', subject: wizyta, latestText: V1, draftText: next, register: async () => ({ version: 3, deduplicated: false }), describeError: String, onAdded: () => {}, onDraft: () => {} });
  assert.equal(reopened.querySelector('[data-role="text"]').value, next);
  assert.match(reopened.querySelector('.tb-explain-box').textContent, /z poprzedniej próby/);
});

test('"Nowa wersja": a new version and the same text again both close with the answer', async () => {
  closeAll();
  const sent = [];
  const done = [];
  const win = openSchemaVersion({
    instanceId: 'i',
    subject: wizyta,
    latestText: V1,
    draftText: null,
    register: async (r) => { sent.push(r); return { version: 3, schemaRefId: 1, deduplicated: false }; },
    describeError: String,
    onAdded: (a) => done.push(a),
    onDraft: () => assert.fail('added, no draft kept'),
  });
  type(win.querySelector('[data-role="text"]'), V2);
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual(sent, [{ instanceId: 'i', subject: 'wizyta', schemaType: 'json_schema', schemaText: V2 }]);
  assert.deepEqual(done, [{ version: 3, deduplicated: false }]);
  await new Promise((r) => setTimeout(r, 300));
});

test('"Zmień zgodność": four choices with what each checks, the change alone is sent', async () => {
  closeAll();
  const sent = [];
  const saved = [];
  const win = openSchemaCompat({ instanceId: 'i', subject: wizyta, setCompatibility: async (r) => { sent.push(r); }, describeError: String, onSaved: (c) => saved.push(c) });
  assert.deepEqual([...win.querySelectorAll('.tb-rc-name')].map((n) => n.textContent), ['Bez sprawdzania', 'Nowe programy przeczytają stare wiadomości', 'Stare programy przeczytają nowe wiadomości', 'W obie strony']);
  assert.ok(win.querySelector('[data-act="save"]').hasAttribute('disabled'));
  pick(win.querySelector('[data-role="compat"]'), 'full');
  assert.equal(norm(win.querySelector('[data-role="impact"]').textContent), 'Co się stanie po zapisaniu: każda kolejna wersja wzoru wizyta będzie sprawdzana warunkiem „w obie strony”. Istniejące wersje się nie zmieniają.');
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual(sent, [{ instanceId: 'i', subject: 'wizyta', compatibility: 'full' }]);
  assert.deepEqual(saved, ['full']);
});

test('"Wycofaj" the pattern and one version: what stays, then the request', async () => {
  closeAll();
  const sent = [];
  let done = 0;
  const versions = [{ version: 1, deprecatedAtMs: 5 }, { version: 2, deprecatedAtMs: null }];
  const whole = openSchemaDeprecate({ instanceId: 'i', subject: wizyta, versions, remove: async (r) => { sent.push(r); }, describeError: String, onDone: () => { done += 1; } });
  assert.match(norm(whole.querySelector('.tb-explain-box').textContent), /Wzór wizyta i 1 jego niewycofana wersja zostaną oznaczone jako wycofane\. Nadal będzie można je przeczytać i pobrać\./);
  assert.match(norm(whole.querySelector('[data-role="impact"]').textContent), /Topik wizyty nadal będzie sprawdzać wiadomości według ostatniej wersji/);
  assert.match(norm(whole.querySelector('.tb-foot-note').textContent), /dzienniku audytu/);
  whole.querySelector('[data-act="go"]').click();
  await tick();
  closeAll();
  const one = openVersionDeprecate({ instanceId: 'i', subject: wizyta, version: 2, versions, remove: async (r) => { sent.push(r); }, describeError: String, onDone: () => { done += 1; } });
  assert.equal(norm((one.shadowRoot || one).querySelector('.tf-window-title').textContent), 'Wycofaj wersję 2 wzoru wizyta');
  assert.match(norm(one.querySelector('[data-role="impact"]').textContent), /to ostatnia niewycofana wersja; wycofanie nie wyłącza sprawdzania, więc topik wizyty nadal będzie sprawdzać wiadomości według wersji 2/);
  one.querySelector('[data-act="go"]').click();
  await tick();
  assert.deepEqual(sent, [
    { instanceId: 'i', subject: 'wizyta', deprecateOnly: true },
    { instanceId: 'i', subject: 'wizyta', deprecateOnly: true, version: 2 },
  ]);
  assert.equal(done, 2);
});

test('"Usuń…": locked until the name is retyped; a topic that took the pattern meanwhile is named', async () => {
  closeAll();
  const unused = { subject: 'wizyta-2025', schemaType: 'json_schema', compatibility: 'backward', latestVersion: 7, deprecatedAtMs: 1, usedByTopics: [] };
  const win = openSchemaDelete({
    instanceId: 'i',
    subject: unused,
    remove: async () => { throw new Error("protocol error BadRequest: bus.invalid_argument: schema subject 'wizyta-2025' is bound by topics: wizyty"); },
    describeError: () => 'Odmowa serwera.',
    onDeleted: () => assert.fail('refused'),
  });
  assert.match(norm(win.querySelector('.tb-danger-box').textContent), /Tego nie da się cofnąć\. Wzór wizyta-2025 \(JSON Schema\) i 7 jego wersji znikną\./);
  const confirm = win.querySelector('[data-action="confirm"]');
  assert.ok(confirm.hasAttribute('disabled'));
  type(win.querySelector('#retype-input'), 'wizyta-2025');
  assert.equal(confirm.hasAttribute('disabled'), false);
  win.dispatchEvent(new CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
  await tick();
  assert.equal(norm(win.querySelector('#retype-error').textContent), 'Nie usunięto: używa go topik wizyty. Najpierw wybierz w nim inny wzór.');

  closeAll();
  const sent = [];
  let deleted = 0;
  const ok = openSchemaDelete({ instanceId: 'i', subject: unused, remove: async (r) => { sent.push(r); }, describeError: String, onDeleted: () => { deleted += 1; } });
  type(ok.querySelector('#retype-input'), 'wizyta-2025');
  ok.dispatchEvent(new CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
  await tick();
  assert.deepEqual(sent, [{ instanceId: 'i', subject: 'wizyta-2025', deprecateOnly: false }]);
  assert.equal(deleted, 1);
});
