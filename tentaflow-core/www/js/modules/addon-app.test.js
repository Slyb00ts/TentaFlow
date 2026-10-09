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

const { isOverlaySlot, handleSlotContent, sendAction, stringifyWithBigInt, __setSessionForTest } = await import('./addon-app.js');
const { ApiBinary } = await import('../protocol/api-binary-shim.js');
const { StateStore } = await import('../sdk-runtime/state-store.js');
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

// ---- sendAction: ActionAck drives the optimistic edit ----

const ON = [{ kind: 'key', value: 'on' }];

// A fake WS client that answers every sent frame through the pending map, the
// same path binary-ws-client uses for a response on the request's correlation.
function fakeClient(answer) {
  return {
    pending: new Map(),
    _corr: 0,
    nextCorrelationId() { this._corr += 1; return this._corr; },
    takeSequence() { return 1; },
    _send() {
      const key = String(this._corr);
      const p = this.pending.get(key);
      this.pending.delete(key);
      queueMicrotask(() => answer(p));
    },
  };
}

function withSession(store, ackStatus, answer, fn) {
  const realClient = ApiBinary.client;
  ApiBinary.client = async () => fakeClient(answer);
  __setSessionForTest({
    store,
    wasm: {
      encodeUiAction: () => new Uint8Array(),
      messageKind: () => ({ META_HEARTBEAT: 0 }),
      encodeEnvelopeDirect: () => new Uint8Array(),
      decodeUiPayload: () => ({ tag: 0x0131, status: ackStatus }),
    },
  });
  return fn().finally(() => {
    ApiBinary.client = realClient;
    __setSessionForTest(null);
    store.destroy();
  });
}

function newStoreOff() {
  const store = new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 1n });
  store.applySnapshot({ entries: [{ path: ON, value: false }], state_revision: 0, truncated: false });
  return store;
}

async function runAction(answer, ackStatus) {
  const store = newStoreOff();
  const token = store.setLocalEdit(ON, true);
  let shown;
  let entry;
  await withSession(store, ackStatus, answer, async () => {
    await sendAction('a', 'p', 1, 'set_on', { value: true }, [token]);
    shown = store.read(ON);
    entry = [...store._pending.values()][0];
  });
  return { shown, entry };
}

const UI_RESPONSE = { envelope: { isError: false }, body: { variant: 'UiChannelCbor', cbor: new Uint8Array() } };

test('sendAction: ok ack keeps the edit and marks it acked', async () => {
  const { shown, entry } = await runAction((p) => p.resolve(UI_RESPONSE), 'ok');
  assert.equal(shown, true);
  assert.equal(entry.acked, true);
});

test('sendAction: error ack restores the server value', async () => {
  assert.equal((await runAction((p) => p.resolve(UI_RESPONSE), 'error')).shown, false);
});

test('sendAction: redirected is not a failure', async () => {
  assert.equal((await runAction((p) => p.resolve(UI_RESPONSE), 'redirected')).shown, true);
});

test('sendAction: protocol error restores the server value', async () => {
  const r = await runAction((p) => p.reject(new Error('protocol error BadRequest: epoch mismatch')), 'ok');
  assert.equal(r.shown, false);
});

test('sendAction: a dropped connection leaves the edit pending', async () => {
  const r = await runAction((p) => p.reject(new Error('transport closed')), 'ok');
  assert.equal(r.shown, true);
  assert.equal(r.entry.acked, false);
});

test('sendAction: a late failure of an older action keeps the newer edit', async () => {
  const store = newStoreOff();
  const older = store.setLocalEdit(ON, true);
  store.setLocalEdit(ON, false);
  store.setLocalEdit(ON, true);
  let shown;
  await withSession(store, 'error', (p) => p.resolve(UI_RESPONSE), async () => {
    await sendAction('a', 'p', 1, 'set_on', { value: true }, [older]);
    shown = store.read(ON);
  });
  assert.equal(shown, true);
});
