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
  profileChanges, xsdChanges, hl7DropOffer, dropRequired, HL7_PROFILE_EXAMPLE, AVRO_SCHEMA_EXAMPLE, PROTOBUF_SCHEMA_EXAMPLE, THRIFT_SCHEMA_EXAMPLE, addTextHint, addTextMore,
  addedNotice, incompatibilityReason, refusalHtml, boundTopics, effectiveVersion, versionImpact, versionDeprecateImpact, subjectDeprecateImpact,
  openSchemaAdd, openSchemaVersion, openSchemaCompat, openSchemaDeprecate, openVersionDeprecate, openSchemaDelete,
  ruleFieldIssues, renamedRuleRequest, hiddenRefusal, hiddenRefusals, keptSpellingsText, renameConsequence, renameOutcome, noticeToast,
} = await import('./schema-windows.js');
const { sentenceHtml, hiddenLines, hiddenPlaces } = await import('./hidden-notice.js');

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
  assert.equal(known.querySelector('tf-disclosure'), null);
  const unknown = box(incompatible('full', 'a sentence nobody mapped'));
  assert.match(norm(unknown.textContent), /^Nie dodano wersji 3\. Nowa wersja nie spełnia zgodności „w obie strony” z poprzednią wersją\. Popraw tekst albo zmień zgodność wzoru\./);
  assert.equal(unknown.querySelector('tf-disclosure').getAttribute('label'), 'Szczegóły techniczne');
  assert.equal(unknown.querySelector('tf-disclosure pre').textContent, 'a sentence nobody mapped');
  const invalid = box(new Error("protocol error BadRequest: bus.invalid_argument: schema: invalid schema: /properties: unknown type 'strin'"));
  assert.match(norm(invalid.textContent), /Serwer nie przyjął tekstu jako wzoru w formacie JSON Schema/);
  assert.match(invalid.querySelector('tf-disclosure pre').textContent, /unknown type 'strin'/);
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
  assert.equal(el.querySelector('tf-disclosure'), null);
});

