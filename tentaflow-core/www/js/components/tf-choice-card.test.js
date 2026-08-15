// =============================================================================
// File: components/tf-choice-card.test.js
// Description: Tests for <tf-choice-card> / <tf-choice-group> — the card that
// presents one option of an irreversible architectural choice. The states that
// matter are the ones a wizard gets wrong: the consequence list, the exclusive
// selection, and the UNAVAILABLE option that must still say what to install.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const WWW_ROOT = join(here, '..', '..');

const { window } = await import('../sdk-runtime/_dom-test-harness.js');
if (typeof globalThis.MutationObserver !== 'function' && window.MutationObserver) {
  globalThis.MutationObserver = window.MutationObserver;
}

const { TfChoiceCard, TfChoiceGroup } = await import('./tf-choice-card.js');

// ---- helpers ---------------------------------------------------------------

function makeCard(attrs = {}, features = null) {
  const card = new TfChoiceCard();
  for (const [k, v] of Object.entries(attrs)) card.setAttribute(k, v);
  if (features) card._pendingFeatures = features;
  return card;
}

function mountCard(attrs = {}, features = null) {
  const card = makeCard(attrs);
  document.body.appendChild(card);
  if (features) card.features = features;
  return card;
}

function mountGroup(cards, groupAttrs = {}) {
  const group = new TfChoiceGroup();
  for (const [k, v] of Object.entries(groupAttrs)) group.setAttribute(k, v);
  for (const c of cards) group.appendChild(makeCard(c.attrs, null));
  document.body.appendChild(group);
  cards.forEach((c, i) => { if (c.features) group.children[i].features = c.features; });
  return group;
}

function inner(card) { return card.querySelector('.tf-choice-card'); }

const NATIVE = {
  value: 'native', icon: 'zap', heading: 'Native', pill: 'default', 'pill-tone': 'warn',
  description: 'Trusted local execution as the service user.',
};
const CONTAINER = {
  value: 'container', icon: 'shield', heading: 'Container',
  description: 'Full isolation from the host.',
};

// ---------------------------------------------------------------------------
// Card content — the mockup's mode-card, part by part
// ---------------------------------------------------------------------------

