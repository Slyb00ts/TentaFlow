// =============================================================================
// File: modules/tentabus/topic-creator.test.js
// Description: "Nowy topik" (T04): names checked as they are typed (reserved,
// invalid, taken), content kinds from what the server reads, patterns offered
// only when they fit the content and are not withdrawn, the copies sentence
// from the server's own resolution, the create request (copies never sent),
// and the window walked step by step — Dalej locked on a bad name, Wstecz,
// the summary, a refused create keeping the window open, and the result.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const {
  topicNameProblem, creatorContentTypes, compatibleSchemas, copiesPlan, copiesReason, copiesCount,
  buildTopicCreateRequest, newDraft, openTopicCreator, partitionsValue,
} = await import('./topic-creator.js');

const CONTENT = ['application/json', 'application/xml', 'application/hl7-v2'];
const subjects = [
  { subject: 'wizyta', schemaType: 'json_schema', latestVersion: 5, deprecatedAtMs: null },
  { subject: 'wizyta-2025', schemaType: 'json_schema', latestVersion: 7, deprecatedAtMs: 123 },
  { subject: 'powiadomienie', schemaType: 'protobuf', latestVersion: 1, deprecatedAtMs: null },
  { subject: 'ankieta', schemaType: 'json_schema', latestVersion: 1, deprecatedAtMs: null },
];

test('names: reserved prefix, shape and a name already taken', () => {
  assert.equal(topicNameProblem(''), 'empty');
  assert.equal(topicNameProblem('__dlq.x'), 'reserved');
  assert.equal(topicNameProblem('Wyniki'), 'invalid');
  assert.equal(topicNameProblem('a'), 'invalid');
  assert.equal(topicNameProblem('wyniki badan'), 'invalid');
  assert.equal(topicNameProblem('wyniki-badan', ['wyniki-badan']), 'taken');
  assert.equal(topicNameProblem('wyniki-z-pracowni', ['wyniki-badan']), null);
  assert.equal(topicNameProblem('lab.results.v2'), null);
  // 121 at most: the dead-letter topic `__dlq.<name>` must still fit in 127.
  assert.equal(topicNameProblem(`a${'b'.repeat(120)}`), null);
  assert.equal(topicNameProblem(`a${'b'.repeat(121)}`), 'invalid');
});

test('content kinds come from the server, once each, in its order; unknown ones are left out', () => {
  const kinds = creatorContentTypes(['application/json', 'text/xml', 'application/xml', 'application/x-future', 'application/hl7-v2']);
  assert.deepEqual(kinds.map((k) => k.kind), ['json', 'xml', 'hl7v2']);
  assert.equal(kinds[1].contentType, 'text/xml');
});

test('patterns: only the format that fits the content, one the server validates, never a withdrawn one', () => {
  assert.deepEqual(compatibleSchemas(subjects, 'application/json', ['json_schema']).map((s) => s.subject), ['ankieta', 'wizyta']);
  assert.deepEqual(compatibleSchemas(subjects, 'application/hl7-v2', ['json_schema']), []);
  assert.deepEqual(compatibleSchemas(subjects, 'application/octet-stream', ['json_schema']), [], 'a binary pattern without a validator is not offered');
  assert.deepEqual(compatibleSchemas(subjects, 'application/octet-stream', ['protobuf']).map((s) => s.subject), ['powiadomienie']);
});

test('copies follow the server resolution, worded for one, several and capped', () => {
  const three = copiesPlan({ defaultReplicationFactor: 3, nodeCount: 3 });
  assert.equal(`${copiesCount(three)}, ${copiesReason(three)}`, '3 kopie, bo w tej instancji są 3 nody');
  const one = copiesPlan({ defaultReplicationFactor: 1, nodeCount: 1 });
  assert.equal(`${copiesCount(one)}, ${copiesReason(one)}`, '1 kopia, bo ta instancja ma jeden node');
  const capped = copiesPlan({ defaultReplicationFactor: 3, nodeCount: 5 });
  assert.equal(capped.kind, 'capped');
  assert.equal(copiesReason(capped), 'bo topik dostaje najwyżej 3 kopie, choć w tej instancji jest 5 nodów');
  assert.equal(copiesReason(copiesPlan({ defaultReplicationFactor: 3, nodeCount: 4 })), 'bo topik dostaje najwyżej 3 kopie, choć w tej instancji są 4 nody');
  const two = copiesPlan({ defaultReplicationFactor: 2, nodeCount: 2 });
  assert.equal(copiesReason(two), 'bo w tej instancji są 2 nody');
  // A number the server's rule (one copy per node, at most 3) does not explain gets no reason.
  assert.equal(copiesPlan({ defaultReplicationFactor: 1, nodeCount: 2 }).kind, 'unknown');
  assert.equal(copiesPlan({}).kind, 'unknown');
});

