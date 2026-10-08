// =============================================================================
// File: modules/tentabus/topic-hiding-windows.test.js
// Description: The windows of Ukrywanie danych (U7): "Dodaj zasadę" (the
// subject from the directory, only those without such a rule; reading or
// writing; a row per known field from a pattern or the HL7 dictionary, typed-in
// addresses checked before the request and marked on their chips; "Wszyscy"),
// "Zmień" (subject and direction fixed, the stored rule read back), "Usuń" (what
// the rule does now, what follows); the exact FieldPolicySet / FieldPolicyDelete
// requests; a refusal that stays in the window; a rule that changed under an
// open window (the server's compare-and-set refuses it: the window closes with
// a note and nothing is overwritten); the warning that a first rule for chosen
// subjects closes the topic to API keys; the field search of a long list; and
// the dirty-draft guard of the shared window ("Anuluj" included).
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const { openHidingAdd, openHidingChange, openHidingRemove, removeLead, removeImpact } = await import('./topic-hiding-windows.js');
const { policyRows, fieldSource } = await import('./topic-hiding.js');

const norm = (s) => String(s).replace(/[  ]/g, ' ').replace(/\s+/g, ' ').trim();
const tick = (ms = 20) => new Promise((r) => setTimeout(r, ms));
const closeAll = () => document.querySelectorAll('tf-window').forEach((w) => w.remove());
const pick = (el, value) => {
  el.value = value;
  el.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value } }));
};
const setTags = (win, role, tags) => {
  const el = win.querySelector(`[data-role="${role}"]`);
  el.tags = tags;
  el.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { tags } }));
};
const rowFor = (win, field) => [...win.querySelectorAll('tf-segmented[data-field]')].find((s) => s.getAttribute('data-field') === field);
const optionLabels = (seg) => [...seg.querySelectorAll('button')].map((b) => norm(b.textContent));

const INSTANCE = 'tentabus-a1b2c3d4';
const TOPIC = 'wizyty';
const NOW = new Date(2026, 8, 30, 12, 0, 0).getTime();
const SCHEMA = {
  subject: 'wizyta',
  version: 5,
  text: JSON.stringify({ type: 'object', additionalProperties: false, properties: { pacjent: { title: 'dane pacjenta' }, lekarz: {}, termin: {}, powod: { title: 'powód wizyty' } } }),
};
const json = fieldSource({ format: 'json', schema: SCHEMA });
const hl7 = fieldSource({ format: 'hl7v2', schema: null });
const typed = fieldSource({ format: 'xml', schema: null });

const POLICIES = [
  { subjectType: 'group', subjectId: 'g-rej', direction: 'read', fields: ['lekarz', 'pacjent', 'termin'], requiredFields: [], updatedAtMs: NOW, subjectLabel: 'Rejestracja', memberCount: 5 },
  { subjectType: 'addon', subjectId: 'asystent', direction: 'read', fields: ['lekarz', 'pacjent', 'powod'], requiredFields: [], updatedAtMs: NOW, subjectLabel: 'Asystent lekarza', memberCount: null },
  { subjectType: 'any', subjectId: '*', direction: 'write', fields: ['lekarz', 'pacjent', 'termin'], requiredFields: ['pacjent', 'termin'], updatedAtMs: NOW, subjectLabel: null, memberCount: null },
];
const chooseSubject = (win, value) => pick(win.querySelector('[data-role="subject"]'), value);
const GROUPS = [
  { subjectType: 'group', subjectId: 'g-rej', label: 'Rejestracja', memberCount: 5 },
  { subjectType: 'group', subjectId: 'g-ksieg', label: 'Księgowość', memberCount: 4 },
];

function addContext(overrides = {}) {
  const sent = [];
  const saved = [];
  const asked = [];
  const ctx = {
    instanceId: INSTANCE,
    topic: TOPIC,
    format: 'json',
    source: json,
    rules: policyRows(POLICIES),
    directory: async (q) => { asked.push(q); return { entries: q.kind === 'group' ? GROUPS : q.kind === 'addon' ? [{ subjectType: 'addon', subjectId: 'asystent', label: 'Asystent lekarza' }] : [] }; },
    setPolicy: async (r) => { sent.push(r); },
    deleteRule: async (r) => { sent.push(r); },
    describeError: (e) => `błąd: ${e.message}`,
    onSaved: (n) => saved.push(n),
    ...overrides,
  };
  return { ctx, sent, saved, asked };
}

// ---------------------------------------------------------------------------
// Dodaj zasadę
// ---------------------------------------------------------------------------

