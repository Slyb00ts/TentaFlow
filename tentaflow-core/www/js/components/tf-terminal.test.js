// =============================================================================
// File: components/tf-terminal.test.js
// Description: Tests for <tf-terminal> — the component that renders the
// server-owned VT cell grid. Covers the revision contract (in-place row
// updates, stale revisions dropped, gaps forcing a resync), the SGR/colour
// mapping and the xterm-256color key encoding.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { TfTerminal, encodeKeyEvent, TERM_ATTR } = await import('./tf-terminal.js');

// ---- helpers ---------------------------------------------------------------

function cell(ch, extra = {}) {
  return { ch, fg: null, bg: null, attrs: 0, ...extra };
}

function textRow(text, extra = {}) {
  return [...text].map((ch) => cell(ch, extra));
}

function mount(attrs = {}) {
  const el = new TfTerminal();
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
  document.body.appendChild(el);
  return el;
}

function rowEls(el) {
  return [...el.querySelectorAll('.tf-terminal__row')];
}

function keyEvent(key, mods = {}) {
  return {
    key,
    ctrlKey: !!mods.ctrl,
    shiftKey: !!mods.shift,
    altKey: !!mods.alt,
    metaKey: !!mods.meta,
    isComposing: false,
  };
}

function decode(bytes) {
  return [...bytes];
}

// ---- revision contract -----------------------------------------------------

test('applySnapshot renders every row and records the revision', () => {
  const el = mount({ rows: '3', cols: '20' });
  el.applySnapshot({
    revision: 7,
    cursor: { row: 0, col: 0, visible: true },
    rows: [textRow('alpha'), textRow('beta'), textRow('gamma')],
  });
  assert.equal(el.revision, 7);
  assert.deepEqual(rowEls(el).map((r) => r.textContent), ['alpha', 'beta', 'gamma']);
});

test('applyChanges rewrites only the listed rows', () => {
  const el = mount();
  el.applySnapshot({
    revision: 1,
    cursor: { row: 0, col: 0, visible: true },
    rows: [textRow('one'), textRow('two'), textRow('three')],
  });
  const before = rowEls(el);
  const untouched0 = before[0];
  const untouched2 = before[2];
  const spans0 = [...untouched0.children];
  const spans2 = [...untouched2.children];

  const applied = el.applyChanges({ revision: 2, rows: [{ index: 1, cells: textRow('TWO!') }] });

  assert.equal(applied, true);
  assert.equal(el.revision, 2);
  const after = rowEls(el);
  assert.equal(after[0], untouched0, 'row 0 element must be the same node');
  assert.equal(after[2], untouched2, 'row 2 element must be the same node');
  assert.deepEqual([...after[0].children], spans0, 'row 0 children must not be rebuilt');
  assert.deepEqual([...after[2].children], spans2, 'row 2 children must not be rebuilt');
  assert.equal(after[1].textContent, 'TWO!');
});

test('applyChanges grows the grid when a change names a new row index', () => {
  const el = mount();
  el.applySnapshot({ revision: 1, cursor: { row: 0, col: 0, visible: true }, rows: [textRow('a')] });
  el.applyChanges({ revision: 2, rows: [{ index: 2, cells: textRow('c') }] });
  assert.deepEqual(rowEls(el).map((r) => r.textContent), ['a', '', 'c']);
});

test('a revision lower than the current one is ignored', () => {
  const el = mount();
  el.applySnapshot({ revision: 5, cursor: { row: 0, col: 0, visible: true }, rows: [textRow('live')] });

  assert.equal(el.applySnapshot({ revision: 4, cursor: {}, rows: [textRow('stale')] }), false);
  assert.equal(el.revision, 5);
  assert.equal(rowEls(el)[0].textContent, 'live');

  assert.equal(el.applyChanges({ revision: 3, rows: [{ index: 0, cells: textRow('older') }] }), false);
  assert.equal(el.revision, 5);
  assert.equal(rowEls(el)[0].textContent, 'live');
});

test('a revision equal to the current one is not re-applied', () => {
  const el = mount();
  el.applySnapshot({ revision: 5, cursor: {}, rows: [textRow('live')] });
  assert.equal(el.applyChanges({ revision: 5, rows: [{ index: 0, cells: textRow('dup') }] }), false);
  assert.equal(rowEls(el)[0].textContent, 'live');
});

