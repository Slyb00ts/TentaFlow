// =============================================================================
// File: modules/tentabus/topic-hiding.test.js
// Description: A topic's Ukrywanie danych section (U7, T07): one row per rule
// with its subject, direction and the fields it hides (the server stores the
// ALLOWED fields, the screen shows the other side); where the known fields
// come from (a JSON Schema's properties, the HL7 v2 dictionary, typed in);
// the form of a rule and the exact FieldPolicySet request built from it; the
// "Co się stanie" sentences in the mockups' words; the section in its states
// (loading, error with retry, empty, blocked, filled) and the legend, whose
// "zamaskuj" and "zahaszuj" appear only when the server offers them.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const {
  policyRows, policyKey, whoTitle, whoSub, hidingCount, topicFormat, jsonSchemaFields, fieldSource, fieldLabel, fieldPhrase, fieldNameProblem,
  ruleFacts, ruleTableRow, readActionList, actionOptions, blankForm, formFromRule, serializeForm, parseForm, buildPolicyRequest, formProblem,
  ruleImpact, legendHtml, directionsClosedToKeys, paintHidingSection,
} = await import('./topic-hiding.js');

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
  assert.equal(words(row.what), 'termin Ukryj');
  assert.equal(words(row.when), 'Odczyt');
  assert.equal(row.changed, 'dziś 11:00');
  assert.equal(row._key, 'addon:asystent:read');
  assert.equal(words(ruleTableRow(group, json, NOW).what), 'powod Ukryj powód wizyty');
  assert.equal(ruleTableRow(group, json, NOW).changed, 'wczoraj 12:00');
  assert.equal(words(ruleTableRow(everyone, json, NOW).what), 'pacjent Wymagane dane pacjenta termin Wymagane powod Niedozwolone powód wizyty', 'a write rule names what it requires and what it refuses');
  assert.equal(words(ruleTableRow(everyone, json, NOW).when), 'Zapis');
});

test('a cell names three fields and counts the rest; a rule that hides nothing says so; without a list it names what stays', () => {
  const many = { ...policyRows(POLICIES)[3], fields: [] };
  const what = ruleTableRow(many, json, NOW).what;
  assert.equal(words(what), 'pacjent Ukryj dane pacjenta lekarz Ukryj Lekarz prowadzący wizytę termin Ukryj i jeszcze 1 pole');
  const none = { ...many, fields: ['pacjent', 'lekarz', 'termin', 'powod'] };
  assert.equal(words(ruleTableRow(none, json, NOW).what), 'Nie ukrywa żadnego ze znanych pól');
  assert.equal(words(ruleTableRow(many, typed, NOW).what), 'Nie widzi żadnego pola');
  assert.equal(words(ruleTableRow(policyRows(POLICIES)[2], typed, NOW).what), 'Widzi tylko: lekarz');
  const writeBlind = { ...policyRows(POLICIES)[0], fields: ['id', 'nazwisko'], requiredFields: ['id'] };
  assert.equal(words(ruleTableRow(writeBlind, typed, NOW).what), 'Może zapisać tylko: id, nazwisko id Wymagane');
});

// ---------------------------------------------------------------------------
// The fields a rule can name
// ---------------------------------------------------------------------------

test('a JSON Schema gives its top-level properties in the order it writes them, named by title or description; a branch of allOf counts', () => {
  assert.deepEqual(jsonSchemaFields(VISIT_SCHEMA), [
    { name: 'pacjent', label: 'dane pacjenta' },
    { name: 'lekarz', label: 'Lekarz prowadzący wizytę' },
    { name: 'termin', label: '' },
    { name: 'powod', label: 'powód wizyty' },
  ]);
  assert.deepEqual(jsonSchemaFields(JSON.stringify({ allOf: [{ properties: { a: {} } }, { properties: { b: { title: 'B' }, a: { title: 'ignored' } } }] })).map((f) => [f.name, f.label]), [['a', ''], ['b', 'B']]);
  assert.deepEqual(jsonSchemaFields('{"type":"object"}'), []);
  assert.deepEqual(jsonSchemaFields('not json'), []);
  assert.equal(jsonSchemaFields(JSON.stringify({ properties: { x: { description: 'a'.repeat(200) } } }))[0].label.length, 70, 'a long description is cut');
});