test('the create request carries what the window asked, never the copies', () => {
  const draft = { ...newDraft(creatorContentTypes(CONTENT)), name: ' wyniki-z-pracowni ', partitions: 4, retentionDays: 90, limitGb: 8, durabilityClass: 'critical', validate: true, schemaId: 'wizyta', onMismatch: 'warn' };
  const req = buildTopicCreateRequest('tentabus-1a2b3c4d', draft);
  assert.deepEqual(req, {
    instanceId: 'tentabus-1a2b3c4d',
    name: 'wyniki-z-pracowni',
    options: {
      partitions: 4,
      retentionMs: 90 * 86_400_000,
      cleanupPolicy: 'delete',
      durabilityClass: 'critical',
      contentType: 'application/json',
      retentionBytesPerPartition: 8 * 1024 ** 3,
      schemaId: 'wizyta',
      validation: 'warn',
    },
  });
  const plain = buildTopicCreateRequest('tentabus-1a2b3c4d', { ...draft, validate: false, limitGb: 0 });
  assert.equal('schemaId' in plain.options, false);
  assert.equal('validation' in plain.options, false);
  assert.equal('retentionBytesPerPartition' in plain.options, false);
  assert.equal('replicationFactor' in plain.options, false);
});

const tick = () => new Promise((r) => setTimeout(r, 0));
const norm = (s) => String(s).replace(/[  ]/g, ' ').replace(/\s+/g, ' ').trim();
const summaryText = (win) => norm([...win.querySelectorAll('.tb-kv-grid > div')].map((d) => d.textContent).join(' '));

function open(overrides = {}) {
  document.querySelectorAll('tf-window').forEach((w) => w.remove());
  const sent = [];
  const created = [];
  const win = openTopicCreator({
    instanceId: 'tentabus-1a2b3c4d',
    instanceLabel: 'Produkcja',
    capabilities: { contentTypes: CONTENT, schemaTypes: ['json_schema'], defaultReplicationFactor: 3, nodeCount: 3 },
    subjects,
    existingNames: ['wyniki-badan'],
    create: async (req) => { sent.push(req); },
    onCreated: (r) => created.push(r),
    ...overrides,
  });
  return { win, sent, created };
}

const nextBtn = (win) => win.querySelector('[data-act="next"]');
const typeName = (win, value) => {
  const input = win.querySelector('#tb-cr-name');
  input.value = value;
  input.dispatchEvent(new CustomEvent('input', { detail: { value } }));
};

test('step 1: Dalej waits for a usable name; a taken name says so', () => {
  const { win } = open();
  assert.equal(win.getAttribute('modal'), '', 'the list behind is dimmed');
  assert.ok(nextBtn(win).hasAttribute('disabled'));
  typeName(win, 'wyniki-badan');
  assert.match(win.querySelector('#tb-cr-name').getAttribute('error'), /już jest/);
  assert.ok(nextBtn(win).hasAttribute('disabled'));
  typeName(win, 'wyniki-z-pracowni');
  assert.equal(win.querySelector('#tb-cr-name').hasAttribute('error'), false);
  assert.equal(nextBtn(win).hasAttribute('disabled'), false);
  assert.equal(win.querySelector('[data-role="heading"]').textContent, 'wyniki-z-pracowni');
  const cards = [...win.querySelectorAll('#tb-cr-kind tf-choice-card')].map((c) => c.getAttribute('heading'));
  assert.deepEqual(cards, ['JSON', 'XML', 'HL7 v2']);
});

