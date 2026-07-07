// =============================================================================
// Plik: sdk-runtime/component-renderer.js
// Opis: Engine renderowania komponentów addon UI (Faza 6 Krok 3.3a-1).
// Przyjmuje zdekodowany `Component` (§1.6) i tworzy realny DOM element
// (web component `tf-*` albo plain HTML element zależnie od tag-a). Pin-
// uje binding'i ze `StateStore`, attachuje handlery zdarzeń przez event
// dispatcher, aplikuje a11y/visibility/test_id. Per-tag renderery
// rejestrują się przez `ComponentRenderer.register(tag, fn)` —
// `layout-atomic-renderers.js`, `layout-containers-renderers.js`, itd.
//
// Boundary contract: dispatcher tłumaczy `#[cbor(map)]` integer keys ze
// spec'u (`tentaflow-sdk-spec/src/protocol/ui/component.rs`) do nazwanych
// pól JS:
//   Component { tag, id, fields, handlers, bind, a11y, visibility, test_id }
//   FieldMap   = Array<[u8 key, Value]>
//   HandlerMap = Array<[EventKind, Handler]>
//   Accessibility { role, label, label_for, described_by, live, expanded,
//                   disabled, required, invalid, pressed, selected }
//   Visibility    { visible, display_above_breakpoint, display_below_breakpoint,
//                   hidden_for_assistive }
// =============================================================================

import {
  subscribeBindRef,
  resolveBindRef,
  subscribeBindSpec,
  readBindSpec,
  formatValue,
} from './bind-resolver.js';
import {
  SEARCHBOX_TAG, renderSearchBox,
  SCROLLCONTAINER_TAG, renderScrollContainer,
} from './form-search-scroll-renderer.js';
import { SPLIT_TAG, renderSplit } from './layout-containers-renderers.js';

// =============================================================================
// Public registry — per-tag rendererzy
// =============================================================================

const TAG_HANDLERS = new Map();

/// Rejestruje funkcję renderującą dla danego tag-u komponentu. Wywoływana
/// raz per chunk-rendererowy (np. `layout-atomic-renderers.js` rejestruje
/// 0x0108/0x0109/0x010F). Duplicate-registration rzuca błąd — chronimy
/// przed kolizjami między grupami komponentów.
export function registerComponentRenderer(tag, renderFn) {
  if (!Number.isInteger(tag) || tag < 0 || tag > 0xFFFF) {
    throw new TypeError('registerComponentRenderer: tag must be u16');
  }
  if (typeof renderFn !== 'function') {
    throw new TypeError('registerComponentRenderer: renderFn must be function');
  }
  if (TAG_HANDLERS.has(tag)) {
    throw new Error(
      `registerComponentRenderer: tag 0x${tag.toString(16).padStart(4, '0')} already registered`
    );
  }
  TAG_HANDLERS.set(tag, renderFn);
}

/// Pobiera zarejestrowany renderer dla tag-u, lub `undefined` jeśli brak.
/// Używany przez testy; produkcyjny kod woła `ComponentRenderer.render`.
export function lookupComponentRenderer(tag) {
  return TAG_HANDLERS.get(tag);
}

/// Resetuje rejestrację — DOM tests muszą zaczynać od czystego stanu.
/// Eksponowane jako pomoc do testów; produkcja nigdy nie woła.
export function _clearComponentRendererRegistry() {
  TAG_HANDLERS.clear();
}

// =============================================================================
// ComponentRenderer
// =============================================================================

export class ComponentRenderer {
  /// @param {object} opts
  /// @param {StateStore} opts.store
  /// @param {EventDispatcher} opts.eventDispatcher  — chunk 3.6 wstawi
  ///   realny dispatcher. Tu wymagamy tylko `.emit(addonId, panelId,
  ///   panelEpoch, eventKind, source_id, payload)` — sygnatura stub'owa
  ///   z możliwością nadpisania w testach.
  /// @param {string=} opts.locale — przekazywane do bind-resolver
  ///   `formatValue`. Domyślnie pochodzi od `navigator.language`.
  constructor({ store, eventDispatcher, locale } = {}) {
    if (!store || typeof store.subscribe !== 'function') {
      throw new TypeError('ComponentRenderer: store must be a StateStore');
    }
    if (eventDispatcher == null) {
      throw new TypeError('ComponentRenderer: eventDispatcher is required');
    }
    if (typeof eventDispatcher.emit !== 'function') {
      throw new TypeError('ComponentRenderer: eventDispatcher.emit must be function');
    }
    this.store = store;
    this.eventDispatcher = eventDispatcher;
    this.locale = locale;
    // Map<Element, Array<unsub fn>> — cleanup'y per element. `destroy`
    // przelatuje, woła każdą funkcję i czyści mapę.
    this._cleanups = new WeakMap();
  }

