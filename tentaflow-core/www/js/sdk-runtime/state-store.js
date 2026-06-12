// =============================================================================
// Plik: sdk-runtime/state-store.js
// Opis: Reaktywny store stanu addonu (Faza 6 Krok 3.1). Trzyma stan
// pojedynczego (addon_id, panel_id, panel_epoch), aplikuje StateSnapshot,
// StatePatch i StateReset z protocol §6.4, broadcastuje zmiany do
// subskrybentów po StatePath. Renderery komponentów subskrybują pod
// ścieżkami z BindRef::Bound / BindSpec::Text / List itd.
//
// =============================================================================
// Boundary contract — odpowiedzialność dispatcher'a (chunk 3.6)
// =============================================================================
// Wszystkie struktury spec'u używają `#[cbor(map)]` z integer keys
// (`#[n(0)]`, `#[n(1)]`, ...). Dispatcher MUSI zrobić schema-driven
// decode i przekazać do store'a JS object z named fields snake_case
// matchującymi Rust field names. Konkretnie:
//
//   - StateSnapshot { addon_id, panel_id, panel_epoch, state_revision,
//                     entries, truncated }
//   - StatePatch    { addon_id, panel_id, panel_epoch, base_revision,
//                     new_revision, ops }
//   - StateReset    { addon_id, panel_id, panel_epoch, new_revision }
//   - StateEntry    { path, value }
//   - PatchOp       { path, op: PatchOpKind }
//   - StatePath     Array<PathSegment>  — bare CBOR array, BEZ wrappera
//   - PathSegment   { kind: 'key'|'index', value: string|u32 }
//   - PatchOpKind   { kind: '<variant>', ...fields }   (tstr keys per spec)
//
// Numeryka:
//   - u64 / i64 (revisions, msg_id, increment.delta) MUSZĄ być BigInt
//     JS-side. Dispatcher dekoduje CBOR int o szerokości >32 bitów jako
//     BigInt; store akceptuje też safe-integer Number dla wygody testów.
//   - u32 (PathSegment.index, insert_array.index, remove_array.index)
//     pozostają jako JS Number — mieszczą się w safe range Number'a.
//
// Value:
//   - Spec `Value::Map(Vec<(Value, Value)>)` ma arbitralne klucze. Store
//     zakłada że state map'y mają tylko tstr keys, więc dispatcher MUSI
//     odrzucić Value::Map z non-tstr keys (jako TypeMismatch) zanim
//     trafi do store'a. `CborMap` (merge_map.value) ma tstr keys per
//     spec — to bezpieczne.
//
// Store nigdy nie widzi raw CBOR bajtów ani indexowanych map.
// =============================================================================

const PATCH_OP_KINDS = Object.freeze([
  'set',
  'delete',
  'append_array',
  'prepend_array',
  'insert_array',
  'remove_array',
  'merge_map',
  'increment',
]);

// Reason names mirror `PatchRejectReason` z tentaflow-sdk-spec
// (`protocol/ui/state.rs` §6.4). Trzymamy się dokładnie tych wartości:
// dispatcher serializuje to do `PatchRejected.reason` bez tłumaczenia.
const PATCH_REJECT_REASON = Object.freeze({
  RevisionMismatch: 'revision_mismatch',
  PathOwnershipViolation: 'path_ownership_violation',
  PathOutOfNamespace: 'path_out_of_namespace',
  TypeMismatch: 'type_mismatch',
  ArrayBounds: 'array_bounds',
  DepthExceeded: 'depth_exceeded',
  StructuralLimit: 'structural_limit',
});

const MAX_STATE_PATH_SEGMENTS = 32;

// Limity strukturalne do obrony przed DoS przez addon. Każde naruszenie
// emituje `StructuralLimit` w PatchRejected (lub w wypadku snapshotu —
// odrzucenie z console.warn).
const MAX_SNAPSHOT_ENTRIES = 65_536;
const MAX_ARRAY_LEN = 65_536;

// Zarezerwowane root keys — pierwszy segment path nie może być żadnym
// z tych (spec §6.4 ServerLimits / dispatcher boundary). Naruszenie:
// PathOutOfNamespace.
const RESERVED_ROOT_KEYS = Object.freeze(new Set(['__system', '__user']));