test('"Dodaj zasadę" for a group: only groups without a reading rule, a row per field of the pattern, the exact request', async () => {
  closeAll();
  const { ctx, sent, saved, asked } = addContext();
  const win = openHidingAdd(ctx);
  assert.deepEqual(asked, [{ kind: 'group', query: '' }], 'opens on groups');
  await tick();
  const select = win.querySelector('[data-role="subject"]');
  assert.deepEqual([...select.querySelectorAll('select option')].map((o) => o.textContent), ['Wybierz…', 'Księgowość (4 osoby)'], 'Rejestracja has a reading rule already');
  assert.equal(select.value, '', 'nobody is chosen for the administrator');
  assert.equal(norm(win.querySelector('[data-role="impact"]').textContent), 'Wybierz, dla kogo jest zasada.');
  assert.equal(norm(win.querySelector('.tb-pick-box label').textContent), 'Szukaj na liście', 'the search box has its own label');
  chooseSubject(win, 'group:g-ksieg');
  assert.equal(win.querySelector('[data-role="pick-note"]').textContent, 'Na liście są osoby, grupy lub addony, które nie mają jeszcze takiej zasady.');
  assert.deepEqual([...win.querySelectorAll('tf-segmented[data-field]')].map((s) => s.getAttribute('data-field')), ['pacjent', 'lekarz', 'termin', 'powod']);
  assert.deepEqual(optionLabels(rowFor(win, 'powod')), ['Pokaż', 'Ukryj']);
  assert.equal(norm(win.querySelector('[data-role="source-note"]').textContent), 'Pola pochodzą ze wzoru wiadomości wizyta, wersja 5.');
  assert.equal(win.querySelector('[data-role="unlisted-note"]').textContent, '', 'a pattern that closes itself has no other fields to warn about');
  assert.match(win.querySelector('[data-role="impact"]').textContent, /Ustaw co najmniej jedno pole na „Ukryj”/);
  assert.ok(win.querySelector('[data-act="save"]').hasAttribute('disabled'));
  pick(rowFor(win, 'powod'), 'hide');
  assert.equal(norm(win.querySelector('[data-role="impact"]').textContent),
    'Co się stanie po zapisaniu: Dla „Księgowość” znikną z wiadomości pola: powód wizyty (powod). Pozostałe pola z listy bez zmian.');
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual(sent, [{
    instanceId: INSTANCE, topic: TOPIC, subjectType: 'group', subjectId: 'g-ksieg', direction: 'read',
    fields: ['lekarz', 'pacjent', 'termin'], requiredFields: [], expectAbsent: true,
  }], 'an add says there must be no such rule yet');
  assert.equal(saved[0].title, 'Zapisano zasadę');
  assert.match(saved[0].text, /^Dla „Księgowość” znikną z wiadomości pola: powód wizyty \(powod\)\./);
});

test('the same group is offered again for writing; switching the direction redraws the rows with the writing choices', async () => {
  closeAll();
  const { ctx, sent } = addContext();
  const win = openHidingAdd(ctx);
  await tick();
  pick(win.querySelector('[data-role="direction"]'), 'write');
  assert.deepEqual([...win.querySelector('[data-role="subject"]').querySelectorAll('select option')].map((o) => o.textContent), ['Wybierz…', 'Rejestracja (5 osób)', 'Księgowość (4 osoby)']);
  chooseSubject(win, 'group:g-rej');
  assert.deepEqual(optionLabels(rowFor(win, 'pacjent')), ['Dozwolone', 'Wymagane', 'Niedozwolone']);
  assert.match(win.querySelector('[data-role="impact"]').textContent, /„Wymagane” albo „Niedozwolone”/);
  pick(rowFor(win, 'pacjent'), 'require');
  pick(rowFor(win, 'powod'), 'forbid');
  setTags(win, 'extra-required', ['nr']);
  assert.equal(norm(win.querySelector('[data-role="impact"]').textContent),
    'Co się stanie po zapisaniu: Wiadomość od „Rejestracja”, w której jest którekolwiek z pól: powód wizyty (powod), zostanie odrzucona i nie trafi do topiku. '
    + 'Wiadomość od „Rejestracja”, w której brakuje któregokolwiek z pól: nr, dane pacjenta (pacjent), zostanie odrzucona i nie trafi do topiku. Odrzucona zostaje cała wiadomość, a nie samo pole.');
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual(sent, [{
    instanceId: INSTANCE, topic: TOPIC, subjectType: 'group', subjectId: 'g-rej', direction: 'write',
    fields: ['lekarz', 'nr', 'pacjent', 'termin'], requiredFields: ['nr', 'pacjent'], expectAbsent: true,
  }]);
});