test('where the known fields come from: the pattern, the HL7 dictionary, or nowhere (typed in); binary content has none', () => {
  assert.equal(json.mode, 'schema');
  assert.deepEqual([json.subject, json.version], ['wizyta', 5]);
  assert.equal(hl7.mode, 'dictionary');
  assert.ok(hl7.fields.length >= 55);
  assert.deepEqual(hl7.fields.find((f) => f.name === 'PID-5'), { name: 'PID-5', label: 'Imię i nazwisko pacjenta' });
  assert.deepEqual(typed, { mode: 'manual', fields: [], failed: false });
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
  for (const bad of ['PID', 'PID-0', 'PID-05', 'pid-5', 'PIDX-5', 'PI-5', 'PID-5.1', 'PID-a']) assert.equal(fieldNameProblem('hl7v2', bad), 'hl7_shape', bad);
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
  assert.deepEqual(blankForm('read', json).actions, { pacjent: 'show', lekarz: 'show', termin: 'show', powod: 'show' });
  assert.deepEqual(blankForm('write', json).actions, { pacjent: 'allow', lekarz: 'allow', termin: 'allow', powod: 'allow' });
  const rule = { ...policyRows(POLICIES)[3], fields: ['lekarz', 'pacjent', 'powod', 'telefon'] };
  assert.deepEqual(formFromRule(rule, json), { actions: { pacjent: 'show', lekarz: 'show', termin: 'hide', powod: 'show' }, extraShown: ['telefon'], extraRequired: [] });
  const writeRule = { ...policyRows(POLICIES)[0], fields: ['lekarz', 'pacjent', 'termin', 'x', 'y'], requiredFields: ['pacjent', 'y'] };
  assert.deepEqual(formFromRule(writeRule, json), {
    actions: { pacjent: 'require', lekarz: 'allow', termin: 'allow', powod: 'forbid' },
    extraShown: ['x'],
    extraRequired: ['y'],
  });
});

test('a form travels as one string and comes back; text that is not a form is refused', () => {
  const form = { actions: { pacjent: 'show', lekarz: 'hide', termin: 'show', powod: 'show' }, extraShown: ['b', 'a', 'a'], extraRequired: [] };
  const text = serializeForm(form, json);
  assert.deepEqual(parseForm(text, json), { actions: form.actions, extraShown: ['a', 'b'], extraRequired: [] });
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

test('"Zamaskuj" and "Zahaszuj" never leave a field allowed: a server that does not store them yet still hides it', () => {
  const where = { instanceId: INSTANCE, topic: TOPIC, subject: { subjectType: 'user', subjectId: 'u-1' }, direction: 'read', source: json };
  const form = { actions: { pacjent: 'hash', lekarz: 'mask', termin: 'show', powod: 'hide' }, extraShown: [], extraRequired: [] };
  assert.deepEqual(buildPolicyRequest({ ...where, form }).fields, ['termin']);
});

test('a rule needs something to do: one hidden field (or a required or refused one), or typed fields when none are listed', () => {
  const ok = (direction, form, source, format) => formProblem({ direction, form, source, format });
  const blank = blankForm('read', json);
  assert.match(ok('read', blank, json, 'json'), /Ustaw co najmniej jedno pole na „Ukryj”/);
  assert.equal(ok('read', { ...blank, actions: { ...blank.actions, powod: 'hide' } }, json, 'json'), null);
  assert.equal(ok('read', { ...blank, actions: { ...blank.actions, powod: 'hash' } }, json, 'json'), null, 'any action that takes the field away counts');
  assert.match(ok('read', { actions: {}, extraShown: [], extraRequired: [] }, typed, 'xml'), /Wpisz co najmniej jedno pole, które ma zostać widoczne/);
  assert.equal(ok('read', { actions: {}, extraShown: ['id'], extraRequired: [] }, typed, 'xml'), null);
  const write = blankForm('write', json);
  assert.match(ok('write', write, json, 'json'), /„Wymagane” albo „Niedozwolone”/);
  assert.equal(ok('write', { ...write, actions: { ...write.actions, termin: 'require' } }, json, 'json'), null);
  assert.equal(ok('write', { ...write, extraRequired: ['id'] }, json, 'json'), null);
  assert.match(ok('write', { actions: {}, extraShown: [], extraRequired: [] }, typed, 'xml'), /Wpisz co najmniej jedno pole, które wolno zapisać/);
  // A typed address the format refuses is named first.
  assert.match(ok('read', { ...blankForm('read', hl7), extraShown: ['PID-0'] }, hl7, 'hl7v2'), /„PID-0” nie jest adresem pola HL7\. Adres ma postać SEGMENT-numer, np\. PID-5\./);
  assert.match(ok('read', { ...blankForm('read', hl7), extraShown: ['MSH-2'] }, hl7, 'hl7v2'), /MSH-2 to separatory samej wiadomości/);
  assert.match(ok('read', { actions: {}, extraShown: ['1a'], extraRequired: [] }, typed, 'xml'), /nie jest nazwą elementu XML/);
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
    'Pola spoza listy też znikną, chyba że wpiszesz je niżej.',
  ]);
  const before = formFromRule(policyRows(POLICIES)[1], json);
  const after = { ...before, actions: { ...before.actions, powod: 'show', termin: 'hide' } };
  assert.deepEqual(ruleImpact({ who: 'Rejestracja', direction: 'read', form: after, current: before, source: json }), [
    'Dla „Rejestracja” znikną z wiadomości pola: termin.',
    'Dla „Rejestracja” pojawią się w wiadomościach pola: powód wizyty (powod).',
    'Pozostałe pola z listy bez zmian.',
    'Pola spoza listy też znikną, chyba że wpiszesz je niżej.',
  ]);
  assert.deepEqual(ruleImpact({ who: 'Rejestracja', direction: 'read', form: before, current: before, source: json }), [], 'no change, nothing to say');
  assert.deepEqual(ruleImpact({ who: 'Wszyscy', direction: 'read', form: { actions: {}, extraShown: ['id', 'nazwa'], extraRequired: [] }, current: null, source: typed }),
    ['Dla „Wszyscy” widoczne będą tylko pola: id, nazwa. Wszystkie inne zostaną ukryte.']);
});

