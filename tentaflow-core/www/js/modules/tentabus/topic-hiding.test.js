// =============================================================================
// File: modules/tentabus/topic-hiding.test.js
// Description: A topic's Ukrywanie danych section (U7, T07): one row per rule
// with its subject, direction and the fields it hides (the server stores the
// ALLOWED fields, the screen shows the other side); where the known fields
// come from (a JSON Schema's properties, the HL7 v2 dictionary, typed in);
// the form of a rule and the exact FieldPolicySet request built from it; the
// "Co się stanie" sentences in the mockups' words; the section in its states
// (loading, error with retry, empty, blocked, filled) and the legend. A real
// HL7 message is run through the default form the way the server reads its
// fields, so a rule built from the dictionary cannot refuse an ordinary
// message.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const {
  policyRows, policyKey, whoTitle, whoSub, hidingCount, topicFormat, readJsonSchema, fieldSource, fieldLabel, fieldPhrase, fieldNameProblem, hl7Suggestion,
  ruleFacts, ruleTableRow, typedProblems, actionOptions, blankForm, formFromRule, serializeForm, parseForm, buildPolicyRequest, formProblem,
  ruleImpact, legendHtml, directionsClosedToKeys, closesTopicToKeys, paintHidingSection,
} = await import('./topic-hiding.js');

// A form's action map has no prototype (a field may be called `__proto__`); compare it as a plain object.
const plain = (form) => ({ ...form, actions: { ...form.actions } });
const norm = (s) => String(s).replace(/[  ]/g, ' ').replace(/\s+/g, ' ').trim();
const words = (html) => { const d = document.createElement('div'); d.innerHTML = String(html).replace(/</g, ' <'); return norm(d.textContent); };
const INSTANCE = 'tentabus-a1b2c3d4';
const TOPIC = 'wizyty';
const NOW = new Date(2026, 8, 30, 12, 0, 0).getTime();

const VISIT_SCHEMA = JSON.stringify({
  type: 'object',
  properties: {
    pacjent: { title: 'dane pacjenta' },
    lekarz: { description: 'Lekarz prowadzący wizytę' },
    termin: {},
    powod: { title: 'powód wizyty' },
  },
});
const SCHEMA = { subject: 'wizyta', version: 5, text: VISIT_SCHEMA };
const json = fieldSource({ format: 'json', schema: SCHEMA });
const hl7 = fieldSource({ format: 'hl7v2', schema: null });
const typed = fieldSource({ format: 'xml', schema: null });

const POLICIES = [
  { subjectType: 'addon', subjectId: 'asystent', direction: 'read', fields: ['lekarz', 'pacjent', 'powod'], requiredFields: [], createdAtMs: 1, updatedAtMs: NOW - 3_600_000, subjectLabel: 'Asystent lekarza', memberCount: null },
  { subjectType: 'group', subjectId: 'g-rej', direction: 'read', fields: ['lekarz', 'pacjent', 'termin'], requiredFields: [], createdAtMs: 1, updatedAtMs: NOW - 86_400_000, subjectLabel: 'Rejestracja', memberCount: 5 },
  { subjectType: 'any', subjectId: '*', direction: 'write', fields: ['lekarz', 'pacjent', 'termin'], requiredFields: ['pacjent', 'termin'], createdAtMs: 1, updatedAtMs: NOW - 5 * 86_400_000, subjectLabel: null, memberCount: null },
  { subjectType: 'user', subjectId: 'u-stary', direction: 'read', fields: ['lekarz'], requiredFields: [], createdAtMs: 1, updatedAtMs: NOW, subjectLabel: null, memberCount: null },
];

// ---------------------------------------------------------------------------
// The rules and their words
// ---------------------------------------------------------------------------

test('one row per rule: everyone first, then groups, people, addons; reading before writing; a missing name is said to be unknown', () => {
  const rows = policyRows(POLICIES);
  assert.deepEqual(rows.map((r) => r.key), ['any:*:write', 'group:g-rej:read', 'user:u-stary:read', 'addon:asystent:read']);
  assert.equal(policyKey(POLICIES[0]), 'addon:asystent:read');
  assert.deepEqual(rows.map(whoTitle), ['Wszyscy', 'Rejestracja', 'Nieznany podmiot', 'Asystent lekarza']);
  assert.deepEqual(rows.map(whoSub), ['Każdy bez własnej zasady i bez zasady swojej grupy', 'Grupa · 5 osób', 'Użytkownik · identyfikator u-stary', 'Addon']);
  assert.equal(hidingCount({ policies: POLICIES }), 4);
  assert.equal(hidingCount({ policies: null }), null, 'nothing is guessed while the rules load');
  assert.equal(hidingCount(null), null);
});

test('the server\'s allowed list is shown as what is hidden: the known fields it leaves out', () => {
  const [, , , assistant] = policyRows(POLICIES);
  const facts = ruleFacts(assistant, json);
  assert.deepEqual(facts.hidden, ['termin']);
  assert.deepEqual(ruleFacts(policyRows(POLICIES)[1], json).hidden, ['powod']);
  const everyone = ruleFacts(policyRows(POLICIES)[0], json);
  assert.deepEqual([everyone.forbidden, everyone.required], [['powod'], ['pacjent', 'termin']]);
  // Without a known list only the allowed fields are known.
  const blind = ruleFacts(policyRows(POLICIES)[2], typed);
  assert.deepEqual([blind.known, blind.visible, blind.hidden], [false, ['lekarz'], []]);
});

test('a rule\'s row: who, what it hides with the plain name of each field, when it applies, when it changed', () => {
  const [everyone, group, , assistant] = policyRows(POLICIES);
  const row = ruleTableRow(assistant, json, NOW);
  assert.equal(words(row.who), 'Asystent lekarza Addon');
  assert.equal(words(row.what), 'termin Ukryj Pola spoza listy: ukryj', 'a reading rule over an open list says it hides what it does not list too');
  assert.equal(words(row.when), 'Odczyt');
  assert.equal(row.changed, 'dziś 11:00');
  assert.equal(row._key, 'addon:asystent:read');
  assert.equal(words(ruleTableRow(group, json, NOW).what), 'powod Ukryj powód wizyty Pola spoza listy: ukryj');
  assert.equal(ruleTableRow(group, json, NOW).changed, 'wczoraj 12:00');
  assert.equal(words(ruleTableRow(everyone, json, NOW).what), 'pacjent Wymagane dane pacjenta termin Wymagane powod Niedozwolone powód wizyty Inne pola albo brak wymaganych: odrzuć wiadomość', 'a write rule names what it requires and what it refuses, and that the rest is refused');
  assert.equal(words(ruleTableRow(everyone, json, NOW).when), 'Zapis');
});