test('"Wszyscy": no directory question, and a rule for everyone that exists is said to exist', async () => {
  closeAll();
  const { ctx, sent, asked } = addContext();
  const win = openHidingAdd(ctx);
  await tick();
  asked.length = 0;
  pick(win.querySelector('[data-role="kind"]'), 'any');
  assert.equal(win.querySelector('[data-role="pick-box"]').hidden, true);
  assert.deepEqual(asked, []);
  assert.match(win.querySelector('[data-role="pick-note"]').textContent, /Systemy z kluczem API podlegają tylko tej zasadzie\./);
  assert.doesNotMatch(win.querySelector('[data-role="pick-note"]').textContent, /podmiot/);
  pick(rowFor(win, 'termin'), 'hide');
  assert.match(norm(win.querySelector('[data-role="impact"]').textContent), /Dla wszystkich znikną z wiadomości pola: termin\./);
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual([sent[0].subjectType, sent[0].subjectId, sent[0].direction, sent[0].fields], ['any', '*', 'read', ['lekarz', 'pacjent', 'powod']]);

  closeAll();
  const again = openHidingAdd(addContext().ctx);
  await tick();
  pick(again.querySelector('[data-role="direction"]'), 'write');
  pick(again.querySelector('[data-role="kind"]'), 'any');
  assert.equal(again.querySelector('[data-role="pick-note"]').textContent, 'Zasada zapisu dla wszystkich już istnieje — zmienisz ją w jej wierszu.');
  assert.match(again.querySelector('[data-role="impact"]').textContent, /Ta osoba, grupa lub ten addon ma już zasadę zapisu/);
  assert.ok(again.querySelector('[data-act="save"]').hasAttribute('disabled'));
});

test('an HL7 topic: the dictionary\'s fields with their names, a typed address checked first, the typed field kept visible', async () => {
  closeAll();
  const { ctx, sent } = addContext({ format: 'hl7v2', source: hl7, directory: async () => ({ entries: [{ subjectType: 'group', subjectId: 'g-ksieg', label: 'Księgowość', memberCount: 4 }] }) });
  const win = openHidingAdd(ctx);
  await tick();
  chooseSubject(win, 'group:g-ksieg');
  assert.ok(win.querySelectorAll('tf-segmented[data-field]').length >= 55);
  const name = rowFor(win, 'PID-5').closest('.tb-right-row').querySelector('.tb-right-name').textContent;
  assert.equal(norm(name), 'PID-5Imię i nazwisko pacjenta');
  assert.match(win.querySelector('[data-role="source-note"]').textContent, /najczęstsze pola HL7 v2/);
  assert.equal(win.querySelector('[data-role="extra-shown"]').getAttribute('aria-label'), 'Inne pola, które mają zostać widoczne');
  pick(rowFor(win, 'PID-5'), 'hide');
  pick(rowFor(win, 'PID-19'), 'hide');
  setTags(win, 'extra-shown', ['PID-0', 'pid5', 'ZZZ-3']);
  const problems = norm(win.querySelector('[data-role="impact"]').textContent);
  assert.match(problems, /„PID-0” nie jest adresem pola HL7\. Adres ma postać SEGMENT-numer, np\. PID-5\./);
  assert.match(problems, /„pid5” wygląda na literówkę\. Czy chodziło o PID-5\?/, 'every wrong address is answered, not only the first');
  assert.doesNotMatch(problems, /ZZZ-3/);
  assert.ok(win.querySelector('[data-act="save"]').hasAttribute('disabled'), 'the server would refuse it, so the window does not send it');
  // The wrong chips are marked and explained under the field itself.
  const chips = [...win.querySelectorAll('[data-role="extra-shown"] tf-chip')];
  assert.deepEqual(chips.map((c) => [c.getAttribute('label'), c.getAttribute('tone')]), [['PID-0', 'critical'], ['pid5', 'critical'], ['ZZZ-3', 'neutral']]);
  const under = win.querySelector('[data-role="extra-shown-problems"]');
  assert.equal(under.hidden, false);
  assert.match(norm(under.textContent), /„PID-0” nie jest adresem pola HL7.*„pid5” wygląda na literówkę/);
  setTags(win, 'extra-shown', ['PID-31']);
  assert.match(norm(win.querySelector('[data-role="impact"]').textContent), /znikną z wiadomości pola: Imię i nazwisko pacjenta \(PID-5\), Numer identyfikacyjny \(np\. PESEL\) \(PID-19\)\./);
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.equal(sent[0].fields.includes('PID-5'), false);
  assert.equal(sent[0].fields.includes('PID-19'), false);
  assert.equal(sent[0].fields.includes('PID-31'), true, 'a typed field stays visible');
  assert.equal(sent[0].fields.includes('PID-3'), true);
  assert.equal(sent[0].fields.length, hl7.fields.length - 2 + 1, 'a reading rule lets through what the window lists and what was typed — nothing else');
  assert.ok(['PID-1', 'PID-4', 'PID-20', 'NK1-6', 'IN1-9'].every((f) => !sent[0].fields.includes(f)), 'the unnamed positions are hidden, as the window says');
  assert.match(win.querySelector('[data-role="unlisted-note"]').textContent, /Pola, których nie ma na liście, też zostaną ukryte/);
});

