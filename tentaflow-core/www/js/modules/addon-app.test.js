// =============================================================================
// File: modules/addon-app.test.js
// Description: Tests for addon-app overlay-slot detection. handlePanelShell must
// NOT create a static container nor registerSlot for overlay slots
// (modal/drawer/sheet/popover or Hidden visibility) — their DOM container is
// produced dynamically by the overlay renderer inside the host slot and is
// auto-registered by SlotManager.observe(). Non-overlay slots keep the static
// container + registerSlot behavior.
//
// addon-app.js imports sibling modules by absolute `/js/...` specifiers (browser
// import-map paths). We register a tiny resolver hook that maps `/js/` to the
// www root so the module graph loads under Node.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { register } from 'node:module';
import { pathToFileURL } from 'node:url';
import { dirname, resolve as pathResolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = fileURLToPath(import.meta.url);
// www root is two levels up from js/modules/.
const WWW_ROOT = pathResolve(dirname(here), '..', '..');

// Inline resolver hook: rewrite `/js/...` absolute browser specifiers to file
// URLs under the www root. Registered as a data: module so we need no extra file.
const hookSource = `
  const WWW_ROOT_URL = ${JSON.stringify(pathToFileURL(WWW_ROOT + '/').href)};
  export async function resolve(specifier, context, nextResolve) {
    if (specifier.startsWith('/js/')) {
      return { url: new URL('.' + specifier, WWW_ROOT_URL).href, shortCircuit: true };
    }
    return nextResolve(specifier, context);
  }
`;
register('data:text/javascript,' + encodeURIComponent(hookSource), import.meta.url);

// addon-app.js transitively imports codec.js, whose module scope eagerly kicks
// off `codecReady = (async () => await initWasm())()` — a WASM fetch that
// rejects in the Node test env. Drain that pending rejection before running
// tests so it does not surface as an after-the-fact "async activity" failure.
// The predicate under test does not depend on the codec being ready.
globalThis.addEventListener?.('unhandledrejection', (e) => e.preventDefault?.());
process.on('unhandledRejection', () => {});

const { isOverlaySlot, handleSlotContent, stringifyWithBigInt, __setSessionForTest } = await import('./addon-app.js');
await import('../protocol/codec.js')
  .then((m) => m.codecReady)
  .catch(() => {});

test('isOverlaySlot: modal semantics is overlay', () => {
  assert.equal(isOverlaySlot({ id: 'add_camera', semantics: 'modal' }), true);
});

test('isOverlaySlot: drawer/sheet/popover semantics are overlay', () => {
  assert.equal(isOverlaySlot({ id: 'd', semantics: 'drawer' }), true);
  assert.equal(isOverlaySlot({ id: 's', semantics: 'sheet' }), true);
  assert.equal(isOverlaySlot({ id: 'p', semantics: 'popover' }), true);
});

test('isOverlaySlot: Hidden visibility (object form) is overlay', () => {
  assert.equal(
    isOverlaySlot({ id: 'x', semantics: 'custom', visibility: { kind: 'hidden' } }),
    true,
  );
});

test('isOverlaySlot: Hidden visibility (defensive string form) is overlay', () => {
  assert.equal(isOverlaySlot({ id: 'x', semantics: 'custom', visibility: 'hidden' }), true);
});

test('isOverlaySlot: main_content + always visibility is NOT overlay', () => {
  assert.equal(
    isOverlaySlot({ id: 'main_content', semantics: 'main_content', visibility: { kind: 'always' } }),
    false,
  );
});

test('isOverlaySlot: tab_pane / side_panel / toast are NOT overlay by semantics', () => {
  assert.equal(isOverlaySlot({ id: 't', semantics: 'tab_pane' }), false);
  assert.equal(isOverlaySlot({ id: 'sp', semantics: 'side_panel' }), false);
  assert.equal(isOverlaySlot({ id: 'to', semantics: 'toast' }), false);
});

test('isOverlaySlot: conditional visibility is NOT overlay (only hidden is)', () => {
  assert.equal(
    isOverlaySlot({ id: 'c', semantics: 'custom', visibility: { kind: 'conditional', path: {} } }),
    false,
  );
});

test('isOverlaySlot: missing/invalid decl is not overlay', () => {
  assert.equal(isOverlaySlot(null), false);
  assert.equal(isOverlaySlot(undefined), false);
  assert.equal(isOverlaySlot('main'), false);
  assert.equal(isOverlaySlot({ id: 'only-id' }), false);
});

test('handleSlotContent forwards decoded.stateOverlay to SlotManager', () => {
  const overlay = [{ path: { segments: [{ kind: 'key', value: 'visible' }] }, value: false }];
  let captured = null;
  __setSessionForTest({
    slotManager: {
      handleSlotContent(arg) {
        captured = arg;
      },
    },
  });
  try {
    handleSlotContent({ slotId: 'wizard', fragment: { foo: 1 }, stateOverlay: overlay });
  } finally {
    __setSessionForTest(null);
  }
  assert.deepEqual(captured, {
    slot_id: 'wizard',
    fragment: { foo: 1 },
    state_overlay: overlay,
  });
});

test('handleSlotContent: missing stateOverlay forwards undefined (not an error)', () => {
  let captured = null;
  __setSessionForTest({
    slotManager: {
      handleSlotContent(arg) {
        captured = arg;
      },
    },
  });
  try {
    handleSlotContent({ slotId: 's', fragment: { foo: 1 } });
  } finally {
    __setSessionForTest(null);
  }
  assert.equal(captured.slot_id, 's');
  assert.equal(captured.state_overlay, undefined);
});

test('stringifyWithBigInt: safe-range BigInt serializes as Number', () => {
  const json = stringifyWithBigInt({ __panel_epoch: 3n, note_id: 'n1' });
  assert.deepEqual(JSON.parse(json), { __panel_epoch: 3, note_id: 'n1' });
});

test('stringifyWithBigInt: out-of-range BigInt serializes as decimal string', () => {
  const big = 9007199254740993n; // MAX_SAFE_INTEGER + 2 — not representable as Number
  const json = stringifyWithBigInt({ v: big, neg: -9007199254740993n });
  assert.deepEqual(JSON.parse(json), { v: '9007199254740993', neg: '-9007199254740993' });
});

test('stringifyWithBigInt: nested params and plain values pass through', () => {
  const json = stringifyWithBigInt({ a: [1n, 'x', { b: 2n }], c: true, d: null });
  assert.deepEqual(JSON.parse(json), { a: [1, 'x', { b: 2 }], c: true, d: null });
});