test('a cell names three fields and counts the rest; a rule that hides nothing says so; without a list it names what stays', () => {
  const many = { ...policyRows(POLICIES)[3], fields: [] };
  const what = ruleTableRow(many, json, NOW).what;
  assert.equal(words(what), 'pacjent Ukryj dane pacjenta lekarz Ukryj Lekarz prowadzący wizytę termin Ukryj i jeszcze 1 pole Pola spoza listy: ukryj');
  const none = { ...many, fields: ['pacjent', 'lekarz', 'termin', 'powod'] };
  assert.equal(words(ruleTableRow(none, json, NOW).what), 'Nie ukrywa żadnego ze znanych pól Pola spoza listy: ukryj');
  assert.equal(words(ruleTableRow(many, typed, NOW).what), 'Nie widzi żadnego pola');
  assert.equal(words(ruleTableRow(policyRows(POLICIES)[2], typed, NOW).what), 'Widzi tylko: lekarz');
  const writeBlind = { ...policyRows(POLICIES)[0], fields: ['id', 'nazwisko'], requiredFields: ['id'] };
  assert.equal(words(ruleTableRow(writeBlind, typed, NOW).what), 'Może zapisać tylko: id, nazwisko id Wymagane Inne pola albo brak wymaganych: odrzuć wiadomość');
});

// ---------------------------------------------------------------------------
// The fields a rule can name
// ---------------------------------------------------------------------------

test('a JSON Schema gives its properties in the order it writes them, named by title or description; a branch of allOf counts', () => {
  assert.deepEqual(readJsonSchema(VISIT_SCHEMA).fields, [
    { name: 'pacjent', label: 'dane pacjenta' },
    { name: 'lekarz', label: 'Lekarz prowadzący wizytę' },
    { name: 'termin', label: '' },
    { name: 'powod', label: 'powód wizyty' },
  ]);
  assert.deepEqual(readJsonSchema(JSON.stringify({ allOf: [{ properties: { a: {} } }, { properties: { b: { title: 'B' }, a: { title: 'ignored' } } }] })).fields.map((f) => [f.name, f.label]), [['a', ''], ['b', 'B']]);
  assert.deepEqual(readJsonSchema('{"type":"object"}').fields, []);
  assert.deepEqual(readJsonSchema('not json'), { fields: [], complex: false, closed: false });
  assert.equal(readJsonSchema(JSON.stringify({ properties: { x: { description: 'a'.repeat(200) } } })).fields[0].label.length, 70, 'a long description is cut');
});

test('properties are found through oneOf, anyOf, if / then / else and local $ref (#/$defs, #/definitions), each once', () => {
  const names = (schema) => readJsonSchema(JSON.stringify(schema)).fields.map((f) => f.name);
  assert.deepEqual(names({ properties: { id: {} }, oneOf: [{ properties: { a: {} } }, { properties: { b: {} } }] }), ['id', 'a', 'b']);
  assert.deepEqual(names({ anyOf: [{ properties: { c: {} } }], properties: { id: {} } }), ['id', 'c']);
  assert.deepEqual(names({ if: { properties: { kind: {} } }, then: { properties: { t: {} } }, else: { properties: { e: {} } } }), ['kind', 't', 'e']);
  assert.deepEqual(names({ $ref: '#/$defs/visit', $defs: { visit: { properties: { pacjent: {}, lekarz: {} } } } }), ['pacjent', 'lekarz']);
  assert.deepEqual(names({ allOf: [{ $ref: '#/definitions/base' }], definitions: { base: { properties: { id: {} } } } }), ['id']);
  assert.deepEqual(names({ $ref: '#/$defs/a%20b', $defs: { 'a b': { properties: { x: {} } } } }), ['x'], 'a pointer is decoded');
  const loop = { $ref: '#/$defs/n', $defs: { n: { properties: { v: {} }, allOf: [{ $ref: '#/$defs/n' }] } } };
  assert.deepEqual(readJsonSchema(JSON.stringify(loop)), { fields: [{ name: 'v', label: '' }], complex: false, closed: false }, 'a reference to itself does not loop');
});

test('a pattern with fields the walk cannot list is "complex": the list would be incomplete, so the fields are typed in', () => {
  const complexOf = (schema) => readJsonSchema(JSON.stringify(schema)).complex;
  assert.equal(complexOf({ properties: { a: {} }, $ref: 'https://example.com/other.json' }), true, 'a remote $ref');
  assert.equal(complexOf({ properties: { a: {} }, $ref: '#/$defs/missing' }), true, 'a $ref to nothing');
  assert.equal(complexOf({ properties: { a: {} }, patternProperties: { '^x_': {} } }), true);
  assert.equal(complexOf({ properties: { a: {} }, additionalProperties: { type: 'string' } }), true);
  assert.equal(complexOf({ properties: { a: {} }, additionalProperties: false }), false);
  assert.equal(complexOf({ properties: { a: {} }, additionalProperties: true }), false);
  const source = fieldSource({ format: 'json', schema: { subject: 's', version: 2, text: JSON.stringify({ properties: { a: {} }, patternProperties: { '^x_': {} } }) } });
  assert.deepEqual([source.mode, source.fields, source.complex], ['manual', [], true]);
  const closed = fieldSource({ format: 'json', schema: { subject: 's', version: 2, text: JSON.stringify({ properties: { a: {} }, additionalProperties: false }) } });
  assert.deepEqual([closed.mode, closed.complete], ['schema', true]);
  assert.equal(json.complete, false, 'a pattern that does not close itself may be followed by other fields');
});

test('a pattern the walk cannot follow to the end is "complex" too: a $ref to true / false, unevaluatedProperties, dependentSchemas, nesting past the cap', () => {
  const complexOf = (schema) => readJsonSchema(JSON.stringify(schema)).complex;
  assert.equal(complexOf({ properties: { a: {} }, $ref: '#/$defs/any', $defs: { any: true } }), true, 'a $ref to the boolean schema true says nothing about fields');
  assert.equal(complexOf({ properties: { a: {} }, $ref: '#/$defs/none', $defs: { none: false } }), true);
  assert.equal(complexOf({ properties: { a: {} }, unevaluatedProperties: { type: 'string' } }), true);
  assert.equal(complexOf({ properties: { a: {} }, unevaluatedProperties: false }), false, 'forbidding the rest names no field');
  assert.equal(complexOf({ properties: { a: {} }, dependentSchemas: { a: { properties: { b: {} } } } }), true);
  assert.equal(complexOf({ properties: { a: {} }, dependentSchemas: {} }), false);
  const deep = (levels) => (levels === 0 ? { properties: { leaf: {} } } : { allOf: [deep(levels - 1)] });
  assert.equal(complexOf(deep(10)), false, 'ordinary nesting is walked');
  assert.equal(complexOf(deep(200)), true, 'a pattern nested past the cap is not walked to the end and says so');
});