  /// Renderuje `Component` do DOM. Zwraca root `Element`. Cleanup
  /// subskrypcji jest powiązany z elementem — `destroy(element)` zwalnia
  /// wszystkie powiązane handlery store'a i listenery DOM.
  render(component) {
    assertComponent(component, 'ComponentRenderer.render');
    const tagHandler = TAG_HANDLERS.get(component.tag);
    if (!tagHandler) {
      throw new Error(
        `ComponentRenderer: no renderer registered for tag 0x${component.tag
          .toString(16)
          .padStart(4, '0')}`
      );
    }
    const cleanups = [];
    // Dzieci wyrenderowane przez `renderChild` mają cleanup'y przypięte do
    // SWOICH elementów (per-element ownership), nie do `cleanups` rodzica. Gdy
    // render rodzica rzuci PO wyrenderowaniu dziecka, lokalna pętla cleanup'ów
    // nie tknie subskrypcji dziecka — leak. Śledzimy je tu i w catch wołamy
    // `destroy(childEl)`, żeby zwolnić całe poddrzewo dziecka.
    const childElements = [];
    const ctx = {
      engine: this,
      store: this.store,
      eventDispatcher: this.eventDispatcher,
      locale: this.locale,
      cleanups,
      registerCleanup: (fn) => cleanups.push(fn),
      renderChild: (child) => {
        // Cleanup'y dziecka ZOSTAJĄ przypięte do elementu dziecka (per-element
        // ownership), a nie spłaszczane do rodzica. `destroy(root)` schodzi po
        // poddrzewie DOM i zwalnia je rekurencyjnie, a reconcile może wymienić
        // pojedyncze dziecko i zwolnić dokładnie jego subskrypcje bez tykania
        // rodzeństwa.
        const childEl = this.render(child);
        childElements.push(childEl);
        return childEl;
      },
      readField: (fields, key) => readField(fields, key),
      formatValue: (value, fmt) => formatValue(value, fmt, this.locale),
    };
    let element;
    try {
      element = tagHandler(component, ctx);
      if (!(element instanceof globalThis.Element)) {
        throw new TypeError(
          `ComponentRenderer: renderer for tag 0x${component.tag.toString(16)} did not return an Element`
        );
      }
      applyCommonAttributes(element, component, ctx);
      applyEventHandlers(element, component, ctx);
      applyBindings(element, component, ctx);
    } catch (err) {
      // Subskrypcje/listenery zarejestrowane przed throw'em muszą zostać
      // zwolnione, inaczej store leak'uje callback'i nieusuniętej skróty.
      for (const fn of cleanups) {
        try {
          fn();
        } catch {}
      }
      // Dzieci wyrenderowane przed throw'em trzymają cleanup'y w WeakMap pod
      // swoimi elementami — `destroy` schodzi rekurencyjnie i jest idempotentny
      // (usuwa wpis z WeakMap), więc nie ma podwójnego zwolnienia.
      for (const childEl of childElements) {
        this.destroy(childEl);
      }
      throw err;
    }
    this._cleanups.set(element, cleanups);
    return element;
  }

  /// Zwalnia subskrypcje store'a i listenery DOM przypięte do `element` ORAZ
  /// całego jego poddrzewa DOM (cleanup'y są przypięte per-element, więc
  /// schodzimy rekurencyjnie po dzieciach). Bezpieczne dla detached nodes.
  destroy(element) {
    if (element instanceof globalThis.Element) {
      // Najpierw dzieci — porządek nie ma znaczenia dla niezależnych
      // subskrypcji, ale trzymamy go deterministycznie (bottom-up).
      for (const child of Array.from(element.children)) {
        this.destroy(child);
      }
    }
    const cleanups = this._cleanups.get(element);
    if (!cleanups) return;
    for (const fn of cleanups) {
      try {
        fn();
      } catch (e) {
        // eslint-disable-next-line no-console
        console.error('[component-renderer] cleanup threw:', e);
      }
    }
    this._cleanups.delete(element);
  }
}