// Walidacja w runtime: pojedynczy PathSegment musi mieć kind ∈ {key,index}.
function assertSegment(seg, ctx) {
  if (!seg || typeof seg !== 'object') {
    throw new TypeError(`${ctx}: PathSegment must be object`);
  }
  if (seg.kind === 'key') {
    if (typeof seg.value !== 'string') {
      throw new TypeError(`${ctx}: PathSegment.key.value must be string`);
    }
    if (FORBIDDEN_MAP_KEYS.has(seg.value)) {
      throw rejectError(
        PATCH_REJECT_REASON.TypeMismatch,
        `${ctx}: PathSegment.key '${seg.value}' is forbidden`
      );
    }
  } else if (seg.kind === 'index') {
    if (typeof seg.value === 'bigint') {
      if (seg.value < 0n || seg.value > 0xFFFFFFFFn) {
        throw new TypeError(`${ctx}: PathSegment.index.value must be u32 integer`);
      }
      // CBOR decoder may deliver the index as BigInt — normalize in place
      // so traversal uses a plain Number array index.
      seg.value = Number(seg.value);
    } else if (!Number.isInteger(seg.value) || seg.value < 0 || seg.value > 0xFFFFFFFF) {
      throw new TypeError(`${ctx}: PathSegment.index.value must be u32 integer`);
    }
  } else {
    throw new TypeError(`${ctx}: PathSegment.kind must be 'key' or 'index', got ${seg.kind}`);
  }
}

// Klucze map, których NIGDY nie zapisujemy do JS object'u — `__proto__`
// w literalu obiektu nie modyfikuje prototypu null-prototype map'y, ale
// trzymanie ich jako legalnych state keys i tak otwierałoby surface'y
// w innych enginach. Odrzucamy je defensywnie z TypeMismatch — addon
// wciąż widzi spójny błąd, a dispatcher dostaje wire-stable reject.
const FORBIDDEN_MAP_KEYS = Object.freeze(
  new Set(['__proto__', 'prototype', 'constructor'])
);

// Deep clone głębokiej struktury (mapy + arrayE + skalary) zachowujący
// null-prototype na każdej mapie. `structuredClone` regeneruje
// `Object.prototype` na map'ach, co psuje nasz invariant — używamy
// własnego walker'a. BigInt, string, number, bool, null są kopiowane
// by-value; Value::Bytes (Uint8Array) jest kopiowany jako nowa kopia
// bufora, bo addony mogą oczekiwać że store nie współdzieli buforów.
function cloneStateValue(v) {
  if (v === null || v === undefined) return v;
  const t = typeof v;
  if (t === 'string' || t === 'number' || t === 'boolean' || t === 'bigint') {
    return v;
  }
  if (Array.isArray(v)) {
    const out = new Array(v.length);
    for (let i = 0; i < v.length; i++) out[i] = cloneStateValue(v[i]);
    return out;
  }
  if (v instanceof Uint8Array) {
    return new Uint8Array(v);
  }
  if (t === 'object') {
    const out = Object.create(null);
    for (const k of Object.keys(v)) out[k] = cloneStateValue(v[k]);
    return out;
  }
  // Defensive: nieoczekiwany typ — zwracamy by-reference, ale to nie
  // powinno się dziać po walidacji w writeAtPath.
  return v;
}

// Normalizuje wartość pochodzącą od addona przed zapisem do state:
// rekursywnie tworzy mapy jako Object.create(null), kopiuje tylko own
// enumerable keys, odrzuca `__proto__` / `prototype` / `constructor`
// jako string keys, kopiuje Uint8Array, oraz blokuje funkcje/symbole
// (nie mają sensu w Value). Używana zawsze gdy addon-supplied value
// trafia do storage'u: snapshot entries, overlay entries, set/append/
// prepend/insert_array, merge_map overlay values.
// Helper: czy `v` to map w naszym znaczeniu (plain obiekt, nie array,
// nie Uint8Array, nie null). Używany w writeAtPath/readAtPath żeby
// nie pomylić binary blob'a z map'ą.
function isStateMap(v) {
  return (
    v != null &&
    typeof v === 'object' &&
    !Array.isArray(v) &&
    !(v instanceof Uint8Array)
  );
}

function normalizeStateValue(v, ctx) {
  // Wire Value używa null jako brak — undefined nie istnieje na wire.
  if (v === undefined) {
    throw rejectError(
      PATCH_REJECT_REASON.TypeMismatch,
      `${ctx}: undefined is not a valid Value (use null)`
    );
  }
  if (v === null) return null;
  const t = typeof v;
  if (t === 'string' || t === 'number' || t === 'boolean' || t === 'bigint') {
    return v;
  }
  if (Array.isArray(v)) {
    if (v.length > MAX_ARRAY_LEN) {
      throw rejectError(
        PATCH_REJECT_REASON.StructuralLimit,
        `${ctx}: array length ${v.length} exceeds MAX_ARRAY_LEN`
      );
    }
    const out = new Array(v.length);
    for (let i = 0; i < v.length; i++) out[i] = normalizeStateValue(v[i], ctx);
    return out;
  }
  if (v instanceof Uint8Array) {
    return new Uint8Array(v);
  }
  if (t === 'object') {
    const out = Object.create(null);
    for (const k of Object.keys(v)) {
      if (FORBIDDEN_MAP_KEYS.has(k)) {
        throw rejectError(
          PATCH_REJECT_REASON.TypeMismatch,
          `${ctx}: forbidden map key '${k}'`
        );
      }
      out[k] = normalizeStateValue(v[k], ctx);
    }
    return out;
  }
  throw rejectError(
    PATCH_REJECT_REASON.TypeMismatch,
    `${ctx}: unsupported value type ${t}`
  );
}

