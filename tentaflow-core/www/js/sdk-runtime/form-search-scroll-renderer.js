// =============================================================================
// Plik: sdk-runtime/form-search-scroll-renderer.js
// Opis: Renderers for SearchBox (0x0307, tf-searchbox web component) and
// ScrollContainer (0x0112, scrollable region wrapping child components).
// Spec refs: tentaflow-sdk-spec/src/protocol/ui/form/inputs.rs (SearchBox),
// tentaflow-sdk-spec/src/protocol/ui/layout/containers.rs (ScrollContainer).
// =============================================================================

import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';
import {
  requireEnum, requireBool, requireU16, requireString, requirePath,
  assertOnlyKnownFields,
} from './data-chart-shared.js';
import { parseDimensionToken } from './data-specialised-renderer.js';

// =============================================================================
// SearchBox (0x0307)
// =============================================================================

export const SEARCHBOX_TAG = 0x0307;
const SEARCHBOX_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);
const SEARCH_VARIANTS = new Set(['default', 'subtle', 'prominent']);

export function renderSearchBox(component, ctx) {
  assertOnlyKnownFields(component.fields, SEARCHBOX_FIELD_KEYS, 'SearchBox');

  const bindPath = requirePath(
    ctx.readField(component.fields, 0), 'SearchBox.bind_path'
  );
  const placeholderBind = ctx.readField(component.fields, 1);
  if (placeholderBind == null) {
    throw new TypeError('SearchBox.placeholder is required (BindRef)');
  }
  const debounceRaw = ctx.readField(component.fields, 2);
  // Spec §5 0x0307 default: debounce_ms = 300.
  const debounceMs = debounceRaw === undefined
    ? 300 : requireU16(debounceRaw, 'SearchBox.debounce_ms');
  const variant = requireEnum(
    ctx.readField(component.fields, 3), SEARCH_VARIANTS, 'SearchBox.variant'
  );
  const shortcutHintRaw = ctx.readField(component.fields, 4);
  const shortcutHint = shortcutHintRaw == null
    ? null : requireString(shortcutHintRaw, 'SearchBox.shortcut_hint');
  const actionIdRaw = ctx.readField(component.fields, 5);
  const onSearchActionId = actionIdRaw == null
    ? null : requireString(actionIdRaw, 'SearchBox.on_search_action_id');

  const el = document.createElement('tf-searchbox');
  el.classList.add(`tf-searchbox--variant-${variant}`);
  el.setAttribute('debounce', String(debounceMs));
  if (shortcutHint != null) el.setAttribute('data-shortcut-hint', shortcutHint);

  // Reactive placeholder (required BindRef).
  const applyPlaceholder = () => {
    const v = resolveBindRef(placeholderBind, ctx.store);
    const s = v == null ? '' : String(v);
    if (s) el.setAttribute('placeholder', s);
    else el.removeAttribute('placeholder');
  };
  applyPlaceholder();
  ctx.registerCleanup(subscribeBindRef(placeholderBind, ctx.store, applyPlaceholder));

  // SearchBox has no label field in the spec — the accessible name must come
  // from Component.a11y.label (mirrors Input/Textarea/Select fallback rule).
  if (component.a11y == null || component.a11y.label == null) {
    throw new TypeError(
      'SearchBox requires Component.a11y.label for accessible name'
    );
  }
  const initialLabel = resolveBindRef(component.a11y.label, ctx.store);
  if (typeof initialLabel !== 'string' || initialLabel.trim().length === 0) {
    throw new TypeError(
      'SearchBox.a11y.label must resolve to non-blank string at initial render'
    );
  }
  const applyAriaLabel = () => {
    const v = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof v === 'string' && v.trim().length > 0) {
      el.setAttribute('aria-label', v);
    } else {
      el.removeAttribute('aria-label');
    }
  };
  applyAriaLabel();
  ctx.registerCleanup(
    subscribeBindRef(component.a11y.label, ctx.store, applyAriaLabel)
  );

  // Store → value (one-way read; write-back goes through the dispatcher).
  const syncValue = () => {
    let v;
    try { v = ctx.store.read(bindPath); } catch { v = undefined; }
    const next = v == null ? '' : String(v);
    if (el.value !== next) el.value = next;
  };
  syncValue();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, syncValue));

  // tf-searchbox emits a debounced 'search' with detail {value}, and the inner
  // native input's input/change bubble to the host. None of the raw events
  // carries the SDK payload shape, so we stopImmediatePropagation each raw
  // event and re-emit a single synthetic event tagged `__tfReemit`:
  //   - 'search' → { query, action_id }  (Combobox/Autocomplete remote shape)
  //   - 'input' / 'change' → { value, kind: 'tstr' } (Input/Textarea shape)
  const reemit = (name, detail) => {
    const ce = new CustomEvent(name, { bubbles: false, detail });
    ce.__tfReemit = true;
    el.dispatchEvent(ce);
  };
  const onSearch = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    const query = e.detail && typeof e.detail.value === 'string'
      ? e.detail.value : el.value;
    reemit('search', { query, action_id: onSearchActionId });
  };
  const onInput = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    reemit('input', { value: el.value, kind: 'tstr' });
  };
  const onChange = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    reemit('change', { value: el.value, kind: 'tstr' });
  };
  el.addEventListener('search', onSearch);
  el.addEventListener('input', onInput);
  el.addEventListener('change', onChange);
  ctx.registerCleanup(() => {
    el.removeEventListener('search', onSearch);
    el.removeEventListener('input', onInput);
    el.removeEventListener('change', onChange);
  });

  return el;
}