// =============================================================================
// Reconcile support — transparent container introspection
// =============================================================================
//
// Reconcile (SlotManager) musi wiedzieć, dla których tagów root element to
// "przezroczysty kontener": plain <div>, którego dzieci DOM odpowiadają 1:1
// (po kolejności) dzieciom-komponentom z FieldMap, BEZ przeplatanych
// wewnętrznych węzłów reaktywnych. Tylko dla takich tagów wolno reużyć
// wrapper i zdiffować dzieci pozycyjnie — w przeciwnym razie field→DOM nie
// jest 1:1 i diff rozjechałby strukturę.
//
// Wartość mapy to u8 key FieldMap, pod którym leży `Array<Component>` dzieci.
// Świadomie wykluczamy:
//   - Grid (0x0102)        — dzieci są w GridChild FieldMap + per-dziecko
//                             inline style (nie 1:1 Component),
//   - SectionCard/Collapsible/Accordion/Tooltip — header/divider/footer i
//                             wewnętrzne węzły reaktywne psują 1:1,
//   - wszystkie leafy interaktywne (Input/Textarea/Select/...) — własne
//                             closures i wewnętrzny stan, których nie da się
//                             odświeżyć bez ponownego uruchomienia renderera.
const TRANSPARENT_CONTAINER_CHILD_KEY = Object.freeze(new Map([
  [0x0101, 5], // Flex.children
  [0x0103, 2], // Stack.children
  [0x0104, 3], // Cluster.children
  [0x0106, 8], // Card.children
]));

/// Zwraca u8 key dzieci dla przezroczystego kontenera, albo `undefined`
/// jeśli tag nie jest reconcile'owalny po dzieciach. SlotManager używa tego
/// żeby zdecydować, czy schodzić w dół (reuse wrappera) czy zrobić replace.
export function transparentContainerChildKey(tag) {
  return TRANSPARENT_CONTAINER_CHILD_KEY.get(tag);
}

/// Wyciąga uporządkowaną listę dzieci-komponentów z FieldMap kontenera.
/// Zwraca `[]` gdy pole nieobecne. Zakłada, że `tag` jest przezroczystym
/// kontenerem (caller sprawdził `transparentContainerChildKey`).
export function readContainerChildren(component) {
  const key = TRANSPARENT_CONTAINER_CHILD_KEY.get(component.tag);
  if (key === undefined) return null;
  const raw = readField(component.fields, key);
  if (raw === undefined || raw === null) return [];
  if (!Array.isArray(raw)) return null;
  return raw;
}

/// Strukturalne porównanie dwóch komponentów Z POMINIĘCIEM jednego field
/// key (zwykle children) — reszta opisuje "shell" kontenera (klasy, padding,
/// handlery, bind, a11y, visibility). Jeśli shell się różni, renderer
/// ustawiłby inne klasy/atrybuty na wrapperze, a tego nie da się odświeżyć
/// bez ponownego renderu — wtedy reconcile robi pełny replace tego węzła.
export function shellEqualsExceptChildKey(oldComp, newComp, exceptKey) {
  if (oldComp.tag !== newComp.tag) return false;
  if (oldComp.id !== newComp.id) return false;
  if (!deepEqual(oldComp.handlers ?? null, newComp.handlers ?? null)) return false;
  if (!deepEqual(oldComp.bind ?? null, newComp.bind ?? null)) return false;
  if (!deepEqual(oldComp.a11y ?? null, newComp.a11y ?? null)) return false;
  if (!deepEqual(oldComp.visibility ?? null, newComp.visibility ?? null)) return false;
  if ((oldComp.test_id ?? null) !== (newComp.test_id ?? null)) return false;
  return fieldMapEqualExcept(oldComp.fields, newComp.fields, exceptKey);
}

/// Pełne strukturalne porównanie dwóch komponentów (łącznie z dziećmi).
/// Używane przez reconcile dla leafów: identyczny komponent → DOM node i jego
/// żywe subskrypcje zostają nietknięte (zero re-renderu, zero utraty focusu).
export function componentDeepEqual(a, b) {
  return deepEqual(a, b);
}

function fieldMapEqualExcept(aFields, bFields, exceptKey) {
  const a = fieldsToMap(aFields, exceptKey);
  const b = fieldsToMap(bFields, exceptKey);
  if (a.size !== b.size) return false;
  for (const [k, v] of a) {
    if (!b.has(k)) return false;
    if (!deepEqual(v, b.get(k))) return false;
  }
  return true;
}

function fieldsToMap(fields, exceptKey) {
  const m = new Map();
  if (!Array.isArray(fields)) return m;
  for (const entry of fields) {
    if (!Array.isArray(entry) || entry.length !== 2) continue;
    if (entry[0] === exceptKey) continue;
    m.set(entry[0], entry[1]);
  }
  return m;
}

/// Strukturalna równość wartości wire'owych: prymitywy (w tym BigInt),
/// tablice i zwykłe obiekty. Wystarcza dla zdekodowanych Component/Value —
/// nie ma tu cyklicznych grafów ani funkcji.
function deepEqual(a, b) {
  if (a === b) return true;
  if (a == null || b == null) return a === b;
  const ta = typeof a;
  if (ta !== typeof b) return false;
  if (ta !== 'object') return false;
  const aArr = Array.isArray(a);
  const bArr = Array.isArray(b);
  if (aArr !== bArr) return false;
  if (aArr) {
    if (a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) {
      if (!deepEqual(a[i], b[i])) return false;
    }
    return true;
  }
  const ak = Object.keys(a);
  const bk = Object.keys(b);
  if (ak.length !== bk.length) return false;
  for (const k of ak) {
    if (!Object.prototype.hasOwnProperty.call(b, k)) return false;
    if (!deepEqual(a[k], b[k])) return false;
  }
  return true;
}

