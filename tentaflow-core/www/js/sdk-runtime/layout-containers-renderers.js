// =============================================================================
// Plik: sdk-runtime/layout-containers-renderers.js
// Opis: Rendererzy containerów Layout (Faza 6 Krok 3.3a-2):
//   - Flex    (tag 0x0101) — flexbox container
//   - Grid    (tag 0x0102) — CSS Grid z GridTrack + GridChild
//   - Stack   (tag 0x0103) — Flex column z domyślnymi gap="md", align="stretch"
//   - Cluster (tag 0x0104) — horizontal auto-wrap flow
// Split (0x0105) i ScrollContainer (0x0112) wymagają slot manager — chunk 3.5.
// Spec ref: `tentaflow-sdk-spec/src/protocol/ui/layout/containers.rs`.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';

// =============================================================================
// Token whitelisty (spec §1.5 / §3)
// =============================================================================

const SPACINGS = new Set([
  'zero', 'xxs', 'xs', 'sm', 'md', 'lg', 'xl', 'xxl',
]);
const FLEX_DIRECTIONS = new Set([
  'row', 'row_reverse', 'column', 'column_reverse',
]);
const FLEX_JUSTIFIES = new Set([
  'start', 'end', 'center', 'space_between', 'space_around', 'space_evenly',
]);
const FLEX_ALIGNS = new Set(['start', 'end', 'center', 'baseline', 'stretch']);
const FLEX_WRAPS = new Set(['no_wrap', 'wrap', 'wrap_reverse']);
const BACKGROUND_TOKENS = new Set([
  'none', 'subtle', 'muted', 'accent', 'inverse',
]);
const RADIUS_TOKENS = new Set([
  'none', 'xs', 'sm', 'md', 'lg', 'xl', 'pill', 'circle',
]);

function requireEnum(value, set, ctx) {
  if (typeof value !== 'string' || !set.has(value)) {
    throw new TypeError(
      `${ctx}: expected one of ${[...set].join('/')}, got ${JSON.stringify(value)}`
    );
  }
  return value;
}

function optionalEnum(value, set, ctx) {
  if (value === undefined) return undefined;
  return requireEnum(value, set, ctx);
}

function requireArray(value, ctx) {
  if (!Array.isArray(value)) {
    throw new TypeError(`${ctx}: expected Array, got ${typeof value}`);
  }
  return value;
}

function requireU8(value, ctx) {
  if (typeof value === 'bigint') {
    if (value < 0n || value > 0xFFn) {
      throw new TypeError(`${ctx}: expected u8 (0..=255), got ${value}`);
    }
    return Number(value);
  }
  if (!Number.isInteger(value) || value < 0 || value > 0xFF) {
    throw new TypeError(`${ctx}: expected u8 (0..=255), got ${value}`);
  }
  return value;
}

function requireU32(value, ctx) {
  if (typeof value === 'bigint') {
    if (value < 0n || value > 0xFFFFFFFFn) {
      throw new TypeError(`${ctx}: expected u32, got ${value}`);
    }
    return Number(value);
  }
  if (!Number.isInteger(value) || value < 0 || value > 0xFFFFFFFF) {
    throw new TypeError(`${ctx}: expected u32, got ${value}`);
  }
  return value;
}

/// Odrzuca każdy klucz FieldMap-y, który nie jest w `allowedKeys`. Mirror
/// Rust `unknown_field(...)` — addon wysyłający nieznane pole MUSI dostać
/// błąd zamiast cichego ignorowania.
function assertOnlyKnownFields(fields, allowedKeys, componentName) {
  for (const [k] of fields) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(
        `${componentName}: unknown field key ${k} (allowed: ${[...allowedKeys].join(',')})`
      );
    }
  }
}

/// Sprawdza, że obiekt union-wariantu ma WYŁĄCZNIE klucze z whitelist'y.
/// Mirror Rust per-variant `ensure_extras_absent` w decoderach inline.rs.
function assertOnlyKnownObjectKeys(obj, allowedKeys, ctx) {
  for (const k of Object.keys(obj)) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(`${ctx}: unexpected key '${k}'`);
    }
  }
}