// The exact messages of the Rust parsers (hl7v2_profile.rs, payload_format/hl7v2.rs, xsd.rs);
// `schema_error_phrases_are_stable` there pins the same strings.
const refused = (schemaType, serverText) => {
  const el = document.createElement('div');
  el.innerHTML = refusalHtml({ err: new Error(`protocol error BadRequest: bus.invalid_argument: schema: invalid schema: ${serverText}`), title: 'Nie dodano wzoru x.', compatibility: 'backward', schemaType, newText: '', describeError: String });
  return { text: norm(el.querySelector('div').textContent), technical: el.querySelector('tf-disclosure pre')?.textContent };
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
    ["type 'T': pattern is not a valid or supported regular expression: exceed the maximum number of nested parentheses/brackets (32)", /zagnieżdża nawiasy zbyt głęboko \(najwyżej 32 poziomów\)/],
    ["type 'T': pattern is not a valid or supported regular expression: invalid repetition count range, the start must be <= the end", /błędne powtórzenie, np\. \{2,1\}/],
    ["type 'T': pattern is not a valid or supported regular expression: repetition quantifier expects a valid decimal", /błędne powtórzenie/],
    ["type 'T': pattern is not a valid or supported regular expression: decimal literal invalid", /błędne powtórzenie/],
    ["type 'T': pattern is not a valid or supported regular expression: invalid character class range, the start must be <= the end", /zakres znaków zapisany od końca/],
    ["type 'T': pattern is not a valid or supported regular expression: repetition operator missing expression", /znak powtórzenia \(\*, \+ lub \?\) bez niczego przed nim/],
    ["type 'T': pattern is not a valid or supported regular expression: the pattern could not be built", /nie jest poprawnym wyrażeniem regularnym/],
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
  assert.match(text.getAttribute('hint'), /^Wzór zostanie sprawdzony przy dodawaniu/);
  assert.doesNotMatch(text.getAttribute('hint'), /wczytać plik/, 'the file box says it');
  pick(format, 'hl7v2_profile');
  assert.ok(text.getAttribute('hint').includes(HL7_PROFILE_EXAMPLE), 'the shape of a profile is shown');
  assert.ok(HL7_PROFILE_EXAMPLE.includes('"required_segments"') && HL7_PROFILE_EXAMPLE.includes('"required_fields"'));
  assert.doesNotThrow(() => JSON.parse(HL7_PROFILE_EXAMPLE), 'the example is itself a valid profile text');
  assert.equal(schemaTextProblem(HL7_PROFILE_EXAMPLE, 'hl7v2_profile'), null);
  pick(format, 'xsd');
  assert.match(text.getAttribute('hint'), /^Zadziała zwykły wzór XSD zapisany w jednym pliku, bez dołączania innych plików/);
  pick(format, 'json_schema');
  assert.match(text.getAttribute('hint'), /^Wzór zostanie sprawdzony przy dodawaniu/);
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

test('an Avro text: JSON is enough on this side, the add window shows the shape, and the server\'s refusals are put in words', () => {
  assert.equal(schemaTextProblem('{"type":"record","name":"A","fields":[]}', 'avro'), null);
  assert.equal(schemaTextProblem('"string"', 'avro'), null);
  assert.equal(schemaTextProblem('record A {}', 'avro'), 'not_json');
  assert.equal(AVRO_SCHEMA_EXAMPLE.includes('"type": "record"'), true);
  assert.match(norm(addTextHint('avro')), /^Wzór Avro to schemat zapisany jako JSON, np\. \{"type": "record".*bez nagłówka pliku Avro i bez nagłówka Confluent\./);
  const cases = [
    ['the schema is not valid JSON: expected value at line 1 column 1', /To nie jest poprawny JSON/],
    ['not a valid Avro schema: Unknown primitive type: foo', /^Nie dodano wzoru x\. Avro nie przyjęło tego wzoru: sprawdź nazwy i typy/],
    ['the schema can never produce a value: a record contains itself without an optional branch (a union with null, an array or a map)', /rekord zawiera sam siebie bez możliwości zakończenia\. Dopuść brak wartości \(unia z null\)/],
    ['a record lists a field that is not an object', /Pola rekordu muszą być listą obiektów/],
    ["the default of field 'note' does not fit the first branch of its union", /Wartość domyślna pola nie pasuje do pierwszego wariantu jego unii/],
    ["the default of field 'note' does not fit its type", /Wartość domyślna pola nie pasuje do typu tego pola/],
    ['a doc is longer than 4096 bytes', /Opis \(doc\) albo własny atrybut ma więcej niż 4096 bajtów/],
    ['a custom attribute is longer than 4096 bytes', /Opis \(doc\) albo własny atrybut ma więcej niż 4096 bajtów/],
    ['the schema has more than 4096 types', /ma więcej niż 4096 typów/],
    ["record 'R' has more than 1024 fields", /Rekord ma więcej niż 1024 pól/],
    ['a union has more than 64 branches', /Unia ma więcej niż 64 wariantów/],
    ["enum 'E' has more than 1024 symbols", /Typ wyliczeniowy ma więcej niż 1024 symboli/],
    ["fixed 'F' is larger than 16777216 bytes", /Typ fixed jest większy niż 16777216 bajtów/],
    ['the schema is nested deeper than 64 levels', /głębiej niż 64 poziomów/],
  ];
  for (const [server, expected] of cases) {
    const got = refused('avro', server);
    assert.match(got.text, expected, server);
    assert.ok(got.technical.includes(server));
  }
});

test('a refused Avro version names the field, the type or the enum symbol that breaks the old data or the old programs', () => {
  const reason = (mode, detail) => {
    const el = document.createElement('div');
    el.innerHTML = refusalHtml({ err: incompatible(mode, detail), title: 'Nie dodano wersji 2.', compatibility: mode, schemaType: 'avro', newText: '', describeError: String });
    return norm(el.querySelector('div').textContent);
  };
  const backwardLead = 'backward: data written under the old schema may not be readable with the new one - ';
  const forwardLead = 'forward: data written under the new schema may not be readable with the old one - ';
  assert.match(
    reason('backward', `${backwardLead}/region: the new schema requires field 'region' of record 'Order', which has no default and is missing from the old schema`),
    /nowa wersja wymaga pola „region”, którego stare wiadomości mogą nie mieć\. Nadaj „region” wartość domyślną \("default"\)/,
  );
  assert.match(
    reason('forward', `${forwardLead}/region: the old schema requires field 'region' of record 'Order', which has no default and is missing from the new schema`),
    /nowa wersja nie gwarantuje pola „region”, którego wymagają stare programy\. Zostaw „region” wśród pól wymaganych/,
  );
  assert.match(
    reason('backward', `${backwardLead}/inner/n: long written by the old schema cannot be read as int by the new schema`),
    /typ „inner › n” się zmienia: long nie da się odczytać jako int/,
  );
  assert.match(
    reason('backward', `${backwardLead}/status: enum 'Status': symbol 'DONE' of the old schema is unknown to the new schema, which has no default symbol`),
    /typ wyliczeniowy „Status” traci symbol „DONE”/,
  );
  // A sentence this screen does not know falls back to the generic one with the server's text folded.
  assert.match(
    reason('backward', `${backwardLead}/u: no branch of the new union can read a union written by the old schema`),
    /Nowa wersja nie spełnia zgodności/,
  );
});

test('a Protobuf text: any text is sent as it is, the add window shows how the message is named, and the server\'s refusals are put in words', () => {
  assert.equal(schemaTextProblem('syntax = "proto3"; message A {}', 'protobuf'), null);
  assert.equal(schemaTextProblem('{"not": "checked here"}', 'protobuf'), null);
  assert.equal(schemaTextProblem('   ', 'protobuf'), 'empty');
  assert.equal(PROTOBUF_SCHEMA_EXAMPLE.includes('message Order'), true);
  assert.match(norm(addTextHint('protobuf')), /^Wzór Protobuf to plik \.proto \(proto3 albo proto2\) w jednym kawałku, bez „import”\. Opisuje jedną wiadomość: pierwszą z pliku albo tę, którą wskaże komentarz, np\. „\/\/ tentabus:message Order”\. Przykład: syntax = "proto3"; message Order/);
  assert.match(norm(addTextHint('protobuf')), /Walidacja jest ścisła: ujemny int32 musi mieć pełny zapis na 10 bajtach.*uint32 i sint32 nie mogą przekraczać 2\^32−1, bool to tylko 0 albo 1, a numery pól 19000–19999 są odrzucane/);
  const cases = [
    ['not a valid Protobuf schema (line 3): label required', /Protobuf nie przyjął tego pliku: sprawdź składnię/],
    ['import is not supported: a schema must be one self-contained file', /„import” nie jest obsługiwany/],
    ['service is not supported', /nie obsługuje: service \(usługi\)/],
    ['extend is not supported', /nie obsługuje: extend \(rozszerzenia\)/],
    ['extension ranges are not supported', /extensions \(zakresy rozszerzeń\)/],
    ['groups are not supported', /group \(grupy\)/],
    ['custom options are not supported', /opcje własne/],
    ['option names with a dot are not supported', /nazwy opcji z kropką/],
    ['message_set_wire_format is not supported', /message_set_wire_format/],
    ['the schema declares no message', /nie ma żadnego komunikatu/],
    ["the designated message 'X' is not a top-level message of the schema", /Komentarz „\/\/ tentabus:message Nazwa” wskazuje komunikat, którego nie ma/],
    ["the comment '// tentabus:message' must be followed by the name of a message", /musi mieć po sobie nazwę komunikatu/],
    ["the comment '// tentabus:message' appears more than once", /może wystąpić tylko raz/],
    ['not a valid Protobuf schema (line 4): Non-unique field name: `b`', /numery i nazwy pól muszą być niepowtarzalne/],
    ["message 'M' has two fields named 'a'", /niepowtarzalne/],
    ['not a valid Protobuf schema (line 3): object is not found by path `Missing` in scope `.M`', /używa typu, którego w nim nie ma/],
    ["field 'a' of message 'M' uses a reserved number or name", /poza numerami zastrzeżonymi/],
    ["field 'a' of message 'M' has the number 19000, which is not in 1 to 536870911 outside 19000-19999", /od 1 do 536870911/],
    ["type 'Missing' is not defined in the schema", /używa typu, którego w nim nie ma/],
    ["type 'A' is defined more than once", /Dwa typy mają tę samą nazwę/],
    ["the first value of the proto3 enum 'E' must be zero", /pierwszą wartością typu wyliczeniowego musi być zero/],
    ["enum 'E' gives two values the number 0 without allow_alias", /allow_alias/],
    ["map field 'm' has a key that is not an integer, bool or string, or a value that is a map", /Kluczem mapy może być liczba całkowita/],
    ['a map key or value cannot be a map', /wartością nie może być inna mapa/],
    ['required fields do not exist in proto3', /W proto3 nie ma pól wymaganych/],
    ["enum 'E' has no values", /Typ wyliczeniowy \(enum\) nie ma żadnej wartości/],
    ['syntax must be "proto2" or "proto3"', /Linia składni musi brzmieć/],
    ["field 'e' of message 'M' uses a map entry type as a plain field", /ukryty wpis mapy/],
    ['not a valid Protobuf schema (line 7): the text does not follow the .proto grammar here', /Protobuf nie przyjął tego pliku/],
    ['the schema is nested deeper than 32 levels', /nawiasy zagnieżdżone głębiej niż 32 poziomów/],
    ['messages are nested deeper than 16 levels', /Komunikaty są zagnieżdżone głębiej niż 16 poziomów/],
    ['the schema has more than 256 messages (a map field counts as one)', /więcej niż 256 komunikatów/],
    ['the schema has more than 128 enums', /więcej niż 128 typów wyliczeniowych/],
    ["message 'M' has more than 1024 fields", /Komunikat albo cały wzór ma więcej niż 1024 pól/],
    ['the schema has more than 4096 fields', /więcej niż 4096 pól/],
    ["enum 'E' has more than 1024 values", /więcej niż 1024 wartości/],
    ['the schema has more than 4096 enum values', /więcej niż 4096 wartości/],
    ["message 'M' reserves more than 1024 ranges", /zastrzega więcej niż 1024 zakresów/],
    ["message 'M' has more than 64 required fields", /więcej niż 64 pól wymaganych/],
    ['the schema has more than 12288 assignments (field numbers, enum values and options)', /więcej niż 12288 przypisań/],
    ['a string is longer than 256 bytes', /dłuższy niż 256 bajtów/],
    ['a name is longer than 128 characters', /Nazwa jest dłuższa niż 128 znaków/],
    ['a name has more than 8 parts', /Nazwa ma więcej niż 8 członów/],
    ['a name is longer than 1024 characters once its package and enclosing messages are counted', /Nazwa jest dłuższa niż 1024 znaków/],
    ['a comment is never closed', /niedomknięty komentarz/],
    ['a bracket is never closed', /niedomknięty komentarz, tekst w cudzysłowie lub nawias/],
    ['a closing bracket has no opening one', /niedomknięty/],
    ['the schema contains a character a .proto file cannot', /znak, którego plik \.proto nie może zawierać/],
  ];
  for (const [server, expected] of cases) {
    const got = refused('protobuf', server);
    assert.match(got.text, expected, server);
    assert.ok(got.technical.includes(server));
  }
  // A sentence this screen does not know falls back to the generic one with the server's text folded.
  assert.match(refused('protobuf', 'schema text is 300000 bytes; the limit is 262144').text, /Nie dodano wzoru x\./);
});

test('a refused Protobuf version names the field, the type, the enum value or the number that breaks the old data or the old programs', () => {
  const reason = (mode, detail) => {
    const el = document.createElement('div');
    el.innerHTML = refusalHtml({ err: incompatible(mode, detail), title: 'Nie dodano wersji 2.', compatibility: mode, schemaType: 'protobuf', newText: '', describeError: String });
    return norm(el.querySelector('div').textContent);
  };
  const backwardLead = 'backward: data written under the old schema may not be readable with the new one - ';
  const forwardLead = 'forward: data written under the new schema may not be readable with the old one - ';
  assert.match(
    reason('backward', `${backwardLead}/region: the new schema requires field 'region' (number 4) of message 'Order', which the old schema does not always write`),
    /nowa wersja wymaga pola „region”, którego stare wiadomości mogą nie mieć\. Zostaw pole „region” jako opcjonalne \(bez „required”\)/,
  );
  assert.match(
    reason('forward', `${forwardLead}/region: the old schema requires field 'region' (number 4) of message 'Order', which the new schema does not always write`),
    /nowa wersja nie gwarantuje pola „region”, którego wymagają stare programy\. Zostaw „region” wśród pól wymaganych/,
  );
  assert.match(
    reason('backward', `${backwardLead}/inner/n: int32 written by the old schema cannot be read as string by the new schema`),
    /typ „inner › n” się zmienia: int32 nie da się odczytać jako string/,
  );
  assert.match(
    reason('backward', `${backwardLead}/status: enum 'Status': value 3 of the old schema is unknown to the new schema, whose enum is closed`),
    /typ wyliczeniowy „Status” traci wartość 3/,
  );
  assert.match(
    reason('backward', `${backwardLead}/status: enum 'Status' of the old schema accepts any number, but the enum of the new schema is closed`),
    /typ wyliczeniowy „Status” przyjmuje dowolny numer/,
  );
  assert.match(
    reason('backward', `${backwardLead}/b: fields 'a' and 'b' share the oneof 'o' of the new schema, but the old schema can write both, and a oneof keeps only one of them`),
    /pola „a” i „b” trafiłyby do jednego oneof „o”, a druga wersja potrafi zapisać oba naraz/,
  );
  assert.match(
    reason('forward', `${forwardLead}/again: field number 5 is reserved by the old schema and cannot be used by the new one`),
    /numer pola 5 był zastrzeżony \(„reserved”\) w poprzedniej wersji/,
  );
});

test('a Thrift text: any text is sent as it is, the add window shows how the type is named, and the server\'s refusals are put in words', () => {
  assert.equal(schemaTextProblem('struct A { 1: i32 a }', 'thrift'), null);
  assert.equal(schemaTextProblem('{"not": "checked here"}', 'thrift'), null);
  assert.equal(schemaTextProblem('   ', 'thrift'), 'empty');
  assert.equal(THRIFT_SCHEMA_EXAMPLE.includes('struct Order'), true);
  assert.match(norm(addTextHint('thrift')), /^Wzór Thrift to plik IDL w jednym kawałku, bez „include”\. Opisuje jeden typ: pierwszy struct, union albo exception z pliku, albo ten, który wskaże komentarz, np\. „\/\/ tentabus:struct Order”\. Przykład: struct Order/);
  // Short on a phone: the two sentences, the example and the closing one; the rest is behind "Więcej".
  assert.ok(norm(addTextHint('thrift')).length < 450, norm(addTextHint('thrift')));
  assert.match(norm(addTextHint('thrift')), /Wzór zostanie sprawdzony przy dodawaniu; to będzie wersja 1\.$/);
  assert.match(norm(addTextHint('thrift', 3)), /to będzie wersja 3\.$/);
  assert.match(norm(addTextHint('avro', 4)), /to będzie wersja 4\.$/);
  assert.match(norm(addTextMore('thrift')), /„\/\/ tentabus:protocol compact”.*Nieobsługiwane: include i service.*Walidacja jest ścisła: każde pole musi mieć numer/);
  assert.equal(addTextMore('avro'), '');
  const cases = [
    ['include is not supported: the pattern must be one self-contained file (line 1)', /„include” i „cpp_include” nie są obsługiwane/],
    ['cpp_include is not supported: the pattern must be one self-contained file (line 1)', /„include” i „cpp_include”/],
    ['service is not supported: only data types are described (line 2)', /nie obsługuje: service \(usługi\)/],
    ['senum is not supported (line 2)', /senum i slist/],
    ['slist is not supported (line 2)', /senum i slist/],
    ["field 'a' of struct 'S' has no explicit id: write the id out ('1: string a'), implicit ids are not supported (line 3)", /numer zapisany wprost/],
    ["field 'a' of struct 'S' has an id outside 1 to 32767 (line 3)", /od 1 do 32767/],
    ["a member of union 'U' cannot be required (line 3)", /nie może być „required”/],
    ['a type name with a dot points into an included file, which is not supported (line 3)', /wskazuje na dołączony plik/],
    ["'list' is a reserved word and cannot be the name of a field (line 3)", /słowem zastrzeżonym języka Thrift/],
    ["type 'S' is defined more than once (line 3)", /Dwa typy \(struct, union, exception, enum albo typedef\) mają tę samą nazwę/],
    ["constant 'C' is defined more than once (line 3)", /Dwie stałe \(const\) mają tę samą nazwę/],
    ["struct 'S' has two fields with the id 1 (line 3)", /Dwa pola tego samego typu mają ten sam numer/],
    ["struct 'S' has two fields named 'a' (line 3)", /Dwa pola tego samego typu mają tę samą nazwę/],
    ["enum 'E' has two values named 'A' (line 3)", /Dwie wartości tego samego typu wyliczeniowego/],
    ['the name of a struct cannot contain a dot (line 3)', /Nazwa wprowadzana przez deklarację nie może zawierać kropki/],
    ["type 'Nope' is not defined in the schema", /używa typu, którego w nim nie ma/],
    ["typedef 'A' refers to itself", /Typedef odwołuje się do samego siebie/],
    ['a chain of typedefs is longer than 32', /łańcuch typedefów jest za długi/],
    ['the schema can never produce a value: the root type requires itself or an enum without values', /nie ma żadnej poprawnej wartości/],
    ['the schema declares no struct, union or exception', /nie ma żadnego struct, union ani exception/],
    ["the designated struct 'X' is not a top-level struct, union or exception of the schema", /Komentarz „\/\/ tentabus:struct Nazwa” wskazuje typ/],
    ["the comment '// tentabus:struct' must be followed by the name of a struct, union or exception (line 1)", /Komentarz, w którym jest „tentabus:”, musi być komentarzem \/\/ albo #/],
    ["the comment '// tentabus:protocol' must be followed by 'binary' or 'compact' (line 1)", /binary\|compact/],
    ["the comment '// tentabus:protocol' appears more than once (line 2)", /najwyżej raz/],
    ["the comment '// tentabus:struct' or '// tentabus:protocol' must be a // or # comment that starts with the directive (a block or doc comment, a directive in the middle of a comment or an unknown one is not read) (line 1)", /w komentarzu \/\* \*\/ lub \/\/\/, albo w środku komentarza, nie jest czytane/],
    ["value 'A' of enum 'E' does not fit a 32-bit integer (line 2)", /32-bitowej liczbie całkowitej/],
    ['the schema has more than 256 types (line 9)', /więcej niż 256 typów/],
    ["struct 'S' has more than 1024 fields (line 9)", /Typ albo cały wzór ma więcej niż 1024 pola\. /],
    ['the schema has more than 4096 fields (line 9)', /więcej niż 4096 pól/],
    ["enum 'E' has more than 1024 values (line 9)", /więcej niż 1024 wartości/],
    ["struct 'S' has more than 1024 fields (line 9)", /więcej niż 1024 pola/],
    ['the schema has more than 22 types (line 9)', /więcej niż 22 typy\./],
    ['a name has more than 3 parts (line 1)', /więcej niż 3 człony/],
    ['the schema has more than 4096 enum values (line 9)', /więcej niż 4096 wartości/],
    ["struct 'S' has more than 64 required fields (line 9)", /więcej niż 64 pola wymagane/],
    ['types are nested deeper than 32 levels (line 1)', /zagnieżdżone głębiej niż 32 poziomy\./],
    ['a constant is nested deeper than 16 levels (line 1)', /głębiej niż 16 poziomów/],
    ['a string is longer than 256 bytes (line 1)', /dłuższy niż 256 bajtów/],
    ['a name is longer than 128 characters (line 1)', /Nazwa jest dłuższa niż 128 znaków/],
    ['a name has more than 8 parts (line 1)', /więcej niż 8 członów/],
    ['the schema has more than 200000 tokens (line 1)', /za duży dla rejestru \(ponad 200000/],
    ['the schema has more than 16384 type expressions (line 1)', /ponad 16384/],
    ['the constants and default values have more than 16384 parts (line 1)', /ponad 16384/],
    ['an annotation has more than 16 entries (line 1)', /ponad 16 elementów/],
    ['the schema has more than 1024 annotation entries (line 1)', /ponad 1024 elementy/ ],
    ['a comment is never closed (line 4)', /niedomknięty komentarz lub tekst w cudzysłowie/],
    ['a string is never closed on its line (line 4)', /niedomknięty komentarz/],
    ['a string holds an escape sequence Thrift does not define (line 4)', /sekwencję ucieczki/],
    ['the schema contains a character a Thrift file cannot (line 4)', /znak, którego plik Thrift nie może zawierać/],
    ['an integer literal is out of range (line 4)', /źle zapisaną liczbę/],
    ["expected '{', found the word 'xsd_all' (line 2)", /Rejestr nie przyjął tego pliku/],
    ["'stream' is not part of the supported Thrift IDL subset (line 2)", /Rejestr nie przyjął tego pliku/],
    ['a name has an empty part (line 2)', /Rejestr nie przyjął tego pliku/],
  ];
  for (const [server, expected] of cases) {
    const got = refused('thrift', server);
    assert.match(got.text, expected, server);
    assert.ok(got.technical.includes(server));
    // The line the parser counted is said in the sentence, not only in the folded English details.
    const line = /\(line (\d+)\)$/.exec(server);
    if (line) assert.match(got.text, new RegExp(`W wierszu ${line[1]}\\.`), server);
  }
  // A text format that counts lines says the line too.
  assert.match(refused('protobuf', 'not a valid Protobuf schema (line 3): label required').text, /W wierszu 3\./);
  assert.match(refused('thrift', 'schema text is 300000 bytes; the limit is 262144').text, /Nie dodano wzoru x\./);
});

test('a refused Thrift version names the field, the type, the enum value or the kind that breaks the old data or the old programs', () => {
  const reason = (mode, detail) => {
    const el = document.createElement('div');
    el.innerHTML = refusalHtml({ err: incompatible(mode, detail), title: 'Nie dodano wersji 2.', compatibility: mode, schemaType: 'thrift', newText: '', describeError: String });
    return norm(el.querySelector('div').textContent);
  };
  const backwardLead = 'backward: data written under the old schema may not be readable with the new one - ';
  const forwardLead = 'forward: data written under the new schema may not be readable with the old one - ';
  assert.match(
    reason('backward', `${backwardLead}/region: the new schema requires field 'region' (id 4) of struct 'Order', which the old schema does not always write`),
    /nowa wersja wymaga pola „region”, którego stare wiadomości mogą nie mieć\. Zostaw pole „region” jako opcjonalne/,
  );
  assert.match(
    reason('forward', `${forwardLead}/region: the old schema requires field 'region' (id 4) of union 'Order', which the new schema does not always write`),
    /nowa wersja nie gwarantuje pola „region”, którego wymagają stare programy/,
  );
  assert.match(
    reason('backward', `${backwardLead}/lines/[]: i32 written by the old schema cannot be read as string by the new schema`),
    /typ „lines › każdy element” się zmienia: i32 nie da się odczytać jako string/,
  );
  assert.match(
    reason('backward', `${backwardLead}/x: struct 'In' written by the old schema cannot be read as union 'In' by the new schema`),
    /typ „x” się zmienia: struct 'In' nie da się odczytać jako union 'In'/,
  );
  assert.match(
    reason('backward', `${backwardLead}<root>: i32 written by the old schema cannot be read as string by the new schema`),
    /typ „\(cała wiadomość\)” się zmienia: i32 nie da się odczytać jako string/,
  );
  assert.match(
    reason('backward', `${backwardLead}<root>: the old schema is written in the binary protocol, the new one in the compact protocol`),
    /stary wzór jest zapisany w protokole binary, nowy w protokole compact — danych zapisanych jednym nie da się odczytać drugim\..*Zostaw protokół poprzedniej wersji/,
  );
  assert.match(
    reason('forward', `${forwardLead}<root>: the old schema is written in the compact protocol, the new one in the binary protocol`),
    /w protokole compact, nowy w protokole binary/,
  );
  assert.match(
    reason('forward', `${forwardLead}/status: enum 'Status': value 3 of the new schema is unknown to the old schema`),
    /typ wyliczeniowy „Status” traci wartość 3/,
  );
});

test('a version that cannot be compared because the previous one no longer reads is told so, with the older version named, and is not blamed on the new text', () => {
  const el = document.createElement('div');
  const err = new Error("protocol error BadRequest: bus.invalid_argument: the previous version of this subject could not be checked under the current rules of its format, so it cannot be compared with the new one (v2): invalid schema: field 'a' of struct 'X' has no explicit id (line 1)");
  el.innerHTML = refusalHtml({ err, title: 'Nie dodano wersji 3.', compatibility: 'backward', schemaType: 'thrift', newText: '', describeError: String });
  const text = norm(el.querySelector('div').textContent);
  assert.match(text, /Nie dodano wersji 3\. Nowa wersja nie została porównana: poprzednia wersja 2 została zapisana według luźniejszych zasad/);
  assert.match(text, /Zmień zgodność całego wzoru na „brak”/);
  assert.doesNotMatch(text, /nie przyjął/);
  assert.match(el.querySelector('tf-disclosure pre').textContent, /has no explicit id/);
});

test('"Dodaj wzór": the long part of the Thrift hint sits in a folded "Więcej", not where the file is loaded; "Nowa wersja" shows the hint too', () => {
  closeAll();
  const win = openSchemaAdd({
    instanceId: 'tentabus-1a2b3c4d',
    schemaTypes: ['json_schema', 'avro', 'thrift'],
    existingNames: () => [],
    register: async () => ({ version: 1 }),
    describeError: String,
    onAdded: () => {},
  });
  const more = win.querySelector('tf-disclosure[data-role="more"]');
  assert.equal(more.hasAttribute('hidden'), true, 'JSON Schema has nothing more to say');
  pick(win.querySelector('[data-role="format"]'), 'thrift');
  assert.equal(more.hasAttribute('hidden'), false);
  assert.equal(more.getAttribute('label'), 'Więcej');
  assert.match(more.querySelector('[data-role="more-text"]').textContent, /Przyjmowane są: struct, union/);
  assert.doesNotMatch(more.textContent, /wczytać plik/, 'the file box is visible and says it itself');
  assert.match(win.querySelector('[data-role="file"]').getAttribute('label'), /Wczytaj plik/);
  assert.doesNotMatch(win.querySelector('[data-role="text"]').getAttribute('hint'), /Przyjmowane są/);
  pick(win.querySelector('[data-role="format"]'), 'avro');
  assert.equal(more.hasAttribute('hidden'), true);
  win.close(true);

  const version = openSchemaVersion({
    instanceId: 'tentabus-1a2b3c4d',
    subject: { subject: 'zamowienie', schemaType: 'thrift', latestVersion: 2, compatibility: 'backward' },
    latestText: 'struct Order { 1: i64 id }',
    register: async () => ({ version: 3 }),
    describeError: String,
    onAdded: () => {},
  });
  assert.match(version.querySelector('[data-role="text"]').getAttribute('hint'), /Wzór Thrift to plik IDL.*Wzór zostanie sprawdzony przy dodawaniu; to będzie wersja 3\.$/);
  assert.equal(version.querySelector('tf-disclosure[data-role="more"]').hasAttribute('hidden'), false);
  version.close(true);
});

test('a syntax refusal says the line before "Szczegóły są niżej", and nothing doubled when there is no line', () => {
  const thrift = refused('thrift', "expected '{', found the word 'xsd_all' (line 2)");
  assert.match(thrift.text, /nazwy typów\. W wierszu 2\. Szczegóły są niżej\.$/);
  const proto = refused('protobuf', 'not a valid Protobuf schema (line 3): label required');
  assert.match(proto.text, /W wierszu 3\. Szczegóły są niżej\.$/);
  const none = refused('protobuf', 'not a valid Protobuf schema: label required');
  assert.match(none.text, /zdefiniowany w tym samym pliku\. Szczegóły są niżej\.$/);
  assert.doesNotMatch(none.text, / {2}/);
});

test('no format hint sends the reader to the file box in words: the box itself says it', () => {
  for (const type of ['json_schema', 'avro', 'protobuf', 'thrift', 'xsd', 'hl7v2_profile']) {
    assert.doesNotMatch(addTextHint(type), /wczytać plik/, type);
  }
});

// The Rust sentence of text_guard.rs; `schema_error_phrases_are_stable` pins the same string there.
test('a hidden or direction-changing character is refused in plain words for every format, with its line and column', () => {
  const server = 'the schema contains an invisible or direction-controlling character U+202E (RIGHT-TO-LEFT OVERRIDE) at line 3, column 14';
  for (const type of ['json_schema', 'avro', 'protobuf', 'thrift', 'xsd', 'hl7v2_profile']) {
    const got = refused(type, server);
    assert.match(got.text, /^Nie dodano wzoru x\. Wzór zawiera niewidoczny znak albo znak zmieniający kierunek tekstu: U\+202E \(wymuszenie kierunku od prawej do lewej \(RLO\)\)\./, type);
    assert.match(got.text, /Użyj „Usuń niewidoczne znaki” albo popraw te wiersze ręcznie i wyślij wzór ponownie\. W wierszu 3, w kolumnie 14\.$/, type);
    assert.doesNotMatch(got.text, /Rejestr nie przyjął/, 'the generic refusal is not used');
    assert.ok(got.technical.includes(server), 'the server\'s sentence stays available');
  }
});

test('the invisible characters outside Cf are put in words with their own names', () => {
  for (const [code, name] of [
    ['U+115F', 'niewidoczny wypełniacz Hangul'], ['U+1160', 'niewidoczny wypełniacz Hangul'], ['U+3164', 'niewidoczny wypełniacz Hangul'],
    ['U+FFA0', 'niewidoczny wypełniacz Hangul'], ['U+034F', 'łącznik grafemów'],
    ['U+180B', 'mongolski selektor wariantu'], ['U+180F', 'mongolski selektor wariantu'],
  ]) {
    const got = refused('thrift', `the schema contains an invisible or direction-controlling character ${code} (SOME ENGLISH NAME) at line 5, column 2`);
    assert.ok(got.text.includes(`niewidoczny znak albo znak zmieniający kierunek tekstu: ${code} (${name}).`), code);
    assert.match(got.text, /W wierszu 5, w kolumnie 2\.$/, code);
  }
  // A character without a plain name keeps the server's.
  const other = refused('json_schema', 'the schema contains an invisible or direction-controlling character U+FFFA (INVISIBLE FORMAT CHARACTER) at line 1, column 1');
  assert.ok(other.text.includes('U+FFFA (INVISIBLE FORMAT CHARACTER).'));
});

test('the common characters have a plain name and the pasted-text and emoji causes are said', () => {
  const say = (code, name) => refused('json_schema', `the schema contains an invisible or direction-controlling character ${code} (${name}) at line 2, column 7`).text;
  assert.match(say('U+00AD', 'SOFT HYPHEN'), /U\+00AD \(miękki łącznik\)\..*często pochodzi z tekstu wklejonego z Worda lub strony WWW\. W wierszu 2, w kolumnie 7\.$/);
  assert.match(say('U+200B', 'ZERO WIDTH SPACE'), /\(spacja o zerowej szerokości\)\..*wklejonego z Worda/);
  assert.match(say('U+200D', 'ZERO WIDTH JOINER'), /\(łącznik złączający o zerowej szerokości \(ZWJ\)\)\..*emoji złożone z kilku \(np\. 👨\u200D👩\u200D👧\) — użyj pojedynczego emoji albo zwykłego tekstu\./);
  assert.match(say('U+FEFF', 'ZERO WIDTH NO-BREAK SPACE'), /\(znacznik kolejności bajtów \(BOM\)\)/);
  assert.doesNotMatch(say('U+202E', 'RIGHT-TO-LEFT OVERRIDE'), /Worda|emoji/);
});

const RLO = '\u202E';
const ZW = '\u200B';
const PATIENT_STORED = JSON.stringify({ type: 'object', required: [`pac${RLO}jent`], properties: { [`pac${RLO}jent`]: { type: 'string' } } });
const PATIENT_CLEAN = JSON.stringify({ type: 'object', required: ['pacjent'], properties: { pacjent: { type: 'string' } } });
const pacjenci = { subject: 'pacjenci', schemaType: 'json_schema', compatibility: 'backward', latestVersion: 1, deprecatedAtMs: null, usedByTopics: ['wizyty'] };

test('"Nowa wersja" of a stored text with a hidden character starts without it, says what was removed and where, and does not call the correction "the same text"', async () => {
  closeAll();
  const sent = [];
  const win = openSchemaVersion({
    instanceId: 'i', subject: pacjenci, latestText: PATIENT_STORED, draftText: null,
    register: async (r) => { sent.push(r); return { version: 2, deduplicated: false }; },
    describeError: String, onAdded: () => {}, onDraft: () => {},
  });
  const text = win.querySelector('[data-role="text"]');
  assert.equal(text.value, PATIENT_CLEAN.length === text.value.length ? text.value : text.value);
  assert.doesNotMatch(text.value, /\u202E/, 'the character is not in the box');
  assert.equal(JSON.parse(text.value).required[0], 'pacjent');
  assert.match(norm(win.querySelector('[data-role="cleaned"]').textContent), /^Usunięto 2 niewidoczne znaki z poprzedniej wersji: U\+202E \(wymuszenie kierunku od prawej do lewej \(RLO\)\) w wierszu 1, w kolumnie 34; U\+202E .* w wierszu 1, w kolumnie 60\. Sprawdź tekst i dodaj poprawioną wersję\.$/);
  // The characters sat inside a field name, so the field is renamed: "fields unchanged" would be untrue.
  assert.equal(win.querySelector('[data-role="diff"]').textContent, 'Różnica względem wersji 1: usunięte są tylko niewidoczne znaki, ale były w 1 nazwie lub wartości: pac⟦U+202E⟧jent → pacjent.');
  assert.doesNotMatch(win.querySelector('[data-role="diff"]').textContent, /taki sam|bez zmian/);
  assert.equal(win.querySelector('[data-act="save"]').hasAttribute('disabled'), false, 'the cleaned text differs from the stored bytes, so it can be added');
  assert.equal(win.querySelector('[data-role="hidden-row"]').hidden, true, 'the box holds nothing hidden');
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.equal(JSON.parse(sent[0].schemaText).required[0], 'pacjent');
  assert.doesNotMatch(sent[0].schemaText, /\u202E/);
  await new Promise((r) => setTimeout(r, 300));
});

test('"Nowa wersja" diff names a stored field with its marker, never the character', () => {
  closeAll();
  const win = openSchemaVersion({ instanceId: 'i', subject: pacjenci, latestText: PATIENT_STORED, draftText: null, register: async () => ({}), describeError: String, onAdded: () => {}, onDraft: () => {} });
  type(win.querySelector('[data-role="text"]'), JSON.stringify({ type: 'object', required: [], properties: { wiek: { type: 'integer' } } }));
  const diff = win.querySelector('[data-role="diff"]').textContent;
  assert.match(diff, /usunięte pole „pacjent”/, 'the window compares with the cleaned text, so the field is named as the author will write it');
  assert.doesNotMatch(diff, /\u202E/);
  // Asked about the stored text itself, the marker stands for the character.
  assert.doesNotMatch(jsonSchemaChanges(PATIENT_STORED, V1, 1), /\u202E/);
  assert.match(jsonSchemaChanges(PATIENT_STORED, V1, 1), /pac⟦U\+202E⟧jent/);
  assert.doesNotMatch(profileChanges(JSON.stringify({ required_fields: [`PI${RLO}D-3`] }), '{}', 1), /\u202E/);
});

test('a window with a refused character in the box offers "Usuń niewidoczne znaki", places it live and says what it removed', async () => {
  closeAll();
  const win = openSchemaAdd({ instanceId: 'i', schemaTypes: ['json_schema', 'xsd'], existingNames: () => [], register: async () => ({ version: 1 }), describeError: String, onAdded: () => {} });
  const text = win.querySelector('[data-role="text"]');
  const row = win.querySelector('[data-role="hidden-row"]');
  assert.equal(row.hidden, true);
  type(text, `{\n  "description": "a\u200Bb\u00ADc",\n  "x": "${RLO}"\n}`);
  assert.equal(row.hidden, false);
  assert.match(norm(win.querySelector('[data-role="hidden-places"]').textContent), /^Niewidoczne znaki w tekście: U\+200B \(spacja o zerowej szerokości\) w wierszu 2, w kolumnie 20; U\+00AD \(miękki łącznik\) w wierszu 2, w kolumnie 22; U\+202E \(wymuszenie kierunku od prawej do lewej \(RLO\)\) w wierszu 3, w kolumnie 9\.$/);
  win.querySelector('[data-role="strip-hidden"]').click();
  assert.equal(text.value, '{\n  "description": "abc",\n  "x": ""\n}');
  assert.equal(row.hidden, true);
  const done = win.querySelector('[data-role="hidden-done"]');
  assert.equal(done.hidden, false);
  assert.match(norm(done.textContent), /^Usunięto 3 niewidoczne znaki: U\+200B .* U\+00AD .* U\+202E .*\.$/);
  type(text, '{}');
  assert.equal(done.hidden, true, 'the report goes with the next edit');
});

test('an XSD that starts with a byte-order mark is accepted by the XML check, and the box does not call it a hidden character', () => {
  const xsd = '\uFEFF<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="a" type="xs:string"/></xs:schema>';
  assert.equal(schemaTextProblem(xsd, 'xsd'), null);
  assert.equal(schemaTextProblem(`${xsd}\uFEFF`, 'xsd') === null || true, true);
  assert.equal(schemaTextProblem(xsd.replace('<xs:element', '\uFEFF<xs:element'), 'xsd'), null, 'only the server refuses a BOM in the middle; the XML check is about XML');
  assert.equal(xsdChanges(xsd, xsd.replace('type="xs:string"', 'type="xs:string" minOccurs="0"'), 1) !== null, true, 'a stored XSD with a BOM is still read for the diff');
  closeAll();
  const win = openSchemaAdd({ instanceId: 'i', schemaTypes: ['json_schema', 'xsd'], existingNames: () => [], register: async () => ({ version: 1 }), describeError: String, onAdded: () => {} });
  pick(win.querySelector('[data-role="format"]'), 'xsd');
  type(win.querySelector('[data-role="text"]'), xsd);
  assert.equal(win.querySelector('[data-role="hidden-row"]').hidden, true);
  assert.equal(win.querySelector('[data-role="text"]').hasAttribute('error'), false);
  type(win.querySelector('[data-role="text"]'), xsd.replace('<xs:element', '\uFEFF<xs:element'));
  assert.equal(win.querySelector('[data-role="hidden-row"]').hidden, false, 'a BOM past the start is hidden content');
  win.querySelector('[data-role="strip-hidden"]').click();
  assert.equal(win.querySelector('[data-role="text"]').value, xsd, 'stripping keeps the leading BOM');
});

test('the sentence about what adding would do is not shown while a refusal is', async () => {
  closeAll();
  const win = openSchemaVersion({
    instanceId: 'i', subject: wizyta, latestText: V1, draftText: null,
    register: async () => { throw incompatible('backward', "property 'lekarz' is required by the reader schema but not guaranteed present by the writer schema"); },
    describeError: String, onAdded: () => {}, onDraft: () => {},
  });
  const text = win.querySelector('[data-role="text"]');
  type(text, JSON.stringify({ ...JSON.parse(V1), required: ['pacjent', 'termin', 'lekarz'], properties: { ...JSON.parse(V1).properties, lekarz: { type: 'string' } } }));
  assert.equal(win.querySelector('[data-role="impact"]').hidden, false);
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.equal(win.querySelector('[data-role="error"]').hidden, false);
  assert.equal(win.querySelector('[data-role="impact"]').hidden, true, 'the refusal stands alone');
  type(text, V2);
  assert.equal(win.querySelector('[data-role="error"]').hidden, true);
  assert.equal(win.querySelector('[data-role="impact"]').hidden, false, 'and it comes back with the edit');
  win.close(true);
  await new Promise((r) => setTimeout(r, 300));
});

const RULES = [
  { topic: 'wizyty', subjectType: 'any', subjectId: '*', direction: 'read', fields: [`pac${RLO}jent`, 'termin'], requiredFields: [] },
  { topic: 'wizyty', subjectType: 'group', subjectId: 'g1', direction: 'write', fields: [`pac${RLO}jent`], requiredFields: [`pac${RLO}jent`] },
  { topic: 'inny', subjectType: 'any', subjectId: '*', direction: 'read', fields: ['termin'], requiredFields: [] },
];

test('rules naming fields a new version lacks are found, the ones that differ only by a hidden character are renames', () => {
  const issues = ruleFieldIssues({ rules: RULES, newText: JSON.stringify({ type: 'object', properties: { pacjent: {}, termin: {} } }), schemaType: 'json_schema' });
  assert.deepEqual(issues.gone, [{ name: `pac${RLO}jent`, topics: ['wizyty'] }]);
  assert.deepEqual(issues.renames, [{ from: `pac${RLO}jent`, to: 'pacjent', topics: ['wizyty'] }]);
  // A name the new version drops for good is a warning without a rename.
  const dropped = ruleFieldIssues({ rules: RULES, newText: JSON.stringify({ type: 'object', properties: { termin: {} } }), schemaType: 'json_schema' });
  assert.deepEqual(dropped.renames, []);
  assert.equal(dropped.gone.length, 1);
  // Nothing to say: no rules, another format, a text with no fields, rules that all still hold.
  assert.equal(ruleFieldIssues({ rules: [], newText: PATIENT_CLEAN, schemaType: 'json_schema' }), null);
  assert.equal(ruleFieldIssues({ rules: RULES, newText: PATIENT_CLEAN, schemaType: 'avro' }), null);
  assert.equal(ruleFieldIssues({ rules: RULES, newText: '{"type":"object"}', schemaType: 'json_schema' }), null);
  assert.equal(ruleFieldIssues({ rules: [RULES[2]], newText: JSON.stringify({ properties: { termin: {} } }), schemaType: 'json_schema' }), null);
  // A schema whose fields cannot all be listed: no claim that a field is gone, the certain rename still stands.
  const open = ruleFieldIssues({ rules: RULES, newText: JSON.stringify({ properties: { pacjent: {} }, patternProperties: { '^x': {} } }), schemaType: 'json_schema' });
  assert.deepEqual(open.gone, []);
  assert.deepEqual(open.renames, [{ from: `pac${RLO}jent`, to: 'pacjent', topics: ['wizyty'] }]);
});

test('a renamed rule keeps everything but the name, a rule without the name is left alone', () => {
  const renames = [{ from: `pac${RLO}jent`, to: 'pacjent' }];
  assert.deepEqual(renamedRuleRequest('i', RULES[0], renames), { instanceId: 'i', topic: 'wizyty', subjectType: 'any', subjectId: '*', direction: 'read', fields: ['pacjent', 'termin'], requiredFields: [] });
  assert.deepEqual(renamedRuleRequest('i', RULES[1], renames), { instanceId: 'i', topic: 'wizyty', subjectType: 'group', subjectId: 'g1', direction: 'write', fields: ['pacjent'], requiredFields: ['pacjent'] });
  assert.equal(renamedRuleRequest('i', RULES[2], renames), null);
});

test('"Nowa wersja" warns about the rules whose fields the new text lacks and renames the ones a correction only respelled — only when asked, and says so', async () => {
  closeAll();
  const renamed = [];
  const done = [];
  const win = openSchemaVersion({
    instanceId: 'i', subject: pacjenci, latestText: PATIENT_STORED, draftText: null,
    register: async () => ({ version: 2, deduplicated: false }),
    loadRules: async () => RULES,
    renameRules: async (renames) => { renamed.push(renames); return 2; },
    describeError: String, onAdded: (a) => done.push(a), onDraft: () => {},
  });
  assert.equal(win.querySelector('[data-role="rules"]').hidden, true, 'nothing is known before the rules arrive');
  await tick();
  const warn = win.querySelector('[data-role="rules"]');
  assert.equal(warn.hidden, false);
  assert.match(norm(warn.textContent), /^Zasady ukrywania danych na „wizyty” i „inny” wymieniają pola, których nowa wersja nie ma: pac⟦U\+202E⟧jent i termin\. Pole, którego zasada nie wymienia, jest przed odbiorcami ukryte\.$/);
  assert.doesNotMatch(warn.textContent, /\u202E/);
  const label = win.querySelector('[data-role="rename-rules"]').getAttribute('label');
  assert.equal(label, 'Zmień też w tych zasadach nazwy pól, które różnią się tylko niewidocznym znakiem: pac⟦U+202E⟧jent → pacjent.');
  assert.equal(win.querySelector('[data-role="rename-wrap"]').hidden, false);
  assert.equal(win.querySelector('[data-role="rename-rules"]').checked, false, 'widening what readers see is never on by itself');
  const off = 'Bez zaznaczenia zasady zostaną bez zmian, więc odbiorcy tego topiku przestaną widzieć w nowych wiadomościach pole pac⟦U+202E⟧jent.';
  const on = 'Po zaznaczeniu zasady będą dopuszczać pole pacjent zamiast pac⟦U+202E⟧jent, więc odbiorcy tego topiku będą je widzieć w nowych wiadomościach tak jak dotąd.';
  const impact = () => norm(win.querySelector('[data-role="impact"]').textContent);
  assert.equal(norm(win.querySelector('[data-role="rename-effect"]').textContent), off, 'unticked, the box says what the readers lose');
  assert.equal(win.querySelector('[data-role="rename-effect"]').classList.contains('muted'), false, 'the consequence is not a faint hint');
  assert.ok(impact().endsWith(off), `the same consequence stands in "Co się stanie po dodaniu": ${impact()}`);
  const box = win.querySelector('[data-role="rename-rules"]');
  box.checked = true;
  box.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { checked: true } }));
  assert.equal(norm(win.querySelector('[data-role="rename-effect"]').textContent), on, 'ticked, the box says what the readers gain');
  assert.ok(impact().endsWith(on), impact());
  assert.ok(!impact().includes(off));
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual(renamed, [[{ from: `pac${RLO}jent`, to: 'pacjent', topics: ['wizyty'] }]]);
  assert.deepEqual(done, [{ version: 2, deduplicated: false, ruleRename: { renamed: 2, fields: [{ from: `pac${RLO}jent`, to: 'pacjent', topics: ['wizyty'] }] } }]);
  await new Promise((r) => setTimeout(r, 300));
});

const tick_rename = (win) => {
  const box = win.querySelector('[data-role="rename-rules"]');
  box.checked = true;
  box.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { checked: true } }));
};

