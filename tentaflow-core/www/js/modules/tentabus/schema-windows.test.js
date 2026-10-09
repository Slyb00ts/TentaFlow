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
if (typeof globalThis.DOMParser === 'undefined' && window.DOMParser) globalThis.DOMParser = window.DOMParser;

const {
  subjectNameProblem, schemaTextProblem, buildRegisterRequest, buildDeleteRequest, jsonSchemaChanges, parseIncompatible,
  profileChanges, xsdChanges, hl7DropOffer, dropRequired, HL7_PROFILE_EXAMPLE,
  addedNotice, incompatibilityReason, refusalHtml, boundTopics, effectiveVersion, versionImpact, versionDeprecateImpact, subjectDeprecateImpact,
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
  const narrowed = (reader) => `property 'termin' type is narrowed: writer allows Some({"number"}), reader allows ${reader}`;
  const intTermin = JSON.stringify({ ...JSON.parse(V1), properties: { ...JSON.parse(V1).properties, termin: { type: 'integer' } } });
  const numTermin = JSON.stringify({ ...JSON.parse(V1), properties: { ...JSON.parse(V1).properties, termin: { type: 'number' } } });
  assert.match(incompatibilityReason({ mode: 'backward', detail: narrowed('Some({"integer"})'), newText: intTermin }).reason, /zawęża rodzaj wartości pola „termin”/);
  assert.match(incompatibilityReason({ mode: 'forward', detail: narrowed('Some({"integer"})'), newText: numTermin }).reason, /poszerza rodzaj wartości pola „termin”/, 'under "forward" the old version is the reader');
  assert.match(incompatibilityReason({ mode: 'full', detail: narrowed('Some({"integer"})'), newText: intTermin }).reason, /zawęża/, 'the reader types are the new version: it narrowed');
  assert.match(incompatibilityReason({ mode: 'full', detail: narrowed('Some({"integer"})'), newText: numTermin }).reason, /poszerza/, 'the reader types are the old version: it widened');
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
  assert.match(norm(invalid.textContent), /Serwer nie przyjął tekstu jako wzoru w formacie JSON Schema/);
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
  const names = [];
  const sent = [];
  const added = [];
  const win = openSchemaAdd({
    instanceId: 'tentabus-1a2b3c4d',
    schemaTypes: ['json_schema', 'avro', 'protobuf', 'thrift'],
    existingNames: () => names,
    register: async (request) => { sent.push(request); return { version: 1, schemaRefId: 7, deduplicated: false }; },
    describeError: () => 'Odmowa serwera.',
    onAdded: (a) => added.push(a),
  });
  assert.equal(win.getAttribute('modal'), '');
  assert.deepEqual([...win.querySelectorAll('tf-choice-card')].map((c) => c.getAttribute('heading')), ['JSON Schema', 'Avro', 'Protobuf', 'Thrift']);
  assert.equal(win.querySelector('[data-role="format"]').getAttribute('value'), 'json_schema');
  const compat = win.querySelector('[data-role="compat"]');
  assert.equal(compat.value, 'backward');
  assert.equal(compat.closest('.form-grid-2'), null, 'the long option names get the whole row');
  assert.equal(compat.getAttribute('hint'), 'Nowa wersja nie może wymagać niczego, czego stare wiadomości nie mają. Sprawdzane, gdy ktoś doda kolejną wersję.');
  const save = win.querySelector('[data-act="save"]');
  assert.equal(norm(save.textContent), 'Dodaj wzór');
  assert.equal(win.querySelector('[data-role="impact"]').hidden, true, 'nothing is marked yet, so nothing to fix is said');
  // The list read when the window opened arrives after it: the name is taken now.
  names.push('wizyta');
  const name = win.querySelector('[data-role="name"]');
  type(name, 'wizyta');
  assert.equal(win.querySelector('[data-role="impact"]').hidden, false);
  assert.match(win.querySelector('[data-role="impact"]').textContent, /Popraw zaznaczone pole/);
  assert.match(name.getAttribute('error'), /już jest/);
  type(win.querySelector('[data-role="text"]'), '{"type": ');
  assert.match(win.querySelector('[data-role="text"]').getAttribute('error'), /To nie jest poprawny JSON/);
  assert.ok(save.hasAttribute('disabled'));
  type(name, 'skierowanie');
  type(win.querySelector('[data-role="text"]'), V1);
  pick(win.querySelector('[data-role="compat"]'), 'full');
  assert.equal(win.querySelector('[data-role="compat"]').getAttribute('hint'), 'Oba warunki naraz. Sprawdzane, gdy ktoś doda kolejną wersję.');
  assert.equal(save.hasAttribute('disabled'), false);
  assert.equal(norm(win.querySelector('[data-role="impact"]').textContent), 'Co się stanie po dodaniu: powstanie wzór skierowanie (JSON Schema), wersja 1. Żaden topik go jeszcze nie używa — wybierzesz go w ustawieniach topiku.');
  save.click();
  await tick();
  assert.deepEqual(sent, [{ instanceId: 'tentabus-1a2b3c4d', subject: 'skierowanie', schemaType: 'json_schema', schemaText: V1, compatibility: 'full' }]);
  assert.deepEqual(added, [{ subject: 'skierowanie', schemaType: 'json_schema', version: 1, deduplicated: false }]);
});