test('a field called __proto__ is a field like any other: its action is kept, round-trips and reaches the request', () => {
  const schema = fieldSource({ format: 'json', schema: { subject: 's', version: 1, text: '{"properties":{"__proto__":{},"constructor":{},"a":{}}}' } });
  assert.deepEqual(schema.fields.map((f) => f.name), ['__proto__', 'constructor', 'a']);
  const blank = blankForm('read', schema);
  assert.equal(blank.actions.__proto__, 'show');
  assert.equal(Object.getPrototypeOf(blank.actions), null);
  const rule = { subjectType: 'group', subjectId: 'g', direction: 'read', fields: ['a', 'constructor'], requiredFields: [] };
  const form = formFromRule(rule, schema);
  assert.equal(form.actions.__proto__, 'hide', 'not allowed by the stored rule, so hidden — not silently dropped');
  assert.equal(form.actions.constructor, 'show');
  const back = parseForm(serializeForm(form, schema), schema);
  assert.equal(back.actions.__proto__, 'hide');
  const flipped = Object.assign(Object.create(null), form.actions);
  flipped.__proto__ = 'show';
  assert.notEqual(serializeForm(form, schema), serializeForm({ ...form, actions: flipped }, schema), 'the action of __proto__ is part of the form');
  const request = buildPolicyRequest({ instanceId: INSTANCE, topic: TOPIC, subject: rule, direction: 'read', form, source: schema });
  assert.deepEqual(request.fields, ['a', 'constructor']);
  const shown = buildPolicyRequest({ instanceId: INSTANCE, topic: TOPIC, subject: rule, direction: 'read', form: blank, source: schema });
  assert.deepEqual(shown.fields, ['__proto__', 'a', 'constructor']);
});

test('where the known fields come from: the pattern, the HL7 dictionary, or nowhere (typed in); binary content has none', () => {
  assert.equal(json.mode, 'schema');
  assert.deepEqual([json.subject, json.version], ['wizyta', 5]);
  assert.equal(hl7.mode, 'dictionary');
  assert.ok(hl7.fields.length >= 55);
  assert.deepEqual(hl7.fields.find((f) => f.name === 'PID-5'), { name: 'PID-5', label: 'Imię i nazwisko pacjenta' });
  assert.deepEqual(typed, { mode: 'manual', fields: [], implicit: [], complete: false, failed: false, complex: false });
  assert.equal(fieldSource({ format: 'json', schema: null }).mode, 'manual');
  assert.equal(fieldSource({ format: 'json', schema: { failed: true } }).failed, true, 'a pattern that could not be read is said so');
  assert.equal(fieldSource({ format: 'json', schema: { subject: 's', version: 1, text: '{"type":"object"}' } }).mode, 'manual', 'a pattern without properties lists nothing');
  assert.equal(fieldSource({ format: 'binary', schema: null }).mode, 'blocked');
  assert.equal(fieldSource({ format: 'xml', schema: SCHEMA }).mode, 'manual', 'a JSON pattern says nothing about XML');
  assert.deepEqual(['application/json', 'application/xml', 'text/xml', 'application/hl7-v2', 'x-application/hl7-v2+er7', 'application/octet-stream', 'application/fhir+json', ''].map(topicFormat),
    ['json', 'xml', 'xml', 'hl7v2', 'hl7v2', 'binary', 'json', 'json'], 'a content type the server does not know is read as JSON, as the server does');
  assert.equal(fieldLabel(json, 'pacjent'), 'dane pacjenta');
  assert.equal(fieldLabel(hl7, 'PID-7'), 'Data urodzenia');
  assert.equal(fieldPhrase(json, 'pacjent'), 'dane pacjenta (pacjent)');
  assert.equal(fieldPhrase(json, 'termin'), 'termin');
});

test('an address the server would refuse is refused before the request: HL7 SEGMENT-n, an XML name, nothing empty', () => {
  assert.equal(fieldNameProblem('hl7v2', 'PID-5'), null);
  assert.equal(fieldNameProblem('hl7v2', 'ZX1-12'), null);
  assert.equal(fieldNameProblem('hl7v2', 'MSH-1'), 'hl7_msh');
  assert.equal(fieldNameProblem('hl7v2', 'MSH-2'), 'hl7_msh');
  for (const bad of ['PID', 'PID-0', 'PID-05', 'PIDX-5', 'PI-5', 'PID-5.1', 'PID-a']) assert.equal(fieldNameProblem('hl7v2', bad), 'hl7_shape', bad);
  for (const typo of ['pid-5', 'pid5', 'PID5']) assert.equal(fieldNameProblem('hl7v2', typo), 'hl7_case', typo);
  assert.equal(hl7Suggestion('pid5'), 'PID-5');
  assert.equal(hl7Suggestion('msh1'), null, 'the separator field is no suggestion');
  assert.equal(hl7Suggestion('PID-0'), null);
  assert.equal(fieldNameProblem('xml', 'pacjent'), null);
  assert.equal(fieldNameProblem('xml', 'ns:tag'), null);
  assert.equal(fieldNameProblem('xml', '_x-1.y'), null);
  for (const bad of ['1abc', 'has space', 'a<b']) assert.equal(fieldNameProblem('xml', bad), 'xml_name', bad);
  assert.equal(fieldNameProblem('json', 'any key at all'), null, 'every string is a JSON key');
  assert.equal(fieldNameProblem('json', '  '), 'empty');
});

// ---------------------------------------------------------------------------
// The form and the request
// ---------------------------------------------------------------------------

test('a new reading rule shows every known field; a stored one is read back as hidden = known − allowed, the rest typed in', () => {
  assert.deepEqual(plain(blankForm('read', json)).actions, { pacjent: 'show', lekarz: 'show', termin: 'show', powod: 'show' });
  assert.deepEqual(plain(blankForm('write', json)).actions, { pacjent: 'allow', lekarz: 'allow', termin: 'allow', powod: 'allow' });
  const rule = { ...policyRows(POLICIES)[3], fields: ['lekarz', 'pacjent', 'powod', 'telefon'] };
  assert.deepEqual(plain(formFromRule(rule, json)), { actions: { pacjent: 'show', lekarz: 'show', termin: 'hide', powod: 'show' }, extraShown: ['telefon'], extraRequired: [], hiddenImplicit: [] });
  const writeRule = { ...policyRows(POLICIES)[0], fields: ['lekarz', 'pacjent', 'termin', 'x', 'y'], requiredFields: ['pacjent', 'y'] };
  assert.deepEqual(plain(formFromRule(writeRule, json)), {
    actions: { pacjent: 'require', lekarz: 'allow', termin: 'allow', powod: 'forbid' },
    extraShown: ['x'],
    extraRequired: ['y'],
    hiddenImplicit: [],
  });
});