function assertOnlyKnownFieldMapKeys(fields, allowedKeys, ctx) {
  if (!Array.isArray(fields)) throw new TypeError(`${ctx}: expected FieldMap`);
  for (const entry of fields) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry must be [u8, Value]`);
    if (!allowedKeys.has(entry[0])) {
      throw new TypeError(`${ctx}: unexpected key ${entry[0]}`);
    }
  }
}

// Limity strukturalne dla Grid — bez tego addon mógłby wymusić ogromny
// layout (DoS przez setki/tysiące kolumn lub wartość px = u32::MAX).
const MAX_GRID_COLS = 256;
const MAX_GRID_PX = 100_000;

// =============================================================================
// Flex (0x0101)
// =============================================================================

export const FLEX_TAG = 0x0101;

const FLEX_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8]);

function renderFlex(component, ctx) {
  assertOnlyKnownFields(component.fields, FLEX_FIELD_KEYS, 'Flex');
  const direction = requireEnum(
    ctx.readField(component.fields, 0),
    FLEX_DIRECTIONS,
    'Flex.direction'
  );
  // §3 0x0101: gap absent → default "md".
  const gapRaw = ctx.readField(component.fields, 1);
  const gap = gapRaw === undefined ? 'md' : requireEnum(gapRaw, SPACINGS, 'Flex.gap');
  const justify = requireEnum(
    ctx.readField(component.fields, 2),
    FLEX_JUSTIFIES,
    'Flex.justify'
  );
  const align = requireEnum(
    ctx.readField(component.fields, 3),
    FLEX_ALIGNS,
    'Flex.align'
  );
  const wrap = requireEnum(
    ctx.readField(component.fields, 4),
    FLEX_WRAPS,
    'Flex.wrap'
  );
  const childrenRaw = ctx.readField(component.fields, 5);
  const children = childrenRaw === undefined ? [] : requireArray(childrenRaw, 'Flex.children');
  const padding = optionalEnum(
    ctx.readField(component.fields, 6),
    SPACINGS,
    'Flex.padding'
  );
  const background = optionalEnum(
    ctx.readField(component.fields, 7),
    BACKGROUND_TOKENS,
    'Flex.background'
  );
  const radius = optionalEnum(
    ctx.readField(component.fields, 8),
    RADIUS_TOKENS,
    'Flex.radius'
  );

  const el = document.createElement('div');
  el.classList.add('tf-flex');
  el.classList.add(`tf-flex--direction-${direction}`);
  el.classList.add(`tf-flex--gap-${gap}`);
  el.classList.add(`tf-flex--justify-${justify}`);
  el.classList.add(`tf-flex--align-${align}`);
  el.classList.add(`tf-flex--wrap-${wrap}`);
  if (padding) el.classList.add(`tf-flex--padding-${padding}`);
  if (background) el.classList.add(`tf-flex--bg-${background}`);
  if (radius) el.classList.add(`tf-flex--radius-${radius}`);

  for (const childComponent of children) {
    const childEl = ctx.renderChild(childComponent);
    el.appendChild(childEl);
  }
  return el;
}

// =============================================================================
// Grid (0x0102)
// =============================================================================

export const GRID_TAG = 0x0102;

const GRID_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);

function renderGrid(component, ctx) {
  assertOnlyKnownFields(component.fields, GRID_FIELD_KEYS, 'Grid');
  const columnsRaw = ctx.readField(component.fields, 0);
  const columnsCss = gridTrackToCss(columnsRaw);
  const gap = requireEnum(
    ctx.readField(component.fields, 1),
    SPACINGS,
    'Grid.gap'
  );
  const rowGap = optionalEnum(
    ctx.readField(component.fields, 2),
    SPACINGS,
    'Grid.row_gap'
  );
  const columnGap = optionalEnum(
    ctx.readField(component.fields, 3),
    SPACINGS,
    'Grid.column_gap'
  );
  const childrenRaw = ctx.readField(component.fields, 4);
  const children = childrenRaw === undefined ? [] : requireArray(childrenRaw, 'Grid.children');
  const padding = optionalEnum(
    ctx.readField(component.fields, 5),
    SPACINGS,
    'Grid.padding'
  );
  const alignItems = optionalEnum(
    ctx.readField(component.fields, 6),
    FLEX_ALIGNS,
    'Grid.align_items'
  );

  const el = document.createElement('div');
  el.classList.add('tf-grid');
  el.classList.add(`tf-grid--gap-${gap}`);
  if (rowGap) el.classList.add(`tf-grid--row-gap-${rowGap}`);
  if (columnGap) el.classList.add(`tf-grid--col-gap-${columnGap}`);
  if (padding) el.classList.add(`tf-grid--padding-${padding}`);
  if (alignItems) el.classList.add(`tf-grid--align-${alignItems}`);
  // grid-template-columns musi być dynamiczne — semantic tokens nie
  // pokrywają arbitralnego trackingu kolumn. Trzymamy je w inline style
  // jako jedyną dozwoloną drogę; addon nie kontroluje raw CSS poza tym.
  el.style.gridTemplateColumns = columnsCss;

  // GridChild: 0=component, 1=col_span(u8), 2=row_span(u8), 3=col_start(u8), 4=row_start(u8), 5=align_self, 6=justify_self
  const GRID_CHILD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);
  for (const gridChild of children) {
    if (!Array.isArray(gridChild)) {
      throw new TypeError('Grid.children entry must be GridChild FieldMap');
    }
    assertOnlyKnownFieldMapKeys(gridChild, GRID_CHILD_KEYS, 'GridChild');
    const gcComponent = ctx.readField(gridChild, 0);
    if (!gcComponent) {
      throw new TypeError('GridChild.component is required');
    }
    const colSpan = requireU8(ctx.readField(gridChild, 1), 'GridChild.col_span');
    const rowSpan = requireU8(ctx.readField(gridChild, 2), 'GridChild.row_span');
    const childEl = ctx.renderChild(gcComponent);
    childEl.style.gridColumn = `span ${colSpan}`;
    childEl.style.gridRow = `span ${rowSpan}`;
    const gcColStart = ctx.readField(gridChild, 3);
    if (gcColStart != null) {
      const cs = requireU8(gcColStart, 'GridChild.col_start');
      childEl.style.gridColumnStart = String(cs);
    }
    const gcRowStart = ctx.readField(gridChild, 4);
    if (gcRowStart != null) {
      const rs = requireU8(gcRowStart, 'GridChild.row_start');
      childEl.style.gridRowStart = String(rs);
    }
    const gcAlignSelf = ctx.readField(gridChild, 5);
    if (gcAlignSelf != null) {
      const a = requireEnum(gcAlignSelf, FLEX_ALIGNS, 'GridChild.align_self');
      childEl.style.alignSelf = flexAlignToCss(a);
    }
    const gcJustifySelf = ctx.readField(gridChild, 6);
    if (gcJustifySelf != null) {
      const j = requireEnum(gcJustifySelf, FLEX_JUSTIFIES, 'GridChild.justify_self');
      childEl.style.justifySelf = flexJustifyToCss(j);
    }
    el.appendChild(childEl);
  }
  return el;
}

/// Konwertuje `GridTrack` z spec'u (Equal | Explicit) do wartości CSS
/// `grid-template-columns`. Wartość zwracana jest stringiem zwalidowanym
/// — wyłącznie tokeny z whitelist'y `GridCol`, więc bezpieczne do wsadzenia
/// w `style.gridTemplateColumns`.
function gridTrackToCss(track) {
  if (!track || typeof track !== 'object') {
    throw new TypeError('Grid.columns must be GridTrack object');
  }
  if (track.kind === 'equal') {
    assertOnlyKnownObjectKeys(
      track,
      new Set(['kind', 'count']),
      'GridTrack.equal'
    );
    const count = requireU8(track.count, 'GridTrack.equal.count');
    if (count === 0 || count > MAX_GRID_COLS) {
      throw new TypeError(
        `GridTrack.equal.count must be 1..=${MAX_GRID_COLS}, got ${count}`
      );
    }
    return `repeat(${count}, minmax(0, 1fr))`;
  }
  if (track.kind === 'explicit') {
    assertOnlyKnownObjectKeys(
      track,
      new Set(['kind', 'cols']),
      'GridTrack.explicit'
    );
    const cols = requireArray(track.cols, 'GridTrack.explicit.cols');
    if (cols.length === 0 || cols.length > MAX_GRID_COLS) {
      throw new TypeError(
        `GridTrack.explicit.cols length must be 1..=${MAX_GRID_COLS}, got ${cols.length}`
      );
    }
    const parts = cols.map((col, i) => gridColToCss(col, `GridTrack.explicit.cols[${i}]`));
    return parts.join(' ');
  }
  throw new TypeError(`GridTrack.kind must be 'equal' or 'explicit', got ${track.kind}`);
}

function gridColToCss(col, ctx) {
  if (!col || typeof col !== 'object') {
    throw new TypeError(`${ctx}: GridCol must be object`);
  }
  // Każdy wariant `GridCol` ma EXACTLY ten zestaw kluczy. Mirror Rust
  // per-variant decoders w `inline.rs` GridCol::decode().
  switch (col.kind) {
    case 'auto':
      assertOnlyKnownObjectKeys(col, new Set(['kind']), `${ctx}.auto`);
      return 'auto';
    case 'fill':
      assertOnlyKnownObjectKeys(col, new Set(['kind']), `${ctx}.fill`);
      return 'minmax(0, 1fr)';
    case 'min_content':
      assertOnlyKnownObjectKeys(col, new Set(['kind']), `${ctx}.min_content`);
      return 'min-content';
    case 'max_content':
      assertOnlyKnownObjectKeys(col, new Set(['kind']), `${ctx}.max_content`);
      return 'max-content';
    case 'fr': {
      assertOnlyKnownObjectKeys(col, new Set(['kind', 'value']), `${ctx}.fr`);
      const v = requireU8(col.value, `${ctx}.fr.value`);
      if (v === 0) throw new TypeError(`${ctx}.fr.value must be > 0`);
      return `${v}fr`;
    }
    case 'px': {
      assertOnlyKnownObjectKeys(col, new Set(['kind', 'value']), `${ctx}.px`);
      const v = requireU32(col.value, `${ctx}.px.value`);
      if (v > MAX_GRID_PX) {
        throw new TypeError(`${ctx}.px.value exceeds MAX_GRID_PX (${MAX_GRID_PX})`);
      }
      return `${v}px`;
    }
    default:
      throw new TypeError(`${ctx}.kind unsupported: ${col.kind}`);
  }
}

function flexAlignToCss(token) {
  // FlexAlign tokens mapowane 1:1 do CSS keyword'ów.
  return token;
}

function flexJustifyToCss(token) {
  // FlexJustify: space_between → space-between itd.
  return token.replace(/_/g, '-');
}

// =============================================================================
// Stack (0x0103)
// =============================================================================

export const STACK_TAG = 0x0103;

const STACK_FIELD_KEYS = new Set([0, 1, 2, 3]);

function renderStack(component, ctx) {
  assertOnlyKnownFields(component.fields, STACK_FIELD_KEYS, 'Stack');
  // §3 0x0103: gap defaultuje na "md", align na "stretch" (spec encoder
  // pomija pole jeśli wartość = default; decoder go materializuje).
  const gapRaw = ctx.readField(component.fields, 0);
  const gap = gapRaw === undefined ? 'md' : requireEnum(gapRaw, SPACINGS, 'Stack.gap');
  const alignRaw = ctx.readField(component.fields, 1);
  const align = alignRaw === undefined ? 'stretch' : requireEnum(alignRaw, FLEX_ALIGNS, 'Stack.align');
  const childrenRaw = ctx.readField(component.fields, 2);
  const children = childrenRaw === undefined ? [] : requireArray(childrenRaw, 'Stack.children');
  const padding = optionalEnum(
    ctx.readField(component.fields, 3),
    SPACINGS,
    'Stack.padding'
  );

  const el = document.createElement('div');
  el.classList.add('tf-stack');
  el.classList.add(`tf-stack--gap-${gap}`);
  el.classList.add(`tf-stack--align-${align}`);
  if (padding) el.classList.add(`tf-stack--padding-${padding}`);

  for (const childComponent of children) {
    el.appendChild(ctx.renderChild(childComponent));
  }
  return el;
}

// =============================================================================
// Cluster (0x0104)
// =============================================================================

export const CLUSTER_TAG = 0x0104;

const CLUSTER_FIELD_KEYS = new Set([0, 1, 2, 3]);

function renderCluster(component, ctx) {
  assertOnlyKnownFields(component.fields, CLUSTER_FIELD_KEYS, 'Cluster');
  const gap = requireEnum(
    ctx.readField(component.fields, 0),
    SPACINGS,
    'Cluster.gap'
  );
  const align = requireEnum(
    ctx.readField(component.fields, 1),
    FLEX_ALIGNS,
    'Cluster.align'
  );
  const justify = requireEnum(
    ctx.readField(component.fields, 2),
    FLEX_JUSTIFIES,
    'Cluster.justify'
  );
  const childrenRaw = ctx.readField(component.fields, 3);
  const children = childrenRaw === undefined ? [] : requireArray(childrenRaw, 'Cluster.children');

  const el = document.createElement('div');
  el.classList.add('tf-cluster');
  el.classList.add(`tf-cluster--gap-${gap}`);
  el.classList.add(`tf-cluster--align-${align}`);
  el.classList.add(`tf-cluster--justify-${justify}`);

  for (const childComponent of children) {
    el.appendChild(ctx.renderChild(childComponent));
  }
  return el;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerLayoutContainersRenderers() {
  if (!lookupComponentRenderer(FLEX_TAG)) registerComponentRenderer(FLEX_TAG, renderFlex);
  if (!lookupComponentRenderer(GRID_TAG)) registerComponentRenderer(GRID_TAG, renderGrid);
  if (!lookupComponentRenderer(STACK_TAG)) registerComponentRenderer(STACK_TAG, renderStack);
  if (!lookupComponentRenderer(CLUSTER_TAG)) registerComponentRenderer(CLUSTER_TAG, renderCluster);
}
