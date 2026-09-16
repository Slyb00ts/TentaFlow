// =============================================================================
// File: modules/tentanas/dom-patch.test.js
// Description: The shared in-place DOM patchers, tested as the primitive they
// are — the number of DOM operations a poll performs, not just the shape it
// leaves behind. Every caller on this screen polls, so a pass that lands the
// right markup by moving six nodes where one would do is still a bug: the
// screen blinks and anything the pointer was over is re-inserted underneath it.
// dom-patch.js imports nothing, so this file takes the bare happy-dom harness
// rather than the TentaNas bootstrap — a primitive's test owes nothing to the
// locale files.
// =============================================================================

import '../../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { setText, patchHtml, patchKeyedList } = await import('./dom-patch.js');

function freshHost() {
  document.body.innerHTML = '';
  const host = document.createElement('div');
  document.body.appendChild(host);
  return host;
}

// `insertBefore` is the operation that costs: it is how a node both moves and
// arrives, and a moved node is a node the browser re-lays-out and re-paints.
function countInserts(host, run) {
  const real = host.insertBefore;
  let inserts = 0;
  host.insertBefore = function counted(node, ref) { inserts += 1; return real.call(this, node, ref); };
  try { run(); } finally { delete host.insertBefore; }
  return inserts;
}

// Six nodes is a realistic fleet. `ticking` is the index whose card changed —
// one node's uptime or used-bytes crossing a rounding boundary, which the
// module's own comment says happens on nearly every 10 s poll.
const FLEET = ['n0', 'n1', 'n2', 'n3', 'n4', 'n5'];
const fleet = (ticking) => FLEET.map((key, i) => ({
  key,
  html: `<div class="node-card" data-node="${key}">${key} ${i === ticking ? '13 min' : '12 min'}</div>`,
}));

// Departing keys used to be removed AFTER the ordering walk, so the cursor
// parked on an element that was already doomed: nothing ever matched it and
// every survivor behind the changed card was re-inserted in front of it. The
// DOM was correct at the end and every surviving element kept its identity —
// which is all an identity assertion can see — while the browser had moved the
// whole row. Measured before the fix: 6 / 4 / 1 insertBefore for a card
// ticking at index 0 / 2 / 5.
test('one ticking card moves one node, not every card behind it', () => {
  for (const ticking of [0, 2, 5]) {
    const host = freshHost();
    patchKeyedList(host, fleet(-1));
    const before = [...host.children];
    assert.equal(before.length, 6, 'six cards to start with');

    const inserts = countInserts(host, () => patchKeyedList(host, fleet(ticking)));
    assert.equal(inserts, 1, `card #${ticking} ticked: 1 insertBefore expected, got ${inserts}`);

    assert.deepEqual([...host.children].map((el) => el.dataset.node), FLEET, `card #${ticking}: order kept`);
    before.forEach((el, i) => {
      if (i === ticking) return;
      assert.equal(host.children[i] === el, true, `card #${ticking} ticked: card ${i} kept its identity`);
    });
    assert.equal(host.children[ticking] === before[ticking], false, `card #${ticking} ticked: that one card was rebuilt`);
  }
});

// The case the whole module exists for; a guard against a "fix" that buys the
// move count by rebuilding on every poll.
test('an unchanged poll touches no node at all', () => {
  const host = freshHost();
  patchKeyedList(host, fleet(-1));
  const before = [...host.childNodes];

  let changed = true;
  const inserts = countInserts(host, () => { changed = patchKeyedList(host, fleet(-1)); });
  assert.equal(inserts, 0, `no insertBefore on an unchanged poll, got ${inserts}`);
  assert.equal(changed, false, 'and the caller is told nothing changed, so it re-wires nothing');
  assert.equal(host.childNodes.length, before.length, 'same node count');
  before.forEach((n, i) => assert.equal(host.childNodes[i] === n, true, `node ${i} untouched`));
});

// One host, one writer is the rule, but this is a shared primitive and the
// rule will be broken. `__tfKeyed` outlives the elements it points at: after
// another writer clears the host they are detached, and putting one back would
// return to the screen a node that writer deliberately replaced.
test('a card another writer destroyed is rebuilt, not resurrected', () => {
  const host = freshHost();
  const items = [{ key: 'n0', html: '<div class="node-card">orion</div>' }];
  patchKeyedList(host, items);
  const first = host.firstElementChild;

  patchHtml(host, '<div class="spinner">…</div>');
  assert.equal(first.parentNode === null, true, 'the keyed card really left the document');

  patchKeyedList(host, items);
  assert.equal(host.children.length, 1, 'one card on screen, not two');
  assert.equal(host.firstElementChild === first, false, 'the card is built fresh, not the detached node put back');
  assert.equal(host.textContent, 'orion', 'and it carries the item markup');
});

// The ordering walk follows firstElementChild/nextElementSibling, so a text
// node is invisible to it — a `setText` (or a raw `textContent =`) on a host
// that later takes a keyed pass left a text node NO later pass could reach,
// and the screen read "ładowanie…12" forever.
test('a keyed pass clears what a text writer left on the same host', () => {
  const host = freshHost();
  const items = [{ key: 'n0', html: '<div class="node-card">12</div>' }];
  patchKeyedList(host, items);
  setText(host, 'ładowanie…');

  patchKeyedList(host, items);
  assert.equal(host.textContent, '12', 'no "ładowanie…12" left on screen');
  assert.equal(host.childNodes.length, 1, 'the stray text node is gone, not merely hidden behind the card');
});

// An item whose html yields no root element has nothing to keep, so it used to
// fall out of the cache entirely — and a caller that keeps producing it paid a
// createElement and an innerHTML parse for it on every poll, forever, with
// nothing on screen to show for it. Remembering the dead key costs one map
// entry and ends the leak.
test('an item whose html has no root element is parsed once, not on every poll', () => {
  const host = freshHost();
  const items = [
    { key: 'n0', html: '<div class="node-card">orion</div>' },
    { key: 'blank', html: '   ' },
  ];
  patchKeyedList(host, items);
  assert.equal(host.children.length, 1, 'the empty item contributes no card');

  const real = document.createElement;
  let created = 0;
  document.createElement = function counted(tag) { created += 1; return real.call(this, tag); };
  try { patchKeyedList(host, items); } finally { document.createElement = real; }
  assert.equal(created, 0, `an unchanged poll re-parses nothing, got ${created} createElement call(s)`);
  assert.equal(host.children.length, 1, 'and the real card is still there');
});