test('a wrong typed address is marked on its chip and explained under the field before anyone is chosen', async () => {
  closeAll();
  const { ctx } = addContext({ format: 'hl7v2', source: hl7, directory: async () => ({ entries: [] }) });
  const win = openHidingAdd(ctx);
  await tick();
  setTags(win, 'extra-shown', ['MSH-2', 'PID-5', 'pid5']);
  assert.deepEqual([...win.querySelectorAll('[data-role="extra-shown"] tf-chip')].map((c) => c.getAttribute('tone')), ['critical', 'neutral', 'critical']);
  const under = norm(win.querySelector('[data-role="extra-shown-problems"]').textContent);
  assert.match(under, /MSH-2 to separatory samej wiadomości/);
  assert.match(under, /„pid5” wygląda na literówkę/);
  assert.ok(under.indexOf('MSH-2') < under.indexOf('pid5'), 'in the order typed');
  assert.match(norm(win.querySelector('[data-role="impact"]').textContent), /Wybierz, dla kogo jest zasada\./, 'nobody chosen yet, and the address is still marked');
  setTags(win, 'extra-shown', ['PID-5']);
  assert.equal(win.querySelector('[data-role="extra-shown-problems"]').hidden, true, 'the mark goes with the mistake');
  assert.ok([...win.querySelectorAll('[data-role="extra-shown"] tf-chip')].every((c) => c.getAttribute('tone') === 'neutral'));
});

test('an HL7 WRITING rule allows the unnamed positions of the listed segments and says so; a missing rule is expected absent', async () => {
  closeAll();
  const { ctx, sent } = addContext({ format: 'hl7v2', source: hl7, rules: [], directory: async () => ({ entries: [{ subjectType: 'group', subjectId: 'g-ksieg', label: 'Księgowość', memberCount: 4 }] }) });
  const win = openHidingAdd(ctx);
  await tick();
  pick(win.querySelector('[data-role="direction"]'), 'write');
  chooseSubject(win, 'group:g-ksieg');
  assert.match(win.querySelector('[data-role="unlisted-note"]').textContent, /^Pozostałe pozycje segmentów z listy \(np\. PID-1, PID-4, MSH-11\) są przyjmowane\./);
  pick(rowFor(win, 'PID-19'), 'forbid');
  assert.match(norm(win.querySelector('[data-role="impact"]').textContent), /Pozostałe pozycje segmentów z listy .* są przyjmowane\. Wiadomość z innym segmentem, np\. segmentem Z, zostanie odrzucona/);
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.ok(hl7.implicit.every((f) => sent[0].fields.includes(f)), 'a real message passes');
  assert.equal(sent[0].fields.includes('PID-19'), false);
  assert.equal(sent[0].expectAbsent, true);
});

test('a field called __proto__ keeps its choice through the window', async () => {
  closeAll();
  const odd = fieldSource({ format: 'json', schema: { subject: 's', version: 1, text: '{"properties":{"__proto__":{},"a":{}}}' } });
  const { ctx, sent } = addContext({ source: odd });
  const win = openHidingAdd(ctx);
  await tick();
  chooseSubject(win, 'group:g-ksieg');
  pick(rowFor(win, '__proto__'), 'hide');
  assert.match(norm(win.querySelector('[data-role="impact"]').textContent), /znikną z wiadomości pola: __proto__\./);
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual(sent[0].fields, ['a'], 'hidden: not in the allowed list');
});

test('"Zmień" of a reading rule that lets unnamed HL7 positions through lists them as typed-in fields the administrator can take out', async () => {
  closeAll();
  const stored = { ...policyRows(POLICIES)[1], fields: [...hl7.fields.map((f) => f.name), 'PID-4', 'PID-20'], requiredFields: [] };
  const { ctx, sent } = addContext({ format: 'hl7v2', source: hl7 });
  const win = openHidingChange(stored, ctx);
  assert.deepEqual(win.querySelector('[data-role="extra-shown"]').tags, ['PID-20', 'PID-4']);
  setTags(win, 'extra-shown', ['PID-4']);
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.equal(sent[0].fields.includes('PID-20'), false, 'taken out');
  assert.equal(sent[0].fields.includes('PID-4'), true);
});

test('a long field list has a search: it narrows the rows by address or plain name and says when nothing matches', async () => {
  closeAll();
  const { ctx } = addContext({ format: 'hl7v2', source: hl7 });
  const win = openHidingAdd(ctx);
  await tick();
  const search = win.querySelector('[data-role="field-search"]');
  assert.ok(search, 'sixty rows need a search');
  const shown = () => [...win.querySelectorAll('[data-role="rows"] > .tb-right-row')].filter((r) => !r.hidden).length;
  const total = shown();
  const find = (value) => {
    search.value = value;
    search.dispatchEvent(new CustomEvent('search', { bubbles: true, detail: { value } }));
  };
  find('pid-19');
  assert.equal(shown(), 1);
  find('pesel');
  assert.equal(shown(), 1, 'the plain name matches too');
  assert.equal(win.querySelector('[data-role="rows-none"]').hidden, true);
  find('zzz-nothing');
  assert.equal(shown(), 0);
  assert.equal(win.querySelector('[data-role="rows-none"]').hidden, false);
  find('');
  assert.equal(shown(), total);
  closeAll();
  const small = openHidingAdd(addContext().ctx);
  assert.equal(small.querySelector('[data-role="field-search"]'), null, 'a short list needs none');
});