test('a form travels as one string and comes back; text that is not a form is refused', () => {
  const form = { actions: { pacjent: 'show', lekarz: 'hide', termin: 'show', powod: 'show' }, extraShown: ['b', 'a', 'a'], extraRequired: [], hiddenImplicit: ['PID-4'] };
  const text = serializeForm(form, json);
  assert.deepEqual(plain(parseForm(text, json)), { actions: form.actions, extraShown: ['a', 'b'], extraRequired: [], hiddenImplicit: ['PID-4'] });
  assert.equal(serializeForm(blankForm('read', json), json), serializeForm(blankForm('read', json), json));
  assert.notEqual(serializeForm(blankForm('read', json), json), serializeForm(blankForm('write', json), json));
  assert.equal(parseForm('nope', json), null);
  assert.equal(parseForm('[1]', json), null);
});

test('FieldPolicySet carries the ALLOWED fields: hidden ones left out, required ones a subset of the allowed', () => {
  const where = { instanceId: INSTANCE, topic: TOPIC, subject: { subjectType: 'group', subjectId: 'g-rej' } };
  const read = { actions: { pacjent: 'show', lekarz: 'show', termin: 'show', powod: 'hide' }, extraShown: [' telefon ', 'pacjent'], extraRequired: ['ignored'] };
  assert.deepEqual(buildPolicyRequest({ ...where, direction: 'read', form: read, source: json }), {
    instanceId: INSTANCE, topic: TOPIC, subjectType: 'group', subjectId: 'g-rej', direction: 'read',
    fields: ['lekarz', 'pacjent', 'telefon', 'termin'], requiredFields: [],
  });
  const write = { actions: { pacjent: 'require', lekarz: 'allow', termin: 'forbid', powod: 'allow' }, extraShown: ['x'], extraRequired: ['y'] };
  assert.deepEqual(buildPolicyRequest({ ...where, subject: { subjectType: 'any', subjectId: '*' }, direction: 'write', form: write, source: json }), {
    instanceId: INSTANCE, topic: TOPIC, subjectType: 'any', subjectId: '*', direction: 'write',
    fields: ['lekarz', 'pacjent', 'powod', 'x', 'y'], requiredFields: ['pacjent', 'y'],
  });
  // A topic whose fields nobody listed: only what is typed in is allowed.
  assert.deepEqual(buildPolicyRequest({ ...where, direction: 'read', form: { actions: {}, extraShown: ['id'], extraRequired: [] }, source: typed }).fields, ['id']);
});

/** The positions the server lists for a message (`Hl7V2Format::list_fields`): MSH-3 on, and every position of every other segment, empty or not. */
function serverFieldList(message) {
  const segments = message.split(/\r\n?|\n/).filter(Boolean);
  const fsep = segments[0][3];
  const out = new Set();
  segments[0].slice(4).split(fsep).forEach((_, idx) => { if (idx >= 1) out.add(`MSH-${idx + 2}`); });
  for (const segment of segments.slice(1)) {
    segment.slice(4).split(fsep).forEach((_, idx) => out.add(`${segment.slice(0, 3)}-${idx + 1}`));
  }
  return [...out];
}

const closedJson = fieldSource({ format: 'json', schema: { ...SCHEMA, text: JSON.stringify({ ...JSON.parse(VISIT_SCHEMA), additionalProperties: false }) } });

test('a rule needs something to do: one hidden field (or a required or refused one), or typed fields when none are listed', () => {
  const ok = (direction, form, source, format) => formProblem({ direction, form, source, format });
  const blank = blankForm('read', closedJson);
  assert.match(ok('read', blank, closedJson, 'json'), /Ustaw co najmniej jedno pole na „Ukryj”\. Lista zawiera wszystkie pola tego topiku/);
  assert.equal(ok('read', { ...blank, actions: { ...blank.actions, powod: 'hide' } }, closedJson, 'json'), null);
  assert.match(ok('read', { actions: {}, extraShown: [], extraRequired: [] }, typed, 'xml'), /Wpisz co najmniej jedno pole, które ma zostać widoczne/);
  assert.equal(ok('read', { actions: {}, extraShown: ['id'], extraRequired: [] }, typed, 'xml'), null);
  const write = blankForm('write', closedJson);
  assert.match(ok('write', write, closedJson, 'json'), /„Wymagane” albo „Niedozwolone”\. Lista zawiera wszystkie pola tego topiku/);
  assert.equal(ok('write', { ...write, actions: { ...write.actions, termin: 'require' } }, closedJson, 'json'), null);
  assert.equal(ok('write', { ...write, extraRequired: ['id'] }, closedJson, 'json'), null);
  assert.match(ok('write', { actions: {}, extraShown: [], extraRequired: [] }, typed, 'xml'), /Wpisz co najmniej jedno pole, które wolno zapisać/);
  // A typed address the format refuses is named.
  assert.match(ok('read', { ...blankForm('read', hl7), extraShown: ['PID-0'] }, hl7, 'hl7v2'), /„PID-0” nie jest adresem pola HL7\. Adres ma postać SEGMENT-numer, np\. PID-5\./);
  assert.match(ok('read', { ...blankForm('read', hl7), extraShown: ['MSH-2'] }, hl7, 'hl7v2'), /MSH-2 to separatory samej wiadomości/);
  assert.match(ok('read', { actions: {}, extraShown: ['1a'], extraRequired: [] }, typed, 'xml'), /nie jest nazwą elementu XML/);
});

test('over a list that is not complete, a rule that touches no listed field still hides (or refuses) the rest, so it is allowed and says so', () => {
  const ok = (direction, source, format) => formProblem({ direction, form: blankForm(direction, source), source, format });
  assert.equal(ok('read', json, 'json'), null, 'the pattern may be followed by other fields');
  assert.equal(ok('write', json, 'json'), null);
  assert.equal(ok('read', hl7, 'hl7v2'), null);
  assert.equal(ok('write', hl7, 'hl7v2'), null);
  assert.deepEqual(ruleImpact({ who: 'Księgowość', direction: 'read', form: blankForm('read', json), current: null, source: json }),
    ['Dla „Księgowość” znikną pola spoza listy. Wszystkie pola z listy zostają widoczne.']);
  assert.deepEqual(ruleImpact({ who: 'Księgowość', direction: 'write', form: blankForm('write', json), current: null, source: json }),
    ['Od „Księgowość” zostanie przyjęta wiadomość tylko z polami z listy. Wiadomość z innym polem zostanie odrzucona.']);
});

test('every typed address that is wrong is named, a near miss gets its probable form', () => {
  const problem = formProblem({ direction: 'read', form: { ...blankForm('read', hl7), extraShown: ['PID-0', 'pid5', 'ZZZ-3'] }, source: hl7, format: 'hl7v2' });
  assert.match(problem, /„PID-0” nie jest adresem pola HL7/);
  assert.match(problem, /„pid5” wygląda na literówkę\. Czy chodziło o PID-5\?/);
  assert.doesNotMatch(problem, /ZZZ-3/, 'a well-formed address is not a problem');
});