test('card: header carries icon, heading and the default pill', () => {
  const card = mountCard(NATIVE);
  const head = card.querySelector('.tf-choice-card__head');
  assert.equal(head.firstElementChild.tagName.toLowerCase(), 'svg');
  assert.match(head.innerHTML, /#i-zap/);
  assert.equal(head.querySelector('.tf-choice-card__heading').textContent, 'Native');
  const pill = head.querySelector('.tf-choice-card__pill');
  assert.equal(pill.textContent, 'default');
  assert.ok(pill.classList.contains('tf-choice-card__pill--warn'));
});

test('card: the description renders under the header', () => {
  const card = mountCard(NATIVE);
  assert.equal(card.querySelector('.tf-choice-card__desc').textContent,
    'Trusted local execution as the service user.');
});

test('card: the consequence list keeps a tone and a bold lead per line', () => {
  const card = mountCard(NATIVE, [
    { icon: 'check', tone: 'ok', text: 'Starts immediately, no container image' },
    { icon: 'alert', tone: 'warn', lead: 'Code reaches the host', text: ' — no isolation' },
  ]);
  const items = [...card.querySelectorAll('.tf-choice-card__feature')];
  assert.equal(items.length, 2);
  assert.ok(items[0].classList.contains('tf-choice-card__feature--ok'));
  assert.equal(items[0].querySelector('.tf-choice-card__feature-text').textContent,
    'Starts immediately, no container image');
  assert.match(items[0].innerHTML, /#i-check/);

  assert.ok(items[1].classList.contains('tf-choice-card__feature--warn'));
  assert.equal(items[1].querySelector('b').textContent, 'Code reaches the host');
  assert.equal(items[1].querySelector('.tf-choice-card__feature-text').textContent,
    'Code reaches the host — no isolation');
});

test('card: empty and malformed features are dropped, not rendered blank', () => {
  const card = mountCard(NATIVE, [{}, null, 'x', { text: '' }, { text: 'kept' }]);
  const items = [...card.querySelectorAll('.tf-choice-card__feature')];
  assert.equal(items.length, 1);
  assert.equal(items[0].textContent, 'kept');
});

test('card: an unsafe icon name is rejected (no markup injection)', () => {
  const card = mountCard({ ...NATIVE, icon: '"><script>x</script>' },
    [{ icon: '"><img>', text: 'y' }]);
  assert.equal(card.querySelectorAll('svg').length, 0);
  assert.equal(card.querySelector('script'), null);
});

test('card: text is written as text, never as markup', () => {
  const card = mountCard({ value: 'v', heading: '<b>bold</b>', description: '<i>i</i>' },
    [{ lead: '<u>u</u>', text: '<s>s</s>' }]);
  assert.equal(card.querySelector('.tf-choice-card__heading').textContent, '<b>bold</b>');
  assert.equal(card.querySelector('.tf-choice-card__heading').children.length, 0);
  assert.equal(card.querySelector('.tf-choice-card__desc').textContent, '<i>i</i>');
  assert.equal(card.querySelector('b').textContent, '<u>u</u>');
});

// ---------------------------------------------------------------------------
// Selected / disabled
// ---------------------------------------------------------------------------

test('card: selected drives the class and the pressed state', () => {
  const card = mountCard(NATIVE);
  assert.equal(inner(card).classList.contains('is-selected'), false);
  assert.equal(inner(card).getAttribute('aria-pressed'), 'false');
  card.selected = true;
  assert.ok(inner(card).classList.contains('is-selected'));
  assert.equal(inner(card).getAttribute('aria-pressed'), 'true');
});

test('card: a standalone card emits choice-select on click and on Enter/Space', () => {
  const card = mountCard(NATIVE);
  const seen = [];
  card.addEventListener('choice-select', (e) => seen.push(e.detail.value));
  inner(card).click();
  for (const key of ['Enter', ' ']) {
    inner(card).dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }));
  }
  assert.deepEqual(seen, ['native', 'native', 'native']);
});

test('card: a disabled card is inert to mouse AND keyboard', () => {
  const card = mountCard({ ...CONTAINER, disabled: '' });
  const seen = [];
  card.addEventListener('choice-select', (e) => seen.push(e.detail.value));
  inner(card).click();
  inner(card).dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
  assert.deepEqual(seen, [], 'an unavailable architecture must not be selectable');
  assert.ok(inner(card).classList.contains('is-disabled'));
  assert.equal(inner(card).getAttribute('aria-disabled'), 'true');
  assert.equal(inner(card).getAttribute('tabindex'), '-1', 'and it is out of the tab order');
});

test('card: an unavailable option says WHAT TO INSTALL, and says it loudly', () => {
  const card = mountCard({ ...CONTAINER, disabled: '',
    note: 'Install Docker or Podman on this node.' });
  const note = card.querySelector('.tf-choice-card__note');
  assert.ok(note, 'the note is rendered');
  assert.equal(note.textContent, 'Install Docker or Podman on this node.');
  assert.ok(note.classList.contains('tf-choice-card__note--blocking'),
    'a blocked choice marks its reason, it does not just grey out');
  assert.equal(inner(card).getAttribute('aria-describedby'), note.id,
    'screen readers get the reason with the card');
});

test('card: an enabled card can still carry a plain note', () => {
  const card = mountCard({ ...NATIVE, note: 'Immutable once the session is created.' });
  const note = card.querySelector('.tf-choice-card__note');
  assert.equal(note.classList.contains('tf-choice-card__note--blocking'), false);
});

test('card: dropping the note removes it and the description link', () => {
  const card = mountCard({ ...NATIVE, note: 'x' });
  card.removeAttribute('note');
  assert.equal(card.querySelector('.tf-choice-card__note'), null);
  assert.equal(inner(card).hasAttribute('aria-describedby'), false);
});