// =============================================================================
// Walidacja Component
// =============================================================================

function assertComponent(c, ctx) {
  if (!c || typeof c !== 'object') {
    throw new TypeError(`${ctx}: Component must be object`);
  }
  if (!Number.isInteger(c.tag) || c.tag < 0 || c.tag > 0xFFFF) {
    throw new TypeError(`${ctx}: Component.tag must be u16`);
  }
  if (typeof c.id !== 'string') {
    throw new TypeError(`${ctx}: Component.id must be string`);
  }
  if (!Array.isArray(c.fields)) {
    throw new TypeError(`${ctx}: Component.fields must be Array<[u8, Value]>`);
  }
  if (c.handlers != null && !Array.isArray(c.handlers)) {
    throw new TypeError(
      `${ctx}: Component.handlers must be Array<[EventKind, Handler]> or null`
    );
  }
  if (c.bind != null && (typeof c.bind !== 'object' || Array.isArray(c.bind))) {
    // Spec §1.6 `Component.bind: Option<BindSpec>` — JEDEN BindSpec, nie
    // lista. Dispatcher decode'uje `null` lub konkretny BindSpec object.
    throw new TypeError(
      `${ctx}: Component.bind must be BindSpec object or null`
    );
  }
  if (c.test_id != null) {
    if (typeof c.test_id !== 'string') {
      throw new TypeError(`${ctx}: Component.test_id must be string`);
    }
    if (c.test_id.length === 0 || c.test_id.length > 64) {
      throw new TypeError(`${ctx}: Component.test_id length must be 1..=64`);
    }
    // Spec §1.6 zezwala tylko na [a-z0-9_-]+.
    if (!/^[a-z0-9_-]+$/.test(c.test_id)) {
      throw new TypeError(
        `${ctx}: Component.test_id must match [a-z0-9_-]+ (got '${c.test_id}')`
      );
    }
  }
  // FieldMap: duplicate key detection + entry-shape check. Mirror Rust
  // `ensure_no_duplicate_keys`.
  const seenKeys = new Set();
  for (const entry of c.fields) {
    if (!Array.isArray(entry) || entry.length !== 2) {
      throw new TypeError(`${ctx}: FieldMap entry must be [u8, Value]`);
    }
    const [k] = entry;
    if (!Number.isInteger(k) || k < 0 || k > 0xFF) {
      throw new TypeError(`${ctx}: FieldMap key must be u8, got ${k}`);
    }
    if (seenKeys.has(k)) {
      throw new TypeError(`${ctx}: FieldMap duplicate key ${k}`);
    }
    seenKeys.add(k);
  }
}

/// Waliduje fragment Component tym samym rygorem co `render()`
/// (`assertComponent`: kształt FieldMap `[u8, Value]`, brak zdublowanych
/// kluczy, kształt `bind`/`handlers`/`test_id`) i schodzi rekurencyjnie po
/// dzieciach przezroczystych kontenerów. Reconcile NIE przechodzi przez
/// `render()` (reużywa wrappera), więc bez tego malformed/zdublowany fragment
/// zostałby cicho przyjęty przez `fieldsToMap`/`readContainerChildren` zamiast
/// odrzucony — `_applySlotContent` woła to na wejściu, zanim cokolwiek
/// zacznie patchować DOM.
export function validateFragmentTree(component, ctx) {
  assertComponent(component, ctx);
  const childKey = TRANSPARENT_CONTAINER_CHILD_KEY.get(component.tag);
  if (childKey === undefined) return;
  const children = readField(component.fields, childKey);
  if (children === undefined || children === null) return;
  if (!Array.isArray(children)) {
    throw new TypeError(`${ctx}: transparent container children must be Array<Component>`);
  }
  for (const child of children) {
    validateFragmentTree(child, ctx);
  }
}

/// Wyszukuje wartość pola po `u8 key` w `FieldMap`. Zwraca `undefined`
/// jeśli brak. Per-tag renderery wołają to bezpośrednio z `ctx.readField`.
/// FieldMap is always an Array of `[u8, Value]` pairs — no object fallback.
function readField(fields, key) {
  if (!Array.isArray(fields)) return undefined;
  for (const entry of fields) {
    if (!Array.isArray(entry) || entry.length !== 2) {
      throw new TypeError('FieldMap entry must be [u8, Value]');
    }
    if (entry[0] === key) return entry[1];
  }
  return undefined;
}