test('a typed "required" name that is also a listed field makes that row required; the sentences say what the request carries', () => {
  const where = { instanceId: INSTANCE, topic: TOPIC, subject: { subjectType: 'user', subjectId: 'u-1' }, direction: 'write', source: json };
  const form = { ...blankForm('write', json), extraRequired: [' termin ', 'nowe'] };
  const request = buildPolicyRequest({ ...where, form });
  assert.deepEqual(request.requiredFields, ['nowe', 'termin']);
  assert.ok(request.fields.includes('termin') && request.fields.includes('nowe'));
  const lines = ruleImpact({ who: 'Jan', direction: 'write', form, current: null, source: json });
  assert.match(lines.join(' '), /brakuje któregokolwiek z pól: nowe, termin, zostanie odrzucona/);
  // A listed field refused in its row cannot also be typed in as required.
  const clash = { ...blankForm('write', json), actions: { ...blankForm('write', json).actions, termin: 'forbid' }, extraRequired: ['termin'] };
  assert.match(formProblem({ direction: 'write', form: clash, source: json, format: 'json' }), /„termin” jest na liście pól i ma tam inne ustawienie niż wpisane niżej\./);
  const hiddenClash = { ...blankForm('read', json), actions: { ...blankForm('read', json).actions, termin: 'hide' }, extraShown: ['termin'] };
  assert.match(formProblem({ direction: 'read', form: hiddenClash, source: json, format: 'json' }), /„termin” jest na liście pól/);
  // A typed name of a field that is shown anyway changes nothing and is not a problem.
  assert.equal(formProblem({ direction: 'read', form: { ...hiddenClash, actions: { ...hiddenClash.actions, termin: 'show', powod: 'hide' } }, source: json, format: 'json' }), null);
});

test('HL7: the dictionary\'s default form allows an ordinary message — every position the server lists, MSH-11 and PID-4 included', () => {
  const where = { instanceId: INSTANCE, topic: 'adt', subject: { subjectType: 'any', subjectId: '*' } };
  const message = [
    'MSH|^~\\&|REJ|PRZYCHODNIA|LAB|SZPITAL|20260930120000||ADT^A01|MSG0001|P|2.5.1',
    'EVN|A01|20260930120000',
    'PID|1||123456^^^PESEL||Kowalska^Anna||19800101|F|||ul. Kwiatowa 1^^Lublin||600100200|||||||||||||||||',
    'PV1|1|I|OIOM^101^1||||1234^Nowak^Jan||||||||||||V0001',
    'OBX|1|NM|8867-4^HR||72|bpm|60-100|N|||F',
  ].join('\r');
  const present = serverFieldList(message);
  assert.ok(present.includes('MSH-11') && present.includes('MSH-12') && present.includes('PID-4') && present.includes('EVN-1') && present.includes('PID-1'));
  const refused = (request) => present.filter((f) => !request.fields.includes(f));
  const writeAll = buildPolicyRequest({ ...where, direction: 'write', form: blankForm('write', hl7), source: hl7 });
  assert.deepEqual(refused(writeAll), [], 'a rule that forbids nothing refuses no field of the message');
  const form = { ...blankForm('write', hl7), actions: { ...blankForm('write', hl7).actions, 'PID-19': 'forbid' } };
  assert.deepEqual(refused(buildPolicyRequest({ ...where, direction: 'write', form, source: hl7 })), ['PID-19'], 'forbidding PID-19 refuses a message that carries it, and nothing else');
});

test('HL7: a READING rule hides every position it does not list — the unnamed ones too — exactly as its window says', () => {
  const where = { instanceId: INSTANCE, topic: 'adt', subject: { subjectType: 'group', subjectId: 'g-rej' } };
  const readForm = { ...blankForm('read', hl7), actions: { ...blankForm('read', hl7).actions, 'PID-5': 'hide' } };
  const request = buildPolicyRequest({ ...where, direction: 'read', form: readForm, source: hl7 });
  const named = hl7.fields.map((f) => f.name);
  assert.deepEqual(request.fields, [...named.filter((n) => n !== 'PID-5')].sort(), 'only the fields shown in the window are allowed');
  assert.equal(request.fields.filter((n) => hl7.implicit.includes(n)).length, 0, 'not one unnamed position (PID-1, PID-4, PID-20, NK1-*, IN1-*, ...) is let through');
  assert.ok(hl7.implicit.length > 250, 'the dictionary has hundreds of them');
  // The same form as a WRITING rule does allow them, or no real message would pass.
  const write = buildPolicyRequest({ ...where, direction: 'write', form: { ...blankForm('write', hl7), actions: { ...blankForm('write', hl7).actions, 'PID-5': 'forbid' } }, source: hl7 });
  assert.ok(hl7.implicit.every((n) => write.fields.includes(n)));
  // Only a writing rule is described with the unnamed positions.
  const readFacts = ruleFacts({ direction: 'read', fields: request.fields, requiredFields: [] }, hl7);
  assert.deepEqual(readFacts.hidden, ['PID-5']);
  const writeFacts = ruleFacts({ direction: 'write', fields: write.fields, requiredFields: [] }, hl7);
  assert.deepEqual(writeFacts.forbidden, ['PID-5']);
});

test('HL7: a stored WRITING rule that leaves an unnamed position out keeps leaving it out when it is edited, and the table names it', () => {
  const row = { ...policyRows(POLICIES)[0], subjectType: 'group', subjectId: 'g', subjectLabel: 'Lab', direction: 'write', fields: hl7.fields.map((f) => f.name).concat(hl7.implicit.filter((n) => n !== 'PID-4')), requiredFields: [] };
  const form = formFromRule(row, hl7);
  assert.deepEqual(form.hiddenImplicit, ['PID-4']);
  assert.deepEqual(form.extraShown, [], 'an unnamed position is not a typed-in field of a writing rule');
  assert.ok(!buildPolicyRequest({ instanceId: INSTANCE, topic: TOPIC, subject: row, direction: 'write', form, source: hl7 }).fields.includes('PID-4'));
  assert.match(words(ruleTableRow(row, hl7, NOW).what), /^PID-4 Niedozwolone/);
});

