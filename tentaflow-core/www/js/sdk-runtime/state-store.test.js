// =============================================================================
// Plik: sdk-runtime/state-store.test.js
// Opis: Testy jednostkowe dla `StateStore` (Krok 3.1). Uruchamiane przez
// `sdk-runtime-test.html` w przeglądarce ALBO przez Node 22+ (
// `node tentaflow-core/www/js/sdk-runtime/state-store.test.js`).
// Zero zewnętrznych zależności.
// =============================================================================

import {
  StateStore,
  PATCH_REJECT_REASON,
  pathKey,
  isPrefixOf,
} from './state-store.js';

// ---- minimalny harness assertion ----

const results = [];
function test(name, fn) {
  try {
    fn();
    results.push({ name, ok: true });
  } catch (err) {
    results.push({ name, ok: false, err });
  }
}
function assertEq(actual, expected, msg) {
  const a = JSON.stringify(actual, bigintReplacer);
  const b = JSON.stringify(expected, bigintReplacer);
  if (a !== b) {
    throw new Error(`${msg || 'assertEq'}: expected ${b}, got ${a}`);
  }
}
function bigintReplacer(_k, v) {
  return typeof v === 'bigint' ? `${v}n` : v;
}
function assert(cond, msg) {
  if (!cond) throw new Error(msg || 'assert failed');
}
function assertThrows(fn, msg) {
  let threw = false;
  try {
    fn();
  } catch {
    threw = true;
  }
  if (!threw) throw new Error(msg || 'expected throw');
}

// ---- helpery ----

// Wire shape: StatePath jest CBOR-arrayem PathSegment-ów, więc helper
// zwraca array bezpośrednio (matchuje to co dispatcher poda store'owi po
// dekodzie CBOR-a w `tentaflow-sdk-spec/src/protocol/ui/bind.rs`).
const PATH = (...segs) =>
  segs.map((s) =>
    typeof s === 'number'
      ? { kind: 'index', value: s }
      : { kind: 'key', value: s }
  );

function newStore(epoch = 1n) {
  return new StateStore({ addon_id: 'test', panel_id: 'p', panel_epoch: epoch });
}

// ---- pathKey + isPrefixOf ----

test('pathKey: deterministic for equal paths', () => {
  const a = PATH('cameras', 5, 'status');
  const b = PATH('cameras', 5, 'status');
  assertEq(pathKey(a), pathKey(b));
});

test('pathKey: distinguishes key from index with same string', () => {
  const a = PATH('5');
  const b = PATH(5);
  assert(pathKey(a) !== pathKey(b));
});

test('isPrefixOf: equal paths', () => {
  assert(isPrefixOf(PATH('a'), PATH('a')));
});

test('isPrefixOf: strict prefix', () => {
  assert(isPrefixOf(PATH('a'), PATH('a', 'b')));
  assert(!isPrefixOf(PATH('a', 'b'), PATH('a')));
});

test('isPrefixOf: divergent rejected', () => {
  assert(!isPrefixOf(PATH('a', 'x'), PATH('a', 'y')));
});

// ---- constructor validation ----

test('ctor rejects empty addonId', () => {
  assertThrows(() => new StateStore({ addon_id: '', panel_id: 'p', panel_epoch: 1 }));
});

test('ctor accepts bigint and integer epoch', () => {
  const s1 = new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 7 });
  const s2 = new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 7n });
  assertEq(s1.panel_epoch, 7n);
  assertEq(s2.panel_epoch, 7n);
});

// ---- applySnapshot ----

test('snapshot single chunk replaces root', () => {
  const s = newStore();
  s.applySnapshot({
    entries: [
      { path: PATH('user', 'name'), value: 'Ada' },
      { path: PATH('counters', 'visits'), value: 7 },
    ],
    state_revision: 5,
    truncated: false,
  });
  assertEq(s.read(PATH('user', 'name')), 'Ada');
  assertEq(s.read(PATH('counters', 'visits')), 7);
  assertEq(s.revision(), 5n);
});