test('a gap in the revision numbering emits resync and applies nothing', () => {
  const el = mount();
  el.applySnapshot({ revision: 1, cursor: {}, rows: [textRow('base')] });

  const seen = [];
  el.addEventListener('resync', (e) => seen.push(e.detail));
  const applied = el.applyChanges({ revision: 3, rows: [{ index: 0, cells: textRow('skipped') }] });

  assert.equal(applied, false);
  assert.deepEqual(seen, [{ have: 1, received: 3 }]);
  assert.equal(el.revision, 1);
  assert.equal(rowEls(el)[0].textContent, 'base');
});

test('applyChanges before any snapshot asks for a full snapshot', () => {
  const el = mount();
  const seen = [];
  el.addEventListener('resync', (e) => seen.push(e.detail));
  assert.equal(el.applyChanges({ revision: 1, rows: [] }), false);
  assert.deepEqual(seen, [{ have: null, received: 1 }]);
});

// ---- rendering -------------------------------------------------------------

test('SGR attributes map to run modifier classes', () => {
  const el = mount();
  const attrs = TERM_ATTR.BOLD | TERM_ATTR.UNDERLINE | TERM_ATTR.ITALIC;
  el.applySnapshot({ revision: 1, cursor: {}, rows: [textRow('hi', { attrs })] });
  const span = rowEls(el)[0].children[0];
  assert.ok(span.classList.contains('tf-terminal__run--bold'));
  assert.ok(span.classList.contains('tf-terminal__run--underline'));
  assert.ok(span.classList.contains('tf-terminal__run--italic'));
  assert.equal(span.textContent, 'hi');
});

test('the 16 ANSI colours use palette classes, not inline colour', () => {
  const el = mount();
  el.applySnapshot({ revision: 1, cursor: {}, rows: [textRow('x', { fg: 4, bg: 1 })] });
  const span = rowEls(el)[0].children[0];
  assert.ok(span.classList.contains('tf-terminal__fg-4'));
  assert.ok(span.classList.contains('tf-terminal__bg-1'));
  assert.equal(span.getAttribute('style'), null);
});