// Wire shape per `tentaflow-sdk-spec/src/protocol/ui/bind.rs`: `StatePath`
// jest CBOR-arrayem `PathSegment`-ów, nie mapą. Trzymamy się tego: w API
// store'a path TO `Array<PathSegment>` bezpośrednio (dispatcher przekazuje
// to wprost z dekodowanego CBOR-a).
function assertPath(path, ctx) {
  if (!Array.isArray(path)) {
    throw new TypeError(`${ctx}: StatePath must be Array<PathSegment>`);
  }
  if (path.length > MAX_STATE_PATH_SEGMENTS) {
    // `rejectError` z reason'em DepthExceeded — `applyPatch` przechwyci
    // i wpisze do wire'owego PatchRejected. Caller spoza applyPatch
    // (np. subscribe()) zobaczy zwykły Error z `__patchRejectReason`,
    // co jest semantycznie informacyjne.
    throw rejectError(
      PATCH_REJECT_REASON.DepthExceeded,
      `${ctx}: StatePath exceeds MAX_STATE_PATH_SEGMENTS (${MAX_STATE_PATH_SEGMENTS})`
    );
  }
  for (const s of path) assertSegment(s, ctx);
  // Defensive subset §6.4: pierwszy segment nie może być zarezerwowanym
  // root key (`__system`, `__user`). Empty path obsługujemy oddzielnie
  // w writeAtPath / deleteAtPath.
  const first = path[0];
  if (first && first.kind === 'key' && RESERVED_ROOT_KEYS.has(first.value)) {
    throw rejectError(
      PATCH_REJECT_REASON.PathOutOfNamespace,
      `${ctx}: root key '${first.value}' is reserved`
    );
  }
}

/// Kanoniczny klucz stringowy z StatePath — używany do indeksowania
/// subskrybentów i porównań prefiksów. Format: każdy segment zakodowany
/// jako `k:<value>` lub `i:<value>`, połączone US (`\x1F`). Format ten
/// nie wycieka na wire — ma sens tylko w pamięci JS.
function pathKey(path) {
  if (path.length === 0) return '';
  const parts = new Array(path.length);
  for (let i = 0; i < path.length; i++) {
    const s = path[i];
    parts[i] = s.kind === 'key' ? `k:${s.value}` : `i:${s.value}`;
  }
  return parts.join('\x1F');
}

/// Czy `prefix` jest prefiksem ścieżki `path` (równość = prefiks). Używane
/// przez `notifyOverlap`.
function isPrefixOf(prefix, path) {
  if (prefix.length > path.length) return false;
  for (let i = 0; i < prefix.length; i++) {
    const a = prefix[i];
    const b = path[i];
    if (a.kind !== b.kind || a.value !== b.value) return false;
  }
  return true;
}

// =============================================================================
// StateStore
// =============================================================================

export class StateStore {
  constructor({ addon_id, panel_id, panel_epoch } = {}) {
    if (typeof addon_id !== 'string' || !addon_id) {
      throw new TypeError('StateStore: addon_id must be non-empty string');
    }
    if (typeof panel_id !== 'string' || !panel_id) {
      throw new TypeError('StateStore: panel_id must be non-empty string');
    }
    const epochIsBig = typeof panel_epoch === 'bigint';
    const epochIsSafeInt =
      typeof panel_epoch === 'number' && Number.isSafeInteger(panel_epoch);
    if (!epochIsBig && !epochIsSafeInt) {
      throw new TypeError(
        'StateStore: panel_epoch must be BigInt or safe-integer Number'
      );
    }
    if (epochIsBig && panel_epoch < 0n) {
      throw new TypeError('StateStore: panel_epoch must be non-negative');
    }
    if (epochIsSafeInt && panel_epoch < 0) {
      throw new TypeError('StateStore: panel_epoch must be non-negative');
    }
    // Publiczne pola są snake_case zgodne z wire shape z spec §6.4. Caller
    // (dispatcher) inicjuje store wprost z pól zdekodowanego CBOR-a bez
    // tłumaczenia kluczy.
    this.addon_id = addon_id;
    this.panel_id = panel_id;
    this.panel_epoch = epochIsBig ? panel_epoch : BigInt(panel_epoch);
    this._root = Object.create(null);
    this._revision = 0n;
    // subscribers: Map<pathKey, { path: StatePath, callbacks: Set<fn> }>.
    this._subscribers = new Map();
    // Bufor truncated snapshot chunks: zbieramy entries aż dostaniemy
    // chunk z truncated=false, dopiero wtedy atomowo zamieniamy root.
    this._snapshotBuffer = null;
    this._patchRejectedListeners = new Set();
    this._destroyed = false;
  }