test('a hidden row keeps its choice: searching does not change what will be saved', async () => {
  closeAll();
  const { ctx, sent } = addContext({ format: 'hl7v2', source: hl7 });
  const win = openHidingAdd(ctx);
  await tick();
  chooseSubject(win, 'group:g-ksieg');
  pick(rowFor(win, 'PID-5'), 'hide');
  const search = win.querySelector('[data-role="field-search"]');
  search.value = 'obx';
  search.dispatchEvent(new CustomEvent('search', { bubbles: true, detail: { value: 'obx' } }));
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.equal(sent[0].fields.includes('PID-5'), false);
});

test('a topic whose fields nobody listed: the fields to keep are typed in; every other field is hidden, and the window says so', async () => {
  closeAll();
  const { ctx, sent } = addContext({ format: 'xml', source: typed });
  const win = openHidingAdd(ctx);
  await tick();
  chooseSubject(win, 'group:g-ksieg');
  assert.equal(win.querySelector('[data-role="fields-box"]').hidden, true);
  assert.equal(win.querySelectorAll('tf-segmented[data-field]').length, 0);
  assert.match(win.querySelector('[data-role="source-note"]').textContent, /Nie znamy pól tego topiku/);
  assert.match(win.querySelector('[data-role="impact"]').textContent, /Wpisz co najmniej jedno pole, które ma zostać widoczne/);
  setTags(win, 'extra-shown', ['1pole']);
  assert.match(win.querySelector('[data-role="impact"]').textContent, /nie jest nazwą elementu XML/);
  setTags(win, 'extra-shown', ['numer', 'wartosc']);
  assert.equal(norm(win.querySelector('[data-role="impact"]').textContent), 'Co się stanie po zapisaniu: Dla „Księgowość” widoczne będą tylko pola: numer, wartosc. Wszystkie inne zostaną ukryte.');
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual([sent[0].fields, sent[0].requiredFields], [['numer', 'wartosc'], []]);
});

test('a refusal stays in the window in plain words and the draft can be sent again once it changes', async () => {
  closeAll();
  let calls = 0;
  const { ctx, saved } = addContext({ setPolicy: async () => { calls += 1; throw new Error('PolicyDenied'); } });
  const win = openHidingAdd(ctx);
  await tick();
  chooseSubject(win, 'group:g-ksieg');
  pick(rowFor(win, 'powod'), 'hide');
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.equal(calls, 1);
  assert.equal(norm(win.querySelector('[data-role="error"]').textContent), 'błąd: PolicyDenied');
  assert.equal(win.isConnected, true);
  assert.deepEqual(saved, []);
  assert.ok(win.querySelector('[data-act="save"]').hasAttribute('disabled'), 'the refused draft is not sent twice');
  pick(rowFor(win, 'termin'), 'hide');
  assert.equal(win.querySelector('[data-act="save"]').hasAttribute('disabled'), false);
});

test('the close button and Escape ask once before dropping a changed draft; an untouched window closes at once', async () => {
  closeAll();
  const later = () => new Promise((r) => setTimeout(r, 300));
  const untouched = openHidingAdd(addContext().ctx);
  await tick();
  untouched.close();
  await later();
  assert.equal(untouched.isConnected, false);
  const { ctx } = addContext();
  const win = openHidingAdd(ctx);
  await tick();
  pick(rowFor(win, 'powod'), 'hide');
  win.close();
  await later();
  assert.equal(win.isConnected, true, 'the first close only asks');
  assert.match(win.querySelector('[data-role="discard"]').textContent, /Zamknij okno jeszcze raz/);
  win.close();
  await later();
  assert.equal(win.isConnected, false);
});

test('the first rule for chosen subjects in a direction warns before saving that API-key systems lose the topic', async () => {
  closeAll();
  const warning = /Systemy zewnętrzne z kluczem API nie będą mogły czytać tego topiku, dopóki nie dodasz zasady dla wszystkich\./;
  const { ctx } = addContext({ rules: [] });
  const win = openHidingAdd(ctx);
  await tick();
  chooseSubject(win, 'group:g-ksieg');
  pick(rowFor(win, 'powod'), 'hide');
  assert.match(win.querySelector('[data-role="impact"]').textContent, warning, 'reading');
  pick(win.querySelector('[data-role="direction"]'), 'write');
  chooseSubject(win, 'group:g-ksieg');
  pick(rowFor(win, 'powod'), 'forbid');
  assert.match(win.querySelector('[data-role="impact"]').textContent, /Systemy zewnętrzne z kluczem API nie będą mogły zapisywać do tego topiku, dopóki/, 'writing');
  pick(win.querySelector('[data-role="direction"]'), 'read');
  pick(win.querySelector('[data-role="kind"]'), 'any');
  pick(rowFor(win, 'powod'), 'hide');
  assert.doesNotMatch(win.querySelector('[data-role="impact"]').textContent, /kluczem API/, 'the rule for everyone is what keys meet');
  closeAll();
  const known = openHidingAdd(addContext().ctx);
  await tick();
  chooseSubject(known, 'group:g-ksieg');
  pick(rowFor(known, 'powod'), 'hide');
  assert.doesNotMatch(known.querySelector('[data-role="impact"]').textContent, /kluczem API/, 'reading already has rules: nothing new to say');
});