test('256-colour indices resolve to the xterm cube hex', () => {
  const el = mount();
  el.applySnapshot({ revision: 1, cursor: {}, rows: [textRow('x', { fg: 196 })] });
  const style = rowEls(el)[0].children[0].getAttribute('style') || '';
  assert.match(style.toLowerCase().replace(/\s/g, ''), /#ff0000|rgb\(255,0,0\)/);
});

test('truecolor is written through as a literal colour', () => {
  const el = mount();
  el.applySnapshot({ revision: 1, cursor: {}, rows: [textRow('x', { fg: '#ff8800', bg: [0, 17, 34] })] });
  const style = (rowEls(el)[0].children[0].getAttribute('style') || '').toLowerCase().replace(/\s/g, '');
  assert.match(style, /#ff8800|rgb\(255,136,0\)/);
  assert.match(style, /#001122|rgb\(0,17,34\)/);
});

test('reverse video swaps the defaulted screen colours', () => {
  const el = mount();
  el.applySnapshot({ revision: 1, cursor: {}, rows: [textRow('x', { attrs: TERM_ATTR.REVERSE })] });
  const span = rowEls(el)[0].children[0];
  assert.ok(span.classList.contains('tf-terminal__run--fg-onbg'));
  assert.ok(span.classList.contains('tf-terminal__run--bg-onfg'));
});

test('cells sharing a style collapse into one run', () => {
  const el = mount();
  el.applySnapshot({
    revision: 1,
    cursor: {},
    rows: [[...textRow('ab', { fg: 2 }), ...textRow('cd', { fg: 3 })]],
  });
  const spans = [...rowEls(el)[0].children];
  assert.equal(spans.length, 2);
  assert.deepEqual(spans.map((s) => s.textContent), ['ab', 'cd']);
});

test('trailing blank cells are dropped so a copy carries no padding', () => {
  const el = mount();
  el.applySnapshot({ revision: 1, cursor: {}, rows: [textRow('ok      ')] });
  assert.equal(rowEls(el)[0].textContent, 'ok');
});

test('a hidden cursor is not rendered', () => {
  const el = mount();
  el.applySnapshot({ revision: 1, cursor: { row: 0, col: 0, visible: false }, rows: [textRow('x')] });
  assert.equal(el.querySelector('.tf-terminal__cursor').hidden, true);
  el.applySnapshot({ revision: 2, cursor: { row: 0, col: 1, visible: true }, rows: [textRow('x')] });
  assert.equal(el.querySelector('.tf-terminal__cursor').hidden, false);
});

// ---- key encoding ----------------------------------------------------------

test('arrow keys encode as CSI, and as SS3 under DECCKM', () => {
  assert.deepEqual(decode(encodeKeyEvent(keyEvent('ArrowUp'))), [0x1b, 0x5b, 0x41]);
  assert.deepEqual(decode(encodeKeyEvent(keyEvent('ArrowDown'))), [0x1b, 0x5b, 0x42]);
  assert.deepEqual(decode(encodeKeyEvent(keyEvent('ArrowRight'))), [0x1b, 0x5b, 0x43]);
  assert.deepEqual(decode(encodeKeyEvent(keyEvent('ArrowLeft'))), [0x1b, 0x5b, 0x44]);
  assert.deepEqual(
    decode(encodeKeyEvent(keyEvent('ArrowUp'), { applicationCursor: true })),
    [0x1b, 0x4f, 0x41],
  );
});

test('modified arrows carry the xterm modifier parameter', () => {
  // Ctrl -> 1 + 4 = 5
  assert.equal(String.fromCharCode(...encodeKeyEvent(keyEvent('ArrowRight', { ctrl: true }))), '\x1b[1;5C');
  // Shift -> 1 + 1 = 2
  assert.equal(String.fromCharCode(...encodeKeyEvent(keyEvent('ArrowLeft', { shift: true }))), '\x1b[1;2D');
});

test('navigation and editing keys encode as xterm-256color expects', () => {
  const s = (k, m) => String.fromCharCode(...encodeKeyEvent(keyEvent(k, m)));
  assert.equal(s('Home'), '\x1b[H');
  assert.equal(s('End'), '\x1b[F');
  assert.equal(s('PageUp'), '\x1b[5~');
  assert.equal(s('PageDown'), '\x1b[6~');
  assert.equal(s('Delete'), '\x1b[3~');
  assert.equal(s('Insert'), '\x1b[2~');
  assert.equal(s('Enter'), '\r');
  assert.equal(s('Tab'), '\t');
  assert.equal(s('Tab', { shift: true }), '\x1b[Z');
  assert.equal(s('Backspace'), '\x7f');
  assert.equal(s('Escape'), '\x1b');
});

test('function keys F1-F12 encode as SS3 and CSI-tilde', () => {
  const s = (k) => String.fromCharCode(...encodeKeyEvent(keyEvent(k)));
  assert.equal(s('F1'), '\x1bOP');
  assert.equal(s('F4'), '\x1bOS');
  assert.equal(s('F5'), '\x1b[15~');
  assert.equal(s('F10'), '\x1b[21~');
  assert.equal(s('F12'), '\x1b[24~');
});

test('control keys encode to their C0 bytes', () => {
  assert.deepEqual(decode(encodeKeyEvent(keyEvent('c', { ctrl: true }))), [0x03]);
  assert.deepEqual(decode(encodeKeyEvent(keyEvent('C', { ctrl: true }))), [0x03]);
  assert.deepEqual(decode(encodeKeyEvent(keyEvent('d', { ctrl: true }))), [0x04]);
  assert.deepEqual(decode(encodeKeyEvent(keyEvent('a', { ctrl: true }))), [0x01]);
  assert.deepEqual(decode(encodeKeyEvent(keyEvent(' ', { ctrl: true }))), [0x00]);
  assert.deepEqual(decode(encodeKeyEvent(keyEvent('[', { ctrl: true }))), [0x1b]);
});

test('printable characters and Alt-prefixed characters encode as UTF-8', () => {
  assert.deepEqual(decode(encodeKeyEvent(keyEvent('a'))), [0x61]);
  assert.deepEqual(decode(encodeKeyEvent(keyEvent('ł'))), [0xc5, 0x82]);
  assert.deepEqual(decode(encodeKeyEvent(keyEvent('b', { alt: true }))), [0x1b, 0x62]);
});

test('modifier-only and composing keys produce nothing', () => {
  assert.equal(encodeKeyEvent(keyEvent('Shift')), null);
  assert.equal(encodeKeyEvent(keyEvent('Control')), null);
  assert.equal(encodeKeyEvent({ ...keyEvent('a'), isComposing: true }), null);
});

test('a keydown on the component emits the encoded bytes', () => {
  const el = mount();
  const seen = [];
  el.addEventListener('key', (e) => seen.push([...e.detail.bytes]));
  const input = el.querySelector('.tf-terminal__input');
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'c', ctrlKey: true, bubbles: true, cancelable: true }));
  assert.deepEqual(seen, [[0x03]]);
});

test('a readonly terminal sends nothing', () => {
  const el = mount({ readonly: '' });
  const seen = [];
  el.addEventListener('key', (e) => seen.push(e.detail));
  const input = el.querySelector('.tf-terminal__input');
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'a', bubbles: true, cancelable: true }));
  assert.equal(seen.length, 0);
});