  revision() {
    return this._revision;
  }

  /// Czyta wartość pod ścieżką. Zwraca `undefined` jeśli ścieżka
  /// niezdefiniowana lub natrafi na typ niezgodny z segmentem (np. Index
  /// na map).
  read(path) {
    this._assertAlive();
    assertPath(path, 'StateStore.read');
    let node = this._root;
    for (const seg of path) {
      if (node == null) return undefined;
      if (seg.kind === 'key') {
        if (!isStateMap(node)) return undefined;
        node = node[seg.value];
      } else {
        if (!Array.isArray(node)) return undefined;
        if (seg.value >= node.length) return undefined;
        node = node[seg.value];
      }
    }
    return node;
  }

  /// Subskrybuje zmiany w `path` i każdym deskendencie. Wywołanie callbacku
  /// jest synchroniczne wewnątrz applyPatch/applySnapshot/applyReset. Zwraca
  /// funkcję do odsubskrybowania.
  subscribe(path, callback) {
    this._assertAlive();
    assertPath(path, 'StateStore.subscribe');
    if (typeof callback !== 'function') {
      throw new TypeError('StateStore.subscribe: callback must be function');
    }
    const key = pathKey(path);
    let entry = this._subscribers.get(key);
    if (!entry) {
      entry = { path, callbacks: new Set() };
      this._subscribers.set(key, entry);
    }
    entry.callbacks.add(callback);
    return () => {
      const e = this._subscribers.get(key);
      if (!e) return;
      e.callbacks.delete(callback);
      if (e.callbacks.size === 0) this._subscribers.delete(key);
    };
  }

  /// Listener dla PatchRejected — addon dispatcher emituje to do backendu.
  onPatchRejected(callback) {
    this._assertAlive();
    if (typeof callback !== 'function') {
      throw new TypeError('StateStore.onPatchRejected: callback must be function');
    }
    this._patchRejectedListeners.add(callback);
    return () => this._patchRejectedListeners.delete(callback);
  }

  /// Aplikuje `StateSnapshot` z §6.4. Obsługuje chunkowanie przez `truncated`:
  /// pierwszy chunk inicjuje bufor, kolejne dokładają entries, ostatni
  /// (truncated=false) atomowo zamienia root i revision. Mid-stream zmiana
  /// `revision` ⇒ porzucamy zarówno bufor jak i bieżący chunk (zgodnie z
  /// §6.4: snapshot musi być spójny per-revision; czekamy aż sender zacznie
  /// od nowa pełną sekwencję dla nowej rewizji).
  applySnapshot({ entries, state_revision, truncated, panel_epoch }) {
    this._assertAlive();
    if (!this._checkEpoch(panel_epoch, 'applySnapshot')) return false;
    if (!Array.isArray(entries)) {
      throw new TypeError('applySnapshot: entries must be array');
    }
    const rev = this._toRev(state_revision, 'applySnapshot.state_revision');
    if (this._snapshotBuffer != null && this._snapshotBuffer.revision !== rev) {
      // Sender bumped revision mid-stream — dropujemy bufor I bieżący chunk;
      // czekamy na świeży snapshot dla nowej rewizji.
      // eslint-disable-next-line no-console
      console.warn(
        '[state-store] applySnapshot: mid-stream revision change',
        { old: this._snapshotBuffer.revision, new: rev }
      );
      this._snapshotBuffer = null;
      return false;
    }
    if (this._snapshotBuffer == null) {
      this._snapshotBuffer = { revision: rev, entries: [] };
    }
    for (const ent of entries) {
      assertPath(ent.path, 'applySnapshot.entry.path');
      this._snapshotBuffer.entries.push(ent);
      if (this._snapshotBuffer.entries.length > MAX_SNAPSHOT_ENTRIES) {
        // eslint-disable-next-line no-console
        console.warn(
          '[state-store] applySnapshot: MAX_SNAPSHOT_ENTRIES exceeded — dropping buffer'
        );
        this._snapshotBuffer = null;
        return false;
      }
    }
    if (truncated) return true;

    // Commit: build fresh root from buffered entries, swap atomically.
    const newRoot = Object.create(null);
    try {
      for (const ent of this._snapshotBuffer.entries) {
        const safeValue = normalizeStateValue(ent.value, 'applySnapshot.entry.value');
        writeAtPath(newRoot, ent.path, safeValue, /*createIntermediates=*/ true);
      }
    } catch (err) {
      // Malformed snapshot — keep current root, abort.
      // eslint-disable-next-line no-console
      console.warn('[state-store] applySnapshot: failed to build root:', err.message);
      this._snapshotBuffer = null;
      return false;
    }
    const oldRoot = this._root;
    this._root = newRoot;
    this._revision = rev;
    this._snapshotBuffer = null;
    // Po snapshotcie powiadamiamy wszystkich subskrybentów (cała ścieżka
    // mogła się zmienić) — dispatchujemy single root-level notify.
    this._notifyRoot(oldRoot);
    return true;
  }