test('the add window says whether the directory is empty or everyone on it already has a rule', async () => {
  closeAll();
  const none = openHidingAdd(addContext({ directory: async () => ({ entries: [] }) }).ctx);
  await tick();
  assert.equal(none.querySelector('[data-role="pick-note"]').textContent, 'W tej organizacji nie ma jeszcze żadnej grupy.');
  pick(none.querySelector('[data-role="kind"]'), 'addon');
  await tick();
  assert.equal(none.querySelector('[data-role="pick-note"]').textContent, 'Żaden addon nie ma dostępu do tej instancji.');
  closeAll();
  const taken = openHidingAdd(addContext({ directory: async () => ({ entries: [GROUPS[0]] }) }).ctx);
  await tick();
  assert.equal(taken.querySelector('[data-role="pick-note"]').textContent, 'Każdy z tej listy ma już taką zasadę w tym topiku.');
});

// What the server says when the stored rule is not the one a request expected.
const changedError = () => new Error('protocol error Conflict: bus.field_policy_changed: topic \'wizyty\'');

test('a rule that appeared since the window opened is refused by the server: the window closes with a note, nothing is overwritten', async () => {
  closeAll();
  const { ctx, saved } = addContext({ setPolicy: async () => { throw changedError(); } });
  const win = openHidingAdd(ctx);
  await tick();
  chooseSubject(win, 'group:g-ksieg');
  pick(rowFor(win, 'powod'), 'hide');
  win.querySelector('[data-act="save"]').click();
  await tick(350);
  assert.equal(win.isConnected, false, 'a window left open on a rule that has moved on would only refuse again');
  assert.deepEqual(saved, [{
    tone: 'warning',
    title: 'Nic nie zapisano — zasada zmieniła się w międzyczasie',
    text: 'Ktoś zmienił, dodał albo usunął tę zasadę, zanim Twoja zmiana została zapisana, więc niczego nie nadpisaliśmy. Tabela pokazuje zasady tak, jak są teraz — jeśli nadal chcesz tej zmiany, wprowadź ją jeszcze raz.',
  }]);
  assert.ok(!JSON.stringify(saved).includes('spróbuj jeszcze raz'), 'no promise a second try in the same window could keep');
});

test('only that refusal closes the window: any other one stays in it with its reason', async () => {
  closeAll();
  const { ctx, saved } = addContext({ setPolicy: async () => { throw new Error('protocol error PolicyDenied: bus.permission_denied: admin on \'wizyty\''); } });
  const win = openHidingAdd(ctx);
  await tick();
  chooseSubject(win, 'group:g-ksieg');
  pick(rowFor(win, 'powod'), 'hide');
  win.querySelector('[data-act="save"]').click();
  await tick(350);
  assert.equal(win.isConnected, true);
  assert.match(norm(win.querySelector('[data-role="error"]').textContent), /^błąd: protocol error PolicyDenied/);
  assert.deepEqual(saved, []);
});

test('"Anuluj" asks once like the close button and Escape when the draft is changed; an untouched window leaves at once', async () => {
  closeAll();
  const later = () => new Promise((r) => setTimeout(r, 300));
  const untouched = openHidingAdd(addContext().ctx);
  await tick();
  untouched.querySelector('[data-act="cancel"]').click();
  await later();
  assert.equal(untouched.isConnected, false);
  const win = openHidingAdd(addContext().ctx);
  await tick();
  pick(rowFor(win, 'powod'), 'hide');
  win.querySelector('[data-act="cancel"]').click();
  await later();
  assert.equal(win.isConnected, true, 'the first click only asks');
  assert.match(win.querySelector('[data-role="discard"]').textContent, /Zamknij okno jeszcze raz/);
  win.querySelector('[data-act="cancel"]').click();
  await later();
  assert.equal(win.isConnected, false);
});

// ---------------------------------------------------------------------------
// Zmień
// ---------------------------------------------------------------------------