// =============================================================================
// ScrollContainer (0x0112)
// =============================================================================

export const SCROLLCONTAINER_TAG = 0x0112;
const SCROLLCONTAINER_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);
const SCROLL_ORIENTATIONS = new Set(['vertical', 'horizontal', 'both']);
const SCROLL_SPACINGS = new Set([
  'zero', 'xxs', 'xs', 'sm', 'md', 'lg', 'xl', 'xxl',
]);

// DimensionToken → CSS length. parseDimensionToken returns the kind string
// for unit variants and a ready CSS string for value variants.
function dimensionToCss(raw, ctx) {
  const t = parseDimensionToken(raw, ctx);
  if (t === 'full') return '100%';
  if (t === 'auto') return 'auto';
  if (t === 'fit_content') return 'fit-content';
  return t;
}

export function renderScrollContainer(component, ctx) {
  assertOnlyKnownFields(component.fields, SCROLLCONTAINER_FIELD_KEYS, 'ScrollContainer');

  const orientation = requireEnum(
    ctx.readField(component.fields, 0), SCROLL_ORIENTATIONS, 'ScrollContainer.orientation'
  );
  const heightRaw = ctx.readField(component.fields, 1);
  // Spec §3 0x0112 default: height = {kind:"full"}.
  const heightCss = heightRaw === undefined
    ? '100%' : dimensionToCss(heightRaw, 'ScrollContainer.height');
  const maxHeightRaw = ctx.readField(component.fields, 2);
  const maxHeightCss = maxHeightRaw == null
    ? null : dimensionToCss(maxHeightRaw, 'ScrollContainer.max_height');
  const childrenRaw = ctx.readField(component.fields, 3);
  const children = childrenRaw === undefined ? [] : childrenRaw;
  if (!Array.isArray(children)) {
    throw new TypeError('ScrollContainer.children: expected Array<Component>');
  }
  const stickySlotRaw = ctx.readField(component.fields, 4);
  const stickySlot = stickySlotRaw == null
    ? null : requireString(stickySlotRaw, 'ScrollContainer.sticky_header_slot');
  const virtualize = requireBool(
    ctx.readField(component.fields, 5), 'ScrollContainer.virtualize'
  );
  // Klucz 6 (gap) opcjonalny — gdy ustawiony, kontener staje się flex-kolumną
  // z odstępem między dziećmi (goła lista bez gapu to główny defekt).
  const gapRaw = ctx.readField(component.fields, 6);
  const gap = gapRaw === undefined
    ? null : requireEnum(gapRaw, SCROLL_SPACINGS, 'ScrollContainer.gap');

  const el = document.createElement('div');
  el.classList.add(
    'tf-scroll-container',
    `tf-scroll-container--${orientation}`,
    'tf-scroll'
  );
  if (virtualize) el.classList.add('tf-scroll-container--virtualize');
  if (gap != null) el.classList.add(`tf-scroll-container--gap-${gap}`);
  el.style.height = heightCss;
  if (maxHeightCss != null) el.style.maxHeight = maxHeightCss;
  if (orientation === 'vertical') {
    el.style.overflowY = 'auto';
    el.style.overflowX = 'hidden';
  } else if (orientation === 'horizontal') {
    el.style.overflowX = 'auto';
    el.style.overflowY = 'hidden';
  } else {
    el.style.overflow = 'auto';
  }

  // Optional sticky header — empty slot container the slot manager fills
  // later (same data-slot-id contract as AppShell/overlay renderers).
  if (stickySlot != null) {
    const header = document.createElement('div');
    header.classList.add('tf-scroll-container__header');
    header.setAttribute('data-slot-id', stickySlot);
    header.style.position = 'sticky';
    header.style.top = '0';
    el.appendChild(header);
  }

  for (const child of children) {
    el.appendChild(ctx.renderChild(child));
  }
  return el;
}