// =============================================================================
// Wspólne atrybuty: id, test_id, a11y, visibility
// =============================================================================

function applyCommonAttributes(element, component, ctx) {
  if (component.id) {
    element.setAttribute('data-component-id', component.id);
    applyFontScale(element, component.id, ctx);
  }
  if (component.test_id) element.setAttribute('data-testid', component.test_id);
  applyAccessibility(element, component.a11y, ctx);
  applyVisibility(element, component.visibility, ctx);
}

// Generic reactive font-size scaling: a component whose id is `fontscale-<key>`
// binds its inline `font-size` (in px) to that numeric state value, so an addon
// can offer user-controlled text sizing (e.g. viewer A−/A+) without a bespoke
// component. Keyed on `id` (not `test_id`) because the UI CBOR decoder does not
// carry test_id through to the JS render tree. Not translator-specific.
function applyFontScale(element, id, ctx) {
  if (typeof id !== 'string' || !id.startsWith('fontscale-')) return;
  const key = id.slice('fontscale-'.length);
  if (!key || !ctx.store) return;
  const path = [{ kind: 'key', value: key }];
  const apply = () => {
    const raw = ctx.store.read(path);
    const n = typeof raw === 'bigint' ? Number(raw) : raw;
    if (typeof n === 'number' && Number.isFinite(n) && n > 0) {
      element.style.fontSize = `${n}px`;
    }
  };
  apply();
  ctx.registerCleanup(ctx.store.subscribe(path, apply));
}

function applyAccessibility(element, a11y, ctx) {
  if (!a11y) return;
  if (typeof a11y.role === 'string') element.setAttribute('role', a11y.role);
  if (a11y.label_for) element.setAttribute('aria-labelledby', a11y.label_for);
  if (a11y.described_by) element.setAttribute('aria-describedby', a11y.described_by);
  if (typeof a11y.live === 'string') element.setAttribute('aria-live', a11y.live);
  // BindRef-based ARIA properties — reagują na zmiany store.
  bindAriaAttr(element, 'aria-label', a11y.label, ctx, valueToString);
  bindAriaAttr(element, 'aria-expanded', a11y.expanded, ctx, valueToBoolStr);
  bindAriaAttr(element, 'aria-disabled', a11y.disabled, ctx, valueToBoolStr);
  bindAriaAttr(element, 'aria-required', a11y.required, ctx, valueToBoolStr);
  bindAriaAttr(element, 'aria-invalid', a11y.invalid, ctx, valueToBoolStr);
  bindAriaAttr(element, 'aria-pressed', a11y.pressed, ctx, valueToBoolStr);
  bindAriaAttr(element, 'aria-selected', a11y.selected, ctx, valueToBoolStr);
}

function bindAriaAttr(element, attrName, bindRef, ctx, convert) {
  if (bindRef == null) return;
  const apply = () => {
    const raw = resolveBindRef(bindRef, ctx.store);
    const str = convert(raw);
    if (str == null) element.removeAttribute(attrName);
    else element.setAttribute(attrName, str);
  };
  apply();
  const off = subscribeBindRef(bindRef, ctx.store, apply);
  ctx.registerCleanup(off);
}

function applyVisibility(element, visibility, ctx) {
  if (!visibility) return;
  // Spec §1.6 Visibility fields:
  //   visible: Option<BindRef>            — reaktywna boolowa flaga
  //   display_above_breakpoint            — minimalny breakpoint
  //   display_below_breakpoint            — maksymalny breakpoint
  //   hidden_for_assistive: bool          — aria-hidden niezależnie od visual
  if (typeof visibility.display_above_breakpoint === 'string') {
    element.setAttribute(
      'data-visibility-above',
      visibility.display_above_breakpoint
    );
  }
  if (typeof visibility.display_below_breakpoint === 'string') {
    element.setAttribute(
      'data-visibility-below',
      visibility.display_below_breakpoint
    );
  }
  const hiddenForAssistive = visibility.hidden_for_assistive === true;
  if (hiddenForAssistive) {
    element.setAttribute('aria-hidden', 'true');
    // Marker dla `BindSpec.show` — żeby przy visible=true nie kasować
    // aria-hidden który musi pozostać przez hidden_for_assistive.
    element.dataset.hiddenForAssistive = 'true';
  }
  if (visibility.visible != null) {
    // Preserve any pre-existing inline display (some renderers set
    // inline-flex/grid directly) so showing again restores it exactly.
    const originalDisplay = element.style.display;
    const apply = () => {
      const v = resolveBindRef(visibility.visible, ctx.store);
      if (v === false) {
        element.setAttribute('hidden', '');
        // The bare `[hidden]` attribute (UA `display:none`) loses to any class
        // or `:host` rule that sets `display:flex/grid/inline-flex` — which the
        // tf-* components and stack containers all do. Force it with an inline
        // `display:none` (inline beats class specificity) so the element is
        // actually hidden, not just attribute-marked.
        element.style.display = 'none';
        // aria-hidden ustawiamy też przy visible=false, ale tylko jeśli
        // nie był wymuszony przez hidden_for_assistive (zachowujemy go).
        element.setAttribute('aria-hidden', 'true');
      } else {
        element.removeAttribute('hidden');
        // Restore the original inline display (usually empty → CSS class wins).
        if (element.style.display === 'none') element.style.display = originalDisplay;
        if (!hiddenForAssistive) element.removeAttribute('aria-hidden');
      }
    };
    apply();
    const off = subscribeBindRef(visibility.visible, ctx.store, apply);
    ctx.registerCleanup(off);
  }
}