test('HL7: a stored READING rule that lets unnamed positions through shows them as typed-in fields to take out, and the table names them', () => {
  const row = { ...policyRows(POLICIES)[3], subjectType: 'group', subjectId: 'g', subjectLabel: 'Lab', direction: 'read', fields: hl7.fields.map((f) => f.name).concat(['PID-1', 'PID-4']), requiredFields: [] };
  const form = formFromRule(row, hl7);
  assert.deepEqual(form.extraShown, ['PID-1', 'PID-4'], 'visible, so the administrator sees them and can remove them');
  assert.deepEqual(form.hiddenImplicit, []);
  const kept = buildPolicyRequest({ instanceId: INSTANCE, topic: TOPIC, subject: row, direction: 'read', form, source: hl7 });
  assert.ok(kept.fields.includes('PID-1') && kept.fields.includes('PID-4'), 'saved as it is, the rule still lets them through');
  const removed = buildPolicyRequest({ instanceId: INSTANCE, topic: TOPIC, subject: row, direction: 'read', form: { ...form, extraShown: ['PID-1'] }, source: hl7 });
  assert.ok(removed.fields.includes('PID-1') && !removed.fields.includes('PID-4'));
  const what = words(ruleTableRow(row, hl7, NOW).what);
  assert.match(what, /Nie ukrywa żadnego ze znanych pól/);
  assert.match(what, /Zostawia też widoczne: PID-1, PID-4/);
  assert.match(what, /Pola spoza listy: ukryj$/, 'the row says the rule hides every position it does not allow, not only the ones it names');
  assert.deepEqual(ruleFacts(row, hl7).visibleOutside, ['PID-1', 'PID-4']);
});

test('a rule is the first for chosen subjects in its direction only when no rule of that direction exists yet', () => {
  const rows = policyRows(POLICIES);
  assert.equal(closesTopicToKeys({ rules: [], subjectType: 'group', direction: 'read' }), true);
  assert.equal(closesTopicToKeys({ rules: rows, subjectType: 'group', direction: 'write' }), false, 'writing has the rule for everyone');
  assert.equal(closesTopicToKeys({ rules: rows, subjectType: 'user', direction: 'read' }), false, 'already closed: nothing new to say');
  assert.equal(closesTopicToKeys({ rules: [], subjectType: 'any', direction: 'read' }), false);
});

// ---------------------------------------------------------------------------
// "Co się stanie"
// ---------------------------------------------------------------------------

test('"Co się stanie" for a new reading rule, a change, and a topic without a list', () => {
  const blank = blankForm('read', json);
  const hide = { ...blank, actions: { ...blank.actions, powod: 'hide', termin: 'hide' } };
  assert.deepEqual(ruleImpact({ who: 'Rejestracja', direction: 'read', form: hide, current: null, source: json }), [
    'Dla „Rejestracja” znikną z wiadomości pola: termin, powód wizyty (powod).',
    'Pozostałe pola z listy bez zmian.',
    'Pola spoza listy też znikną, chyba że je dopiszesz.',
  ]);
  const before = formFromRule(policyRows(POLICIES)[1], json);
  const after = { ...before, actions: { ...before.actions, powod: 'show', termin: 'hide' } };
  assert.deepEqual(ruleImpact({ who: 'Rejestracja', direction: 'read', form: after, current: before, source: json }), [
    'Dla „Rejestracja” znikną z wiadomości pola: termin.',
    'Dla „Rejestracja” pojawią się w wiadomościach pola: powód wizyty (powod).',
    'Pozostałe pola z listy bez zmian.',
    'Pola spoza listy też znikną, chyba że je dopiszesz.',
  ]);
  assert.deepEqual(ruleImpact({ who: 'Rejestracja', direction: 'read', form: before, current: before, source: json }), [], 'no change, nothing to say');
  // The rule for everyone has no name to quote.
  assert.deepEqual(ruleImpact({ who: 'Wszyscy', everyone: true, direction: 'read', form: { actions: {}, extraShown: ['id', 'nazwa'], extraRequired: [] }, current: null, source: typed }),
    ['Dla wszystkich widoczne będą tylko pola: id, nazwa. Wszystkie inne zostaną ukryte.']);
});

test('"Co się stanie" for a writing rule: whole messages are refused, and the sentences hold for a list or typed fields', () => {
  const form = { actions: { pacjent: 'require', lekarz: 'allow', termin: 'forbid', powod: 'allow' }, extraShown: [], extraRequired: [] };
  assert.deepEqual(ruleImpact({ who: 'Księgowość', direction: 'write', form, current: null, source: json }), [
    'Wiadomość od „Księgowość”, w której jest którekolwiek z pól: termin, zostanie odrzucona i nie trafi do topiku.',
    'Wiadomość od „Księgowość”, w której brakuje któregokolwiek z pól: dane pacjenta (pacjent), zostanie odrzucona i nie trafi do topiku.',
    'Odrzucona zostaje cała wiadomość, a nie samo pole.',
    'Wiadomość z polem spoza listy też zostanie odrzucona, chyba że dopiszesz to pole.',
  ]);
  assert.deepEqual(ruleImpact({ who: 'Księgowość', direction: 'write', form: { actions: {}, extraShown: ['id'], extraRequired: ['nr'] }, current: null, source: typed }), [
    'Od „Księgowość” zostanie przyjęta wiadomość tylko z polami: id, nr. Wiadomość z innym polem zostanie odrzucona.',
    'Wiadomość od „Księgowość”, w której brakuje któregokolwiek z pól: nr, zostanie odrzucona i nie trafi do topiku.',
    'Odrzucona zostaje cała wiadomość, a nie samo pole.',
  ]);
});

test('the note after the window closed does not tell anyone to "add them below"; a closed pattern has no "other fields" to speak of', () => {
  const blank = blankForm('read', json);
  const hide = { ...blank, actions: { ...blank.actions, powod: 'hide' } };
  const saved = ruleImpact({ who: 'Rejestracja', saved: true, direction: 'read', form: hide, current: null, source: json });
  assert.equal(saved.at(-1), 'Pola spoza listy też znikną.');
  assert.ok(!saved.join(' ').includes('chyba że'), 'the window is gone: nowhere to add anything');
  const wForm = { actions: { pacjent: 'require', lekarz: 'allow', termin: 'forbid', powod: 'allow' }, extraShown: [], extraRequired: [] };
  const wSaved = ruleImpact({ who: 'Księgowość', saved: true, direction: 'write', form: wForm, current: null, source: json });
  assert.equal(wSaved.at(-1), 'Wiadomość z polem spoza listy też zostanie odrzucona.');
  assert.ok(!wSaved.join(' ').includes('chyba że'));
  const closedJson = fieldSource({ format: 'json', schema: { subject: 's', version: 1, text: JSON.stringify({ properties: { a: {}, b: {} }, additionalProperties: false }) } });
  const closedHide = { ...blankForm('read', closedJson), actions: { a: 'hide', b: 'show' } };
  assert.deepEqual(ruleImpact({ who: 'Rejestracja', direction: 'read', form: closedHide, current: null, source: closedJson }), [
    'Dla „Rejestracja” znikną z wiadomości pola: a.',
    'Pozostałe pola z listy bez zmian.',
  ], 'no sentence about fields outside a list that is every field there can be');
});