test('"Dodaj wzór": a file loads into the text; the server\'s refusal stays in the window and clears on the next edit', async () => {
  closeAll();
  const win = openSchemaAdd({
    instanceId: 'i',
    schemaTypes: ['json_schema'],
    existingNames: () => [],
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
  assert.ok(win.querySelector('[data-act="save"]').hasAttribute('disabled'), 'the refused text is not sent again as it is');
  type(win.querySelector('[data-role="text"]'), '{"type": "string"}');
  assert.equal(error.hidden, true, 'an edited text is no longer the refused one');
  assert.equal(win.querySelector('[data-act="save"]').hasAttribute('disabled'), false);
  const big = { size: 256 * 1024 + 1, text: async () => 'x' };
  win.querySelector('[data-role="file"]').dispatchEvent(new CustomEvent('change', { detail: { files: [big] } }));
  await tick();
  assert.match(win.querySelector('[data-role="text"]').getAttribute('error'), /najwyżej 256 KB/);
  assert.ok(win.querySelector('[data-act="save"]').hasAttribute('disabled'), 'a file that did not load leaves nothing to send');
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
  assert.match(reopened.querySelector('.tb-explain-box').textContent, /To tekst z poprzedniego otwarcia tego okna — nie został dodany jako wersja\./);
});

test('"Nowa wersja": the question before closing a changed draft says the text is kept, because it is', async () => {
  closeAll();
  const drafts = [];
  const win = openSchemaVersion({ instanceId: 'i', subject: wizyta, latestText: V1, draftText: null, register: async () => ({ version: 3, deduplicated: false }), describeError: String, onAdded: () => {}, onDraft: (t) => drafts.push(t) });
  const next = JSON.stringify({ ...JSON.parse(V1), title: 'inna' });
  type(win.querySelector('[data-role="text"]'), next);
  win.close();
  await new Promise((r) => setTimeout(r, 300));
  assert.equal(win.isConnected, true, 'the first close only asks');
  const asked = norm(win.querySelector('[data-role="discard"]').textContent);
  assert.match(asked, /Ta wersja nie jest jeszcze dodana\. Tekst zostanie zachowany i wróci, gdy otworzysz to okno ponownie\./);
  assert.doesNotMatch(asked, /porzuc/, 'nothing is discarded');
  win.close();
  await new Promise((r) => setTimeout(r, 300));
  assert.equal(win.isConnected, false);
  assert.deepEqual(drafts, [next], 'and it is kept, as the question said');
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
  assert.match(norm(win.querySelector('.tb-danger-box').textContent), /Tego nie da się cofnąć\. Wzór wizyta-2025 \(JSON Schema\) i wszystkie jego wersje znikną\./);
  const confirm = win.querySelector('[data-action="confirm"]');
  assert.ok(confirm.hasAttribute('disabled'));
  type(win.querySelector('#retype-input'), 'wizyta-2025');
  assert.equal(confirm.hasAttribute('disabled'), false);
  win.dispatchEvent(new CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
  await tick();
  assert.equal(norm(win.querySelector('#retype-error').textContent), 'Nie usunięto: używa go topik wizyty. Najpierw wybierz inny wzór w ustawieniach tego topiku.');

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

test('the note after "Dodaj wzór" says what the server did: a new pattern, a version of one taken meanwhile, or nothing', () => {
  assert.deepEqual(addedNotice({ subject: 'skierowanie', schemaType: 'json_schema', version: 1, deduplicated: false }),
    { tone: 'success', title: 'Dodano wzór skierowanie', text: 'JSON Schema, wersja 1. Wybierzesz go w ustawieniach topiku z treścią JSON.' });
  const version = addedNotice({ subject: 'wizyta', schemaType: 'json_schema', version: 4, deduplicated: false });
  assert.equal(version.tone, 'warning');
  assert.equal(version.title, 'Wzór wizyta już istniał');
  assert.match(version.text, /jako wersja 4/);
  assert.match(addedNotice({ subject: 'wizyta', schemaType: 'json_schema', version: 3, deduplicated: true }).text, /\(wersja 3\) — nic się nie zmieniło/);
});

test('Escape on a changed draft asks once; "Anuluj" asks too', async () => {
  closeAll();
  const open = () => openSchemaAdd({ instanceId: 'i', schemaTypes: ['json_schema'], existingNames: () => [], register: async () => ({}), describeError: String, onAdded: () => {} });
  const win = open();
  win.close();
  await new Promise((r) => setTimeout(r, 300));
  assert.equal(win.isConnected, false, 'an untouched window just closes');

  const dirty = open();
  type(dirty.querySelector('[data-role="name"]'), 'skierowanie');
  dirty.close();
  await new Promise((r) => setTimeout(r, 300));
  assert.equal(dirty.isConnected, true, 'the first close only asks');
  assert.match(dirty.querySelector('[data-role="discard"]').textContent, /Zamknij okno jeszcze raz/);
  dirty.close();
  await new Promise((r) => setTimeout(r, 300));
  assert.equal(dirty.isConnected, false, 'the second close drops the draft');

  const cancelled = open();
  type(cancelled.querySelector('[data-role="name"]'), 'skierowanie');
  cancelled.querySelector('[data-act="cancel"]').click();
  await new Promise((r) => setTimeout(r, 300));
  assert.equal(cancelled.isConnected, true, '"Anuluj" asks like the close button');
  cancelled.querySelector('[data-act="cancel"]').click();
  await new Promise((r) => setTimeout(r, 300));
  assert.equal(cancelled.isConnected, false);
});

test('"Dodaj wzór" hands on what the server did when the name was taken meanwhile', async () => {
  closeAll();
  const added = [];
  const win = openSchemaAdd({
    instanceId: 'i', schemaTypes: ['json_schema'], existingNames: () => [],
    register: async () => ({ version: 4, schemaRefId: 1, deduplicated: true }), describeError: String, onAdded: (a) => added.push(a),
  });
  type(win.querySelector('[data-role="name"]'), 'wizyta');
  type(win.querySelector('[data-role="text"]'), V1);
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual(added, [{ subject: 'wizyta', schemaType: 'json_schema', version: 4, deduplicated: true }]);
});

// ---------------------------------------------------------------------------
// HL7 v2 profiles and XSD (F4 B4/B5)
// ---------------------------------------------------------------------------

const PROFILE_V3 = JSON.stringify({ required_segments: ['MSH', 'PID', 'OBR', 'OBX'], required_fields: ['PID-3', 'PID-5', 'OBR-4', 'OBX-3', 'OBX-5'] });
const PROFILE_V4 = JSON.stringify({ required_segments: ['MSH', 'PID', 'OBR', 'OBX'], required_fields: ['PID-3', 'PID-5', 'OBR-4', 'OBX-3', 'OBX-5', 'OBX-8'] });
// V4 as the one-click fix leaves it: the field is gone, a changed description stays.
const PROFILE_V4_DESCRIBED = JSON.stringify({ description: 'Wynik badania, wersja robocza', ...JSON.parse(PROFILE_V4) });
const wynik = { subject: 'wynik-badania', schemaType: 'hl7v2_profile', compatibility: 'backward', latestVersion: 3, deprecatedAtMs: null, usedByTopics: ['wyniki-badan'] };
const BACKWARD_OBX8 = 'backward (the new profile requires more than the old one guarantees): fields [OBX-8] are not guaranteed';

test('a profile text must be JSON and an XSD text well-formed XML', () => {
  assert.equal(schemaTextProblem('{"required_segments": ', 'hl7v2_profile'), 'not_json');
  assert.equal(schemaTextProblem(PROFILE_V3, 'hl7v2_profile'), null);
  const xsd = '<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="a" type="xs:string"/></xs:schema>';
  assert.equal(schemaTextProblem(xsd, 'xsd'), null);
  assert.equal(schemaTextProblem(xsd.replace('</xs:schema>', ''), 'xsd'), 'not_xml');
  assert.equal(schemaTextProblem('{"json": true}', 'xsd'), 'not_xml');
});

test('the difference of a new profile from the newest one, in words', () => {
  assert.equal(profileChanges(PROFILE_V3, PROFILE_V3, 3), 'Tekst jest taki sam jak wersja 3.');
  assert.equal(profileChanges(PROFILE_V3, PROFILE_V4, 3), 'Różnica względem wersji 3: nowe, wymagane pole „OBX-8”.');
  const noObr = JSON.stringify({ required_segments: ['MSH', 'PID', 'OBX'], required_fields: ['PID-3', 'PID-5', 'OBX-3', 'OBX-5'] });
  assert.equal(profileChanges(PROFILE_V3, noObr, 3), 'Różnica względem wersji 3: segment „OBR” nie jest już wymagany i pole „OBR-4” nie jest już wymagane.');
  const withNte = JSON.stringify({ required_segments: ['MSH', 'PID', 'OBR', 'OBX', 'NTE'], required_fields: ['PID-3', 'PID-5', 'OBR-4', 'OBX-3', 'OBX-5', 'NTE-3'] });
  assert.equal(profileChanges(PROFILE_V3, withNte, 3), 'Różnica względem wersji 3: nowy wymagany segment „NTE” i nowe, wymagane pole „NTE-3”.');
  assert.match(profileChanges(PROFILE_V3, JSON.stringify({ ...JSON.parse(PROFILE_V3), description: 'x' }), 3), /nie dotyczą/);
  assert.equal(profileChanges(PROFILE_V3, 'not json', 3), null);
});

test('the profile checker\'s sentences have plain words, in both directions', () => {
  const backward = incompatibilityReason({ mode: 'backward', detail: BACKWARD_OBX8, newText: PROFILE_V4 });
  assert.equal(backward.reason, 'nowa wersja wymaga pola „OBX-8”, którego stare wiadomości mogą nie mieć');
  assert.equal(backward.fix, 'Usuń „OBX-8” z pól wymaganych albo zmień zgodność wzoru.');
  const full = incompatibilityReason({ mode: 'full', detail: 'forward (the old profile requires more than the new one guarantees): segments [OBR] are not guaranteed; fields [OBR-4] are not guaranteed', newText: PROFILE_V3 });
  assert.equal(full.reason, 'nowa wersja nie gwarantuje pola „OBR-4”, którego wymagają stare programy');
  const segmentOnly = incompatibilityReason({ mode: 'backward', detail: 'backward (the new profile requires more than the old one guarantees): segments [NTE] are not guaranteed', newText: PROFILE_V4 });
  assert.match(segmentOnly.reason, /„NTE”/);
});

test('what a refused profile can drop, and the text without it', () => {
  assert.deepEqual(hl7DropOffer(BACKWARD_OBX8), { segments: [], fields: ['OBX-8'], items: ['OBX-8'] });
  assert.deepEqual(
    hl7DropOffer('backward (the new profile requires more than the old one guarantees): segments [NTE, OBX] are not guaranteed; fields [OBX-8] are not guaranteed'),
    { segments: ['NTE', 'OBX'], fields: ['OBX-8'], items: ['NTE', 'OBX-8'] },
    'a segment only a dropped field needs is not offered separately',
  );
  assert.equal(hl7DropOffer('forward (the old profile requires more than the new one guarantees): fields [PID-3] are not guaranteed'), null, 'the other direction has no one-click fix');
  assert.equal(hl7DropOffer('something else'), null);
  const next = JSON.parse(dropRequired(PROFILE_V4, { segments: [], fields: ['OBX-8'] }));
  assert.deepEqual(next, JSON.parse(PROFILE_V3));
  assert.deepEqual(Object.keys(next), ['required_segments', 'required_fields']);
  assert.equal(dropRequired('not json', { fields: ['OBX-8'] }), null);
});

test('the refusal box offers the one-click fix only for an HL7 profile and only when asked', () => {
  const err = incompatible('backward', BACKWARD_OBX8);
  const box = (extra) => {
    const el = document.createElement('div');
    el.innerHTML = refusalHtml({ err, title: 'Nie dodano wersji 4.', compatibility: 'backward', schemaType: 'hl7v2_profile', newText: PROFILE_V4, describeError: String, ...extra });
    return el;
  };
  assert.match(norm(box({}).textContent), /^Nie dodano wersji 4\. Ten wzór ma zgodność „nowe programy przeczytają stare wiadomości”, a nowa wersja wymaga pola „OBX-8”, którego stare wiadomości mogą nie mieć\. Usuń „OBX-8” z pól wymaganych albo zmień zgodność wzoru\.$/);
  assert.equal(box({}).querySelector('[data-act="drop-required"]'), null);
  const offered = box({ offerDrop: true }).querySelector('[data-act="drop-required"]');
  assert.equal(norm(offered.textContent), 'Usuń OBX-8 i dodaj wersję');
  assert.deepEqual(JSON.parse(offered.dataset.drop), { segments: [], fields: ['OBX-8'] });
  assert.equal(box({ offerDrop: true, schemaType: 'json_schema' }).querySelector('[data-act="drop-required"]'), null);
});

test('"Nowa wersja" of an HL7 profile: "Usuń OBX-8 i dodaj wersję" removes the field and adds the version in one click', async () => {
  closeAll();
  const sent = [];
  const added = [];
  const win = openSchemaVersion({
    instanceId: 'i',
    subject: wynik,
    latestText: PROFILE_V3,
    draftText: null,
    register: async (request) => {
      sent.push(request.schemaText);
      if (JSON.parse(request.schemaText).required_fields.includes('OBX-8')) throw incompatible('backward', BACKWARD_OBX8);
      return { version: 4, deduplicated: false };
    },
    describeError: String,
    onAdded: (a) => added.push(a),
    onDraft: () => {},
  });
  const text = win.querySelector('[data-role="text"]');
  type(text, PROFILE_V4_DESCRIBED);
  assert.equal(win.querySelector('[data-role="diff"]').textContent, 'Różnica względem wersji 3: nowe, wymagane pole „OBX-8”.');
  win.querySelector('[data-act="save"]').click();
  await tick();
  const button = win.querySelector('[data-act="drop-required"]');
  assert.ok(button, 'the refusal offers the fix');
  assert.equal(norm(button.textContent), 'Usuń OBX-8 i dodaj wersję');
  assert.match(norm(win.querySelector('[data-role="error"]').textContent), /Opis wzoru zostanie bez zmian — popraw go, jeśli wspominał usunięte pola\./, 'the description is kept, and the window says so before the click');
  button.click();
  await tick();
  assert.equal(sent.length, 2);
  assert.deepEqual(JSON.parse(sent[1]), JSON.parse(PROFILE_V4_DESCRIBED.replace('"OBX-5","OBX-8"', '"OBX-5"')), 'the field is gone from the text that was sent'
);
  assert.deepEqual(added, [{ version: 4, deduplicated: false }]);
  closeAll();
});

test('"Dodaj wzór" offers an XSD and an HL7 profile when the server validates them, and checks each text for its own syntax', async () => {
  closeAll();
  const sent = [];
  const win = openSchemaAdd({
    instanceId: 'i',
    schemaTypes: ['json_schema', 'xsd', 'hl7v2_profile'],
    existingNames: () => [],
    register: async (request) => { sent.push(request); return { version: 1, deduplicated: false }; },
    describeError: String,
    onAdded: () => {},
  });
  const cards = [...win.querySelectorAll('tf-choice-card')];
  assert.deepEqual(cards.map((c) => c.getAttribute('heading')), ['JSON Schema', 'XSD', 'profil HL7 v2']);
  assert.deepEqual(cards.map((c) => c.getAttribute('description')), ['dane JSON', 'dokumenty XML', 'wiadomości HL7 v2']);
  type(win.querySelector('[data-role="name"]'), 'wynik-badania');
  const format = win.querySelector('[data-role="format"]');
  const text = win.querySelector('[data-role="text"]');
  pick(format, 'hl7v2_profile');
  type(text, '<xs:schema/>');
  assert.match(text.getAttribute('error'), /To nie jest poprawny JSON/);
  type(text, PROFILE_V3);
  assert.equal(text.hasAttribute('error'), false);
  pick(format, 'xsd');
  assert.match(text.getAttribute('error'), /To nie jest poprawny dokument XML/, 'switching the format re-checks the text');
  const xsd = '<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="a" type="xs:string"/></xs:schema>';
  type(text, xsd);
  assert.equal(text.hasAttribute('error'), false);
  const save = win.querySelector('[data-act="save"]');
  assert.equal(save.hasAttribute('disabled'), false);
  assert.match(norm(win.querySelector('[data-role="impact"]').textContent), /powstanie wzór wynik-badania \(XSD\), wersja 1/);
  save.click();
  await tick();
  assert.equal(sent[0].schemaType, 'xsd');
  assert.equal(sent[0].schemaText, xsd);
  closeAll();
});

test('a refused HL7 profile names segments and fields apart, and the button lists them without a double "i"', () => {
  const detail = 'backward (the new profile requires more than the old one guarantees): segments [NK1] are not guaranteed; fields [PV1-7] are not guaranteed';
  const reason = incompatibilityReason({ mode: 'backward', detail, newText: PROFILE_V4 });
  assert.equal(reason.reason, 'nowa wersja wymaga segmentu „NK1” i pola „PV1-7”, których stare wiadomości mogą nie mieć');
  assert.equal(reason.fix, 'Usuń „NK1” z wymaganych segmentów i „PV1-7” z pól wymaganych albo zmień zgodność wzoru.');
  assert.deepEqual(hl7DropOffer(detail).items, ['NK1', 'PV1-7']);
  const el = document.createElement('div');
  el.innerHTML = refusalHtml({ err: incompatible('backward', detail), title: 'Nie dodano wersji 4.', compatibility: 'backward', schemaType: 'hl7v2_profile', newText: JSON.stringify({ required_segments: ['PID', 'NK1'], required_fields: ['PV1-7'] }), describeError: String, offerDrop: true });
  assert.equal(norm(el.querySelector('[data-act="drop-required"]').textContent), 'Usuń NK1, PV1-7 i dodaj wersję');
  // Several fields use the plural forms.
  const many = incompatibilityReason({ mode: 'backward', detail: 'backward (the new profile requires more than the old one guarantees): fields [PV1-7, PV1-8] are not guaranteed', newText: PROFILE_V4 });
  assert.equal(many.reason, 'nowa wersja wymaga pól „PV1-7” i „PV1-8”, których stare wiadomości mogą nie mieć');
});

test('when removing what the old messages lack brings back the newest version, no button is offered and the window says why', () => {
  const err = incompatible('backward', BACKWARD_OBX8);
  const el = document.createElement('div');
  el.innerHTML = refusalHtml({ err, title: 'Nie dodano wersji 4.', compatibility: 'backward', schemaType: 'hl7v2_profile', newText: PROFILE_V4, describeError: String, offerDrop: true, latest: { version: 3, text: PROFILE_V3 } });
  assert.equal(el.querySelector('[data-act="drop-required"]'), null, 'a button that adds nothing is not offered');
  assert.match(norm(el.textContent), /Po usunięciu OBX-8 nowa wersja niczym nie różniłaby się od wersji 3, więc nie ma czego dodawać\. Zmień zgodność wzoru albo wprowadź inne zmiany\.$/);
});

test('an XSD new version refusal names the element that is newly required', () => {
  const backward = 'backward: /faktura: the new schema requires element \'termin\' where the old schema may have child element \'pozycja\'';
  const reason = incompatibilityReason({ mode: 'backward', detail: backward, newText: '<xs:schema/>' });
  assert.equal(reason.reason, 'nowa wersja wymaga elementu „termin”, którego stare wiadomości mogą nie mieć');
  assert.equal(reason.fix, 'Dodaj minOccurs="0" do elementu „termin” albo zmień zgodność wzoru.');
  const early = incompatibilityReason({ mode: 'backward', detail: "backward: /faktura: the old schema accepts a sequence of child elements that ends where the new schema still requires element 'termin'", newText: '' });
  assert.match(early.reason, /wymaga elementu „termin”/);
  const several = incompatibilityReason({ mode: 'backward', detail: "backward: /faktura: the new schema requires one of the elements 'x', 'y' where the old schema may have child element 'z'", newText: '' });
  assert.equal(several.reason, 'nowa wersja wymaga elementów „x” i „y”, których stare wiadomości mogą nie mieć');
  const forward = incompatibilityReason({ mode: 'forward', detail: "forward: /faktura: the old schema requires element 'termin' where the new schema may have child element 'pozycja'", newText: '' });
  assert.equal(forward.reason, 'nowa wersja nie gwarantuje elementu „termin”, którego wymagają stare programy');
  const el = document.createElement('div');
  el.innerHTML = refusalHtml({ err: incompatible('backward', backward), title: 'Nie dodano wersji 3.', compatibility: 'backward', schemaType: 'xsd', newText: '', describeError: String });
  assert.equal(norm(el.textContent), 'Nie dodano wersji 3. Ten wzór ma zgodność „nowe programy przeczytają stare wiadomości”, a nowa wersja wymaga elementu „termin”, którego stare wiadomości mogą nie mieć. Dodaj minOccurs="0" do elementu „termin” albo zmień zgodność wzoru.');
  assert.equal(el.querySelector('details'), null);
});

// The exact messages of the Rust parsers (hl7v2_profile.rs, payload_format/hl7v2.rs, xsd.rs);
// `schema_error_phrases_are_stable` there pins the same strings.
const refused = (schemaType, serverText) => {
  const el = document.createElement('div');
  el.innerHTML = refusalHtml({ err: new Error(`protocol error BadRequest: bus.invalid_argument: schema: invalid schema: ${serverText}`), title: 'Nie dodano wzoru x.', compatibility: 'backward', schemaType, newText: '', describeError: String });
  return { text: norm(el.querySelector('div').textContent), technical: el.querySelector('details pre')?.textContent };
};

test('a refused HL7 profile text says in plain words what is wrong, the server\'s sentence folded below', () => {
  const cases = [
    ["hl7: 'MSH-1' is the message's own field-separator/encoding-characters definition and can never be filtered", /„MSH-1” opisuje same znaki podziału wiadomości, więc nie może być wymagane\. Usuń „MSH-1” z pól wymaganych\./],
    ["hl7: 'pid5' is not SEGMENT-N shaped (e.g. 'PID-5')", /„pid5” to nie adres pola\. Adres ma postać SEGMENT-numer, np\. PID-5\./],
    ["hl7: '0' is not a valid positive field number", /Numer pola w adresie musi być liczbą od 1 do 999/],
    ['hl7: field number \'1000\' exceeds the supported maximum of 999', /Numer pola w adresie musi być liczbą od 1 do 999/],
    ["hl7: 'PACJENT' is not a valid 3-character segment id", /„PACJENT” to nie nazwa segmentu\. Nazwa segmentu ma trzy znaki/],
    ["'pid' is not a valid 3-character segment id", /„pid” to nie nazwa segmentu/],
    ['not a valid HL7 v2 profile: unknown field `required`, expected one of `description`, `required_segments`, `required_fields` at line 1 column 11', /W profilu nie ma klucza „required”\. Dozwolone są: description, required_segments i required_fields\./],
    ["required_fields lists 'PID-3' more than once", /„PID-3” jest na liście wymaganych pól więcej niż raz\. Zostaw jedno wystąpienie\./],
    ["required_segments lists 'PID' more than once", /na liście wymaganych segmentów/],
    ['required_fields lists 513 entries, exceeding the 512-entry limit', /Lista ma za dużo pozycji — najwyżej 512\./],
    ['description exceeds 1000 characters', /Opis jest za długi/],
    ['not a valid HL7 v2 profile: invalid type: string "x", expected a sequence at line 1 column 20', /Tekst nie jest profilem HL7 v2\./],
  ];
  for (const [server, expected] of cases) {
    const got = refused('hl7v2_profile', server);
    assert.match(got.text, expected, server);
    assert.match(got.text, /^Nie dodano wzoru x\. /);
    assert.doesNotMatch(got.text, /nie jest poprawny wzór/, 'a refusal is not called an invalid pattern');
    assert.ok(got.technical.includes(server), 'the server\'s sentence stays available');
  }
});

test('a refused XSD says which construct or type is not supported, and that the XSD itself may be fine', () => {
  const cases = [
    ['xs:include: schema composition is not supported; put every declaration in one document', /Ten XSD jest poprawny, ale konstrukcja xs:include nie jest obsługiwana\. Wklej wszystkie deklaracje do jednego pliku\./],
    ['xs:import: schema composition is not supported; put every declaration in one document', /xs:import nie jest obsługiwana/],
    ['xs:any: wildcards are not supported; declare the allowed elements and attributes explicitly', /konstrukcja xs:any nie jest obsługiwana\. Wypisz jawnie elementy i atrybuty/],
    ['xs:group: named groups are not supported; declare the content inline', /konstrukcja xs:group nie jest obsługiwana\. Zapisz zawartość grupy bezpośrednio w elemencie\./],
    ['xs:key: identity constraints are not supported', /xs:key nie jest obsługiwana\. Usuń ograniczenia unikalności i kluczy\./],
    ['xs:complexContent: type derivation (extension/restriction of complex types) is not supported', /xs:complexContent nie jest obsługiwana/],
    ['xs:union: list and union simple types are not supported', /xs:union nie jest obsługiwana/],
    ['xs:attribute: global attributes are not supported; declare attributes inside a complexType', /Zadeklaruj atrybut wewnątrz elementu/],
    ['xs:notation: notations are not supported', /xs:notation nie jest obsługiwana/],
    ['xs:foo: this construct is not part of the supported XSD subset', /xs:foo nie jest obsługiwana\. Usuń ją albo zastąp prostszą\./],
    ['built-in type xs:float is not supported (supported: string, int, integer, decimal, boolean, date, dateTime)', /Typ xs:float nie jest obsługiwany\. Obsługiwane są: string, int, integer, decimal, boolean, date i dateTime\. Zamiast niego użyj xs:decimal\.$/],
    ['built-in type xs:gYear is not supported (supported: string, int, integer, decimal, boolean, date, dateTime)', /Typ xs:gYear nie jest obsługiwany\. Obsługiwane są: .* dateTime\.$/],
    ['facet xs:totalDigits is not supported (supported: minLength, maxLength, pattern, enumeration)', /Ograniczenie xs:totalDigits nie jest obsługiwane/],
    ["type 'T': an enumeration value is longer than 1024 bytes", /Jedna z dozwolonych wartości jest za długa \(najwyżej 1024 bajty\)\./],
    ['the enumeration values of the schema hold more than 128 KiB in total', /Listy dozwolonych wartości są razem za duże \(najwyżej 128 KB\)\./],
    ['the schema lists more than 4096 enumeration values', /za dużo pozycji \(najwyżej 4096\)/],
    ["type 'T': pattern exceeds 512 characters", /Jeden ze wzorców pattern jest za długi \(najwyżej 512 znaków\)\./],
    ['the schema uses more than 256 patterns', /za dużo ograniczeń typu pattern \(najwyżej 256\)/],
    ['the patterns of the schema need more than 8192 KiB of memory in total', /potrzebują ponad 8192 KB pamięci/],
    ["type 'T': pattern is not a valid or supported regular expression: the compiled pattern is too large", /zbyt rozbudowany, np\. powtarza wiele razy klasę znaków/],
    ['a type declares more than 1024 attributes', /ma za dużo atrybutów \(najwyżej 1024\)/],
    ['the schema exceeds the compile work limit; simplify it', /Ten wzór jest zbyt złożony, żeby go przyjąć\./],
    ['the schema declares no global element; at least one document root is required', /nie deklaruje żadnego elementu głównego/],
    ['mixed content (mixed="true") is not supported', /Treść mieszana/],
    ['xs:element ref= is not supported; declare it in place', /Odwołania ref= nie są obsługiwane/],
  ];
  for (const [server, expected] of cases) {
    const got = refused('xsd', server);
    assert.match(got.text, expected, server);
    assert.ok(got.technical.includes(server));
  }
  // A parser message nobody mapped keeps the careful generic sentence.
  const unknown = refused('xsd', 'something else entirely');
  assert.match(unknown.text, /Serwer nie przyjął tekstu jako wzoru w formacie XSD\./);
  assert.equal(unknown.technical, 'invalid schema: something else entirely');
});

test('the difference of a new XSD from the newest version, in words', () => {
  const xsd = (inner, attrs = '') => `<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="faktura"><xs:complexType><xs:sequence>${inner}</xs:sequence></xs:complexType></xs:element></xs:schema>${attrs}`;
  const base = xsd('<xs:element name="numer" type="xs:string"/><xs:element name="nabywca" type="xs:string"/>');
  assert.equal(xsdChanges(base, base, 2), 'Tekst jest taki sam jak wersja 2.');
  assert.equal(
    xsdChanges(base, xsd('<xs:element name="numer" type="xs:string"/><xs:element name="nabywca" type="xs:string"/><xs:element name="termin" type="xs:date" minOccurs="0"/>'), 2),
    'Różnica względem wersji 2: nowy, nieobowiązkowy element „termin”.',
  );
  assert.equal(
    xsdChanges(base, xsd('<xs:element name="numer" type="xs:string"/><xs:element name="nabywca" type="xs:string"/><xs:element name="termin" type="xs:date"/>'), 2),
    'Różnica względem wersji 2: nowy, wymagany element „termin”.',
  );
  assert.equal(
    xsdChanges(base, xsd('<xs:element name="numer" type="xs:string" minOccurs="0"/>'), 2),
    'Różnica względem wersji 2: usunięty element „nabywca” i element „numer” nie jest już wymagany.',
  );
  assert.equal(xsdChanges(base, xsd('<xs:choice><xs:element name="numer" type="xs:string"/></xs:choice><xs:element name="nabywca" type="xs:string"/>'), 2), 'Różnica względem wersji 2: element „numer” nie jest już wymagany.', 'an alternative of a choice may be left out');
  assert.equal(xsdChanges(base, '<not xml', 2), null);
});

test('"Nowa wersja" of an XSD says the difference like the other formats', async () => {
  closeAll();
  const xsd = (inner) => `<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="faktura"><xs:complexType><xs:sequence>${inner}</xs:sequence></xs:complexType></xs:element></xs:schema>`;
  const v2 = xsd('<xs:element name="numer" type="xs:string"/>');
  const win = openSchemaVersion({
    instanceId: 'i', subject: { ...wizyta, subject: 'faktura', schemaType: 'xsd', latestVersion: 2 }, latestText: v2, draftText: null,
    register: async () => ({ version: 3, deduplicated: false }), describeError: String, onAdded: () => {}, onDraft: () => {},
  });
  type(win.querySelector('[data-role="text"]'), xsd('<xs:element name="numer" type="xs:string"/><xs:element name="termin" type="xs:date" minOccurs="0"/>'));
  assert.equal(win.querySelector('[data-role="diff"]').textContent, 'Różnica względem wersji 2: nowy, nieobowiązkowy element „termin”.');
  closeAll();
});

test('"Dodaj wzór" tells how to write an HL7 profile and which XSD work, following the chosen format', () => {
  closeAll();
  const win = openSchemaAdd({
    instanceId: 'i', schemaTypes: ['json_schema', 'xsd', 'hl7v2_profile'], existingNames: () => [],
    register: async () => ({ version: 1 }), describeError: String, onAdded: () => {},
  });
  const text = win.querySelector('[data-role="text"]');
  const format = win.querySelector('[data-role="format"]');
  assert.match(text.getAttribute('hint'), /^Możesz też wczytać plik\./);
  pick(format, 'hl7v2_profile');
  assert.ok(text.getAttribute('hint').includes(HL7_PROFILE_EXAMPLE), 'the shape of a profile is shown');
  assert.ok(HL7_PROFILE_EXAMPLE.includes('"required_segments"') && HL7_PROFILE_EXAMPLE.includes('"required_fields"'));
  assert.doesNotThrow(() => JSON.parse(HL7_PROFILE_EXAMPLE), 'the example is itself a valid profile text');
  assert.equal(schemaTextProblem(HL7_PROFILE_EXAMPLE, 'hl7v2_profile'), null);
  pick(format, 'xsd');
  assert.match(text.getAttribute('hint'), /^Zadziała zwykły wzór XSD zapisany w jednym pliku, bez dołączania innych plików/);
  pick(format, 'json_schema');
  assert.match(text.getAttribute('hint'), /^Możesz też wczytać plik\./);
  closeAll();
});

test('a refusal is scrolled into view when the form is taller than the window', async () => {
  closeAll();
  const scrolled = [];
  const original = window.HTMLElement.prototype.scrollIntoView;
  window.HTMLElement.prototype.scrollIntoView = function scrollIntoView(options) { scrolled.push([this.dataset.role, options]); };
  try {
    const win = openSchemaAdd({
      instanceId: 'i', schemaTypes: ['hl7v2_profile'], existingNames: () => [],
      register: async () => { throw new Error("protocol error BadRequest: bus.invalid_argument: schema: invalid schema: hl7: 'pid5' is not SEGMENT-N shaped (e.g. 'PID-5')"); },
      describeError: String, onAdded: () => {},
    });
    type(win.querySelector('[data-role="name"]'), 'profil');
    type(win.querySelector('[data-role="text"]'), '{"required_fields": ["pid5"]}');
    win.querySelector('[data-act="save"]').click();
    await tick();
    assert.deepEqual(scrolled, [['error', { block: 'nearest' }]]);
  } finally {
    window.HTMLElement.prototype.scrollIntoView = original;
    closeAll();
  }
});

test('an HL7 profile must be a JSON object: a list, a text, a number or null is stopped before sending', async () => {
  for (const text of ['["PID-3"]', '[]', 'null', '"PID-3"', '7', ' [ "x" ]']) {
    assert.equal(schemaTextProblem(text, 'hl7v2_profile'), 'not_object', text);
  }
  assert.equal(schemaTextProblem('{}', 'hl7v2_profile'), null);
  assert.equal(schemaTextProblem('["a"]', 'json_schema'), null, 'only the profile is an object by definition');
  closeAll();
  const win = openSchemaAdd({
    instanceId: 'i', schemaTypes: ['json_schema', 'xsd', 'hl7v2_profile'], existingNames: () => [],
    register: async () => ({ version: 1 }), describeError: String, onAdded: () => {},
  });
  pick(win.querySelector('[data-role="format"]'), 'hl7v2_profile');
  type(win.querySelector('[data-role="name"]'), 'tablica');
  const text = win.querySelector('[data-role="text"]');
  type(text, '["PID-3"]');
  assert.match(text.getAttribute('error'), /^Tekst nie jest profilem HL7 v2\. Profil to obiekt JSON/);
  assert.equal(win.querySelector('[data-act="save"]').hasAttribute('disabled'), true, 'nothing is sent');
  closeAll();
});

test('an XSD whose root is not xs:schema of the XSD namespace is stopped, and the server\'s namespace refusals are put in words', () => {
  assert.equal(schemaTextProblem('<schema xmlns="urn:nie-xsd"><element name="a"/></schema>', 'xsd'), 'not_xsd');
  assert.equal(schemaTextProblem('<a/>', 'xsd'), 'not_xsd');
  assert.equal(schemaTextProblem('<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"/>', 'xsd'), null);
  assert.equal(schemaTextProblem('<schema xmlns="http://www.w3.org/2001/XMLSchema"/>', 'xsd'), null);
  const cases = [
    ['the root element must be xs:schema', /Główny element musi być xs:schema z przestrzeni nazw http:\/\/www\.w3\.org\/2001\/XMLSchema\./],
    ["type 'p:x' belongs to namespace 'urn:inne'; types from other namespaces are not supported", /Typy z innej przestrzeni nazw \(urn:inne\) nie są obsługiwane\./],
    ['namespace declarations are only supported on xs:schema', /Przestrzenie nazw można deklarować tylko na elemencie xs:schema/],
    ["namespace prefix 'p' is not declared on xs:schema", /każdy użyty przedrostek musi być tam zadeklarowany/],
    ["type 'p:x' uses a namespace prefix that is not declared on xs:schema", /każdy użyty przedrostek musi być tam zadeklarowany/],
    ["element 'a' is not in the XML Schema namespace", /element spoza przestrzeni nazw XML Schema/],
  ];
  for (const [server, expected] of cases) {
    const got = refused('xsd', server);
    assert.match(got.text, expected, server);
    assert.ok(got.technical.includes(server));
  }
});

test('removing a newly required field together with its segment says so in the button, and the same-as-latest sentence lists with "i"', () => {
  const detail = 'backward (the new profile requires more than the old one guarantees): segments [NK1] are not guaranteed; fields [NK1-2] are not guaranteed';
  const newText = JSON.stringify({ required_segments: ['PID', 'NK1'], required_fields: ['PID-3', 'NK1-2'] });
  const el = document.createElement('div');
  el.innerHTML = refusalHtml({ err: incompatible('backward', detail), title: 'Nie dodano wersji 4.', compatibility: 'backward', schemaType: 'hl7v2_profile', newText, describeError: String, offerDrop: true });
  const button = el.querySelector('[data-act="drop-required"]');
  assert.equal(norm(button.textContent), 'Usuń NK1, NK1-2 i dodaj wersję');
  assert.deepEqual(JSON.parse(button.dataset.drop), { segments: ['NK1'], fields: ['NK1-2'] });
  // A segment only implied by a field was never written by the author: not named.
  const implied = 'backward (the new profile requires more than the old one guarantees): segments [PV1] are not guaranteed; fields [PV1-7] are not guaranteed';
  el.innerHTML = refusalHtml({ err: incompatible('backward', implied), title: 'x', compatibility: 'backward', schemaType: 'hl7v2_profile', newText: JSON.stringify({ required_segments: ['PID'], required_fields: ['PV1-7'] }), describeError: String, offerDrop: true });
  assert.equal(norm(el.querySelector('[data-act="drop-required"]').textContent), 'Usuń PV1-7 i dodaj wersję');
  // Two items read "NK1 i PV1-7" in the sentence that explains why nothing is offered.
  const latest = JSON.stringify({ required_segments: ['PID'], required_fields: ['PID-3'] });
  const two = 'backward (the new profile requires more than the old one guarantees): segments [NK1] are not guaranteed; fields [PV1-7] are not guaranteed';
  el.innerHTML = refusalHtml({ err: incompatible('backward', two), title: 'x', compatibility: 'backward', schemaType: 'hl7v2_profile', newText: JSON.stringify({ required_segments: ['PID', 'NK1'], required_fields: ['PID-3', 'PV1-7'] }), describeError: String, offerDrop: true, latest: { version: 1, text: latest } });
  assert.match(norm(el.textContent), /Po usunięciu NK1 i PV1-7 nowa wersja niczym nie różniłaby się od wersji 1/);
});

test('a comparison that gave up is told as too complex, not as an incompatibility', () => {
  const err = new Error("protocol error BadRequest: bus.schema_compare_too_complex: 'faktura' mode=backward: the schemas are too complex to compare; compatibility cannot be proven");
  const el = document.createElement('div');
  el.innerHTML = refusalHtml({ err, title: 'Nie dodano wersji 3.', compatibility: 'backward', schemaType: 'xsd', newText: '', describeError: String });
  assert.equal(norm(el.textContent), 'Nie dodano wersji 3. Wzory są zbyt złożone do porównania, więc nie da się sprawdzić zgodności nowej wersji z poprzednią. Uprość wzór albo zmień zgodność wzoru.');
});