test('"Zmień": subject and direction fixed, the stored rule read back as hidden fields, only the change sent', async () => {
  closeAll();
  const [everyone, group] = policyRows(POLICIES);
  const { ctx, sent, saved } = addContext();
  const win = openHidingChange(group, ctx);
  assert.equal(norm(win._titleEl.textContent), 'Zasada ukrywania danych — Rejestracja');
  assert.equal(norm(win.querySelectorAll('.tb-explain-box')[0].textContent), 'RejestracjaGrupa · 5 osóbZasada należy do tej osoby, grupy lub addonu — tego się nie zmienia.', 'the line about what cannot change stands under "Dla kogo"');
  assert.equal(norm(win.querySelectorAll('.tb-explain-box')[1].textContent), 'Odczyt');
  assert.deepEqual(['pacjent', 'lekarz', 'termin', 'powod'].map((f) => rowFor(win, f).value), ['show', 'show', 'show', 'hide']);
  assert.ok(win.querySelector('[data-act="save"]').hasAttribute('disabled'), 'nothing changed yet');
  assert.equal(norm(win.querySelector('[data-role="impact"]').textContent), 'Nic jeszcze nie zmieniono.');
  pick(rowFor(win, 'powod'), 'show');
  assert.match(win.querySelector('[data-role="impact"]').textContent, /Ustaw co najmniej jedno pole na „Ukryj”/, 'a rule that hides nothing is better deleted');
  pick(rowFor(win, 'termin'), 'hide');
  assert.equal(norm(win.querySelector('[data-role="impact"]').textContent),
    'Co się stanie po zapisaniu: Dla „Rejestracja” znikną z wiadomości pola: termin. '
    + 'Dla „Rejestracja” pojawią się w wiadomościach pola: powód wizyty (powod). Pozostałe pola z listy bez zmian.');
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual(sent, [{
    instanceId: INSTANCE, topic: TOPIC, subjectType: 'group', subjectId: 'g-rej', direction: 'read',
    fields: ['lekarz', 'pacjent', 'powod'], requiredFields: [], expectedUpdatedAtMs: NOW,
  }], 'a change says which version of the rule it was made from');
  assert.equal(saved[0].title, 'Zapisano zmianę zasady');
  assert.doesNotMatch(saved[0].text, /niżej/, 'the note outlives the window, so it points at nothing below');

  closeAll();
  const write = openHidingChange(everyone, addContext().ctx);
  assert.deepEqual(['pacjent', 'lekarz', 'termin', 'powod'].map((f) => rowFor(write, f).value), ['require', 'allow', 'require', 'forbid']);
  assert.deepEqual(optionLabels(rowFor(write, 'lekarz')), ['Dozwolone', 'Wymagane', 'Niedozwolone']);
});

test('a stored field outside the known list comes back in the typed-in list and is kept', async () => {
  closeAll();
  const rule = { ...policyRows(POLICIES)[1], fields: ['lekarz', 'pacjent', 'powod', 'telefon'] };
  const { ctx, sent } = addContext();
  const win = openHidingChange(rule, ctx);
  assert.deepEqual(win.querySelector('[data-role="extra-shown"]').tags, ['telefon']);
  pick(rowFor(win, 'lekarz'), 'hide');
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual(sent[0].fields, ['pacjent', 'powod', 'telefon']);
});

test('a rule that changed or vanished since "Zmień" opened is refused by the server; the window closes with the note', async () => {
  closeAll();
  const [, group] = policyRows(POLICIES);
  const harness = addContext({ setPolicy: async () => { throw changedError(); } });
  const win = openHidingChange(group, harness.ctx);
  pick(rowFor(win, 'termin'), 'hide');
  win.querySelector('[data-act="save"]').click();
  await tick(350);
  assert.equal(win.isConnected, false);
  assert.equal(harness.saved.length, 1);
  assert.equal(harness.saved[0].tone, 'warning');
  assert.equal(harness.saved[0].title, 'Nic nie zapisano — zasada zmieniła się w międzyczasie');
});

test('a window does not guess about the rule it started from: it sends what it was given, so the server alone decides', async () => {
  closeAll();
  const [, group] = policyRows(POLICIES);
  const { ctx, sent, saved } = addContext();
  const win = openHidingChange({ ...group, updatedAtMs: 1234 }, ctx);
  pick(rowFor(win, 'termin'), 'hide');
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.equal(sent.length, 1);
  assert.equal(sent[0].expectedUpdatedAtMs, 1234);
  assert.equal(sent[0].expectAbsent, undefined);
  assert.equal(saved[0].tone, undefined, 'a saved rule is a success note');
});

// ---------------------------------------------------------------------------
// Usuń
// ---------------------------------------------------------------------------