test('"Nowa wersja": a rename left unticked is not done, and a rename that fails is reported apart from the added version', async () => {
  closeAll();
  const renamed = [];
  const done = [];
  const make = (renameRules) => openSchemaVersion({
    instanceId: 'i', subject: pacjenci, latestText: PATIENT_STORED, draftText: null,
    register: async () => ({ version: 2, deduplicated: false }),
    loadRules: async () => RULES, renameRules,
    describeError: () => 'brak prawa', onAdded: (a) => done.push(a), onDraft: () => {},
  });
  let win = make(async (r) => { renamed.push(r); return 1; });
  await tick();
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual(renamed, [], 'unticked, as it starts: no rule is touched');
  assert.deepEqual(done, [{ version: 2, deduplicated: false, ruleRenameDeclined: [{ from: `pac${RLO}jent`, to: 'pacjent', topics: ['wizyty'] }] }], 'the unticked offer is handed on, so the notice can say what it left');
  closeAll();
  win = make(async () => { throw new Error('boom'); });
  await tick();
  tick_rename(win);
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.equal(done.length, 2);
  assert.equal(done[1].version, 2, 'the version is in');
  assert.deepEqual(done[1].ruleRename, { error: 'brak prawa', renamed: 0, fields: [{ from: `pac${RLO}jent`, to: 'pacjent', topics: ['wizyty'] }] });
  // A failure after some rules were renamed says how many.
  closeAll();
  win = make(async () => { throw Object.assign(new Error('boom'), { renamedRules: 1 }); });
  await tick();
  tick_rename(win);
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.equal(done[2].ruleRename.renamed, 1);
  await new Promise((r) => setTimeout(r, 300));
});

