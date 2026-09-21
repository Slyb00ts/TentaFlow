// =============================================================================
// File: modules/agent-accounts.test.js
// Description: The node NAME a cell title is built from.
//       Core has no name to send for a node it does not know, so
//       `dispatch/provider_account.rs` substitutes the node id and lets this
//       layer decide what a missing name looks like — which is the whole point
//       of `nodeTitle`. A 64-hex id in a cell TITLE is the thing this project
//       forbids, and the defect this pins is exactly that: the id was shown as
//       a name, and a name that IS the id was shown just the same. Both the
//       "Na nodach" table and the sessions `node` column render it through
//       `usedOnLabel`, so the two are pinned together. The functions ARE
//       exported, so the shipped code is imported rather than cut out.
// =============================================================================

import '/js/sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, resolve } from 'node:path';

const WWW_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');

// The harness serves `file:` URLs (the wasm codec) from disk; the locale files
// are served the same way so the labels are the strings the app ships.
const harnessFetch = globalThis.fetch;
globalThis.fetch = (url, init) => {
  const match = /^\/i18n\/(\w+)\.json$/.exec(String(url));
  if (!match) return harnessFetch(url, init);
  const text = readFileSync(join(WWW_ROOT, 'i18n', `${match[1]}.json`), 'utf8');
  return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(JSON.parse(text)) });
};
if (typeof globalThis.localStorage === 'undefined') {
  const store = new Map();
  globalThis.localStorage = {
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => store.set(k, String(v)),
    removeItem: (k) => store.delete(k),
  };
}

// Switching the language tells Core about the preference, which would open the
// dashboard's WebSocket and leave it reconnecting for as long as the process
// lives. The layer under test never touches it, so the one call i18n makes
// answers here.
const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
ApiBinary.action = () => Promise.resolve({});

const { I18n } = await import('/js/i18n.js');
const { nodeTitle, usedOnLabel } = await import('/js/modules/agent-accounts.js');

const LOCALES = ['pl', 'en', 'de', 'es', 'fr'];

// A real node id: 64 hex is what the wire sends when Core knows no name, and
// what must never reach a cell title.
const NODE_ID = 'a3f1c07d5b28e64f90d1ab34c7e5082f6b1d9a40e3c75f8216ba4d09e7c31f58';

async function locale(lang) {
  await I18n.setLanguage(lang);
  const root = JSON.parse(readFileSync(join(WWW_ROOT, 'i18n', `${lang}.json`), 'utf8'));
  return root.agent_accounts;
}

// ---------------------------------------------------------------------------
// A node that has a name
// ---------------------------------------------------------------------------

test('a node with a name shows that name, and never its id', async () => {
  for (const lang of LOCALES) {
    await locale(lang);
    assert.equal(nodeTitle({ node_id: NODE_ID, node_name: 'Helios' }), 'Helios', `${lang} snake_case`);
    assert.equal(nodeTitle({ nodeId: NODE_ID, nodeName: 'Helios' }), 'Helios', `${lang} camelCase`);
    // Whitespace around a name is not a name.
    assert.equal(nodeTitle({ node_id: NODE_ID, node_name: '  Helios  ' }), 'Helios', `${lang} trimmed`);
    // A short id is still an id: the wire's `node_name` may be any node id.
    assert.equal(nodeTitle({ node_id: NODE_ID, node_name: 'helios' }), 'helios', `${lang} a real short name`);
  }
});

// ---------------------------------------------------------------------------
// A node whose name is missing — the defect
// ---------------------------------------------------------------------------

test('a node with no usable name says so in the operator’s language, never as an id', async () => {
  for (const lang of LOCALES) {
    const dict = await locale(lang);
    const fallback = dict.node_unnamed;
    assert.ok(typeof fallback === 'string' && fallback.trim(), `${lang} has no node_unnamed text`);

    const cases = {
      // Core sends the id BOTH ways when it has no name to send.
      'name is the id': { node_id: NODE_ID, node_name: NODE_ID },
      'name is the id, camelCase': { nodeId: NODE_ID, nodeName: NODE_ID },
      'no name field': { node_id: NODE_ID },
      'blank name': { node_id: NODE_ID, node_name: '   ' },
      'no node at all': null,
      'no fields at all': {},
    };
    for (const [what, node] of Object.entries(cases)) {
      const title = nodeTitle(node);
      assert.equal(title, fallback, `${lang} ${what}`);
      assert.notEqual(title, NODE_ID, `${lang} ${what}: the id is on the screen`);
      assert.ok(title.trim().length > 0, `${lang} ${what}: an empty cell title`);
      assert.ok(!title.includes(NODE_ID), `${lang} ${what}: the id is inside the title`);
    }
  }
});

test('the fallback is the locale’s own text, not one language for all five', async () => {
  const seen = new Map();
  for (const lang of LOCALES) {
    const dict = await locale(lang);
    const title = nodeTitle({ node_id: NODE_ID, node_name: NODE_ID });
    seen.set(lang, title);
    assert.equal(title, dict.node_unnamed, `${lang} fallback text`);
  }
  // Four languages that spell "unnamed node" differently must not all print the
  // same word: that is what an untranslated key looks like.
  assert.equal(new Set(seen.values()).size, LOCALES.length,
    `the fallback is not translated per locale: ${JSON.stringify([...seen])}`);
});

// ---------------------------------------------------------------------------
// The "Używane na" cell and the sessions `node` column
// ---------------------------------------------------------------------------

test('"used on" names every node it holds, and joins them for one cell', async () => {
  for (const lang of LOCALES) {
    const dict = await locale(lang);
    assert.equal(
      usedOnLabel({ used_on: [{ node_id: NODE_ID, node_name: 'Helios' }, { node_id: 'b', node_name: 'Selene' }] }),
      'Helios, Selene',
      `${lang} two named nodes`,
    );
    assert.equal(usedOnLabel({ usedOn: [{ nodeId: 'c', nodeName: 'Eos' }] }), 'Eos', `${lang} camelCase`);
    // Nothing reported here is an em dash, not an empty cell.
    assert.equal(usedOnLabel({ used_on: [] }), dict.value_none, `${lang} empty list`);
    assert.equal(usedOnLabel({}), dict.value_none, `${lang} no list at all`);
  }
});

test('an unnamed node in "used on" reads as the localized fallback, not as an id', async () => {
  for (const lang of LOCALES) {
    const dict = await locale(lang);
    const label = usedOnLabel({
      used_on: [{ node_id: NODE_ID, node_name: NODE_ID }, { node_id: 'b', node_name: 'Selene' }],
    });
    assert.ok(label.includes(dict.node_unnamed), `${lang}: the unnamed node lost its fallback`);
    assert.ok(label.includes('Selene'), `${lang}: the named node lost its name`);
    assert.ok(!label.includes(NODE_ID), `${lang}: the id is on the screen`);
  }
});