// ---------------------------------------------------------------------------
// Group — exclusivity, keyboard, ARIA
// ---------------------------------------------------------------------------

test('group: is a radiogroup whose cards are radios', () => {
  const group = mountGroup([{ attrs: NATIVE }, { attrs: CONTAINER }], { value: 'native' });
  assert.equal(group.getAttribute('role'), 'radiogroup');
  for (const card of group.cards) assert.equal(inner(card).getAttribute('role'), 'radio');
  assert.equal(inner(group.cards[0]).getAttribute('aria-checked'), 'true');
  assert.equal(inner(group.cards[1]).getAttribute('aria-checked'), 'false');
});

test('group: exactly one card is selected and it follows value', () => {
  const group = mountGroup([{ attrs: NATIVE }, { attrs: CONTAINER }], { value: 'native' });
  assert.deepEqual(group.cards.map((c) => c.selected), [true, false]);
  group.value = 'container';
  assert.deepEqual(group.cards.map((c) => c.selected), [false, true]);
});

test('card: a property written BEFORE the upgrade reaches the attribute', () => {
  // A group parsed from one innerHTML string upgrades before its cards, so its
  // first _sync() writes `card.selected` onto a plain element. That own property
  // shadows the accessor: the attribute is never written, `_render` keeps
  // painting the stale state and the selection can never move (observed in the
  // Code Studio wizard, where clicking "container" left "native" highlighted).
  const card = new TfChoiceCard();
  card.setAttribute('value', 'container');
  Object.defineProperty(card, 'selected', {
    value: true, writable: true, configurable: true, enumerable: true,
  });
  document.body.appendChild(card);

  assert.equal(Object.prototype.hasOwnProperty.call(card, 'selected'), false,
    'the own property is handed over to the accessor');
  assert.ok(card.hasAttribute('selected'), 'the value landed on the attribute');
  assert.equal(inner(card).className, 'tf-choice-card is-selected');

  card.selected = false;
  assert.equal(inner(card).className, 'tf-choice-card', 'the card repaints on deselect');
});

test('group: clicking a card selects it and emits change once', () => {
  const group = mountGroup([{ attrs: NATIVE }, { attrs: CONTAINER }], { value: 'native' });
  const seen = [];
  group.addEventListener('change', (e) => seen.push(e.detail.value));
  inner(group.cards[1]).click();
  assert.deepEqual(seen, ['container']);
  assert.equal(group.value, 'container');
  inner(group.cards[1]).click();
  assert.deepEqual(seen, ['container'], 're-selecting the current value is not a change');
});

test('group: a disabled card cannot be selected by click', () => {
  const group = mountGroup([{ attrs: NATIVE }, { attrs: { ...CONTAINER, disabled: '' } }],
    { value: 'native' });
  const seen = [];
  group.addEventListener('change', (e) => seen.push(e.detail.value));
  inner(group.cards[1]).click();
  assert.deepEqual(seen, []);
  assert.equal(group.value, 'native');
});

test('group: roving tabindex keeps one card in the tab order', () => {
  const group = mountGroup([{ attrs: NATIVE }, { attrs: CONTAINER }], { value: 'container' });
  assert.equal(inner(group.cards[0]).getAttribute('tabindex'), '-1');
  assert.equal(inner(group.cards[1]).getAttribute('tabindex'), '0');
});

test('group: with no value the first enabled card becomes the tab stop', () => {
  const group = mountGroup([{ attrs: { ...NATIVE, disabled: '' } }, { attrs: CONTAINER }]);
  assert.equal(inner(group.cards[0]).getAttribute('tabindex'), '-1');
  assert.equal(inner(group.cards[1]).getAttribute('tabindex'), '0');
});