  /// Aplikuje `StatePatch` z §6.4 atomowo. Albo wszystkie operacje przejdą
  /// i revision skacze do `newRevision`, albo żadna i emitowany jest
  /// `PatchRejected` (0x0123) zgodny ze spec'em. `msgId` to identyfikator
  /// wire'owy odrzucanego StatePatch — caller go zna z dispatcher'a.
  /// Stale `panelEpoch` jest dropowane bez emisji PatchRejected (spec nie
  /// przewiduje EpochMismatch jako powodu — kontroluje to dispatcher).
  applyPatch({ base_revision, new_revision, ops, panel_epoch, msg_id }) {
    this._assertAlive();
    if (!this._checkEpoch(panel_epoch, 'applyPatch')) return false;
    if (!Array.isArray(ops)) {
      throw new TypeError('applyPatch: ops must be array');
    }
    const base = this._toRev(base_revision, 'applyPatch.base_revision');
    const next = this._toRev(new_revision, 'applyPatch.new_revision');
    const rejected_msg_id = msg_id == null ? 0n : this._toRev(msg_id, 'applyPatch.msg_id');
    if (base !== this._revision) {
      this._emitRejected({
        rejected_msg_id,
        reason: PATCH_REJECT_REASON.RevisionMismatch,
        current_revision: this._revision,
      });
      return false;
    }
    // Snapshot root (structuredClone) — atomowy rollback przy błędzie operacji.
    const snapshot = cloneStateValue(this._root);
    const changedPaths = [];
    try {
      for (const op of ops) {
        assertPath(op.path, 'applyPatch.op.path');
        applyOpInPlace(this._root, op);
        changedPaths.push(op.path);
      }
    } catch (err) {
      // Rollback — restore root, emit PatchRejected.
      this._root = snapshot;
      const reason =
        err && err.__patchRejectReason
          ? err.__patchRejectReason
          : PATCH_REJECT_REASON.TypeMismatch;
      this._emitRejected({
        rejected_msg_id,
        reason,
        // §6.4 PatchRejected.current_revision: Option<u64>. Dispatcher
        // (CBOR encoder) musi emitować CBOR `null`, nie absent — explicite
        // ustawiamy null żeby JS-encoder nie pominął klucza.
        current_revision: null,
      });
      return false;
    }
    this._revision = next;
    for (const p of changedPaths) this._notifyOverlap(p);
    return true;
  }

  /// `StateReset` z §6.4 (0x0122) — czyści cały stan, ustawia revision na
  /// `new_revision`. Wymagane pole zgodnie ze spec — brak go to bug w
  /// dispatcher'ze, nie tryb domyślny.
  applyReset({ panel_epoch, new_revision } = {}) {
    this._assertAlive();
    if (!this._checkEpoch(panel_epoch, 'applyReset')) return false;
    if (new_revision == null) {
      throw new TypeError('applyReset: new_revision is required (§6.4)');
    }
    const next = this._toRev(new_revision, 'applyReset.new_revision');
    const oldRoot = this._root;
    this._root = Object.create(null);
    this._revision = next;
    this._snapshotBuffer = null;
    this._notifyRoot(oldRoot);
    return true;
  }

  /// Atomowy overlay (np. z `SlotContent.state_overlay`) — bez zmiany
  /// revision i bez weryfikacji baseRevision. Stosowane razem z
  /// SlotContent w jednym wire frame.
  applyOverlay(entries) {
    this._assertAlive();
    if (!Array.isArray(entries)) {
      throw new TypeError('applyOverlay: entries must be array');
    }
    const snapshot = cloneStateValue(this._root);
    const changed = [];
    try {
      for (const ent of entries) {
        assertPath(ent.path, 'applyOverlay.entry.path');
        const safeValue = normalizeStateValue(ent.value, 'applyOverlay.entry.value');
        writeAtPath(this._root, ent.path, safeValue, true);
        changed.push(ent.path);
      }
    } catch (err) {
      this._root = snapshot;
      throw err;
    }
    for (const p of changed) this._notifyOverlap(p);
    return true;
  }

  destroy() {
    this._destroyed = true;
    this._subscribers.clear();
    this._patchRejectedListeners.clear();
    this._root = Object.create(null);
    this._snapshotBuffer = null;
  }

  // ---- internals ----

  _assertAlive() {
    if (this._destroyed) {
      throw new Error('StateStore: instance was destroyed');
    }
  }