test('snapshot chunked: commits only on final truncated=false', () => {
  const s = newStore();
  s.applySnapshot({
    entries: [{ path: PATH('a'), value: 1 }],
    state_revision: 9,
    truncated: true,
  });
  // Mid-chunk: state still pristine
  assertEq(s.read(PATH('a')), undefined);
  assertEq(s.revision(), 0n);
  s.applySnapshot({
    entries: [{ path: PATH('b'), value: 2 }],
    state_revision: 9,
    truncated: false,
  });
  assertEq(s.read(PATH('a')), 1);
  assertEq(s.read(PATH('b')), 2);
  assertEq(s.revision(), 9n);
});

test('snapshot mid-stream revision change drops buffer AND current chunk', () => {
  const s = newStore();
  s.applySnapshot({ entries: [{ path: PATH('x'), value: 1 }], state_revision: 3, truncated: true });
  // Sender bumped revision mid-stream → §6.4 nakazuje porzucić zarówno
  // bufor jak i bieżący chunk i czekać na świeży snapshot dla nowej
  // rewizji. State zostaje nienaruszone.
  const ok = s.applySnapshot({
    entries: [{ path: PATH('y'), value: 2 }],
    state_revision: 4,
    truncated: false,
  });
  assertEq(ok, false);
  assertEq(s.read(PATH('x')), undefined);
  assertEq(s.read(PATH('y')), undefined);
  assertEq(s.revision(), 0n);
});

// ---- subscribers ----

test('subscribe: exact path notified on Set', () => {
  const s = newStore();
  s.applySnapshot({ entries: [{ path: PATH('a'), value: 1 }], state_revision: 1, truncated: false });
  let hits = 0;
  s.subscribe(PATH('a'), () => hits++);
  s.applyPatch({
    base_revision: 1,
    new_revision: 2,
    ops: [{ path: PATH('a'), op: { kind: 'set', value: 2 } }],
  });
  assertEq(hits, 1);
});

test('subscribe: prefix subscriber notified on descendant change', () => {
  const s = newStore();
  s.applySnapshot({
    entries: [{ path: PATH('user', 'name'), value: 'Ada' }],
    state_revision: 1,
    truncated: false,
  });
  let hits = 0;
  s.subscribe(PATH('user'), () => hits++);
  s.applyPatch({
    base_revision: 1,
    new_revision: 2,
    ops: [{ path: PATH('user', 'name'), op: { kind: 'set', value: 'Boris' } }],
  });
  assertEq(hits, 1);
});

test('subscribe: descendant subscriber notified on ancestor replace', () => {
  const s = newStore();
  s.applySnapshot({
    entries: [{ path: PATH('user', 'name'), value: 'Ada' }],
    state_revision: 1,
    truncated: false,
  });
  let hits = 0;
  s.subscribe(PATH('user', 'name'), () => hits++);
  s.applyPatch({
    base_revision: 1,
    new_revision: 2,
    ops: [{ path: PATH('user'), op: { kind: 'set', value: { name: 'Boris' } } }],
  });
  assertEq(hits, 1);
});

test('subscribe: unrelated subscriber NOT notified', () => {
  const s = newStore();
  s.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  let hits = 0;
  s.subscribe(PATH('other'), () => hits++);
  s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('a'), op: { kind: 'set', value: 1 } }],
  });
  assertEq(hits, 0);
});

test('subscribe: unsubscribe stops notifications', () => {
  const s = newStore();
  s.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  let hits = 0;
  const off = s.subscribe(PATH('a'), () => hits++);
  s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('a'), op: { kind: 'set', value: 1 } }],
  });
  off();
  s.applyPatch({
    base_revision: 1,
    new_revision: 2,
    ops: [{ path: PATH('a'), op: { kind: 'set', value: 2 } }],
  });
  assertEq(hits, 1);
});

