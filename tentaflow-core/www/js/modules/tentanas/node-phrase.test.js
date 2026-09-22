// =============================================================================
// File: modules/tentanas/node-phrase.test.js
// Description: `nodeT` picks a whole sentence for a nameless node instead of
// dropping the "Węzeł bez nazwy" fallback into a template that already says
// "węzeł" — for every template that needs it, in every locale — and the
// Pools heading uses it. Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, I18n } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { nodeT, nodeHeadSub } = await import('./node-phrase.js');
const { drawPools } = await import('./pools.js');

// Every template that embeds a node name after a word that already names the
// node ("węzeł", "na węźle", "{user}@").
const CASES = [
  ['pools.title', {}],
  ['setup.node_scope', {}],
  ['arc.settings_hint', { ram: '64 GB' }],
  ['env.armed_node', {}],
  ['sudo.explain_remote', {}],
  ['sudo.password_label', { user: 'tentaflow' }],
];

test('a named node goes into the template, a nameless one gets its own sentence', () => {
  assert.equal(nodeT('pools.title', { nodeName: 'orion' }), 'Pule na węźle orion');
  assert.equal(nodeT('pools.title', { nodeName: '  ' }), 'Pule na węźle bez nazwy');
  assert.equal(nodeT('pools.title', null), 'Pule na węźle bez nazwy', 'no node at all reads the same');
  assert.equal(nodeT('sudo.password_label', { nodeName: 'orion' }, { user: 'tentaflow' }), 'Hasło sudo użytkownika tentaflow@orion');
  assert.equal(nodeT('sudo.password_label', {}, { user: 'tentaflow' }), 'Hasło sudo użytkownika tentaflow na węźle bez nazwy');
});

// The header subline used to be its own copy of this same chooser, in
// format.js (`nodeHeadSub`), with no `node.head_sub_node_unnamed` sibling
// key of its own — the bare "Węzeł bez nazwy" already is the whole
// sentence. Routed through the single `nodeT` mechanism, a missing sibling
// key must fall back to that phrase, never leak the raw
// "tentanas.node.head_sub_node_unnamed" string onto the screen.
test('nodeHeadSub reads naturally for a named and a nameless node, through nodeT', () => {
  assert.equal(nodeHeadSub({ nodeName: 'orion' }), 'węzeł orion');
  assert.equal(nodeHeadSub({ nodeName: '' }), 'Węzeł bez nazwy');
  assert.equal(nodeHeadSub(null), 'Węzeł bez nazwy', 'no node at all reads the same');
  assert.doesNotMatch(nodeHeadSub({ nodeName: '' }), /tentanas\./, 'no missing-key leak when there is no dedicated _unnamed sibling');
});

test('no nameless sentence doubles the noun or glues the fallback label in, in any locale', async () => {
  try {
    for (const lang of ['pl', 'en', 'de', 'es', 'fr']) {
      await I18n.setLanguage(lang);
      const fallback = I18n.t('tentanas.node.unnamed');
      for (const [key, params] of CASES) {
        const text = nodeT(key, { nodeName: '' }, params);
        assert.ok(!text.includes('tentanas.'), `${lang} ${key}: the _unnamed key exists (${text})`);
        const nested = I18n.t('tentanas.' + key, { ...params, node: fallback });
        assert.notEqual(text, nested, `${lang} ${key}: the fallback label is not nested in the template`);
        assert.ok(!text.includes('{'), `${lang} ${key}: every placeholder is filled (${text})`);
        assert.ok(!/@/.test(text), `${lang} ${key}: no "user@<nothing>" (${text})`);
        assert.ok(!/(węz\w*|node|knoten|nodo|nœud)\s+(węz\w*|node|knoten|nodo|nœud)/i.test(text), `${lang} ${key}: ${text}`);
      }
    }
  } finally {
    await I18n.setLanguage('pl');
  }
});

test('the Pools heading of a nameless node reads naturally', async () => {
  const screen = fakeScreen({
    tentaNasPoolsListRequest: { pools: [], freeDisks: [] },
    tentaNasDisksListRequest: { disks: [] },
    tentaNasElasticArraysListRequest: { arrays: [] },
    tentaNasElasticCapabilitiesRequest: { freeDisks: [] },
  });
  screen.currentNode = () => ({ nodeId: 'node-x', nodeName: '', isLocal: true });
  const body = document.createElement('div');
  document.body.append(body);
  try {
    await drawPools(screen, body);
    await flush();
    const title = body.querySelector('.nas-pools-heading .title').textContent;
    assert.match(title, /Pule na węźle bez nazwy/);
    assert.doesNotMatch(title, /Węzeł bez nazwy/);
  } finally {
    // Stops the tab's poll even when an assertion fails, so a failure never
    // keeps the test process alive.
    screen.dispose();
  }
});