test('the whole walk: kind, storage with the copies sentence, pattern, summary, create', async () => {
  const { win, sent, created } = open();
  typeName(win, 'wyniki-z-pracowni');
  win.querySelector('#tb-cr-kind').dispatchEvent(new CustomEvent('change', { detail: { value: 'application/json' } }));
  nextBtn(win).click();
  assert.match(norm(win.querySelector('.tb-copies-box').textContent), /3 kopie, bo w tej instancji są 3 nody\. Gdy jeden node przestanie działać/);
  assert.match(win.querySelector('.install-step.done .label').textContent, /Nazwa, treść i partycje/);
  win.querySelector('#tb-cr-retention').dispatchEvent(new CustomEvent('change', { detail: { value: '90' } }));
  win.querySelector('#tb-cr-durability').dispatchEvent(new CustomEvent('change', { detail: { value: 'critical' } }));
  nextBtn(win).click();
  // Patterns that fit JSON, the withdrawn one left out, checking on by default.
  assert.ok(win.querySelector('#tb-cr-validate').hasAttribute('checked'));
  const summary = summaryText(win);
  assert.match(summary, /Rodzaj treści JSON/);
  assert.match(summary, /Przechowywanie 90 dni, usuwaj stare wiadomości/);
  assert.match(summary, /Kopie 3, bo w tej instancji są 3 nody/);
  assert.match(summary, /Trwałość krytyczna/);
  assert.match(summary, /Wzór wiadomości ankieta, najnowsza wersja \(1\); niepasujące do nieprzetworzonych/);
  assert.match(win.querySelector('#tb-cr-schema').getAttribute('hint'), /JSON Schema.*JSON/);
  win.querySelector('[data-act="back"]').click();
  assert.equal(win.querySelector('#tb-cr-retention') !== null, true, 'Wstecz returns to step 2');
  nextBtn(win).click();
  nextBtn(win).click();
  await tick();
  assert.equal(sent.length, 1);
  assert.equal(sent[0].options.schemaId, 'ankieta');
  assert.equal(sent[0].options.validation, 'dlq');
  assert.equal(sent[0].options.retentionMs, 90 * 86_400_000);
  assert.equal(created.length, 1);
  assert.equal(created[0].name, 'wyniki-z-pracowni');
});

test('HL7 v2 with no fitting pattern: step 3 says the topic starts without one', () => {
  const { win } = open();
  typeName(win, 'nowy');
  win.querySelector('#tb-cr-kind').dispatchEvent(new CustomEvent('change', { detail: { value: 'application/hl7-v2' } }));
  nextBtn(win).click();
  nextBtn(win).click();
  assert.equal(win.querySelector('#tb-cr-validate'), null);
  assert.match(win.querySelector('.tb-explain-box').textContent, /nie ma wzorów dla treści HL7 v2/);
  assert.match(summaryText(win), /Wzór wiadomości bez wzoru/);
});

test('an instance with one node and no patterns (Szkolenia)', () => {
  const { win } = open({ subjects: [], capabilities: { contentTypes: CONTENT, schemaTypes: ['json_schema'], defaultReplicationFactor: 1, nodeCount: 1 } });
  typeName(win, 'nowy');
  nextBtn(win).click();
  assert.match(norm(win.querySelector('.tb-copies-box').textContent), /1 kopia, bo ta instancja ma jeden node\. Gdy dołączysz kolejne nody/);
  nextBtn(win).click();
  assert.match(win.querySelector('.tb-explain-box').textContent, /nie ma jeszcze wzorów wiadomości/);
});

test('a refused create keeps the window open with the reason, and it can be sent again', async () => {
  let fail = true;
  const { win, created } = open({
    create: async () => { if (fail) throw new Error('bus.topic_already_exists'); },
    describeError: () => 'Topik o tej nazwie już istnieje.',
  });
  typeName(win, 'nowy');
  nextBtn(win).click();
  nextBtn(win).click();
  nextBtn(win).click();
  await tick();
  assert.equal(win.isConnected, true);
  assert.equal(win.querySelector('.tb-window-error').hidden, false);
  assert.match(win.querySelector('.tb-window-error').textContent, /już istnieje/);
  assert.equal(created.length, 0);
  fail = false;
  nextBtn(win).click();
  await tick();
  assert.equal(created.length, 1);
});