// ---- revision gate / PatchRejected ----

test('patch with stale baseRevision rejected, state unchanged', () => {
  const s = newStore();
  s.applySnapshot({ entries: [{ path: PATH('a'), value: 1 }], state_revision: 5, truncated: false });
  const rejections = [];
  s.onPatchRejected((evt) => rejections.push(evt));
  const ok = s.applyPatch({
    base_revision: 4,
    new_revision: 5,
    ops: [{ path: PATH('a'), op: { kind: 'set', value: 99 } }],
  });
  assertEq(ok, false);
  assertEq(s.read(PATH('a')), 1);
  assertEq(s.revision(), 5n);
  assertEq(rejections.length, 1);
  assertEq(rejections[0].reason, PATCH_REJECT_REASON.RevisionMismatch);
});

test('patch with stale panel_epoch is dropped silently (no PatchRejected emitted)', () => {
  // Spec §6.4 nie definiuje EpochMismatch jako reason — filtrowanie po
  // epoch jest odpowiedzialnością dispatcher'a transport-level. Store
  // dropuje stale message bez emisji wire'owego eventu.
  const s = newStore(1n);
  s.applySnapshot({ entries: [{ path: PATH('a'), value: 1 }], state_revision: 1, truncated: false });
  const rejections = [];
  s.onPatchRejected((evt) => rejections.push(evt));
  const ok = s.applyPatch({
    base_revision: 1,
    new_revision: 2,
    ops: [{ path: PATH('a'), op: { kind: 'set', value: 2 } }],
    panel_epoch: 99,
  });
  assertEq(ok, false);
  assertEq(s.read(PATH('a')), 1);
  assertEq(rejections.length, 0);
});

// ---- PatchOp variants ----

test('PatchOp.set creates intermediates with right container kind (sequential push)', () => {
  // Write sequencyjny: rows[0] tworzy array+intermediate map, rows[1]
  // pushuje kolejny. Bezpośrednie set na rows[2] przy pustej tablicy
  // odrzucamy (ArrayBounds) — sender musi użyć append_array lub
  // pisać sekwencyjnie po indexach ≤ length.
  const s = newStore();
  s.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  const ok = s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [
      { path: PATH('rows', 0, 'name'), op: { kind: 'set', value: 'r0' } },
      { path: PATH('rows', 1, 'name'), op: { kind: 'set', value: 'r1' } },
    ],
  });
  assertEq(ok, true);
  assertEq(s.read(PATH('rows', 0, 'name')), 'r0');
  assertEq(s.read(PATH('rows', 1, 'name')), 'r1');
});

test('PatchOp.set with array index beyond length rejects ArrayBounds', () => {
  const s = newStore();
  s.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  const rejections = [];
  s.onPatchRejected((e) => rejections.push(e));
  const ok = s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('rows', 5, 'name'), op: { kind: 'set', value: 'r5' } }],
  });
  assertEq(ok, false);
  assertEq(rejections[0].reason, PATCH_REJECT_REASON.ArrayBounds);
});

test('PatchOp.delete removes map key', () => {
  const s = newStore();
  s.applySnapshot({
    entries: [{ path: PATH('a', 'b'), value: 1 }],
    state_revision: 0,
    truncated: false,
  });
  s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('a', 'b'), op: { kind: 'delete' } }],
  });
  assertEq(s.read(PATH('a', 'b')), undefined);
});

test('PatchOp.append_array / prepend_array / insert / remove', () => {
  const s = newStore();
  s.applySnapshot({
    entries: [{ path: PATH('xs'), value: [10, 20, 30] }],
    state_revision: 0,
    truncated: false,
  });
  s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [
      { path: PATH('xs'), op: { kind: 'append_array', value: 40 } },
      { path: PATH('xs'), op: { kind: 'prepend_array', value: 0 } },
      { path: PATH('xs'), op: { kind: 'insert_array', index: 2, value: 15 } },
      { path: PATH('xs'), op: { kind: 'remove_array', index: 0 } },
    ],
  });
  // start: [10,20,30]
  // append 40 → [10,20,30,40]
  // prepend 0 → [0,10,20,30,40]
  // insert@2 15 → [0,10,15,20,30,40]
  // remove@0 → [10,15,20,30,40]
  assertEq(s.read(PATH('xs')), [10, 15, 20, 30, 40]);
});