test('"Usuń": what the rule does now, what follows, then one FieldPolicyDelete', async () => {
  closeAll();
  const rows = policyRows(POLICIES);
  const [everyone, group, addon] = rows;
  assert.equal(removeLead(addon, json), 'Zasada odczytu dla: Asystent lekarza. Ukrywa pola: termin.');
  assert.equal(removeLead(group, json), 'Zasada odczytu dla: Rejestracja. Ukrywa pola: powód wizyty (powod).');
  assert.equal(removeLead(everyone, json), 'Zasada zapisu dla: Wszyscy — niedozwolone pola: powód wizyty (powod); wymagane pola: dane pacjenta (pacjent), termin.');
  assert.equal(removeLead({ ...group, fields: ['lekarz'] }, typed), 'Zasada odczytu dla: Rejestracja. Zostawia widoczne tylko pola: lekarz.');
  assert.deepEqual(removeImpact(group, json, TOPIC, rows), ['Osoby z grupy „Rejestracja” w topiku wizyty będą czytać według zasady odczytu swojej innej grupy, a gdy jej nie mają — według zasady dla wszystkich; gdy nie ma żadnej, zobaczą całe wiadomości.']);
  const person = { ...group, key: 'user:u-1:read', subjectType: 'user', subjectId: 'u-1', label: 'Jan Nowak' };
  assert.deepEqual(removeImpact(person, json, TOPIC, rows), ['Dla użytkownika „Jan Nowak” w topiku wizyty zacznie obowiązywać zasada odczytu jednej z jego grup, a gdy żadna z nich jej nie ma — zasada dla wszystkich; gdy nie ma żadnej, zobaczy całe wiadomości.']);
  assert.deepEqual(removeImpact(addon, json, TOPIC, rows), ['Addon „Asystent lekarza” w topiku wizyty będzie czytał według zasady dla wszystkich; gdy jej nie ma, zobaczy całe wiadomości.']);
  const groupWrite = { ...group, key: 'group:g-rej:write', direction: 'write' };
  assert.deepEqual(removeImpact(groupWrite, json, TOPIC, rows), ['Wiadomości osób z grupy „Rejestracja” do topiku wizyty będą sprawdzane według zasady zapisu ich innej grupy, a gdy jej nie mają — według zasady dla wszystkich; gdy nie ma żadnej, nie będą sprawdzane wcale.']);
  assert.deepEqual(removeImpact({ ...addon, direction: 'write' }, json, TOPIC, rows), ['Wiadomości addonu „Asystent lekarza” do topiku wizyty będą sprawdzane według zasady dla wszystkich; gdy jej nie ma, nie będą sprawdzane wcale.']);
  assert.deepEqual(removeImpact({ ...person, direction: 'write' }, json, TOPIC, rows), ['Wiadomości użytkownika „Jan Nowak” do topiku wizyty będą sprawdzane według zasady zapisu jednej z jego grup, a gdy żadna z nich jej nie ma — według zasady dla wszystkich; gdy nie ma żadnej, nie będą sprawdzane wcale.']);
  assert.deepEqual(removeImpact(everyone, json, TOPIC, rows), ['Zapis do topiku wizyty przestanie być sprawdzany u każdego, kto nie ma własnej zasady zapisu.'], 'writing has no other rule to close the topic to keys');
  const anyRead = { ...everyone, key: 'any:*:read', direction: 'read' };
  assert.deepEqual(removeImpact(anyRead, json, TOPIC, [...rows, anyRead]), [
    'Każdy, kto nie ma własnej zasady odczytu, zobaczy w topiku wizyty całe wiadomości.',
    'Zostaną tylko zasady dla wybranych osób, grup lub addonów, więc systemy zewnętrzne z kluczem API nie będą mogły czytać tego topiku.',
  ]);

  const { ctx, sent, saved } = addContext();
  const win = openHidingRemove(group, ctx);
  assert.equal(norm(win._titleEl.textContent), 'Usuń zasadę ukrywania danych — Rejestracja');
  assert.equal(norm(win.querySelector('.tb-explain-box').textContent), 'Zasada odczytu dla: Rejestracja. Ukrywa pola: powód wizyty (powod).');
  assert.match(norm(win.querySelector('[data-role="impact"]').textContent), /^Co się stanie po usunięciu: Osoby z grupy „Rejestracja” w topiku wizyty/);
  win.querySelector('[data-act="go"]').click();
  await tick();
  assert.deepEqual(sent, [{ instanceId: INSTANCE, topic: TOPIC, subjectType: 'group', subjectId: 'g-rej', direction: 'read' }]);
  assert.deepEqual(saved, [{ title: 'Usunięto zasadę', text: 'Usunięto zasadę odczytu dla: Rejestracja.' }]);
});

test('a refused removal stays in the window with the reason', async () => {
  closeAll();
  const { ctx, saved } = addContext({ deleteRule: async () => { throw new Error('PolicyDenied'); } });
  const win = openHidingRemove(policyRows(POLICIES)[1], ctx);
  win.querySelector('[data-act="go"]').click();
  await tick();
  assert.equal(norm(win.querySelector('[data-role="error"]').textContent), 'błąd: PolicyDenied');
  assert.deepEqual(saved, []);
});