test('HL7 writing sentences say the other positions of the listed segments are accepted and only other segments are refused', () => {
  const form = { ...blankForm('write', hl7), actions: { ...blankForm('write', hl7).actions, 'PID-19': 'forbid' } };
  const lines = ruleImpact({ who: 'Lab', direction: 'write', form, current: null, source: hl7 });
  assert.equal(lines.at(-1), 'Pozostałe pozycje segmentów z listy (np. PID-1, PID-4, PV1-1) są przyjmowane. Wiadomość z innym segmentem, np. segmentem Z, zostanie odrzucona, chyba że dopiszesz jego adres.');
  assert.ok(!lines.join(' ').includes('Wiadomość z polem spoza listy'), 'the JSON sentence would be false here');
  const untouched = ruleImpact({ who: 'Lab', direction: 'write', form: blankForm('write', hl7), current: null, source: hl7 });
  assert.match(untouched[0], /^Od „Lab” będą przyjmowane wiadomości z segmentami z listy, także w ich pozostałych pozycjach/);
  const saved = ruleImpact({ who: 'Lab', saved: true, direction: 'write', form, current: null, source: hl7 });
  assert.equal(saved.at(-1), 'Pozostałe pozycje segmentów z listy (np. PID-1, PID-4, PV1-1) są przyjmowane. Wiadomość z innym segmentem zostanie odrzucona.');
  // A reading rule says what its window says: what is not listed is hidden, positions nobody named included.
  const read = ruleImpact({ who: 'Lab', direction: 'read', form: { ...blankForm('read', hl7), actions: { ...blankForm('read', hl7).actions, 'PID-5': 'hide' } }, current: null, source: hl7 });
  assert.equal(read.at(-1), 'Pola spoza listy też znikną, chyba że je dopiszesz.');
});

test('every typed address the format refuses is named with its reason, in the order typed', () => {
  assert.deepEqual(typedProblems('hl7v2', ['PID-5', 'MSH-2', 'pid5', 'PID-0']).map((p) => p.name), ['MSH-2', 'pid5', 'PID-0']);
  assert.match(typedProblems('hl7v2', ['pid5'])[0].text, /Czy chodziło o PID-5\?/);
  assert.deepEqual(typedProblems('json', ['a', 'b c']), [], 'a JSON name can be anything');
  assert.equal(formProblem({ direction: 'read', form: { actions: {}, extraShown: ['PID-0', 'pid5'], extraRequired: [] }, source: hl7, format: 'hl7v2' }), typedProblems('hl7v2', ['PID-0', 'pid5']).map((p) => p.text).join(' '));
});

test('changing a writing rule says what changes: what is newly refused, newly accepted and no longer required', () => {
  const stored = { ...policyRows(POLICIES)[0], fields: ['lekarz', 'pacjent', 'termin', 'stary'], requiredFields: ['pacjent', 'termin'] };
  const before = formFromRule(stored, json);
  const after = { ...before, actions: { ...before.actions, powod: 'allow', lekarz: 'forbid', termin: 'allow' }, extraShown: ['x'] };
  assert.deepEqual(ruleImpact({ who: 'Wszyscy', everyone: true, direction: 'write', form: after, current: before, source: json }), [
    'Wiadomość od wszystkich, w której jest którekolwiek z pól: Lekarz prowadzący wizytę (lekarz), stary, zostanie odrzucona i nie trafi do topiku.',
    'Od wszystkich będą przyjmowane wiadomości z polami: powód wizyty (powod), x.',
    'Od wszystkich przestanie być wymagane: termin.',
    'Odrzucona zostaje cała wiadomość, a nie samo pole.',
    'Wiadomość z polem spoza listy też zostanie odrzucona, chyba że dopiszesz to pole.',
  ], 'the typed-in "stary" that was allowed and is no longer typed is now refused');
  assert.deepEqual(ruleImpact({ who: 'Wszyscy', direction: 'write', form: before, current: before, source: json }), [], 'nothing changed, nothing to say');
});

// ---------------------------------------------------------------------------
// What the server offers
// ---------------------------------------------------------------------------

test('reading offers "Pokaż" and "Ukryj" only: the wire has no per-field action to carry "Zamaskuj" or "Zahaszuj"', () => {
  assert.deepEqual(actionOptions('read').map((o) => [o.value, o.label]), [['show', 'Pokaż'], ['hide', 'Ukryj']]);
  assert.deepEqual(actionOptions('write').map((o) => o.label), ['Dozwolone', 'Wymagane', 'Niedozwolone'], 'writing has no hiding action');
});

test('the legend explains "ukryj", "odrzuć wiadomość" and which rule wins — and nothing the screen cannot do', () => {
  const plain = words(legendHtml());
  assert.match(plain, /Ukryj — Pole znika z wiadomości\./);
  assert.match(plain, /Odrzuć wiadomość — Przy zapisie: wiadomość z niedozwolonym polem albo bez wymaganego nie trafi do topiku\./);
  assert.match(plain, /Zasada osoby wygrywa z zasadą jej grupy/);
  assert.doesNotMatch(plain, /Zamaskuj|Zahaszuj/);
});

test('a direction with rules for chosen subjects and none for everyone is closed to API keys', () => {
  const rows = policyRows(POLICIES);
  assert.deepEqual(directionsClosedToKeys(rows), ['read'], 'reading has a group, a person and an addon; writing has the rule for everyone');
  assert.deepEqual(directionsClosedToKeys(rows.filter((r) => r.subjectType !== 'any')), ['read']);
  assert.deepEqual(directionsClosedToKeys([]), []);
});

// ---------------------------------------------------------------------------
// The section
// ---------------------------------------------------------------------------

function paint({ hidingData, contentType = 'application/json', access = { canRead: true, canAdmin: true }, notice = null, host = document.createElement('div') }) {
  document.body.appendChild(host);
  const moves = [];
  paintHidingSection(host, { topic: { name: TOPIC, contentType }, access, notice, nowMs: NOW, hidingData }, { go: (a) => moves.push(a) });
  return { host, moves };
}

test('while the rules load the section says so and offers no rule; "Dodaj zasadę" waits for the pattern too', () => {
  const { host } = paint({ hidingData: { policies: null, policiesError: null, schema: null, schemaSettled: false } });
  assert.ok(host.querySelector('[data-role="state"] tf-spinner'));
  assert.ok(host.querySelector('[data-role="add"]').hasAttribute('disabled'));
  assert.ok(host.querySelector('[data-role="preview"]').hasAttribute('disabled'));
  assert.equal(host.querySelector('[data-role="rules"]').hidden, true);
  const waiting = paint({ hidingData: { policies: [], policiesError: null, schema: null, schemaSettled: false } });
  assert.ok(waiting.host.querySelector('[data-role="add"]').hasAttribute('disabled'), 'the fields are not known yet');
  assert.ok(waiting.host.querySelector('tf-empty-state [data-go="hiding-add"]').hasAttribute('disabled'));
});