test('PatchOp.insert_array out-of-bounds rolls back atomically', () => {
  const s = newStore();
  s.applySnapshot({
    entries: [{ path: PATH('xs'), value: [10, 20] }],
    state_revision: 0,
    truncated: false,
  });
  const rejections = [];
  s.onPatchRejected((e) => rejections.push(e));
  const ok = s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [
      { path: PATH('xs'), op: { kind: 'append_array', value: 30 } },
      { path: PATH('xs'), op: { kind: 'insert_array', index: 99, value: 'bad' } },
    ],
  });
  assertEq(ok, false);
  // rollback — pierwsza operacja też nie powinna przejść
  assertEq(s.read(PATH('xs')), [10, 20]);
  assertEq(s.revision(), 0n);
  assertEq(rejections.length, 1);
  assertEq(rejections[0].reason, PATCH_REJECT_REASON.ArrayBounds);
});

test('PatchOp.merge_map shallow-merges keys', () => {
  const s = newStore();
  s.applySnapshot({
    entries: [{ path: PATH('cfg'), value: { a: 1, b: 2 } }],
    state_revision: 0,
    truncated: false,
  });
  s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('cfg'), op: { kind: 'merge_map', value: { b: 22, c: 3 } } }],
  });
  assertEq(s.read(PATH('cfg')), { a: 1, b: 22, c: 3 });
});

test('PatchOp.increment integer', () => {
  const s = newStore();
  s.applySnapshot({
    entries: [{ path: PATH('n'), value: 10 }],
    state_revision: 0,
    truncated: false,
  });
  s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('n'), op: { kind: 'increment', delta: 5 } }],
  });
  assertEq(s.read(PATH('n')), 15);
});

test('PatchOp.increment with unsafe Number delta rejects with TypeMismatch', () => {
  const s = newStore();
  s.applySnapshot({
    entries: [{ path: PATH('n'), value: 0 }],
    state_revision: 0,
    truncated: false,
  });
  const rejections = [];
  s.onPatchRejected((e) => rejections.push(e));
  const ok = s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    // delta = 2^53 + 1 is beyond Number.MAX_SAFE_INTEGER — sender powinien
    // użyć BigInt; Number-em rejecting.
    ops: [{ path: PATH('n'), op: { kind: 'increment', delta: 9007199254740993 } }],
  });
  assertEq(ok, false);
  assertEq(rejections[0].reason, PATCH_REJECT_REASON.TypeMismatch);
});

test('PatchOp.increment with unsafe Number current rejects with TypeMismatch', () => {
  const s = newStore();
  // Wstrzykujemy unsafe Number do state przez snapshot — bypassujemy
  // walidację, którą dispatcher robi przy CBOR-decode.
  s.applySnapshot({
    entries: [{ path: PATH('n'), value: 1e20 }],
    state_revision: 0,
    truncated: false,
  });
  const rejections = [];
  s.onPatchRejected((e) => rejections.push(e));
  const ok = s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('n'), op: { kind: 'increment', delta: 1 } }],
  });
  assertEq(ok, false);
  assertEq(rejections[0].reason, PATCH_REJECT_REASON.TypeMismatch);
});

test('PatchOp.increment promotes to BigInt on Number overflow', () => {
  const s = newStore();
  s.applySnapshot({
    entries: [{ path: PATH('n'), value: Number.MAX_SAFE_INTEGER }],
    state_revision: 0,
    truncated: false,
  });
  s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('n'), op: { kind: 'increment', delta: 10 } }],
  });
  // Number.MAX_SAFE_INTEGER + 10 wykracza poza safe range → store promotuje
  // do BigInt zamiast tracić precyzję.
  const got = s.read(PATH('n'));
  assertEq(typeof got, 'bigint');
  assertEq(got, BigInt(Number.MAX_SAFE_INTEGER) + 10n);
});