test('partitions: a whole number from 1 to 256, anything else stops Dalej with a reason', () => {
  assert.equal(partitionsValue('3'), 3);
  assert.equal(partitionsValue(' 256 '), 256);
  for (const bad of ['0', '257', '', '2.5', '-1', 'x']) assert.equal(partitionsValue(bad), null, bad);
  const { win } = open();
  typeName(win, 'nowy');
  const field = win.querySelector('#tb-cr-partitions');
  field.value = '0';
  field.dispatchEvent(new CustomEvent('input', { detail: { value: '0' } }));
  assert.match(field.getAttribute('error'), /od 1 do 256/);
  assert.ok(nextBtn(win).hasAttribute('disabled'));
  field.value = '12';
  field.dispatchEvent(new CustomEvent('input', { detail: { value: '12' } }));
  assert.equal(field.hasAttribute('error'), false);
  assert.equal(nextBtn(win).hasAttribute('disabled'), false);
});

test('the heading shows the typed name only once it could be a topic name', () => {
  const { win } = open();
  typeName(win, 'Zła Nazwa');
  assert.equal(win.querySelector('[data-role="heading"]').textContent, 'nowy-topik');
  typeName(win, 'wyniki-badan');
  assert.equal(win.querySelector('[data-role="heading"]').textContent, 'wyniki-badan');
});

test('checking turned on for JSON does not lock the window after switching to a kind without patterns', () => {
  const { win } = open();
  typeName(win, 'nowy');
  win.querySelector('#tb-cr-kind').dispatchEvent(new CustomEvent('change', { detail: { value: 'application/json' } }));
  nextBtn(win).click();
  nextBtn(win).click();
  assert.ok(win.querySelector('#tb-cr-validate').hasAttribute('checked'), 'JSON has patterns: checking is on');
  win.querySelector('[data-act="back"]').click();
  win.querySelector('[data-act="back"]').click();
  win.querySelector('#tb-cr-kind').dispatchEvent(new CustomEvent('change', { detail: { value: 'application/xml' } }));
  nextBtn(win).click();
  nextBtn(win).click();
  assert.equal(win.querySelector('#tb-cr-validate'), null);
  assert.equal(nextBtn(win).hasAttribute('disabled'), false, 'Utwórz stays reachable');
  assert.match(summaryText(win), /Wzór wiadomości bez wzoru/);
});

test('a pattern with no known version is summed up without one', () => {
  const { win } = open({ subjects: [{ subject: 'wizyta', schemaType: 'json_schema', deprecatedAtMs: null }] });
  typeName(win, 'nowy');
  nextBtn(win).click();
  nextBtn(win).click();
  const summary = summaryText(win);
  assert.match(summary, /Wzór wiadomości wizyta; niepasujące do nieprzetworzonych/);
  assert.doesNotMatch(summary, /wersja/);
});

test('patterns that could not be read are said to be missing, not absent, and can be asked for again', async () => {
  let fail = true;
  let asked = 0;
  const { win, sent } = open({
    subjects: null,
    reloadSubjects: async () => { asked += 1; if (fail) throw new Error('bus.internal'); return subjects; },
  });
  await tick();
  assert.equal(asked, 1, 'a list that has not arrived is asked for when the window opens');
  typeName(win, 'nowy');
  nextBtn(win).click();
  nextBtn(win).click();
  const box = win.querySelector('.tb-explain-box--error');
  assert.match(box.textContent, /Nie udało się wczytać wzorów wiadomości instancji Produkcja/);
  assert.doesNotMatch(win.querySelector('.install-step-body').textContent, /nie ma jeszcze wzorów/);
  assert.equal(nextBtn(win).hasAttribute('disabled'), false, 'the topic can still be created without a pattern');
  fail = false;
  win.querySelector('[data-act="reload-subjects"]').click();
  await tick();
  await tick();
  assert.equal(asked, 2);
  assert.ok(win.querySelector('#tb-cr-validate').hasAttribute('checked'));
  assert.match(summaryText(win), /Wzór wiadomości ankieta, najnowsza wersja \(1\)/);
  nextBtn(win).click();
  await tick();
  assert.equal(sent[0].options.schemaId, 'ankieta');
});