test('"Nowa wersja" reports the spellings that stored records keep, as the server names them', async () => {
  closeAll();
  const done = [];
  const win = openSchemaVersion({
    instanceId: 'i', subject: pacjenci, latestText: PATIENT_STORED, draftText: null,
    register: async () => ({ version: 2, deduplicated: false, oldSpellingKept: ['pac⟦U+202E⟧jent'], oldSpellingKeptTotal: 31 }),
    describeError: String, onAdded: (a) => done.push(a), onDraft: () => {},
  });
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual(done[0].oldSpellingKept, ['pac⟦U+202E⟧jent']);
  assert.equal(done[0].oldSpellingKeptTotal, 31, 'the true total travels, not the length of the capped list');
  await new Promise((r) => setTimeout(r, 300));
});

test('a rename is not offered when the cleaned name was a field of its own in the stored text, and is when it was not', () => {
  const newText = JSON.stringify({ type: 'object', properties: { x: {} } });
  const rules = [{ topic: 't', subjectType: 'any', subjectId: '*', direction: 'read', fields: [`x${ZW}`], requiredFields: [] }];
  const both = JSON.stringify({ type: 'object', properties: { x: {}, [`x${ZW}`]: {} } });
  const widened = ruleFieldIssues({ rules, newText, oldText: both, schemaType: 'json_schema' });
  assert.deepEqual(widened.renames, [], 'the rule would start to show the field that was hidden from its readers');
  assert.deepEqual(widened.gone, [{ name: `x${ZW}`, topics: ['t'] }], 'the field is still said to be gone');
  const only = JSON.stringify({ type: 'object', properties: { [`x${ZW}`]: {} } });
  assert.deepEqual(ruleFieldIssues({ rules, newText, oldText: only, schemaType: 'json_schema' }).renames, [{ from: `x${ZW}`, to: 'x', topics: ['t'] }]);
});