  _toRev(v, ctx) {
    if (typeof v === 'bigint') {
      if (v < 0n) {
        throw new TypeError(`${ctx} must be non-negative bigint`);
      }
      return v;
    }
    // u64 z wire'u idealnie przychodzi jako BigInt; dla wygody testów /
    // małych wartości akceptujemy safe-integer Number, ale unsafe Number
    // odrzucamy (cicha utrata precyzji przy konwersji).
    if (typeof v === 'number' && Number.isSafeInteger(v) && v >= 0) {
      return BigInt(v);
    }
    throw new TypeError(
      `${ctx} must be non-negative BigInt or safe-integer Number`
    );
  }

  _checkEpoch(epoch, ctx) {
    if (epoch == null) return true;
    let e;
    if (typeof epoch === 'bigint') {
      e = epoch;
    } else if (typeof epoch === 'number' && Number.isSafeInteger(epoch)) {
      e = BigInt(epoch);
    } else {
      throw new TypeError(
        `${ctx}: panel_epoch must be BigInt or safe-integer Number`
      );
    }
    if (e !== this.panel_epoch) {
      // Stale message for an old panel epoch — silently drop. Spec §6.4 nie
      // przewiduje EpochMismatch jako reason w PatchRejected; filtrowanie
      // po epoch należy do dispatcher'a, store jest tu defensywny.
      // eslint-disable-next-line no-console
      console.debug(
        `[state-store] ${ctx}: stale panel_epoch (${e} vs current ${this.panel_epoch}), dropping`
      );
      return false;
    }
    return true;
  }

  _notifyOverlap(changedPath) {
    for (const entry of this._subscribers.values()) {
      if (
        isPrefixOf(entry.path, changedPath) ||
        isPrefixOf(changedPath, entry.path)
      ) {
        for (const cb of entry.callbacks) {
          try {
            cb({ path: changedPath, store: this });
          } catch (e) {
            // eslint-disable-next-line no-console
            console.error('[state-store] subscriber callback threw:', e);
          }
        }
      }
    }
  }

  _notifyRoot(_oldRoot) {
    for (const entry of this._subscribers.values()) {
      for (const cb of entry.callbacks) {
        try {
          cb({ path: [], store: this });
        } catch (e) {
          // eslint-disable-next-line no-console
          console.error('[state-store] subscriber callback threw:', e);
        }
      }
    }
  }

  _emitRejected(payload) {
    // Wire shape per spec `PatchRejected` (0x0123): snake_case keys
    // (addon_id, panel_id, panel_epoch, rejected_msg_id, reason,
    // current_revision). Dispatcher forwarduje payload bezpośrednio
    // do CBOR-encode bez tłumaczenia, więc trzymamy się dokładnie tych
    // nazw i kolejności pól z wire'u.
    const evt = Object.freeze({
      addon_id: this.addon_id,
      panel_id: this.panel_id,
      panel_epoch: this.panel_epoch,
      ...payload,
    });
    for (const cb of this._patchRejectedListeners) {
      try {
        cb(evt);
      } catch (e) {
        // eslint-disable-next-line no-console
        console.error('[state-store] patch_rejected listener threw:', e);
      }
    }
  }
}

// =============================================================================
// PatchOp dispatcher — mutuje in-place, rzuca Error z `__patchRejectReason`
// przy konfliktach typu / index out of bounds / nieznanej operacji.
// =============================================================================

