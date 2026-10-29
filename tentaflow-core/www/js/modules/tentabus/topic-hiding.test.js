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
  ruleImpact, legendHtml, directionsClosedToKeys, closesTopicToKeys, paintHidingSection, readXsdFields, keyNote, keyAwaited, keyButtonProblem,
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
  assert.equal(words(what), 'pacjent Ukryj dane pacjenta lekarz Ukryj Lekarz prowadzący wizytę termin Ukryj powod Ukryj powód wizyty Pola spoza listy: ukryj', 'a fourth field is named rather than counted as "one more"');
  const five = { ...many, fields: ['x'], maskedFields: ['x'] };
  // The cell cuts the plain "Ukryj" lines, never the masked or hashed ones.
  assert.equal(words(ruleTableRow(five, json, NOW).what), 'x Zamaskuj pacjent Ukryj dane pacjenta lekarz Ukryj Lekarz prowadzący wizytę i jeszcze 2 pola Pola spoza listy: ukryj');
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
  assert.deepEqual(typed, { mode: 'manual', fields: [], implicit: [], complete: false, failed: false, complex: false, xsd: false });
  assert.equal(fieldSource({ format: 'json', schema: null }).mode, 'manual');
  assert.equal(fieldSource({ format: 'json', schema: { failed: true } }).failed, true, 'a pattern that could not be read is said so');
  assert.equal(fieldSource({ format: 'json', schema: { subject: 's', version: 1, text: '{"type":"object"}' } }).mode, 'manual', 'a pattern without properties lists nothing');
  for (const schemaType of ['avro', 'protobuf', 'thrift']) {
    assert.equal(fieldSource({ format: 'json', schema: { subject: 's', schemaType } }).mode, 'blocked', `${schemaType} pattern: the server refuses rules`);
  }
  assert.equal(fieldSource({ format: 'json', schema: { subject: 's', schemaType: 'json_schema', version: 1, text: '{"type":"object","properties":{"a":{"type":"string"}}}' } }).mode, 'schema');
  assert.equal(fieldSource({ format: 'json', schema: null }).mode, 'manual', 'no pattern, no block: octet-stream alone never closes the screen');
  assert.equal(fieldSource({ format: 'xml', schema: SCHEMA }).mode, 'manual', 'a JSON pattern says nothing about XML');
  assert.deepEqual(['application/json', 'application/xml', 'text/xml', 'application/hl7-v2', 'x-application/hl7-v2+er7', 'application/octet-stream', 'application/fhir+json', ''].map(topicFormat),
    ['json', 'xml', 'xml', 'hl7v2', 'hl7v2', 'json', 'json', 'json'], 'a content type the server does not know, octet-stream included, is read as JSON, as the server does');
  assert.equal(fieldLabel(json, 'pacjent'), 'dane pacjenta');
  assert.equal(fieldLabel(hl7, 'PID-7'), 'Data urodzenia');
  assert.equal(fieldPhrase(json, 'pacjent'), 'dane pacjenta (pacjent)');
  assert.equal(fieldPhrase(json, 'termin'), 'termin');
  assert.equal(fieldPhrase(json, 'na\u202Ame'), 'na⟦U+202A⟧me', 'a stored name with a hidden character is written out');
  assert.equal(fieldLabel({ fields: [{ name: 'x', label: 'etykieta\u200B' }] }, 'x'), 'etykieta⟦U+200B⟧');
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
  assert.deepEqual(plain(formFromRule(rule, json)), { actions: { pacjent: 'show', lekarz: 'show', termin: 'hide', powod: 'show' }, extraShown: ['telefon'], extraMasked: [], extraHashed: [], extraRequired: [], hiddenImplicit: [] });
  const writeRule = { ...policyRows(POLICIES)[0], fields: ['lekarz', 'pacjent', 'termin', 'x', 'y'], requiredFields: ['pacjent', 'y'] };
  assert.deepEqual(plain(formFromRule(writeRule, json)), {
    actions: { pacjent: 'require', lekarz: 'allow', termin: 'allow', powod: 'forbid' },
    extraShown: ['x'],
    extraMasked: [],
    extraHashed: [],
    extraRequired: ['y'],
    hiddenImplicit: [],
  });
});