test('group: arrow keys move and select, wrapping past the ends', () => {
  const group = mountGroup([{ attrs: NATIVE }, { attrs: CONTAINER }], { value: 'native' });
  const seen = [];
  group.addEventListener('change', (e) => seen.push(e.detail.value));

  const press = (idx, key) => {
    const ev = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true });
    inner(group.cards[idx]).dispatchEvent(ev);
    return ev;
  };

  const ev = press(0, 'ArrowRight');
  assert.equal(ev.defaultPrevented, true);
  assert.deepEqual(seen, ['container']);
  press(1, 'ArrowRight');
  assert.deepEqual(seen, ['container', 'native'], 'wraps to the first card');
  press(0, 'ArrowUp');
  assert.deepEqual(seen, ['container', 'native', 'container'], 'ArrowUp is the backwards step');
});

test('group: Home and End jump to the ends', () => {
  const group = mountGroup([{ attrs: NATIVE }, { attrs: CONTAINER }], { value: 'native' });
  const seen = [];
  group.addEventListener('change', (e) => seen.push(e.detail.value));
  const press = (idx, key) => inner(group.cards[idx])
    .dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }));
  press(0, 'End');
  assert.deepEqual(seen, ['container']);
  press(1, 'Home');
  assert.deepEqual(seen, ['container', 'native']);
});

test('group: arrow navigation skips a disabled card entirely', () => {
  const group = mountGroup([
    { attrs: NATIVE },
    { attrs: { value: 'vm', heading: 'VM', disabled: '' } },
    { attrs: CONTAINER },
  ], { value: 'native', columns: '3' });
  const seen = [];
  group.addEventListener('change', (e) => seen.push(e.detail.value));
  inner(group.cards[0]).dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true }));
  assert.deepEqual(seen, ['container'], 'the unavailable option is not a stop');
});

test('group: unrelated keys are left alone', () => {
  const group = mountGroup([{ attrs: NATIVE }, { attrs: CONTAINER }], { value: 'native' });
  const ev = new KeyboardEvent('keydown', { key: 'x', bubbles: true, cancelable: true });
  inner(group.cards[0]).dispatchEvent(ev);
  assert.equal(ev.defaultPrevented, false);
});

test('group: a card appended later joins the group', () => {
  const group = mountGroup([{ attrs: NATIVE }], { value: 'container' });
  const late = makeCard(CONTAINER);
  group.appendChild(late);
  group.value = 'container';
  assert.equal(late.selected, true);
  assert.equal(inner(late).getAttribute('role'), 'radio');
});

test('group: a lone card does not trap arrow keys', () => {
  const group = mountGroup([{ attrs: NATIVE }], { value: 'native' });
  const ev = new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true });
  inner(group.cards[0]).dispatchEvent(ev);
  assert.equal(ev.defaultPrevented, false);
});

// ---------------------------------------------------------------------------
// CSS contract
// ---------------------------------------------------------------------------

test('css: the blocked card keeps its note at full contrast', () => {
  const css = readFileSync(join(WWW_ROOT, 'css', 'controls.css'), 'utf8');
  const dim = css.slice(css.indexOf('.tf-choice-card.is-disabled .tf-choice-card__head'));
  const block = dim.slice(0, dim.indexOf('}') + 1);
  assert.match(block, /opacity:\s*0\.45/);
  assert.equal(/tf-choice-card__note/.test(block), false,
    'the note must NOT be in the dimmed set — it is the reason the card is blocked');
});

test('css: every state the wizard needs has a rule', () => {
  const css = readFileSync(join(WWW_ROOT, 'css', 'controls.css'), 'utf8');
  for (const needle of [
    'tf-choice-group {',
    '.tf-choice-card {',
    '.tf-choice-card.is-selected',
    '.tf-choice-card.is-disabled',
    '.tf-choice-card:focus-visible',
    '.tf-choice-card__pill--warn',
    '.tf-choice-card__feature--warn',
    '.tf-choice-card__note--blocking',
  ]) {
    assert.ok(css.includes(needle), `missing rule ${needle}`);
  }
  const media = css.slice(css.lastIndexOf('@media (max-width: 900px)'));
  assert.match(media, /tf-choice-group/, 'the grid collapses to one column on a phone');
});