test('PatchOp.increment bigint', () => {
  const s = newStore();
  s.applySnapshot({
    entries: [{ path: PATH('n'), value: 100n }],
    state_revision: 0,
    truncated: false,
  });
  s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('n'), op: { kind: 'increment', delta: 7n } }],
  });
  assertEq(s.read(PATH('n')), 107n);
});

test('PatchOp.increment on non-integer rejects with TypeMismatch', () => {
  const s = newStore();
  s.applySnapshot({
    entries: [{ path: PATH('s'), value: 'abc' }],
    state_revision: 0,
    truncated: false,
  });
  const rejections = [];
  s.onPatchRejected((e) => rejections.push(e));
  const ok = s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('s'), op: { kind: 'increment', delta: 1 } }],
  });
  assertEq(ok, false);
  assertEq(rejections[0].reason, PATCH_REJECT_REASON.TypeMismatch);
  assertEq(s.read(PATH('s')), 'abc');
});

test('PatchOp unknown kind rejects with StructuralLimit', () => {
  const s = newStore();
  s.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  const rejections = [];
  s.onPatchRejected((e) => rejections.push(e));
  const ok = s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('a'), op: { kind: 'magic', value: 1 } }],
  });
  assertEq(ok, false);
  assertEq(rejections[0].reason, PATCH_REJECT_REASON.StructuralLimit);
});

test('PatchRejected payload carries rejected_msg_id and current_revision', () => {
  const s = newStore();
  s.applySnapshot({ entries: [{ path: PATH('a'), value: 1 }], state_revision: 5, truncated: false });
  const rejections = [];
  s.onPatchRejected((e) => rejections.push(e));
  s.applyPatch({
    base_revision: 4,
    new_revision: 5,
    msg_id: 12345,
    ops: [{ path: PATH('a'), op: { kind: 'set', value: 99 } }],
  });
  // Wire shape: snake_case zgodne z `PatchRejected` (0x0123).
  assertEq(rejections[0].rejected_msg_id, 12345n);
  assertEq(rejections[0].current_revision, 5n);
  assertEq(rejections[0].reason, PATCH_REJECT_REASON.RevisionMismatch);
  assertEq(rejections[0].addon_id, 'test');
  assertEq(rejections[0].panel_id, 'p');
  assertEq(rejections[0].panel_epoch, 1n);
});

test('applyPatch with reserved root key rejects with PathOutOfNamespace', () => {
  const s = newStore();
  s.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  const rejections = [];
  s.onPatchRejected((e) => rejections.push(e));
  const ok = s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('__system', 'evil'), op: { kind: 'set', value: 1 } }],
  });
  assertEq(ok, false);
  assertEq(rejections[0].reason, PATCH_REJECT_REASON.PathOutOfNamespace);
});

test('PatchRejected for non-revision reject carries current_revision: null', () => {
  const s = newStore();
  s.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  const rejections = [];
  s.onPatchRejected((e) => rejections.push(e));
  s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('a'), op: { kind: 'magic' } }],
  });
  assertEq(rejections[0].current_revision, null);
});

test('applyPatch with path > 32 segments rejects with DepthExceeded', () => {
  const s = newStore();
  s.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  const rejections = [];
  s.onPatchRejected((e) => rejections.push(e));
  const tooDeep = Array.from({ length: 33 }, (_, i) => ({ kind: 'index', value: i }));
  const ok = s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: tooDeep, op: { kind: 'set', value: 1 } }],
  });
  assertEq(ok, false);
  assertEq(rejections[0].reason, PATCH_REJECT_REASON.DepthExceeded);
});