test('a form travels as one string and comes back; text that is not a form is refused', () => {
  const form = { actions: { pacjent: 'show', lekarz: 'hide', termin: 'mask', powod: 'hash' }, extraShown: ['b', 'a', 'a'], extraMasked: ['m'], extraHashed: ['h2', 'h1'], extraRequired: [], hiddenImplicit: ['PID-4'] };
  const text = serializeForm(form, json);
  assert.deepEqual(plain(parseForm(text, json)), { actions: form.actions, extraShown: ['a', 'b'], extraMasked: ['m'], extraHashed: ['h1', 'h2'], extraRequired: [], hiddenImplicit: ['PID-4'] });
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
    fields: ['lekarz', 'pacjent', 'telefon', 'termin'], requiredFields: [], maskedFields: [], hashedFields: [],
  });
  const write = { actions: { pacjent: 'require', lekarz: 'allow', termin: 'forbid', powod: 'allow' }, extraShown: ['x'], extraRequired: ['y'] };
  assert.deepEqual(buildPolicyRequest({ ...where, subject: { subjectType: 'any', subjectId: '*' }, direction: 'write', form: write, source: json }), {
    instanceId: INSTANCE, topic: TOPIC, subjectType: 'any', subjectId: '*', direction: 'write',
    fields: ['lekarz', 'pacjent', 'powod', 'x', 'y'], requiredFields: ['pacjent', 'y'], maskedFields: [], hashedFields: [],
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
  assert.match(ok('read', blank, closedJson, 'json'), /Ustaw co najmniej jedno pole na „Ukryj”, „Zamaskuj” albo „Zahaszuj”\. Lista zawiera wszystkie pola tego topiku/);
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
// Zamaskuj and Zahaszuj (F4 B7)
// ---------------------------------------------------------------------------

const CAPABILITIES = ['hide', 'mask', 'hash'];
// What the server stores for "pacjent masked, lekarz hashed, powod hidden, termin as it is": every field it shows, and the two subsets.
const MASKING = {
  subjectType: 'group', subjectId: 'g-ksieg', direction: 'read', subjectLabel: 'Księgowość', memberCount: 4,
  fields: ['lekarz', 'pacjent', 'termin'], requiredFields: [], maskedFields: ['pacjent'], hashedFields: ['lekarz'], createdAtMs: 1, updatedAtMs: NOW,
};

test('a rule from an older server has no masked or hashed fields', () => {
  const [row] = policyRows([{ ...MASKING, maskedFields: undefined, hashedFields: undefined }]);
  assert.deepEqual([row.maskedFields, row.hashedFields], [[], []]);
  assert.deepEqual(policyRows([MASKING]).map((r) => [r.maskedFields, r.hashedFields]), [[['pacjent'], ['lekarz']]]);
});

test('a masking rule is read back by field: masked and hashed fields stay in the shown list and are marked, in the order of the pattern', () => {
  const [row] = policyRows([MASKING]);
  const facts = ruleFacts(row, json);
  assert.deepEqual(facts.marks, [['pacjent', 'mask'], ['lekarz', 'hash'], ['powod', 'hide']]);
  assert.deepEqual([facts.hidden, facts.plain, facts.visible], [['powod'], ['termin'], ['lekarz', 'pacjent', 'termin']]);
  assert.deepEqual(facts.visibleOutside, []);
  const what = ruleTableRow(row, json, NOW).what;
  assert.equal(words(what), 'pacjent Zamaskuj dane pacjenta lekarz Zahaszuj Lekarz prowadzący wizytę powod Ukryj powód wizyty Pola spoza listy: ukryj');
  const chips = [...Object.assign(document.createElement('div'), { innerHTML: what }).querySelectorAll('.tf-chip')].map((c) => [c.textContent, c.className.replace('tf-chip tf-chip--outline ', '')]);
  assert.deepEqual(chips, [['Zamaskuj', 'info'], ['Zahaszuj', 'accent'], ['Ukryj', 'neutral']], 'each action has its own tone');
});

test('over typed fields a masking rule says what stays as it is, then each masked or hashed field with its chip', () => {
  const [row] = policyRows([{ ...MASKING, fields: ['id', 'nazwisko', 'pesel'], maskedFields: ['pesel'], hashedFields: ['nazwisko'] }]);
  assert.equal(words(ruleTableRow(row, typed, NOW).what), 'Widzi tylko: id oraz pola poniżej nazwisko Zahaszuj pesel Zamaskuj');
  const onlyMarked = policyRows([{ ...MASKING, fields: ['pesel'], maskedFields: ['pesel'], hashedFields: [] }])[0];
  assert.equal(words(ruleTableRow(onlyMarked, typed, NOW).what), 'Widzi tylko pola poniżej pesel Zamaskuj');
  // A masked field outside the list of a topic that has one is still named, after the listed ones.
  const outside = policyRows([{ ...MASKING, fields: ['pacjent', 'lekarz', 'termin', 'powod', 'pesel'], maskedFields: ['pesel'], hashedFields: [] }])[0];
  assert.equal(words(ruleTableRow(outside, json, NOW).what), 'pesel Zamaskuj Pola spoza listy: ukryj');
});

test('a writing rule never carries masked or hashed fields, whatever the row holds', () => {
  const write = policyRows([{ ...MASKING, direction: 'write', requiredFields: ['pacjent'] }])[0];
  const facts = ruleFacts(write, json);
  assert.deepEqual([facts.marks, facts.plain.length], [[], 3]);
  assert.deepEqual(plain(formFromRule(write, json)).actions, { pacjent: 'require', lekarz: 'allow', termin: 'allow', powod: 'forbid' });
});

test('the form of a masking rule: a row per field, the typed fields in the list of what happens to them, and back into the exact request', () => {
  const [row] = policyRows([{ ...MASKING, fields: ['lekarz', 'pacjent', 'telefon', 'termin', 'pesel'], maskedFields: ['pacjent', 'telefon'], hashedFields: ['lekarz', 'pesel'] }]);
  const form = formFromRule(row, json);
  assert.deepEqual(plain(form), {
    actions: { pacjent: 'mask', lekarz: 'hash', termin: 'show', powod: 'hide' },
    extraShown: [], extraMasked: ['telefon'], extraHashed: ['pesel'], extraRequired: [], hiddenImplicit: [],
  });
  const request = buildPolicyRequest({ instanceId: INSTANCE, topic: TOPIC, subject: { subjectType: 'group', subjectId: 'g-ksieg' }, direction: 'read', form, source: json });
  assert.deepEqual(request, {
    instanceId: INSTANCE, topic: TOPIC, subjectType: 'group', subjectId: 'g-ksieg', direction: 'read',
    fields: ['lekarz', 'pacjent', 'pesel', 'telefon', 'termin'],
    requiredFields: [],
    maskedFields: ['pacjent', 'telefon'],
    hashedFields: ['lekarz', 'pesel'],
  }, 'a masked or hashed field is also in the shown list; the hidden one is in none');
  assert.equal(serializeForm(parseForm(serializeForm(form, json), json), json), serializeForm(form, json));
  // A field typed into the "masked" list that has a row still shown makes that row masked.
  const typedOnRow = { ...blankForm('read', json), extraMasked: [' termin '], extraHashed: ['powod'] };
  const sent = buildPolicyRequest({ instanceId: INSTANCE, topic: TOPIC, subject: { subjectType: 'user', subjectId: 'u' }, direction: 'read', form: typedOnRow, source: json });
  assert.deepEqual([sent.maskedFields, sent.hashedFields], [['termin'], ['powod']]);
  assert.deepEqual(sent.fields, ['lekarz', 'pacjent', 'powod', 'termin']);
  // A writing form that still holds masked entries sends none.
  const write = buildPolicyRequest({ instanceId: INSTANCE, topic: TOPIC, subject: { subjectType: 'user', subjectId: 'u' }, direction: 'write', form: { ...blankForm('write', json), extraMasked: ['x'], extraHashed: ['y'] }, source: json });
  assert.deepEqual([write.fields.includes('x'), write.maskedFields, write.hashedFields], [false, [], []]);
});

test('a masking form is a rule worth storing when it masks or hashes something; a name may not sit in two lists or contradict its row', () => {
  const ok = (form, source = closedJson, format = 'json') => formProblem({ direction: 'read', form, source, format });
  const blank = blankForm('read', closedJson);
  assert.match(ok(blank), /Ustaw co najmniej jedno pole na „Ukryj”, „Zamaskuj” albo „Zahaszuj”/);
  assert.equal(ok({ ...blank, actions: { ...blank.actions, pacjent: 'mask' } }), null, 'masking is something');
  assert.equal(ok({ ...blank, actions: { ...blank.actions, pacjent: 'hash' } }), null, 'so is hashing');
  assert.equal(ok({ actions: {}, extraShown: [], extraMasked: ['pesel'], extraHashed: [] }, typed, 'xml'), null, 'a typed masked field is a visible field');
  assert.match(ok({ actions: {}, extraShown: [], extraMasked: [], extraHashed: [] }, typed, 'xml'), /Wpisz co najmniej jedno pole, które ma zostać widoczne \(bez zmian, zamaskowane albo zahaszowane\)/);
  assert.match(ok({ actions: {}, extraShown: ['id'], extraMasked: ['id'], extraHashed: [] }, typed, 'xml'), /„id” wpisano w więcej niż jednej liście/);
  assert.match(ok({ actions: {}, extraShown: [], extraMasked: ['id'], extraHashed: ['id'] }, typed, 'xml'), /„id” wpisano w więcej niż jednej liście/);
  // A typed name that is a listed field: fine on a shown row (it makes it masked), refused against a different action.
  assert.equal(ok({ ...blank, extraMasked: ['pacjent'] }), null);
  assert.match(ok({ ...blank, actions: { ...blank.actions, pacjent: 'hide' }, extraMasked: ['pacjent'] }), /„pacjent” jest na liście pól i ma tam inne ustawienie/);
  assert.match(ok({ ...blank, actions: { ...blank.actions, pacjent: 'hash' }, extraMasked: ['pacjent'] }), /„pacjent” jest na liście pól i ma tam inne ustawienie/);
  assert.match(ok({ ...blank, actions: { ...blank.actions, pacjent: 'mask' }, extraShown: ['pacjent'] }), /„pacjent” jest na liście pól i ma tam inne ustawienie/);
  assert.equal(ok({ ...blank, actions: { ...blank.actions, pacjent: 'mask' }, extraMasked: ['pacjent'] }), null, 'the same thing said twice is not a contradiction');
  // The format checks the typed addresses of the masked and hashed lists too.
  assert.match(ok({ ...blankForm('read', hl7), extraHashed: ['PID-0'] }, hl7, 'hl7v2'), /„PID-0” nie jest adresem pola HL7/);
  assert.match(ok({ ...blankForm('read', hl7), extraMasked: ['MSH-2'] }, hl7, 'hl7v2'), /MSH-2 to separatory samej wiadomości/);
});

test('"Co się stanie" names what is masked and what is hashed, and moves between actions say what changed', () => {
  const form = { ...blankForm('read', json), actions: { pacjent: 'mask', lekarz: 'hash', termin: 'hide', powod: 'show' } };
  assert.deepEqual(ruleImpact({ who: 'Księgowość', direction: 'read', form, current: null, source: json }), [
    'Dla „Księgowość” znikną z wiadomości pola: termin.',
    'Dla „Księgowość” pole dane pacjenta (pacjent) będzie widoczne tylko jako gwiazdki z 3 ostatnimi znakami.',
    'Dla „Księgowość” zamiast wartości pola Lekarz prowadzący wizytę (lekarz) pojawi się hasz — stały ciąg znaków, taki sam dla tej samej wartości w całej organizacji.',
    'Pozostałe pola z listy bez zmian.',
    'Pola spoza listy też znikną, chyba że je dopiszesz.',
  ]);
  const current = formFromRule(policyRows([MASKING])[0], json);
  const next = { ...current, actions: { ...current.actions, pacjent: 'hash', lekarz: 'show', powod: 'mask' } };
  assert.deepEqual(ruleImpact({ who: 'Księgowość', direction: 'read', form: next, current, source: closedJson }), [
    'Dla „Księgowość” pole powód wizyty (powod) będzie widoczne tylko jako gwiazdki z 3 ostatnimi znakami.',
    'Dla „Księgowość” zamiast wartości pola dane pacjenta (pacjent) pojawi się hasz — stały ciąg znaków, taki sam dla tej samej wartości w całej organizacji.',
    'Dla „Księgowość” pojawią się w wiadomościach pola: Lekarz prowadzący wizytę (lekarz).',
    'Pozostałe pola z listy bez zmian.',
  ]);
  assert.deepEqual(ruleImpact({ who: 'Księgowość', direction: 'read', form: current, current, source: json }), [], 'nothing changed, nothing to say');
  // Over typed fields: the visible ones, then what is masked and what is hashed.
  const typedForm = { actions: {}, extraShown: ['id'], extraMasked: ['pesel'], extraHashed: ['nazwisko'], extraRequired: [], hiddenImplicit: [] };
  assert.deepEqual(ruleImpact({ who: 'Wszyscy', everyone: true, direction: 'read', form: typedForm, current: null, source: typed }), [
    'Dla wszystkich widoczne będą tylko pola: id, nazwisko, pesel. Wszystkie inne zostaną ukryte.',
    'Dla wszystkich pole pesel będzie widoczne tylko jako gwiazdki z 3 ostatnimi znakami.',
    'Dla wszystkich zamiast wartości pola nazwisko pojawi się hasz — stały ciąg znaków, taki sam dla tej samej wartości w całej organizacji.',
  ]);
});

// ---------------------------------------------------------------------------
// What the server offers
// ---------------------------------------------------------------------------

test('reading offers "Pokaż" and "Ukryj", and "Zamaskuj" / "Zahaszuj" only where the server\'s capabilities list them', () => {
  const labels = (actions) => actionOptions('read', actions).map((o) => [o.value, o.label]);
  assert.deepEqual(labels(undefined), [['show', 'Pokaż'], ['hide', 'Ukryj']], 'a server that does not say offers hiding only');
  assert.deepEqual(labels(['hide']), [['show', 'Pokaż'], ['hide', 'Ukryj']]);
  assert.deepEqual(labels(['hide', 'mask', 'hash']), [['show', 'Pokaż'], ['hide', 'Ukryj'], ['mask', 'Zamaskuj'], ['hash', 'Zahaszuj']]);
  assert.deepEqual(labels(['hash']), [['show', 'Pokaż'], ['hide', 'Ukryj'], ['hash', 'Zahaszuj']], 'hiding is always there; each other action only when listed');
  assert.deepEqual(actionOptions('write', ['hide', 'mask', 'hash']).map((o) => o.label), ['Dozwolone', 'Wymagane', 'Niedozwolone'], 'writing has no masking or hashing');
});

test('the legend explains "ukryj", "odrzuć wiadomość" and which rule wins; "zamaskuj" and "zahaszuj" join it only where the server offers them', () => {
  const hideOnly = words(legendHtml());
  assert.match(hideOnly, /Ukryj — Pole znika z wiadomości\./);
  assert.match(hideOnly, /Odrzuć wiadomość — Przy zapisie: wiadomość z niedozwolonym polem albo bez wymaganego nie trafi do topiku\./);
  assert.match(hideOnly, /Zasada osoby wygrywa z zasadą jej grupy/);
  assert.doesNotMatch(hideOnly, /Zamaskuj|Zahaszuj/);
  const all = words(legendHtml(['hide', 'mask', 'hash']));
  assert.match(all, /Zamaskuj — Widać 3 ostatnie znaki, reszta to gwiazdki \(wartość krótsza niż 5 znaków jest zamaskowana w całości\), np\. \*{8}359\./);
  assert.match(all, /Zahaszuj — Zamiast wartości widać stały ciąg znaków: ta sama wartość daje zawsze ten sam hasz w całej organizacji/);
  assert.ok(all.indexOf('Ukryj —') < all.indexOf('Zamaskuj —') && all.indexOf('Zamaskuj —') < all.indexOf('Zahaszuj —') && all.indexOf('Zahaszuj —') < all.indexOf('Odrzuć wiadomość —'));
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

function paint({ hidingData, contentType = 'application/json', access = { canRead: true, canAdmin: true }, notice = null, capabilities, host = document.createElement('div') }) {
  document.body.appendChild(host);
  const moves = [];
  paintHidingSection(host, { topic: { name: TOPIC, contentType }, access, notice, capabilities, nowMs: NOW, hidingData }, { go: (a) => moves.push(a) });
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

const THRIFT = { subject: 'zamowienie', schemaType: 'thrift' };

test('a topic checked with a binary pattern has no fields to name: the section says so and "Dodaj zasadę" is closed', () => {
  const { host } = paint({ contentType: 'application/octet-stream', hidingData: { policies: [], policiesError: null, schema: THRIFT, schemaSettled: true } });
  const empty = host.querySelector('[data-role="state"] tf-empty-state');
  assert.equal(empty.getAttribute('title'), 'Treść binarna nie ma ukrywania danych');
  assert.match(empty.getAttribute('message'), /nie podlega ukrywaniu pól.*każdy, kto ma prawo czytania topiku, widzi ją w całości/);
  assert.doesNotMatch(empty.getAttribute('message'), /na tym ekranie/, 'there is no other screen where it could be done');
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

test('the legend follows the server: masking and hashing appear with the capabilities that offer them', () => {
  const data = { policies: POLICIES, policiesError: null, schema: SCHEMA, schemaSettled: true };
  const legend = (capabilities) => norm(paint({ hidingData: data, capabilities }).host.querySelector('[data-role="legend"]').textContent);
  assert.doesNotMatch(legend(undefined), /Zamaskuj|Zahaszuj/);
  assert.doesNotMatch(legend({ fieldActions: ['hide'] }), /Zamaskuj|Zahaszuj/);
  assert.match(legend({ fieldActions: ['hide', 'mask', 'hash'] }), /^Ukryj — Pole znika z wiadomości\.Zamaskuj — Widać 3 ostatnie znaki.*Zahaszuj — Zamiast wartości widać stały ciąg znaków.*Odrzuć wiadomość/);
});

test('Klucz haszowania: the explanation for everybody, the date and the button only for the instance administrator, the button waits for a key', () => {
  const data = (hashKey) => ({ policies: POLICIES, policiesError: null, schema: SCHEMA, schemaSettled: true, hashKey });
  const key = (host) => ({
    text: norm(host.querySelector('[data-role="key-text"]').textContent),
    button: host.querySelector('[data-role="key-button"]'),
    card: host.querySelector('[data-role="key-card"]'),
  });
  const info = { exists: true, keyVersion: 3, createdAtMs: new Date(2026, 7, 12).getTime(), rotatedAtMs: new Date(2026, 7, 12).getTime(), hashingRules: 3, hashingTopics: 2, hashingInstances: 1 };

  // Instance administrator, a key exists.
  const admin = paint({ hidingData: data({ info, error: null }), capabilities: { canAdmin: true, orgName: 'Przychodnia Zdrowie' } });
  const shown = key(admin.host);
  assert.match(shown.text, /^Klucz haszowania to tajny ciąg znaków, z którego liczy się hasz\. Dzięki niemu nikt spoza organizacji nie odgadnie wartości z hasza\. Klucz jest wspólny dla całej organizacji Przychodnia Zdrowie\.Obecny klucz ustawiono 12\.08\.2026 \(wersja 3\)\.$/);
  assert.equal(shown.button.hidden, false);
  assert.ok(!shown.button.hasAttribute('disabled'));
  shown.button.click();
  assert.equal(shown.button.getAttribute('data-go'), 'hiding-key');

  // No key yet: nothing to change, and the reason is on the button.
  const none = key(paint({ hidingData: data({ info: { exists: false, keyVersion: 0, createdAtMs: 0, rotatedAtMs: 0, hashingRules: 0, hashingTopics: 0, hashingInstances: 0 }, error: null }), capabilities: { canAdmin: true } }).host);
  assert.match(none.text, /Klucz powstanie przy pierwszej zasadzie „Zahaszuj” w organizacji — dopóki żadnej nie ma, nie ma czego zmieniać\.$/);
  assert.match(none.text, /wspólny dla całej organizacji\.Klucz powstanie/, 'without a name the sentence has no organization to quote');
  assert.ok(none.button.hasAttribute('disabled'));
  assert.equal(none.button.getAttribute('title'), 'Klucz powstanie dopiero przy pierwszej zasadzie „Zahaszuj”.');

  // Still loading, and a failed load.
  const loading = key(paint({ hidingData: data(null), capabilities: { canAdmin: true } }).host);
  assert.ok(loading.button.hasAttribute('disabled'));
  assert.equal(loading.button.getAttribute('title'), null);
  const failed = key(paint({ hidingData: data({ info: null, error: 'Brak uprawnień do tej operacji.' }), capabilities: { canAdmin: true } }).host);
  assert.match(failed.text, /Nie udało się wczytać informacji o kluczu: Brak uprawnień do tej operacji\.$/);

  // A topic administrator who is not the instance administrator reads the explanation and who changes the key; no button.
  const reader = key(paint({ hidingData: data(null), capabilities: { canAdmin: false, orgName: 'Przychodnia Zdrowie' } }).host);
  assert.match(reader.text, /Klucz zmienia administrator instancji\.$/);
  assert.doesNotMatch(reader.text, /Obecny klucz/, 'the date of the key is not shown to who cannot ask for it');
  assert.equal(reader.button.hidden, true);

  // A topic that cannot have rules has no key card.
  const blocked = paint({ hidingData: { policies: [], policiesError: null, schema: { subject: 'p', schemaType: 'protobuf' }, schemaSettled: true }, capabilities: { canAdmin: true } });
  assert.equal(key(blocked.host).card.hidden, true);
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

test('a topic checked with a binary pattern that already has rules says they will not work and leaves only "Usuń"', () => {
  const { host } = paint({ contentType: 'application/octet-stream', hidingData: { policies: POLICIES, policiesError: null, schema: THRIFT, schemaSettled: true } });
  const note = host.querySelector('[data-role="blocked-note"] tf-alert');
  assert.equal(note.getAttribute('message'), 'Ten topik ma zasady ukrywania danych, ale jego treść jest binarna — zasady nie zadziałają. Usuń je albo utwórz topik JSON/XML/HL7.');
  assert.equal(host.querySelector('[data-role="rules"]').hidden, false, 'the rules are listed');
  const table = host.querySelector('[data-role="rules"]');
  const actions = table.rowActions(table.rows[0], 0, () => table.rows[0]);
  assert.deepEqual([...actions.querySelectorAll('tf-button')].map((b) => b.dataset.act || b.getAttribute('data-act')), ['remove']);
  const add = host.querySelector('[data-role="add"]');
  assert.ok(add.hasAttribute('disabled'));
  assert.match(add.getAttribute('title'), /binarna/, 'the disabled button says why');
  // The same rules on a JSON topic: no note, and both "Zmień" and "Usuń".
  const json = paint({ hidingData: { policies: POLICIES, policiesError: null, schema: SCHEMA, schemaSettled: true } });
  assert.equal(json.host.querySelector('[data-role="blocked-note"]').children.length, 0);
  const jsonTable = json.host.querySelector('[data-role="rules"]');
  const jsonActions = jsonTable.rowActions(jsonTable.rows[0], 0, () => jsonTable.rows[0]);
  assert.deepEqual([...jsonActions.querySelectorAll('tf-button')].map((b) => b.dataset.act || b.getAttribute('data-act')), ['change', 'remove']);
});

test('the content type alone never closes the screen: an octet-stream topic with no pattern and a rule is edited like a JSON one', () => {
  const { host } = paint({ contentType: 'application/octet-stream', hidingData: { policies: POLICIES, policiesError: null, schema: null, schemaSettled: true } });
  assert.equal(host.querySelector('[data-role="blocked-note"]').children.length, 0, 'no warning: the server applies these rules');
  const add = host.querySelector('[data-role="add"]');
  assert.equal(add.hasAttribute('disabled'), false);
  assert.equal(add.getAttribute('title'), null);
  const table = host.querySelector('[data-role="rules"]');
  const actions = table.rowActions(table.rows[0], 0, () => table.rows[0]);
  assert.deepEqual([...actions.querySelectorAll('tf-button')].map((b) => b.dataset.act || b.getAttribute('data-act')), ['change', 'remove']);
  assert.equal(host.querySelector('[data-role="preview"]').hasAttribute('disabled'), false);
});

test('a topic with Avro content and an Avro pattern is blocked; the same content type with a JSON pattern is not', () => {
  const avro = paint({ contentType: 'avro/binary', hidingData: { policies: [], policiesError: null, schema: { subject: 'a', schemaType: 'avro' }, schemaSettled: true } });
  assert.ok(avro.host.querySelector('[data-role="add"]').hasAttribute('disabled'));
  assert.match(avro.host.querySelector('[data-role="add"]').getAttribute('title'), /binarna/);
  const json = paint({ contentType: 'avro/binary', hidingData: { policies: [], policiesError: null, schema: null, schemaSettled: true } });
  assert.equal(json.host.querySelector('[data-role="add"]').hasAttribute('disabled'), false);
});

// ---------------------------------------------------------------------------
// F4 B7 R2 + critic: XSD fields, the key without a rule or a node, doubled names
// ---------------------------------------------------------------------------

const INVOICE_XSD = `<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <!-- faktura -->
  <xs:element name="faktura" type="Faktura"/>
  <xs:complexType name="Faktura">
    <xs:sequence>
      <xs:element name="numer" type="xs:string"/>
      <xs:element name="nabywca"><xs:complexType><xs:sequence><xs:element name="nip" type="xs:string"/></xs:sequence></xs:complexType></xs:element>
      <xs:choice><xs:element name="gotowka" type="xs:string"/><xs:element name="przelew" type="xs:string"/></xs:choice>
    </xs:sequence>
    <xs:attribute name="id" type="xs:string"/>
  </xs:complexType>
</xs:schema>`;

test('an XSD pattern lists the children of its root, through sequence, choice and a named type; what it cannot list it says', () => {
  assert.deepEqual(readXsdFields(INVOICE_XSD), { fields: ['numer', 'nabywca', 'gotowka', 'przelew'].map((name) => ({ name, label: '' })), complex: false });
  const inline = '<xsd:schema xmlns:xsd="http://www.w3.org/2001/XMLSchema"><xsd:element name="r"><xsd:complexType><xsd:all><xsd:element name="a" type="xsd:int"/><xsd:element ref="b"/></xsd:all></xsd:complexType></xsd:element></xsd:schema>';
  assert.deepEqual(readXsdFields(inline).fields.map((f) => f.name), ['a', 'b']);
  // Things whose children cannot be written out.
  assert.equal(readXsdFields('<xs:schema xmlns:xs="x"><xs:element name="r"><xs:complexType><xs:sequence><xs:any/></xs:sequence></xs:complexType></xs:element></xs:schema>').complex, true);
  assert.equal(readXsdFields('<xs:schema xmlns:xs="x"><xs:element name="r" type="Missing"/></xs:schema>').complex, true, 'a type that is not in the file');
  assert.equal(readXsdFields('<xs:schema xmlns:xs="x" targetNamespace="urn:a" elementFormDefault="qualified"><xs:element name="r"/></xs:schema>').complex, true, 'the prefix of a namespaced child is anybody\'s guess');
  assert.deepEqual(readXsdFields('<xs:schema'), { fields: [], complex: false }, 'text that is not a pattern lists nothing');
  assert.deepEqual(readXsdFields('<a><b></a>'), { fields: [], complex: false });
});

test('an XML topic with an XSD gets its fields from it; one it cannot read is told as an XSD, never as "no pattern"', () => {
  const xsd = { subject: 'faktura', schemaType: 'xsd', version: 2, text: INVOICE_XSD };
  const source = fieldSource({ format: 'xml', schema: xsd });
  assert.equal(source.mode, 'schema');
  assert.deepEqual(source.fields.map((f) => f.name), ['numer', 'nabywca', 'gotowka', 'przelew']);
  assert.deepEqual([source.subject, source.version], ['faktura', 2]);
  const hard = fieldSource({ format: 'xml', schema: { ...xsd, text: '<xs:schema xmlns:xs="x"><xs:element name="r"><xs:complexType><xs:group ref="g"/></xs:complexType></xs:element></xs:schema>' } });
  assert.equal(hard.mode, 'manual');
  assert.equal(hard.xsd, true);
  assert.equal(fieldSource({ format: 'xml', schema: null }).xsd, false, 'no pattern at all is a different sentence');
  // A request over such a field is the usual one.
  const form = { ...blankForm('read', source), actions: { numer: 'show', nabywca: 'hash', gotowka: 'mask', przelew: 'hide' } };
  const request = buildPolicyRequest({ instanceId: INSTANCE, topic: 'faktury', subject: { subjectType: 'any', subjectId: '*' }, direction: 'read', form, source });
  assert.deepEqual([request.fields, request.maskedFields, request.hashedFields], [['gotowka', 'nabywca', 'numer'], ['gotowka'], ['nabywca']]);
});

test('the key card tells the three states apart: no rules, rules without the key on this node, and a node that is not an operator', () => {
  const base = { exists: false, keyVersion: 0, createdAtMs: 0, rotatedAtMs: 0, hashingRules: 0, hashingTopics: 0, hashingInstances: 0 };
  const last = (info, extra = {}) => keyNote({ canAdmin: true, orgName: null, key: { info: { ...base, ...info }, error: null }, ...extra }).slice(1);

  assert.deepEqual(last({}), ['Klucz powstanie przy pierwszej zasadzie „Zahaszuj” w organizacji — dopóki żadnej nie ma, nie ma czego zmieniać.']);
  // Rules that hash and no key: the key is on its way, and the hashed fields are hidden meanwhile.
  assert.deepEqual(last({ hashingRules: 2 }), ['Ten node ma zasady „Zahaszuj”, ale klucz organizacji jeszcze na niego nie dotarł — pola haszowane są tymczasowo ukryte. Klucz przychodzi z node\'a operatora; jeśli tak się nie dzieje, sprawdź połączenie z nim.']);
  assert.equal(keyAwaited({ ...base, hashingRules: 2 }), true);
  assert.equal(keyAwaited({ ...base }), false);
  assert.equal(keyAwaited({ ...base, exists: true, hashingRules: 2 }), false);
  // A key, on a node that may not change it.
  const held = { exists: true, keyVersion: 2, rotatedAtMs: new Date(2026, 7, 12).getTime(), hashingRules: 2 };
  assert.deepEqual(last({ ...held, canRotateHere: false }), ['Obecny klucz ustawiono 12.08.2026 (wersja 2).', 'Ten node nie jest operatorem organizacji, więc klucza tu nie zmienisz — zrób to na node\'zie operatora.']);
  assert.deepEqual(last({ ...held }), ['Obecny klucz ustawiono 12.08.2026 (wersja 2).'], 'a server that does not say is taken to allow it');
  assert.deepEqual(last({ ...held, canRotateHere: true }), ['Obecny klucz ustawiono 12.08.2026 (wersja 2).']);

  assert.equal(keyButtonProblem(null), null);
  assert.equal(keyButtonProblem({ ...base }), 'Klucz powstanie dopiero przy pierwszej zasadzie „Zahaszuj”.');
  assert.equal(keyButtonProblem({ ...base, hashingRules: 1 }), 'Klucz jeszcze nie dotarł na ten node, więc nie ma czego zmieniać.');
  assert.match(keyButtonProblem({ ...held, canRotateHere: false }), /nie jest operatorem organizacji/);
  assert.equal(keyButtonProblem({ ...held, canRotateHere: true }), null);
});

test('the key button is closed on a node that is not an operator, with the reason, and open on one that is', () => {
  const data = (info) => ({ policies: POLICIES, policiesError: null, schema: SCHEMA, schemaSettled: true, hashKey: { info, error: null } });
  const held = { exists: true, keyVersion: 2, createdAtMs: 1, rotatedAtMs: new Date(2026, 7, 12).getTime(), hashingRules: 1, hashingTopics: 1, hashingInstances: 1 };
  const button = (info) => paint({ hidingData: data(info), capabilities: { canAdmin: true } }).host.querySelector('[data-role="key-button"]');
  const closed = button({ ...held, canRotateHere: false });
  assert.ok(closed.hasAttribute('disabled'));
  assert.match(closed.getAttribute('title'), /nie jest operatorem organizacji/);
  const open = button({ ...held, canRotateHere: true });
  assert.ok(!open.hasAttribute('disabled'));
  assert.equal(open.getAttribute('title'), null);
  const waiting = button({ exists: false, keyVersion: 0, createdAtMs: 0, rotatedAtMs: 0, hashingRules: 3, hashingTopics: 1, hashingInstances: 1, canRotateHere: true });
  assert.ok(waiting.hasAttribute('disabled'));
  assert.equal(waiting.getAttribute('title'), 'Klucz jeszcze nie dotarł na ten node, więc nie ma czego zmieniać.');
});

test('every doubled name is named at once, not one by the next fix', () => {
  const form = { ...blankForm('read', typed), extraShown: ['aktywny', 'pesel'], extraMasked: ['pesel', 'aktywny'], extraHashed: ['lekarz', 'pesel'] };
  const text = formProblem({ direction: 'read', form, source: typed, format: 'json' });
  assert.match(text, /„aktywny” wpisano w więcej niż jednej liście/);
  assert.match(text, /„pesel” wpisano w więcej niż jednej liście/);
  assert.doesNotMatch(text, /lekarz/);
});

test('one masked or hashed field is "pole … będzie", several are "pola … będą": the sentence agrees with the number', () => {
  const form = (actions) => ({ ...blankForm('read', json), actions: { pacjent: 'show', lekarz: 'show', termin: 'show', powod: 'show', ...actions } });
  const lines = (actions) => ruleImpact({ who: 'Księgowość', direction: 'read', form: form(actions), current: null, source: json });
  assert.match(lines({ pacjent: 'mask' })[0], /^Dla „Księgowość” pole dane pacjenta \(pacjent\) będzie widoczne tylko jako gwiazdki/);
  assert.match(lines({ pacjent: 'mask', termin: 'mask' })[0], /^Dla „Księgowość” pola dane pacjenta \(pacjent\), termin będą widoczne tylko jako gwiazdki/);
  assert.match(lines({ pacjent: 'hash' })[0], /zamiast wartości pola dane pacjenta \(pacjent\) pojawi się hasz/);
  assert.match(lines({ pacjent: 'hash', lekarz: 'hash' })[0], /zamiast wartości pól dane pacjenta \(pacjent\), Lekarz prowadzący wizytę \(lekarz\) pojawi się hasz/);
});
