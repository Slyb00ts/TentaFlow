// =============================================================================
// File: modules/flows-builder/config.test.js
// Description: Unit tests for `FlowConfig._renderField` in config.js — the
//       generic node-config-form renderer. Before this change a JSON-schema
//       `type: "object"` field (e.g. bus_publish's `headers`, an object of
//       string -> CEL string) had no arm and silently fell through to the
//       plain-string `<tf-input>` fallback, so it could not actually be
//       edited. This pins the new 'object' arm: it must render
//       <tf-keyvalue-editor>, not the string fallback, and it must still
//       leave the existing boolean/enum/number/textarea/string arms alone.
//       `_renderField` is a class method (not exported — the module pulls in
//       the whole flow builder), so its real source is cut out of the shipped
//       file by brace matching and evaluated against stubs, same convention
//       as ml-studio.derive-targets.test.js / code-studio.list-cells.test.js.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, 'config.js'), 'utf8');

// Cuts a class method's source by locating its exact signature and balancing
// braces from the opening `{` of its body — same technique the other
// `*.test.js` files in this repo use for a top-level `function`, adapted for
// a method (no `function` keyword to anchor on).
function cutMethod(src, signature) {
  const start = src.indexOf(signature);
  if (start < 0) throw new Error(`no definition: ${signature}`);
  let depth = 0;
  let i = src.indexOf('{', start);
  for (; i < src.length; i += 1) {
    if (src[i] === '{') depth += 1;
    else if (src[i] === '}') {
      depth -= 1;
      if (depth === 0) break;
    }
  }
  return src.slice(start, i + 1);
}

const escapeHtml = (v) => String(v ?? '')
  .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
const escapeAttr = (v) => String(v ?? '')
  .replace(/&/g, '&amp;').replace(/"/g, '&quot;');

const methodSrc = cutMethod(source, '_renderField(key, def, value, isRequired) {');
// eslint-disable-next-line no-new-func
const renderField = new Function(
  'escapeHtml', 'escapeAttr', 'I18n',
  `return { ${methodSrc} }._renderField;`,
)(escapeHtml, escapeAttr, { t: (key) => key });

// ---------------------------------------------------------------------------
// 'object' arm
// ---------------------------------------------------------------------------

test("_renderField: a schema type 'object' field renders <tf-keyvalue-editor>, not the string fallback", () => {
  const html = renderField('headers', { type: 'object', title: 'Nagłówki' }, undefined, false);
  assert.match(html, /<tf-keyvalue-editor\b/);
  assert.doesNotMatch(html, /<tf-input\b/);
  assert.doesNotMatch(html, /<tf-textarea\b/);
});

test("_renderField: the object arm binds the schema key via data-bind and marks data-type=\"object\"", () => {
  const html = renderField('headers', { type: 'object', title: 'Nagłówki' }, undefined, false);
  assert.match(html, /data-bind="headers"/);
  assert.match(html, /data-type="object"/);
});

test('_renderField: the object arm carries the field title and required marker like every other arm', () => {
  const html = renderField('headers', { type: 'object', title: 'Nagłówki' }, undefined, true);
  assert.match(html, /Nagłówki/);
  assert.match(html, /\*/);
});

test('_renderField: the object arm surfaces the hint text when the schema has a description', () => {
  const html = renderField(
    'headers',
    { type: 'object', title: 'Nagłówki', description: 'Mapa nazwa -> CEL' },
    undefined,
    false,
  );
  assert.match(html, /Mapa nazwa -&gt; CEL|Mapa nazwa -> CEL/);
});

// ---------------------------------------------------------------------------
// Existing arms are unaffected by the new 'object' branch
// ---------------------------------------------------------------------------

test('_renderField: boolean still renders <tf-toggle>', () => {
  const html = renderField('enabled', { type: 'boolean' }, true, false);
  assert.match(html, /<tf-toggle\b/);
});

test('_renderField: an enum still renders <tf-select> with its options', () => {
  const html = renderField('mode', { type: 'string', enum: ['a', 'b'] }, 'a', false);
  assert.match(html, /<tf-select\b/);
  assert.match(html, /value="a"/);
});

test('_renderField: a plain string still falls back to <tf-input>', () => {
  const html = renderField('topic', { type: 'string' }, 'orders', false);
  assert.match(html, /<tf-input\b/);
  assert.doesNotMatch(html, /<tf-keyvalue-editor\b/);
});

// ---------------------------------------------------------------------------
// The node a binding writes to is the node the panel showed when it bound —
// selecting another block while a field still has focus fires that field's
// `change` on the way out, and the edit belongs to the block being left.
// ---------------------------------------------------------------------------

function bindAdvanced(panel, body) {
  // The DEFINITION, not the call site a few hundred lines above it.
  const method = cutMethod(source, '\n  _bindAdvancedInputs(body) {');
  // eslint-disable-next-line no-new-func
  const fn = new Function(`return function ${method.replace('_bindAdvancedInputs', '')}`)();
  fn.call(panel, body);
}

function fakeField(dataset, value) {
  const listeners = [];
  return {
    dataset,
    value,
    addEventListener: (_name, cb) => listeners.push(cb),
    fire: () => listeners.forEach((cb) => cb()),
  };
}

test('_bindAdvancedInputs writes the position to the node it was bound to', () => {
  const moved = [];
  const position = fakeField({ bindPos: 'x' }, '120');
  const body = {
    querySelectorAll: () => [position],
    querySelector: () => null,
  };
  const panel = {
    node: { id: 'node-a' },
    opts: { onPositionChange: (id, patch) => moved.push([id, patch]) },
  };
  bindAdvanced(panel, body);

  // The operator clicks another block: the panel points at it before the
  // focused field emits its change.
  panel.node = { id: 'node-b' };
  position.fire();
  assert.deepEqual(moved, [['node-a', { x: 120 }]]);
});

test('_bindAdvancedInputs writes pasted JSON to the node it was bound to', () => {
  const written = [];
  const raw = fakeField({}, '{"topic":"orders"}');
  const body = {
    querySelectorAll: () => [],
    querySelector: (sel) => (sel === '[data-bind-raw="config"]' ? raw : null),
  };
  const panel = {
    node: { id: 'node-a' },
    opts: { onRawConfigChange: (id, config) => written.push([id, config]) },
  };
  bindAdvanced(panel, body);

  panel.node = { id: 'node-b' };
  raw.fire();
  assert.deepEqual(written, [['node-a', { topic: 'orders' }]]);
});