test('applyReset accepts new_revision per §6.4', () => {
  const s = newStore();
  s.applySnapshot({ entries: [{ path: PATH('a'), value: 1 }], state_revision: 5, truncated: false });
  s.applyReset({ new_revision: 9 });
  assertEq(s.read(PATH('a')), undefined);
  assertEq(s.revision(), 9n);
  // Next patch must use base = 9 (the reset-anchored revision).
  const ok = s.applyPatch({
    base_revision: 9,
    new_revision: 10,
    ops: [{ path: PATH('b'), op: { kind: 'set', value: 'x' } }],
  });
  assertEq(ok, true);
  assertEq(s.read(PATH('b')), 'x');
});

test('PatchOp.merge_map with __proto__ key rejects with TypeMismatch', () => {
  const s = newStore();
  s.applySnapshot({
    entries: [{ path: PATH('cfg'), value: {} }],
    state_revision: 0,
    truncated: false,
  });
  const rejections = [];
  s.onPatchRejected((e) => rejections.push(e));
  // `{__proto__: ...}` w literalu ustawia prototype zamiast tworzyć
  // własność — żeby przetestować attack vector przez JSON-decode,
  // konstruujemy obiekt z faktyczną własnością `__proto__` przez
  // JSON.parse (parser traktuje to jako zwykłą string-key).
  const overlay = JSON.parse('{"__proto__": {"polluted": true}}');
  const ok = s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('cfg'), op: { kind: 'merge_map', value: overlay } }],
  });
  assertEq(ok, false);
  assertEq(rejections[0].reason, PATCH_REJECT_REASON.TypeMismatch);
  // Sanity check: Object.prototype nigdy nie zostało dotknięte.
  assert(!('polluted' in {}), 'Object.prototype was polluted by merge_map');
});

test('PatchOp.set with nested __proto__ inside value rejects with TypeMismatch', () => {
  const s = newStore();
  s.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  const rejections = [];
  s.onPatchRejected((e) => rejections.push(e));
  // Wartość skonstruowana przez JSON-parser ma `__proto__` jako własność,
  // nie metadata prototypu. Normalize MUSI to wykryć i odrzucić.
  const evil = JSON.parse('{"deep": {"__proto__": {"polluted": true}}}');
  const ok = s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('cfg'), op: { kind: 'set', value: evil } }],
  });
  assertEq(ok, false);
  assertEq(rejections[0].reason, PATCH_REJECT_REASON.TypeMismatch);
  assert(!('polluted' in {}), 'Object.prototype polluted');
});

test('PatchOp.set normalizes plain object to null-prototype map', () => {
  const s = newStore();
  s.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('cfg'), op: { kind: 'set', value: { a: 1, nested: { b: 2 } } } }],
  });
  const cfg = s.read(PATH('cfg'));
  assertEq(Object.getPrototypeOf(cfg), null);
  assertEq(Object.getPrototypeOf(cfg.nested), null);
});

test('PatchOp.set with __proto__ path segment rejects with TypeMismatch', () => {
  const s = newStore();
  s.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  const rejections = [];
  s.onPatchRejected((e) => rejections.push(e));
  const ok = s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [
      { path: PATH('cfg', '__proto__'), op: { kind: 'set', value: 'pwn' } },
    ],
  });
  assertEq(ok, false);
  assertEq(rejections[0].reason, PATCH_REJECT_REASON.TypeMismatch);
});

test('applyPatch rollback preserves null-prototype on rolled-back root', () => {
  // Sanity check: po nieudanym patchu mapy w root muszą nadal mieć
  // null-prototype, inaczej rollback przeciekałby Object.prototype.
  const s = newStore();
  s.applySnapshot({
    entries: [{ path: PATH('cfg'), value: { a: 1 } }],
    state_revision: 0,
    truncated: false,
  });
  s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [
      { path: PATH('cfg'), op: { kind: 'merge_map', value: { b: 2 } } },
      // Drugi op intencjonalnie zły — wymusza rollback.
      { path: PATH('cfg'), op: { kind: 'merge_map', value: 'not-a-map' } },
    ],
  });
  const cfg = s.read(PATH('cfg'));
  assertEq(Object.getPrototypeOf(cfg), null);
});

