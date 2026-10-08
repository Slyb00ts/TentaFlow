// =============================================================================
// File: modules/tentabus/hl7-fields.test.js
// Description: The HL7 v2 dictionary of the data-hiding window: every entry is
// an address the server accepts, none is a separator field, and each has a
// plain name in all five languages; the unnamed positions of the same segments
// are allowed by default, so a message's own MSH-11 or PID-4 is never refused.
// =============================================================================

import { WWW_ROOT } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const { HL7_FIELDS, HL7_IMPLICIT_FIELDS, hl7FieldLabel, hl7LabelKey } = await import('./hl7-fields.js');
const { fieldNameProblem } = await import('./topic-hiding.js');

const LOCALES = ['pl', 'en', 'de', 'es', 'fr'];
const bundles = Object.fromEntries(LOCALES.map((l) => [l, JSON.parse(readFileSync(join(WWW_ROOT, 'i18n', `${l}.json`), 'utf8'))]));
const lookup = (bundle, path) => path.split('.').reduce((node, part) => node?.[part], bundle.tentabus);

test('the dictionary lists the common fields once each, every one an address the server accepts', () => {
  assert.ok(HL7_FIELDS.length >= 55 && HL7_FIELDS.length <= 70, `about sixty fields, got ${HL7_FIELDS.length}`);
  assert.equal(new Set(HL7_FIELDS).size, HL7_FIELDS.length);
  for (const address of HL7_FIELDS) assert.equal(fieldNameProblem('hl7v2', address), null, address);
  assert.ok(!HL7_FIELDS.includes('MSH-1') && !HL7_FIELDS.includes('MSH-2'), 'the separators of the message cannot be filtered');
  for (const common of ['PID-3', 'PID-5', 'PID-7', 'PID-8', 'PID-11', 'PID-13', 'PID-19', 'OBX-5', 'OBX-8', 'NK1-2', 'IN1-36']) {
    assert.ok(HL7_FIELDS.includes(common), common);
  }
});

test('every field has a plain name in all five languages', () => {
  for (const address of HL7_FIELDS) {
    for (const l of LOCALES) {
      const text = lookup(bundles[l], hl7LabelKey(address));
      assert.equal(typeof text, 'string', `${address} in ${l}`);
      assert.ok(text.trim().length > 0, `${address} in ${l} is not blank`);
    }
  }
  assert.equal(hl7LabelKey('PID-5'), 'hiding.hl7.pid_5');
});

test('a name comes from the dictionary only; another address has none', () => {
  assert.equal(hl7FieldLabel('PID-5'), 'Imię i nazwisko pacjenta');
  assert.equal(hl7FieldLabel('OBX-8'), 'Znaczniki nieprawidłowego wyniku');
  assert.equal(hl7FieldLabel('PID-31'), '');
  assert.equal(hl7FieldLabel('pacjent'), '');
});

test('the unnamed positions of the dictionary\'s segments are allowed by default: set-ids, EVN, the rest of MSH — never the separators', () => {
  assert.equal(new Set(HL7_IMPLICIT_FIELDS).size, HL7_IMPLICIT_FIELDS.length);
  for (const address of HL7_IMPLICIT_FIELDS) assert.equal(fieldNameProblem('hl7v2', address), null, address);
  for (const must of ['MSH-13', 'PID-1', 'PID-4', 'PID-9', 'PV1-1', 'OBX-1', 'EVN-1', 'EVN-2', 'NTE-3']) assert.ok(HL7_IMPLICIT_FIELDS.includes(must), must);
  assert.ok(HL7_IMPLICIT_FIELDS.every((a) => !HL7_FIELDS.includes(a)), 'a named field is a row, not an implicit position');
  assert.ok(['MSH-1', 'MSH-2'].every((a) => !HL7_IMPLICIT_FIELDS.includes(a)));
  assert.ok(['MSH-8', 'MSH-11', 'MSH-12'].every((a) => HL7_FIELDS.includes(a)), 'the structural fields every message carries have rows');
});

test('the common segments beyond the first dozen are covered up to the width HL7 v2.8 defines, so an ordinary message is not refused', () => {
  const widest = {
    PV2: 49, PD1: 22, ROL: 13, GT1: 57, IN2: 85, SFT: 6, TQ1: 14, SPM: 30, MSA: 6, ERR: 12, MRG: 7, NTE: 9,
  };
  for (const [segment, width] of Object.entries(widest)) {
    for (const n of [1, 2, width]) assert.ok(HL7_IMPLICIT_FIELDS.includes(`${segment}-${n}`), `${segment}-${n}`);
    assert.ok(!HL7_IMPLICIT_FIELDS.includes(`${segment}-${width + 1}`), `${segment} stops at ${width}`);
  }
  assert.ok(HL7_IMPLICIT_FIELDS.includes('NTE-9'), 'NTE has nine fields since v2.7, not four');
  assert.ok(!HL7_IMPLICIT_FIELDS.includes('ZPI-1'), 'a Z segment is the administrator\'s to name');
});