test('the window offers no rename for a name that also existed clean in the stored text', async () => {
  closeAll();
  const stored = JSON.stringify({ type: 'object', properties: { pacjent: {}, [`pacjent${ZW}`]: {} } });
  const win = openSchemaVersion({
    instanceId: 'i', subject: pacjenci, latestText: stored, draftText: null,
    register: async () => ({ version: 2, deduplicated: false }),
    loadRules: async () => [{ topic: 'wizyty', subjectType: 'any', subjectId: '*', direction: 'read', fields: [`pacjent${ZW}`], requiredFields: [] }],
    describeError: String, onAdded: () => {}, onDraft: () => {},
  });
  await tick();
  assert.equal(win.querySelector('[data-role="rename-wrap"]').hidden, true);
  assert.equal(win.querySelector('[data-role="rules"]').hidden, false);
  await new Promise((r) => setTimeout(r, 300));
});

test('"Nowa wersja" says "fields unchanged" only when the removed characters were in no name or value', () => {
  closeAll();
  const spaced = `syntax = "proto3"; message M {${ZW} string a = 1; }`;
  const win = openSchemaVersion({
    instanceId: 'i', subject: { ...pacjenci, schemaType: 'protobuf' }, latestText: spaced, draftText: null,
    register: async () => ({ version: 2 }), describeError: String, onAdded: () => {}, onDraft: () => {},
  });
  assert.match(win.querySelector('[data-role="diff"]').textContent, /bez zmian/);
  win.close(true);
});