function valueToString(v) {
  if (v == null) return null;
  if (typeof v === 'string') return v;
  if (typeof v === 'bigint') return v.toString();
  return String(v);
}

function valueToBoolStr(v) {
  if (v == null) return null;
  return v ? 'true' : 'false';
}

// =============================================================================
// Event handlers
// =============================================================================

function applyEventHandlers(element, component, ctx) {
  if (!component.handlers) return;
  for (const entry of component.handlers) {
    if (!Array.isArray(entry) || entry.length !== 2) {
      throw new TypeError(
        'Component.handlers entry must be [EventKind, Handler]'
      );
    }
    const [eventKind, handler] = entry;
    if (typeof eventKind !== 'string' || !EVENT_KIND_WIRE.has(eventKind)) {
      throw new TypeError(
        `EventKind '${eventKind}' is not in the spec whitelist (§1.6)`
      );
    }
    // Native DOM events idą do prawdziwej nazwy zdarzenia, komponent-
    // -specyficzne (np. row_click, files_selected) lecą jako CustomEvent
    // o tej samej nazwie — emit'owane są przez kod konkretnego renderer'a.
    const domEventName = NATIVE_EVENT_MAP.get(eventKind) || eventKind;
    const listener = (domEvent) => {
      ctx.eventDispatcher.emit({
        addon_id: ctx.store.addon_id,
        panel_id: ctx.store.panel_id,
        panel_epoch: ctx.store.panel_epoch,
        source_id: component.id,
        event_kind: eventKind,
        handler,
        dom_event: domEvent,
      });
    };
    element.addEventListener(domEventName, listener);
    ctx.registerCleanup(() => element.removeEventListener(domEventName, listener));
  }
}

/// Pełna lista wartości `EventKind` z spec'u §1.6 (`a11y.rs`). Wszystkie są
/// wire'owymi tstr-ami w snake_case. Renderer odrzuca nieznane wartości —
/// spec jest zamkniętą enum'ą.
const EVENT_KIND_WIRE = new Set([
  'click', 'double_click', 'long_press', 'context_menu',
  'change', 'input', 'submit', 'reset', 'commit',
  'focus', 'blur', 'key_down', 'key_up', 'key_press', 'save_shortcut',
  'open', 'close', 'select', 'deselect', 'dismiss', 'confirm', 'cancel',
  'drag_start', 'drag_end', 'drop',
  'scroll', 'scroll_end', 'resize', 'intersect',
  'pointer_down', 'pointer_up', 'pointer_move', 'pointer_cancel', 'wheel',
  'play', 'pause', 'ended', 'loaded', 'stream_error', 'fullscreen',
  'stream_chunk',
  'row_click', 'row_double_click', 'selection_change', 'cell_click',
  'cell_hover', 'item_click', 'marker_click', 'node_click', 'edge_click',
  'zoom_end', 'pan_end', 'point_hover', 'range_select',
  'files_selected', 'upload_progress', 'upload_complete', 'upload_error',
  'step_change', 'step_click', 'expand', 'collapse',
  'frame',
  'remove', 'image_click', 'day_click', 'slot_click',
  'event_click', 'event_drop', 'cell_toggle', 'cell_change',
  'bulk_apply', 'add_rule', 'remove_rule', 'approve_rule', 'mark_read',
  'field_change', 'scroll_top', 'filter_change', 'retry',
]);