test('a failed load keeps the retry in the section (T12), marked for the shell to load the rules again', () => {
  const { host } = paint({ hidingData: { policies: null, policiesError: 'Brak uprawnień do tej operacji.', schema: null, schemaSettled: true } });
  assert.match(norm(host.querySelector('[data-role="state"]').textContent), /Brak uprawnień do tej operacji\./);
  const retry = host.querySelector('[data-role="state"] [data-go="hiding-reload"]');
  assert.equal(retry.textContent, 'Spróbuj ponownie');
  assert.equal(retry.hasAttribute('disabled'), false);
  assert.equal(host.querySelector('[data-role="rules"]').hidden, true);
});

test('a topic without rules shows the empty state (T11) whose action opens the add window', () => {
  const { host } = paint({ hidingData: { policies: [], policiesError: null, schema: SCHEMA, schemaSettled: true } });
  const empty = host.querySelector('[data-role="state"] tf-empty-state');
  assert.equal(empty.getAttribute('title'), 'Ten topik nie ma zasad ukrywania danych');
  assert.equal(empty.getAttribute('message'), 'Każdy, kto może czytać ten topik, widzi całe wiadomości. Dodaj zasadę, aby ukryć wybrane pola.');
  const action = empty.querySelector('[data-go="hiding-add"]');
  assert.equal(action.textContent, 'Dodaj zasadę');
  assert.equal(action.hasAttribute('disabled'), false);
  assert.equal(host.querySelector('[data-role="rules"]').hidden, true);
  assert.equal(host.querySelector('[data-role="legend"]').hidden, true);
  assert.equal(host.querySelector('[data-role="add"]').hasAttribute('disabled'), false);
});

test('a binary topic has no fields to name: the section says so and "Dodaj zasadę" is closed', () => {
  const { host } = paint({ contentType: 'application/octet-stream', hidingData: { policies: [], policiesError: null, schema: null, schemaSettled: true } });
  const empty = host.querySelector('[data-role="state"] tf-empty-state');
  assert.equal(empty.getAttribute('title'), 'Zasady tego topiku nie są tu edytowane');
  assert.match(empty.getAttribute('message'), /nie edytuje się na tym ekranie, bo jego treść jest binarna i nie da się jej odczytać jako pól/);
  assert.doesNotMatch(empty.getAttribute('message'), /nie działają/, 'the server does apply rules to such a topic; the screen just does not edit them');
  const open = empty.querySelector('[data-go="section"]');
  assert.equal(open.getAttribute('data-section'), 'settings');
  assert.equal(open.textContent, 'Otwórz ustawienia topiku', 'the button opens the settings, it does not promise a change of content type');
  assert.ok(host.querySelector('[data-role="add"]').hasAttribute('disabled'));
  assert.equal(host.querySelector('[data-role="add"]').getAttribute('title'), empty.getAttribute('message'));
  const preview = host.querySelector('[data-role="preview"]');
  assert.ok(preview.hasAttribute('disabled'), 'a binary message has no fields to preview');
  assert.equal(preview.getAttribute('title'), 'Podgląd jest niedostępny: treść tego topiku nie jest czytelna jako pola.');
});

test('the filled section: count, columns, one row per rule with Zmień and Usuń, the legend, who the keys cannot reach', () => {
  const { host, moves } = paint({ hidingData: { policies: POLICIES, policiesError: null, schema: SCHEMA, schemaSettled: true }, notice: { tone: 'success', title: 'Zapisano zasadę', text: 'Dla „Rejestracja” znikną z wiadomości pola: powód.' } });
  assert.equal(host.querySelector('[data-role="count"] tf-chip').getAttribute('label'), '4');
  assert.equal(host.querySelector('[data-role="notice"] tf-alert').getAttribute('title'), 'Zapisano zasadę');
  const table = host.querySelector('[data-role="rules"]');
  assert.equal(table.hidden, false);
  assert.deepEqual([...table.querySelectorAll('tf-column')].map((c) => c.getAttribute('label')), ['Kto', 'Co widzi inaczej', 'Kiedy', 'Zmieniono']);
  assert.equal(table.rows.length, 4);
  assert.equal(words(table.rows[0].who), 'Wszyscy Każdy bez własnej zasady i bez zasady swojej grupy');
  const actions = table.rowActions(table.rows[1], 1);
  assert.deepEqual([...actions.querySelectorAll('tf-button')].map((b) => b.textContent), ['Zmień', 'Usuń']);
  actions.querySelector('[data-act="change"]').click();
  assert.deepEqual(moves.at(-1), { kind: 'hiding-change', rule: 'group:g-rej:read' });
  actions.querySelector('[data-act="remove"]').click();
  assert.deepEqual(moves.at(-1), { kind: 'hiding-remove', rule: 'group:g-rej:read' });
  assert.equal(host.querySelector('[data-role="legend"]').hidden, false);
  assert.match(norm(host.querySelector('[data-role="legend"]').textContent), /^Ukryj — Pole znika z wiadomości\.Odrzuć wiadomość/);
  assert.match(norm(host.querySelector('[data-role="keys-note"]').textContent), /Systemy zewnętrzne z kluczem API mają zamknięty ten topik w zakresie odczytu: są w nim zasady tylko dla wybranych osób, grup lub addonów, a nie ma zasady dla wszystkich\./);
  assert.equal(host.querySelector('[data-role="state"]').textContent.trim(), '');
});

test('a repaint with unchanged rules keeps the table\'s buttons', () => {
  const host = document.createElement('div');
  paint({ host, hidingData: { policies: POLICIES, policiesError: null, schema: SCHEMA, schemaSettled: true } });
  const table = host.querySelector('[data-role="rules"]');
  const rowsBefore = table.rows;
  paint({ host, hidingData: { policies: POLICIES, policiesError: null, schema: SCHEMA, schemaSettled: true } });
  assert.equal(table.rows, rowsBefore, 'an unchanged poll does not hand the table its rows again');
});

test('previewing is closed with its reason to an administrator who may not read the topic', () => {
  const { host } = paint({ access: { canRead: false, canAdmin: true }, hidingData: { policies: POLICIES, policiesError: null, schema: SCHEMA, schemaSettled: true } });
  const preview = host.querySelector('[data-role="preview"]');
  assert.ok(preview.hasAttribute('disabled'));
  assert.match(preview.getAttribute('title'), /Podgląd wiadomości wymaga prawa czytania topiku wizyty\./);
  assert.equal(host.querySelector('[data-role="add"]').hasAttribute('disabled'), false, 'managing rules needs administration only');
});