test('"Co się stanie" for a writing rule: whole messages are refused, and the sentences hold for a list or typed fields', () => {
  const form = { actions: { pacjent: 'require', lekarz: 'allow', termin: 'forbid', powod: 'allow' }, extraShown: [], extraRequired: [] };
  assert.deepEqual(ruleImpact({ who: 'Księgowość', direction: 'write', form, current: null, source: json }), [
    'Wiadomość od „Księgowość”, w której jest którekolwiek z pól: termin, zostanie odrzucona i nie trafi do topiku.',
    'Wiadomość od „Księgowość”, w której brakuje któregokolwiek z pól: dane pacjenta (pacjent), zostanie odrzucona i nie trafi do topiku.',
    'Odrzucona zostaje cała wiadomość, a nie samo pole.',
    'Wiadomość z polem spoza listy też zostanie odrzucona, chyba że wpiszesz je niżej.',
  ]);
  assert.deepEqual(ruleImpact({ who: 'Księgowość', direction: 'write', form: { actions: {}, extraShown: ['id'], extraRequired: ['nr'] }, current: null, source: typed }), [
    'Od „Księgowość” zostanie przyjęta wiadomość tylko z polami: id, nr. Wiadomość z innym polem zostanie odrzucona.',
    'Wiadomość od „Księgowość”, w której brakuje któregokolwiek z pól: nr, zostanie odrzucona i nie trafi do topiku.',
    'Odrzucona zostaje cała wiadomość, a nie samo pole.',
  ]);
});

// ---------------------------------------------------------------------------
// What the server offers
// ---------------------------------------------------------------------------

test('reading offers "Pokaż" and "Ukryj"; "Zamaskuj" and "Zahaszuj" appear on their own when the server lists them', () => {
  assert.deepEqual(readActionList(['hide']), ['show', 'hide']);
  assert.deepEqual(readActionList([]), ['show', 'hide'], 'hiding has always been there, an older answer without a list included');
  assert.deepEqual(readActionList(undefined), ['show', 'hide']);
  assert.deepEqual(readActionList(['hide', 'mask', 'hash']), ['show', 'hide', 'mask', 'hash']);
  assert.deepEqual(readActionList(['mask']), ['show', 'hide', 'mask'], 'an action is offered because the server lists it, not because another is');
  assert.deepEqual(actionOptions('read', ['hide']).map((o) => o.label), ['Pokaż', 'Ukryj']);
  assert.deepEqual(actionOptions('read', ['hide', 'mask', 'hash']).map((o) => [o.value, o.label]), [['show', 'Pokaż'], ['hide', 'Ukryj'], ['mask', 'Zamaskuj'], ['hash', 'Zahaszuj']]);
  assert.deepEqual(actionOptions('write', ['hide', 'mask']).map((o) => o.label), ['Dozwolone', 'Wymagane', 'Niedozwolone'], 'writing has no hiding action');
});