/// Mapowanie wire EventKind → natywna nazwa DOM event. Klucze NIEobecne
/// tu są custom events emitowanymi przez renderer komponentu (np.
/// `row_click` przez data/tables renderer w chunku 3.3d).
const NATIVE_EVENT_MAP = new Map([
  ['click', 'click'],
  ['double_click', 'dblclick'],
  ['context_menu', 'contextmenu'],
  ['change', 'change'],
  ['input', 'input'],
  ['submit', 'submit'],
  ['reset', 'reset'],
  ['focus', 'focus'],
  ['blur', 'blur'],
  ['key_down', 'keydown'],
  ['key_up', 'keyup'],
  ['key_press', 'keypress'],
  ['scroll', 'scroll'],
  ['scroll_end', 'scrollend'],
  ['resize', 'resize'],
  ['drag_start', 'dragstart'],
  ['drag_end', 'dragend'],
  ['drop', 'drop'],
  ['pointer_down', 'pointerdown'],
  ['pointer_up', 'pointerup'],
  ['pointer_move', 'pointermove'],
  ['pointer_cancel', 'pointercancel'],
  ['wheel', 'wheel'],
  ['play', 'play'],
  ['pause', 'pause'],
  ['ended', 'ended'],
  ['loaded', 'loadeddata'],
]);

// =============================================================================
// BindSpec — Text / Attr / ClassToggle / Show / List / TwoWay
// =============================================================================

function applyBindings(element, component, ctx) {
  if (!component.bind) return;
  applyBindSpec(element, component.bind, ctx);
}

// Allowlist atrybutów dopuszczonych dla BindSpec.attr — addon NIE może
// pisać do `on*` (DOM event listeners), `style`, `srcdoc`/`src` (iframe
// injection), `formaction`/`action` (form-action redirect), navigation
// URL attrs bez walidacji scheme. Lista zezwala na bezpieczne ARIA,
// data-*, plus konkretne attrs często bind'owane (title, placeholder,
// alt, value-like).
const SAFE_ATTR_NAMES = new Set([
  'title',
  'placeholder',
  'alt',
  'value',
  'min',
  'max',
  'step',
  'maxlength',
  'minlength',
  'pattern',
  'rows',
  'cols',
  'lang',
  'dir',
  'tabindex',
  'autocomplete',
  'inputmode',
  'enterkeyhint',
  'spellcheck',
  'readonly',
  'required',
  'disabled',
  'checked',
  'selected',
  'open',
  'multiple',
  'size',
]);

// Allowlist scheme dla atrybutów URL-like (href, src, formaction, ...).
// Wszystko spoza listy → reject.
const SAFE_URL_SCHEMES = new Set(['http:', 'https:', 'mailto:', 'tel:']);
// `srcdoc` to NIE URL — przyjmuje raw HTML i jest wektorem XSS. Trzymamy go
// w osobnej, twardo zakazanej liście (nie w URL_LIKE_ATTRS).
const FORBIDDEN_ATTRS = new Set([
  'srcdoc',
  // `formaction` może nadpisać akcję formularza przez addon — blokujemy.
  // `action` na formie jest również niebezpieczna z arbitralnym schemą,
  // więc też blokujemy.
  'formaction',
  'action',
]);
const URL_LIKE_ATTRS = new Set([
  'href',
  'src',
  'cite',
  'background',
  'data',
  'codebase',
  'icon',
  'manifest',
  'poster',
  'usemap',
  'longdesc',
]);

function assertSafeAttrName(name) {
  if (typeof name !== 'string' || name.length === 0) {
    throw new TypeError('BindSpec.attr.name must be non-empty string');
  }
  const lower = name.toLowerCase();
  if (lower.startsWith('on')) {
    throw new Error(`BindSpec.attr: 'on*' event-handler attrs forbidden (${name})`);
  }
  if (lower === 'style') {
    throw new Error('BindSpec.attr: style attribute forbidden');
  }
  if (FORBIDDEN_ATTRS.has(lower)) {
    throw new Error(`BindSpec.attr: '${name}' attribute is forbidden`);
  }
  if (lower.startsWith('aria-') || lower.startsWith('data-')) return lower;
  if (SAFE_ATTR_NAMES.has(lower)) return lower;
  if (URL_LIKE_ATTRS.has(lower)) {
    // URL-like attrs są dozwolone TYLKO z walidacją scheme'a w wartości.
    return lower;
  }
  throw new Error(`BindSpec.attr: attribute '${name}' is not on the safe list`);
}

// Wykrywa explicite-podany scheme w stringu URL'a (przed jakąkolwiek
// normalizacją). Liczne wektory bypass'u używają wiodących whitespace'ów
// albo control characterów (ASCII < 0x20). Trimmujemy wszystkie kontrolne
// bajty i tabulatory/newliny — HTML spec też je usuwa w URL-like attrs.
function stripUrlLeadingControl(str) {
  return str.replace(/^[\x00-\x20]+/, '').replace(/[\t\r\n]/g, '');
}