function applyOpInPlace(root, opEntry) {
  const { path, op } = opEntry;
  if (!op || typeof op !== 'object') {
    throw rejectError(PATCH_REJECT_REASON.StructuralLimit, 'PatchOp missing op');
  }
  // Wire shape: `PatchOpKind` ma pole `kind` (§6.4 patch.rs). Trzymamy się
  // tej nazwy bez tłumaczenia, żeby dispatcher mógł podać op surowy z
  // CBOR-decode.
  const { kind } = op;
  if (!PATCH_OP_KINDS.includes(kind)) {
    throw rejectError(
      PATCH_REJECT_REASON.StructuralLimit,
      `unknown PatchOp.kind: ${kind}`
    );
  }
  switch (kind) {
    case 'set': {
      const safe = normalizeStateValue(op.value, 'set.value');
      writeAtPath(root, path, safe, true);
      return;
    }
    case 'delete':
      deleteAtPath(root, path);
      return;
    case 'append_array': {
      const arr = readForArrayMutation(root, path);
      assertArrayCanGrow(arr, 1);
      const safe = normalizeStateValue(op.value, 'append_array.value');
      arr.push(safe);
      return;
    }
    case 'prepend_array': {
      const arr = readForArrayMutation(root, path);
      assertArrayCanGrow(arr, 1);
      const safe = normalizeStateValue(op.value, 'prepend_array.value');
      arr.unshift(safe);
      return;
    }
    case 'insert_array': {
      const arr = readForArrayMutation(root, path);
      const idx = typeof op.index === 'bigint' && op.index >= 0n && op.index <= 0xFFFFFFFFn
        ? Number(op.index)
        : op.index;
      if (!Number.isInteger(idx) || idx < 0 || idx > arr.length) {
        throw rejectError(
          PATCH_REJECT_REASON.ArrayBounds,
          `insert_array index ${idx} out of bounds (len=${arr.length})`
        );
      }
      assertArrayCanGrow(arr, 1);
      const safe = normalizeStateValue(op.value, 'insert_array.value');
      arr.splice(idx, 0, safe);
      return;
    }
    case 'remove_array': {
      const arr = readForArrayMutation(root, path);
      const idx = typeof op.index === 'bigint' && op.index >= 0n && op.index <= 0xFFFFFFFFn
        ? Number(op.index)
        : op.index;
      if (!Number.isInteger(idx) || idx < 0 || idx >= arr.length) {
        throw rejectError(
          PATCH_REJECT_REASON.ArrayBounds,
          `remove_array index ${idx} out of bounds (len=${arr.length})`
        );
      }
      arr.splice(idx, 1);
      return;
    }
    case 'merge_map': {
      const target = readForMapMutation(root, path);
      const overlay = op.value;
      if (!isStateMap(overlay)) {
        throw rejectError(PATCH_REJECT_REASON.TypeMismatch, 'merge_map.value must be map');
      }
      for (const k of Object.keys(overlay)) {
        if (FORBIDDEN_MAP_KEYS.has(k)) {
          throw rejectError(
            PATCH_REJECT_REASON.TypeMismatch,
            `merge_map: forbidden key '${k}'`
          );
        }
        target[k] = normalizeStateValue(overlay[k], 'merge_map.value');
      }
      return;
    }
    case 'increment': {
      const current = readAtPath(root, path);
      const delta = op.delta;
      // i64 z wire'u powinno być BigInt; Number akceptujemy tylko jeśli
      // jest safe-integer, żeby nie wpuścić cichej utraty precyzji.
      if (
        typeof delta !== 'bigint' &&
        !(typeof delta === 'number' && Number.isSafeInteger(delta))
      ) {
        throw rejectError(
          PATCH_REJECT_REASON.TypeMismatch,
          'increment.delta must be BigInt or safe-integer Number'
        );
      }
      const next = incrementValue(current, delta);
      writeAtPath(root, path, next, true);
      return;
    }
  }
}

function assertArrayCanGrow(arr, by) {
  if (arr.length + by > MAX_ARRAY_LEN) {
    throw rejectError(
      PATCH_REJECT_REASON.StructuralLimit,
      `array would exceed MAX_ARRAY_LEN (${MAX_ARRAY_LEN})`
    );
  }
}

function rejectError(reason, message) {
  const e = new Error(message);
  e.__patchRejectReason = reason;
  return e;
}

function incrementValue(current, delta) {
  if (current === undefined || current === null) {
    // Brak wartości pod ścieżką: traktujemy jak 0.
    return delta;
  }
  const currentIsBig = typeof current === 'bigint';
  const currentIsSafeInt =
    typeof current === 'number' && Number.isSafeInteger(current);
  if (!currentIsBig && !currentIsSafeInt) {
    // Explicite odrzucamy stringi, booleany, floaty oraz unsafe Number-y
    // — żaden inny typ nie ma sensu liczbowego dla increment.
    throw rejectError(
      PATCH_REJECT_REASON.TypeMismatch,
      `increment expects BigInt or safe-integer Number at path, got ${describeType(current)}`
    );
  }
  if (currentIsBig || typeof delta === 'bigint') {
    const a = currentIsBig ? current : BigInt(current);
    const b = typeof delta === 'bigint' ? delta : BigInt(delta);
    return a + b;
  }
  // Oba operandy są Number. Promotujemy do BigInt, jeśli wynik wypadłby
  // poza safe-integer range — żeby nie tracić cichą precyzję. i64 z wire'u
  // może być spoza Number.MAX_SAFE_INTEGER.
  const sum = current + delta;
  if (!Number.isSafeInteger(sum)) {
    return BigInt(current) + BigInt(delta);
  }
  return sum;
}

function readAtPath(root, path) {
  let node = root;
  for (const seg of path) {
    if (node == null) return undefined;
    if (seg.kind === 'key') {
      if (!isStateMap(node)) return undefined;
      node = node[seg.value];
    } else {
      if (!Array.isArray(node)) return undefined;
      node = node[seg.value];
    }
  }
  return node;
}

function readForArrayMutation(root, path) {
  const v = readAtPath(root, path);
  if (!Array.isArray(v)) {
    throw rejectError(
      PATCH_REJECT_REASON.TypeMismatch,
      `array op requires array at path, got ${describeType(v)}`
    );
  }
  return v;
}