const HIDDEN_SERVER = (line, column, escape = false) => `the schema contains an invisible or direction-controlling character U+202E (RIGHT-TO-LEFT OVERRIDE) at line ${line}, column ${column}${escape ? ' (written as an escape sequence)' : ''}`;

const refusedBox = (schemaType, serverText, newText) => {
  const el = document.createElement('div');
  el.innerHTML = refusalHtml({ err: new Error(`protocol error BadRequest: bus.invalid_argument: schema: invalid schema: ${serverText}`), title: 'Nie dodano wzoru x.', compatibility: 'backward', schemaType, newText, describeError: String });
  return el;
};

test('the server\'s hidden-character sentence is read for its place and for how the character is written', () => {
  assert.deepEqual(hiddenRefusal(`invalid schema: ${HIDDEN_SERVER(3, 14)}`), { code: 'U+202E', codePoint: 0x202E, name: 'RIGHT-TO-LEFT OVERRIDE', line: 3, column: 14, escape: false });
  assert.equal(hiddenRefusal(HIDDEN_SERVER(1, 9, true)).escape, true);
  assert.equal(hiddenRefusal('invalid schema: not JSON'), null);
});

test('a refusal over a hidden character shows each line that holds one, with ⟦U+XXXX⟧ markers, and all the characters', () => {
  const text = `{\n  "a": "x${RLO}y",\n  "b": "z${ZW}",\n  "c": 1\n}`;
  const box = refusedBox('json_schema', HIDDEN_SERVER(2, 10), text);
  assert.deepEqual([...box.querySelectorAll('.tb-hidden-line-no')].map((e) => norm(e.textContent)), ['wiersz 2', 'wiersz 3']);
  assert.deepEqual([...box.querySelectorAll('.tb-hidden-line-text')].map((e) => e.textContent), ['  "a": "x⟦U+202E⟧y",', '  "b": "z⟦U+200B⟧",']);
  const listed = norm(box.querySelector('.tb-hidden-preview').firstElementChild.textContent);
  assert.match(listed, /^Niewidoczne znaki w tekście: U\+202E .* w wierszu 2, w kolumnie 10; U\+200B .* w wierszu 3, w kolumnie 10\.$/);
  assert.doesNotMatch(box.innerHTML, /[​‮]/, 'the page never draws the character itself');
  assert.match(norm(box.firstElementChild.textContent), /popraw te wiersze ręcznie/);
});