function safeUrlValueOrNull(str) {
  if (str == null) return null;
  const stripped = stripUrlLeadingControl(String(str));
  if (stripped.length === 0) return null;
  let u;
  try {
    u = new URL(stripped, 'http://_/');
  } catch {
    throw new Error(`BindSpec.attr URL value rejected: ${str}`);
  }
  // Sprawdzamy parsed protocol — zawsze, niezależnie od scheme prefix'u
  // w raw input. Jeśli URL() użył base "http:" przez brak scheme'u, OK.
  // Jeśli explicite-podany scheme jest spoza allowlisty, throw.
  const rawHasScheme = /^[a-z][a-z0-9+.-]*:/i.test(stripped);
  if (rawHasScheme && !SAFE_URL_SCHEMES.has(u.protocol)) {
    throw new Error(`unsafe URL scheme '${u.protocol}' in ${str}`);
  }
  return stripped;
}

function applyBindSpec(element, bindSpec, ctx) {
  switch (bindSpec.kind) {
    case 'text': {
      const apply = () => {
        const v = readBindSpec(bindSpec, ctx.store);
        if (bindSpec.format) {
          element.textContent = ctx.formatValue(v, bindSpec.format);
        } else {
          element.textContent = valueToString(v) ?? '';
        }
      };
      apply();
      const off = subscribeBindSpec(bindSpec, ctx.store, apply);
      ctx.registerCleanup(off);
      return;
    }
    case 'attr': {
      // Walidujemy attr name RAZ przed pierwszym apply'em — niebezpieczne
      // nazwy (on*, style, srcdoc, ...) odrzucamy zanim cokolwiek zapiszę.
      const safeName = assertSafeAttrName(bindSpec.name);
      const isUrlAttr = URL_LIKE_ATTRS.has(safeName);
      const apply = () => {
        const v = readBindSpec(bindSpec, ctx.store);
        let str = valueToString(v);
        if (str == null) {
          element.removeAttribute(safeName);
          return;
        }
        if (isUrlAttr) {
          try {
            str = safeUrlValueOrNull(str);
          } catch (e) {
            // eslint-disable-next-line no-console
            console.warn('[component-renderer]', e.message);
            element.removeAttribute(safeName);
            return;
          }
        }
        element.setAttribute(safeName, str);
      };
      apply();
      const off = subscribeBindSpec(bindSpec, ctx.store, apply);
      ctx.registerCleanup(off);
      return;
    }
    case 'class_toggle': {
      const apply = () => {
        const v = readBindSpec(bindSpec, ctx.store);
        const on = bindSpec.negate ? !v : !!v;
        if (on) element.classList.add(bindSpec.class_name);
        else element.classList.remove(bindSpec.class_name);
      };
      apply();
      const off = subscribeBindSpec(bindSpec, ctx.store, apply);
      ctx.registerCleanup(off);
      return;
    }
    case 'show': {
      // `hidden_for_assistive` z visibility wymaga zachowania
      // `aria-hidden=true` nawet kiedy element jest visually widoczny.
      // Engine ustawia data-hidden-for-assistive attr przy `applyVisibility`
      // (poniżej) — `show` go uszanuje przy czyszczeniu aria-hidden.
      const apply = () => {
        const v = readBindSpec(bindSpec, ctx.store);
        const visible = bindSpec.negate ? !v : !!v;
        if (visible) {
          element.removeAttribute('hidden');
          if (element.dataset.hiddenForAssistive !== 'true') {
            element.removeAttribute('aria-hidden');
          }
        } else {
          element.setAttribute('hidden', '');
          element.setAttribute('aria-hidden', 'true');
        }
      };
      apply();
      const off = subscribeBindSpec(bindSpec, ctx.store, apply);
      ctx.registerCleanup(off);
      return;
    }
    case 'list':
    case 'two_way':
      // List wymaga template lookup z PanelShell — chunk 3.5 (slot manager)
      // dostarczy registry templates. TwoWay wymaga input-element-style
      // pisania z powrotem do store'a + msg_id correlation — chunk 3.6
      // (event dispatcher). Tutaj zamarkuj-i-trzymaj — element dostanie
      // data-bind-list / data-bind-two-way + path attrs, a 3.5/3.6 dopnie
      // logikę. Bez stub'ów logiki na razie — odrzucamy.
      throw new Error(
        `applyBindSpec: BindSpec.${bindSpec.kind} wymaga slot manager/event dispatcher (chunki 3.5/3.6)`
      );
    default:
      throw new TypeError(`applyBindSpec: unknown BindSpec.kind ${bindSpec.kind}`);
  }
}

registerComponentRenderer(SEARCHBOX_TAG, renderSearchBox);
registerComponentRenderer(SCROLLCONTAINER_TAG, renderScrollContainer);
registerComponentRenderer(SPLIT_TAG, renderSplit);