test('applyReset throws if new_revision missing', () => {
  const s = newStore();
  s.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  assertThrows(() => s.applyReset({}));
});

test('array grow beyond MAX_ARRAY_LEN rejects with StructuralLimit', () => {
  // Sztuczny test: budujemy tablicę 65535 elementów przez snapshot, a potem
  // próbujemy dopchnąć 2 elementy patchem → drugi przekracza limit.
  const s = newStore();
  const big = new Array(65535).fill(0);
  s.applySnapshot({
    entries: [{ path: PATH('xs'), value: big }],
    state_revision: 0,
    truncated: false,
  });
  const rejections = [];
  s.onPatchRejected((e) => rejections.push(e));
  const ok = s.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [
      { path: PATH('xs'), op: { kind: 'append_array', value: 1 } },
      { path: PATH('xs'), op: { kind: 'append_array', value: 2 } },
    ],
  });
  assertEq(ok, false);
  assertEq(rejections[0].reason, PATCH_REJECT_REASON.StructuralLimit);
});

// ---- applyReset ----

test('applyReset clears state and updates revision', () => {
  const s = newStore();
  s.applySnapshot({ entries: [{ path: PATH('a'), value: 1 }], state_revision: 5, truncated: false });
  let hits = 0;
  s.subscribe(PATH('a'), () => hits++);
  s.applyReset({ new_revision: 7 });
  assertEq(s.read(PATH('a')), undefined);
  assertEq(s.revision(), 7n);
  assertEq(hits, 1);
});

// ---- applyOverlay ----

test('applyOverlay writes entries atomically without revision bump', () => {
  const s = newStore();
  s.applySnapshot({ entries: [{ path: PATH('a'), value: 1 }], state_revision: 3, truncated: false });
  s.applyOverlay([
    { path: PATH('b'), value: 2 },
    { path: PATH('c'), value: 3 },
  ]);
  assertEq(s.read(PATH('a')), 1);
  assertEq(s.read(PATH('b')), 2);
  assertEq(s.read(PATH('c')), 3);
  assertEq(s.revision(), 3n);
});

// ---- StatePath limits ----

test('subscribe rejects path > 32 segments', () => {
  const s = newStore();
  const tooLong = Array.from({ length: 33 }, (_, i) => ({ kind: 'index', value: i }));
  assertThrows(() => s.subscribe(tooLong, () => {}));
});

// ---- destroy ----

test('destroy makes further calls throw', () => {
  const s = newStore();
  s.destroy();
  assertThrows(() => s.read(PATH('a')));
});

// ---- report ----

function reportResults(target) {
  let pass = 0;
  let fail = 0;
  const lines = [];
  for (const r of results) {
    if (r.ok) {
      pass++;
      lines.push(`✓ ${r.name}`);
    } else {
      fail++;
      lines.push(`✗ ${r.name}\n    ${r.err && r.err.stack ? r.err.stack : r.err}`);
    }
  }
  lines.push('');
  lines.push(`${pass}/${pass + fail} tests passed${fail ? ` — ${fail} FAILED` : ''}`);
  const text = lines.join('\n');
  if (target) {
    target.textContent = text;
    target.dataset.status = fail === 0 ? 'pass' : 'fail';
  }
  return { pass, fail, text };
}

// Node entry point: when imported via `node`, fire reportResults to stdout
// and exit non-zero on failure.
if (typeof window === 'undefined' && typeof process !== 'undefined') {
  const r = reportResults(null);
  // eslint-disable-next-line no-console
  console.log(r.text);
  if (r.fail > 0) process.exit(1);
}

export { reportResults };