test('the legend explains "ukryj" and "odrzuć wiadomość"; "zamaskuj" and "zahaszuj" only where the server offers them', () => {
  const plain = words(legendHtml(['hide']));
  assert.match(plain, /Ukryj — Pole znika z wiadomości\./);
  assert.match(plain, /Odrzuć wiadomość — Przy zapisie: wiadomość z niedozwolonym polem albo bez wymaganego nie trafi do topiku\./);
  assert.match(plain, /Zasada osoby wygrywa z zasadą jej grupy/);
  assert.doesNotMatch(plain, /Zamaskuj|Zahaszuj/);
  const full = words(legendHtml(['hide', 'mask', 'hash']));
  assert.match(full, /Zamaskuj — Widać 3 ostatnie znaki, reszta to gwiazdki\./);
  assert.match(full, /Zahaszuj — Zamiast wartości jest ciąg znaków/);
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

function paint({ hidingData, contentType = 'application/json', access = { canRead: true, canAdmin: true }, capabilities = { fieldActions: ['hide'] }, notice = null, host = document.createElement('div') }) {
  document.body.appendChild(host);
  const moves = [];
  paintHidingSection(host, { topic: { name: TOPIC, contentType }, access, capabilities, notice, nowMs: NOW, hidingData }, { go: (a) => moves.push(a) });
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

test('a failed load keeps the retry in the section (T12) and the retry asks again', () => {
  const { host, moves } = paint({ hidingData: { policies: null, policiesError: 'Brak uprawnień do tej operacji.', schema: null, schemaSettled: true } });
  assert.match(norm(host.querySelector('[data-role="state"]').textContent), /Brak uprawnień do tej operacji\./);
  host.querySelector('[data-role="state"] [data-go="hiding-reload"]').click?.();
  assert.equal(host.querySelector('[data-role="state"] [data-go="hiding-reload"]').textContent, 'Spróbuj ponownie');
  assert.equal(moves.length, 0, 'the page wires data-go to the shell; the section only marks the button');
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
  assert.equal(empty.getAttribute('title'), 'Nie można dodać zasady w tym topiku');
  assert.match(empty.getAttribute('message'), /przenosi treść binarną/);
  assert.equal(empty.querySelector('[data-go]'), null);
  assert.ok(host.querySelector('[data-role="add"]').hasAttribute('disabled'));
  assert.equal(host.querySelector('[data-role="add"]').getAttribute('title'), empty.getAttribute('message'));
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
  assert.match(norm(host.querySelector('[data-role="keys-note"]').textContent), /Systemy zewnętrzne z kluczem API mają zamknięty ten topik w zakresie odczytu: są w nim zasady tylko dla wybranych podmiotów, a nie ma zasady dla wszystkich\./);
  assert.equal(host.querySelector('[data-role="state"]').textContent.trim(), '');
});

test('the legend gains "zamaskuj" and "zahaszuj" with the server\'s list, and a repaint keeps the table\'s buttons', () => {
  const host = document.createElement('div');
  paint({ host, hidingData: { policies: POLICIES, policiesError: null, schema: SCHEMA, schemaSettled: true } });
  const table = host.querySelector('[data-role="rules"]');
  const rowsBefore = table.rows;
  assert.doesNotMatch(host.querySelector('[data-role="legend"]').textContent, /Zahaszuj/);
  paint({ host, capabilities: { fieldActions: ['hide', 'mask', 'hash'] }, hidingData: { policies: POLICIES, policiesError: null, schema: SCHEMA, schemaSettled: true } });
  assert.match(host.querySelector('[data-role="legend"]').textContent, /Zamaskuj/);
  assert.match(host.querySelector('[data-role="legend"]').textContent, /Zahaszuj/);
  assert.equal(table.rows, rowsBefore, 'an unchanged poll does not hand the table its rows again');
});

test('previewing is closed with its reason to an administrator who may not read the topic', () => {
  const { host } = paint({ access: { canRead: false, canAdmin: true }, hidingData: { policies: POLICIES, policiesError: null, schema: SCHEMA, schemaSettled: true } });
  const preview = host.querySelector('[data-role="preview"]');
  assert.ok(preview.hasAttribute('disabled'));
  assert.match(preview.getAttribute('title'), /Podgląd wiadomości wymaga prawa czytania topiku wizyty\./);
  assert.equal(host.querySelector('[data-role="add"]').hasAttribute('disabled'), false, 'managing rules needs administration only');
});