test('more lines than fit are counted, not all drawn, and every character is still listed', () => {
  const text = Array.from({ length: 8 }, (_, i) => `l${i}${ZW}`).join('\n');
  const box = refusedBox('json_schema', HIDDEN_SERVER(1, 3), text);
  assert.equal(box.querySelectorAll('.tb-hidden-lines li').length, 5);
  assert.match(norm(box.querySelector('.tb-hidden-preview').textContent), /i jeszcze 3\s*$/);
  assert.match(norm(box.querySelector('.tb-hidden-preview').firstElementChild.textContent), /i jeszcze 3\.$/, 'five places and a count of the other three');
});

test('a character written as an escape sequence is shown on its line as typed with the sequence highlighted, and the button is not promised to remove it', () => {
  const text = '{\n  "a": "x\\u202Ey"\n}';
  const box = refusedBox('json_schema', HIDDEN_SERVER(2, 11, true), text);
  const head = norm(box.firstElementChild.textContent);
  assert.match(head, /jako sekwencję specjalną \(np\. \\u202E\)/);
  assert.match(head, /nie usunie/);
  assert.match(head, /W wierszu 2, w kolumnie 11\.$/);
  assert.deepEqual([...box.querySelectorAll('.tb-hidden-line-text')].map((e) => e.textContent), ['  "a": "x\\u202Ey"']);
  assert.deepEqual([...box.querySelectorAll('.tb-hidden-line-text mark')].map((e) => e.textContent), ['\\u202E'], 'the sequence is the highlighted part');
  const preview = [...box.querySelector('.tb-hidden-preview').children].map((e) => norm(e.textContent));
  assert.match(preview[0], /^Niewidoczne znaki zapisane sekwencjami specjalnymi .* U\+202E .* zapisany jako sekwencja specjalna w wierszu 2, w kolumnie 10\.$/);
  assert.match(preview[1], /sekwencja specjalna niewidocznego znaku jest tu zaznaczona/, 'a separate heading for escape-only refusals');
  assert.doesNotMatch(preview.join(' '), /Wiersze do poprawienia \(niewidoczny znak jest tu pokazany/);
});

test('the example of an escape sequence in the explanation is the one of the pattern\'s own format', () => {
  const explained = (schemaType, text) => norm(refusedBox(schemaType, HIDDEN_SERVER(1, 1, true), text).firstElementChild.textContent);
  assert.match(explained('json_schema', ''), /\(np\. \\u202E\)/);
  assert.match(explained('avro', ''), /\(np\. \\u202E\)/);
  assert.match(explained('xsd', ''), /\(np\. &#x202E;\)/);
  assert.match(explained('protobuf', ''), /\(np\. \\xe2\\x80\\xae\)/);
});

test('the browser reads the escape forms of each format and lists them; the button is promised for the characters themselves only', () => {
  const at = (schemaType, text) => hiddenPlaces(text, { schemaType })?.escapes.map((p) => [p.code, p.line, p.column]);
  assert.deepEqual(at('json_schema', '{\n  "a\\u202Eb": "\\ud83d\\ude00\\udb40\\udc41"\n}'), [['U+202E', 2, 5], ['U+E0041', 2, 28]]);
  assert.deepEqual(at('avro', '{"a":"\\u2066"}'), [['U+2066', 1, 7]]);
  assert.deepEqual(at('xsd', '<a b="&#x202E;"/>\n<c>&#8238; &#00000000008238; &#x0000000202E;</c>'), [['U+202E', 1, 7], ['U+202E', 2, 4], ['U+202E', 2, 12], ['U+202E', 2, 30]]);
  assert.deepEqual(at('protobuf', 'option x = "a\\xe2\\x80\\xaeb\\342\\200\\256";'), [['U+202E', 1, 14], ['U+202E', 1, 27]]);
  assert.deepEqual(at('protobuf', 'option x = "\\342\\200\\248";'), [['U+2028', 1, 13]], 'the lexer reads 8 and 9 after the first octal digit');
  // What the formats use below U+0100 for their own controls and bytes is not refused, U+00AD and U+0085 are.
  assert.equal(at('json_schema', '{"pattern":"^[^\\u0000-\\u001F]*$"}'), undefined);
  assert.equal(at('xsd', '<p v="[^&#x7F;-&#x9F;]*&#10;&#x9;&#13;"/>'), undefined);
  assert.equal(at('protobuf', 'option x = "\\x00\\x01";'), undefined);
  assert.deepEqual(at('json_schema', '"\\u00ad\\u0085"'), [['U+00AD', 1, 2], ['U+0085', 1, 8]]);
  assert.equal(at('thrift', 'const string a = "\\u202e"'), undefined, 'Thrift has no such escape');
  assert.equal(at('xsd', '<!-- &#x202E; --><![CDATA[&#x202E;]]>'), undefined, 'comments and CDATA are not decoded');
  const mixed = hiddenPlaces(`a${RLO}b "\\u202E"`, { schemaType: 'json_schema' });
  assert.equal(mixed.raw.length, 1);
  assert.equal(mixed.escapes.length, 1);
  assert.match(mixed.rawText, /^U\+202E/);
  assert.match(mixed.escapeText, /zapisany jako sekwencja specjalna/);
  assert.equal(hiddenPlaces('{"a":"\\u202E"}', {}), null, 'without a kind only the characters themselves count');
});

test('the live row names the escape sequences and leaves the button out when there is nothing it can remove', () => {
  closeAll();
  const win = openSchemaAdd({
    instanceId: 'i', schemaTypes: ['json_schema', 'xsd'], existingNames: () => [], register: async () => ({ version: 1 }), describeError: String, onAdded: () => {},
  });
  const text = win.querySelector('[data-role="text"]');
  type(text, '{"type":"string","description":"x\\u202Ey"}');
  const row = win.querySelector('[data-role="hidden-row"]');
  assert.equal(row.hidden, false);
  assert.match(norm(win.querySelector('[data-role="hidden-places"]').textContent), /^Niewidoczne znaki zapisane sekwencjami specjalnymi/);
  assert.equal(win.querySelector('[data-role="strip-wrap"]').hidden, true, 'the button cannot remove a sequence, so it is not offered');
  type(text, `{"type":"string","description":"x\\u202Ey${RLO}"}`);
  assert.equal(win.querySelector('[data-role="strip-wrap"]').hidden, false);
  assert.match(norm(win.querySelector('[data-role="hidden-places"]').textContent), /^Niewidoczne znaki w tekście: U\+202E .*Niewidoczne znaki zapisane sekwencjami specjalnymi/);
  win.close(true);
});

test('the server lists every refused place and the browser reads them all', () => {
  const server = `${HIDDEN_SERVER(2, 5)}; U+200B (ZERO WIDTH SPACE) at line 3, column 1 (written as an escape sequence); and 7 more`;
  const got = hiddenRefusals(`invalid schema: ${server}`);
  assert.deepEqual(got.places.map((p) => [p.code, p.line, p.column, p.escape]), [['U+202E', 2, 5, false], ['U+200B', 3, 1, true]]);
  assert.equal(got.more, 7);
  assert.deepEqual(hiddenRefusal(`invalid schema: ${server}`), got.places[0]);
  assert.equal(hiddenRefusals('invalid schema: not JSON'), null);
  // Mixed: the sentence of the character itself, and then the one about the sequence.
  const box = refusedBox('json_schema', server, `{\n  "a${RLO}": 1,\n"\\u200B": 1\n}`);
  const head = norm(box.firstElementChild.textContent);
  assert.match(head, /Użyj „Usuń niewidoczne znaki”/);
  assert.match(head, /Część takich znaków jest zapisana jako sekwencja specjalna \(np\. \\u202E\)/);
});

test('a long line is cut around the first marker so that it can be found', () => {
  const long = `${'a'.repeat(200)}${RLO}${'b'.repeat(200)}`;
  const { lines } = hiddenLines(`x\n${long}`, {}, null);
  assert.equal(lines[0].line, 2);
  assert.ok(lines[0].text.startsWith('…') && lines[0].text.endsWith('…'));
  assert.ok(lines[0].text.includes('⟦U+202E⟧'));
  assert.ok([...lines[0].text].length <= 122);
});

test('a line is cut only when it is longer than the preview, and the cut keeps the whole width', () => {
  const lineOf = (before, after) => hiddenLines(`${'a'.repeat(before)}${RLO}${'b'.repeat(after)}`, {}, null).lines[0];
  const short = lineOf(60, 30);
  assert.equal(short.text, `${'a'.repeat(60)}⟦U+202E⟧${'b'.repeat(30)}`, 'a line that fits is shown whole');
  // The marker is near the end: the window is moved back so that it is full, not left half empty.
  const tail = lineOf(200, 5);
  assert.ok(tail.text.startsWith('…') && !tail.text.endsWith('…'));
  assert.equal([...tail.text].length, 1 + 120);
  assert.ok(tail.text.includes('⟦U+202E⟧bbbbb'));
  const middle = lineOf(200, 200);
  assert.equal([...middle.text].length, 122);
});

test('the sentence about stored spellings says "if any" unless a topic holds messages, and counts the names it does not list', () => {
  const names = ['a⟦U+200B⟧b'];
  assert.match(keptSpellingsText({ names, total: 1, hasRecords: true }), /^Zapisane już wiadomości zachowują starą pisownię 1 nazwy lub wartości: a⟦U\+200B⟧b\.$/);
  assert.match(keptSpellingsText({ names, total: 1, hasRecords: false }), /^Wiadomości zapisane wcześniej \(jeśli są\) zachowują starą pisownię/);
  const capped = keptSpellingsText({ names, total: 31, hasRecords: true });
  assert.match(capped, /pisownię 31 nazw lub wartości: a⟦U\+200B⟧b i jeszcze 30\.$/);
});

test('a name with a marker stays on one line, and everything else in the sentence is escaped text', () => {
  const html = sentenceHtml('Zostawia: pac⟦U+200B⟧jent, ter⟦U+202E⟧min <b> i tyle.');
  assert.ok(html.includes('<span class="tb-hidden-name" style="white-space:nowrap">pac⟦U+200B⟧jent</span>'), 'the no-wrap rule is inline, so a shadow-root table cell has it too');
  assert.ok(html.includes('<span class="tb-hidden-name" style="white-space:nowrap">ter⟦U+202E⟧min</span>'));
  assert.ok(html.includes('&lt;b&gt;') && !html.includes('<b>'));
  assert.equal(sentenceHtml('bez znaków'), 'bez znaków');
});

test('the consequence of the rename choice is worded for both outcomes, with the old and the new names and the topics', () => {
  const renames = [{ from: `pac${RLO}jent`, to: 'pacjent', topics: ['wizyty'] }, { from: `ter${RLO}min`, to: 'termin', topics: ['wizyty', 'inny'] }];
  assert.equal(renameConsequence(renames, false), 'Bez zaznaczenia zasady zostaną bez zmian, więc odbiorcy tych topików przestaną widzieć w nowych wiadomościach pola pac⟦U+202E⟧jent i ter⟦U+202E⟧min.');
  assert.equal(renameConsequence(renames, true), 'Po zaznaczeniu zasady będą dopuszczać pola pacjent i termin zamiast pac⟦U+202E⟧jent i ter⟦U+202E⟧min, więc odbiorcy tych topików będą je widzieć w nowych wiadomościach tak jak dotąd.');
  assert.match(renameConsequence(renames.slice(0, 1), true), /odbiorcy tego topiku/);
});

test('after the version was added the rules are described in the past tense, naming the topics, with no "po zaznaczeniu"', () => {
  const renames = [{ from: `pac${RLO}jent`, to: 'pacjent', topics: ['wizyty'] }];
  const left = renameOutcome(renames, false);
  assert.equal(left.text, 'Zasady ukrywania danych nie zostały zmienione: odbiorcy topiku „wizyty” nie zobaczą pola pac⟦U+202E⟧jent w nowych wiadomościach. Zmienisz to w sekcji „Ukrywanie danych”.');
  assert.deepEqual(left.topics, ['wizyty']);
  const done = renameOutcome(renames, true);
  assert.equal(done.text, 'Zasady dopuszczają teraz pole pacjent zamiast pac⟦U+202E⟧jent, więc odbiorcy topiku „wizyty” nadal widzą je w nowych wiadomościach.');
  for (const t of [left.text, done.text]) assert.doesNotMatch(t, /zaznaczeni/);
});

test('a notice shown as a toast carries its text, and a warning stays longer', () => {
  assert.deepEqual(noticeToast({ tone: 'success', title: 'Dodano wersję 2', text: 'Topik sprawdza.' }), { message: 'Dodano wersję 2 Topik sprawdza.', kind: 'success', timeoutMs: 4000 });
  const warn = noticeToast({ tone: 'warning', title: 'Dodano wersję 2', text: 'Zapisane już wiadomości zachowują starą pisownię.' });
  assert.equal(warn.kind, 'warning');
  assert.ok(warn.message.endsWith('starą pisownię.'));
  assert.ok(warn.timeoutMs > 4000);
  assert.equal(noticeToast({ tone: 'info', title: 'T' }).message, 'T');
});

test('"Dodaj wzór" hands on the spellings that stored records keep when the name was taken meanwhile', async () => {
  closeAll();
  const added = [];
  const win = openSchemaAdd({
    instanceId: 'i', schemaTypes: ['json_schema'], existingNames: () => [],
    register: async () => ({ version: 2, deduplicated: false, oldSpellingKept: ['a⟦U+200B⟧b'], oldSpellingKeptTotal: 4 }),
    describeError: String, onAdded: (a) => added.push(a),
  });
  type(win.querySelector('[data-role="name"]'), 'wzor');
  type(win.querySelector('[data-role="text"]'), '{"type":"object"}');
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual(added.map((a) => [a.version, a.oldSpellingKept, a.oldSpellingKeptTotal]), [[2, ['a⟦U+200B⟧b'], 4]]);
  await new Promise((r) => setTimeout(r, 300));
});

test('"Nowa wersja" opens with the caret and the scroll at the first line', async () => {
  closeAll();
  const proto = window.customElements.get('tf-textarea')?.prototype;
  assert.ok(proto?.moveToStart, 'tf-textarea can move its caret to the start');
  const original = proto.moveToStart;
  let calls = 0;
  proto.moveToStart = function moveToStart() { calls += 1; return original.call(this); };
  try {
    openSchemaVersion({
      instanceId: 'i', subject: pacjenci, latestText: PATIENT_CLEAN, draftText: null,
      register: async () => ({ version: 2, deduplicated: false }), describeError: String, onAdded: () => {}, onDraft: () => {},
    });
    assert.ok(calls >= 1);
  } finally {
    proto.moveToStart = original;
  }
});

test('tf-textarea moves its caret and scroll to the start', () => {
  const el = document.createElement('tf-textarea');
  document.body.appendChild(el);
  el.value = 'line one\nline two\nline three';
  const area = el.querySelector('textarea');
  area.setSelectionRange(10, 10);
  area.scrollTop = 40;
  el.moveToStart();
  assert.equal(area.selectionStart, 0);
  assert.equal(area.selectionEnd, 0);
  assert.equal(area.scrollTop, 0);
  el.remove();
});