function readForMapMutation(root, path) {
  const v = readAtPath(root, path);
  if (v == null || typeof v !== 'object' || Array.isArray(v)) {
    throw rejectError(
      PATCH_REJECT_REASON.TypeMismatch,
      `map op requires map at path, got ${describeType(v)}`
    );
  }
  return v;
}

function describeType(v) {
  if (v === null) return 'null';
  if (Array.isArray(v)) return 'array';
  if (v === undefined) return 'undefined';
  return typeof v;
}

/// Pisze wartość w `root` pod `path`. Jeśli `createIntermediates`, brakujące
/// kontenery są tworzone na podstawie kindu KOLEJNEGO segmentu (Key → obj,
/// Index → array). Rzuca TypeMismatch jeśli istniejący kontener ma inny typ
/// niż wymaga segment.
function writeAtPath(root, path, value, createIntermediates) {
  if (path.length === 0) {
    throw rejectError(PATCH_REJECT_REASON.TypeMismatch, 'cannot write at empty path');
  }
  let node = root;
  for (let i = 0; i < path.length - 1; i++) {
    const seg = path[i];
    const nextSeg = path[i + 1];
    if (seg.kind === 'key') {
      if (!isStateMap(node)) {
        throw rejectError(
          PATCH_REJECT_REASON.TypeMismatch,
          `path segment ${i}: expected map, got ${describeType(node)}`
        );
      }
      if (!(seg.value in node) || node[seg.value] == null) {
        if (!createIntermediates) {
          throw rejectError(
            PATCH_REJECT_REASON.TypeMismatch,
            'missing intermediate node'
          );
        }
        node[seg.value] = nextSeg.kind === 'index' ? [] : Object.create(null);
      }
      node = node[seg.value];
    } else {
      if (!Array.isArray(node)) {
        throw rejectError(
          PATCH_REJECT_REASON.TypeMismatch,
          `path segment ${i}: expected array, got ${describeType(node)}`
        );
      }
      if (seg.value > node.length) {
        // Padding wymagałby wpisania nulli powyżej top — wymagamy ścisłej
        // sekwencji indeksów (write-through dopuszczone wyłącznie na
        // pozycję ≤ length, by uniknąć DoS przez gigantyczny u32 index).
        throw rejectError(
          PATCH_REJECT_REASON.ArrayBounds,
          `path segment ${i}: index ${seg.value} > length ${node.length}`
        );
      }
      if (seg.value === node.length || node[seg.value] == null) {
        if (!createIntermediates) {
          throw rejectError(
            PATCH_REJECT_REASON.ArrayBounds,
            `path segment ${i}: index ${seg.value} not present`
          );
        }
        assertArrayCanGrow(node, seg.value === node.length ? 1 : 0);
        if (seg.value === node.length) node.push(null);
        node[seg.value] = nextSeg.kind === 'index' ? [] : Object.create(null);
      }
      node = node[seg.value];
    }
  }
  const last = path[path.length - 1];
  if (last.kind === 'key') {
    if (!isStateMap(node)) {
      throw rejectError(
        PATCH_REJECT_REASON.TypeMismatch,
        `final segment: expected map, got ${describeType(node)}`
      );
    }
    node[last.value] = value;
  } else {
    if (!Array.isArray(node)) {
      throw rejectError(
        PATCH_REJECT_REASON.TypeMismatch,
        `final segment: expected array, got ${describeType(node)}`
      );
    }
    if (last.value > node.length) {
      throw rejectError(
        PATCH_REJECT_REASON.ArrayBounds,
        `final segment: index ${last.value} > length ${node.length}`
      );
    }
    if (last.value === node.length) {
      assertArrayCanGrow(node, 1);
      node.push(value);
    } else {
      node[last.value] = value;
    }
  }
}

function deleteAtPath(root, path) {
  if (path.length === 0) {
    throw rejectError(PATCH_REJECT_REASON.TypeMismatch, 'cannot delete at empty path');
  }
  let node = root;
  for (let i = 0; i < path.length - 1; i++) {
    const seg = path[i];
    if (node == null) return; // already absent
    if (seg.kind === 'key') {
      if (!isStateMap(node)) return;
      node = node[seg.value];
    } else {
      if (!Array.isArray(node)) return;
      node = node[seg.value];
    }
  }
  const last = path[path.length - 1];
  if (node == null) return;
  if (last.kind === 'key') {
    if (!isStateMap(node)) return;
    delete node[last.value];
  } else {
    if (!Array.isArray(node)) return;
    if (last.value < node.length) node.splice(last.value, 1);
  }
}

// =============================================================================
// Eksport pomocniczy — używany przez testy i higher-level renderery.
// =============================================================================

export {
  PATCH_REJECT_REASON,
  PATCH_OP_KINDS,
  MAX_STATE_PATH_SEGMENTS,
  MAX_SNAPSHOT_ENTRIES,
  MAX_ARRAY_LEN,
  pathKey,
  isPrefixOf,
};
