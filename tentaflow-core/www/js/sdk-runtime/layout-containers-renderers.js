// =============================================================================
// Plik: sdk-runtime/layout-containers-renderers.js
// Opis: Rendererzy containerów Layout (Faza 6 Krok 3.3a-2):
//   - Flex    (tag 0x0101) — flexbox container
//   - Grid    (tag 0x0102) — CSS Grid z GridTrack + GridChild
//   - Stack   (tag 0x0103) — Flex column z domyślnymi gap="md", align="stretch"
//   - Cluster (tag 0x0104) — horizontal auto-wrap flow
//   - Split   (tag 0x0105) — 2-pane split with optional resizable divider
// ScrollContainer (0x0112) lives in form-search-scroll-renderer.js.
// Spec ref: `tentaflow-sdk-spec/src/protocol/ui/layout/containers.rs`.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
  injectResponsiveCss,
} from './component-renderer.js';
import { parseDimensionToken } from './data-specialised-renderer.js';

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
  // Mirror of Rust `ensure_no_duplicate_keys` — a duplicate key would
  // silently resolve first-wins in readField, so reject it outright.
  const seen = new Set();
  for (const entry of fields) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry must be [u8, Value]`);
    if (!allowedKeys.has(entry[0])) {
      throw new TypeError(`${ctx}: unexpected key ${entry[0]}`);
    }
    if (seen.has(entry[0])) {
      throw new TypeError(`${ctx}: duplicate key ${entry[0]}`);
    }
    seen.add(entry[0]);
  }
}

// Limity strukturalne dla Grid — bez tego addon mógłby wymusić ogromny
// layout (DoS przez setki/tysiące kolumn lub wartość px = u32::MAX).
const MAX_GRID_COLS = 256;
const MAX_GRID_PX = 100_000;

// =============================================================================
// BoxStyle (spec §1.5) — shared container styling (margin/padding/border/
// background/radius/dimensions/overflow). Px values go to inline style
// (controlled fields, not free CSS); tokens map to `var(--tf-*)` variables.
// =============================================================================

const OVERFLOWS = new Set(['visible', 'hidden', 'auto', 'scroll']);
const BORDER_LINE_STYLES = new Set(['solid', 'dashed', 'none']);
// BorderColor → theme CSS var. Mirror of Rust enum `BorderColor` in tokens.rs.
const BORDER_COLOR_CSS = {
  default: 'var(--tf-border)',
  hover: 'var(--tf-border-hover)',
  accent: 'var(--tf-accent-1)',
  success: 'var(--tf-success)',
  warning: 'var(--tf-warning)',
  danger: 'var(--tf-danger)',
  transparent: 'transparent',
};

function requireU16(value, ctx) {
  if (typeof value === 'bigint') {
    if (value < 0n || value > 0xFFFFn) {
      throw new TypeError(`${ctx}: expected u16 (0..=65535), got ${value}`);
    }
    return Number(value);
  }
  if (!Number.isInteger(value) || value < 0 || value > 0xFFFF) {
    throw new TypeError(`${ctx}: expected u16 (0..=65535), got ${value}`);
  }
  return value;
}

/// SpaceValue / RadiusValue: `{kind:"token", value:<tstr>}` | `{kind:"px", value:u16}`.
/// `tokenSet` is the token whitelist, `cssVarPrefix` e.g. 'tf-space'.
function spaceLikeToCss(raw, tokenSet, cssVarPrefix, ctx) {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) {
    throw new TypeError(`${ctx}: expected {kind, value} object`);
  }
  assertOnlyKnownObjectKeys(raw, new Set(['kind', 'value']), ctx);
  switch (raw.kind) {
    case 'token': {
      const t = requireEnum(raw.value, tokenSet, `${ctx}.token`);
      return `var(--${cssVarPrefix}-${t})`;
    }
    case 'px': {
      const v = requireU16(raw.value, `${ctx}.px.value`);
      return `${v}px`;
    }
    default:
      throw new TypeError(`${ctx}.kind must be 'token'/'px', got ${raw.kind}`);
  }
}

// EdgeValues / BorderEdges edges and CornerValues corners are int-keyed
// FieldMaps: 0=top/top_left, 1=right/top_right, 2=bottom/bottom_right,
// 3=left/bottom_left. Mirror of Rust `#[cbor(map)]` in inline.rs.
const EDGE_KEYS = new Set([0, 1, 2, 3]);

function applyEdgeValues(el, raw, cssProp, ctx) {
  assertOnlyKnownFieldMapKeys(raw, EDGE_KEYS, ctx);
  const sides = ['Top', 'Right', 'Bottom', 'Left'];
  for (let i = 0; i < 4; i += 1) {
    const v = readFieldMap(raw, i);
    if (v === undefined) continue;
    el.style[`${cssProp}${sides[i]}`] =
      spaceLikeToCss(v, SPACINGS, 'tf-space', `${ctx}.${sides[i].toLowerCase()}`);
  }
}

// BorderSide: 0=width_px(u8), 1=color(BorderColor), 2=style(BorderLineStyle).
const BORDER_SIDE_KEYS = new Set([0, 1, 2]);

// Longhands instead of the `borderTop` shorthand — precise and independent
// of shorthand-parser behavior in the test environment.
function applyBorderSide(el, raw, side, ctx) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: BorderSide must be FieldMap`);
  assertOnlyKnownFieldMapKeys(raw, BORDER_SIDE_KEYS, ctx);
  const width = requireU8(readFieldMap(raw, 0), `${ctx}.width_px`);
  const colorToken = readFieldMap(raw, 1);
  if (typeof colorToken !== 'string' || !(colorToken in BORDER_COLOR_CSS)) {
    throw new TypeError(`${ctx}.color: unknown BorderColor ${JSON.stringify(colorToken)}`);
  }
  const style = requireEnum(readFieldMap(raw, 2), BORDER_LINE_STYLES, `${ctx}.style`);
  if (style === 'none') {
    el.style[`border${side}Style`] = 'none';
    return;
  }
  el.style[`border${side}Width`] = `${width}px`;
  el.style[`border${side}Style`] = style;
  el.style[`border${side}Color`] = BORDER_COLOR_CSS[colorToken];
}

function applyBorderEdges(el, raw, ctx) {
  assertOnlyKnownFieldMapKeys(raw, EDGE_KEYS, ctx);
  const sides = ['Top', 'Right', 'Bottom', 'Left'];
  for (let i = 0; i < 4; i += 1) {
    const v = readFieldMap(raw, i);
    if (v === undefined) continue;
    applyBorderSide(el, v, sides[i], `${ctx}.${sides[i].toLowerCase()}`);
  }
}

function applyCornerValues(el, raw, ctx) {
  assertOnlyKnownFieldMapKeys(raw, EDGE_KEYS, ctx);
  const corners = ['TopLeft', 'TopRight', 'BottomRight', 'BottomLeft'];
  for (let i = 0; i < 4; i += 1) {
    const v = readFieldMap(raw, i);
    if (v === undefined) continue;
    el.style[`border${corners[i]}Radius`] =
      spaceLikeToCss(v, RADIUS_TOKENS, 'tf-radius', `${ctx}.${corners[i]}`);
  }
}

/// Local reader for an int-keyed FieldMap (pair-array [[u8, Value], ...]) —
/// like ctx.readField, but also usable for nested BoxStyle structures.
function readFieldMap(fields, key) {
  for (const [k, v] of fields) {
    if (k === key) return v;
  }
  return undefined;
}

const BOX_STYLE_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13]);

// ShadowToken (spec tokens.rs) → box-shadow. Elevation scale reuses the shared
// theme shadow tokens; `accent_glow` maps to the accent halo (as used by the
// translation output pane / live stage). `none` clears any inherited shadow.
const SHADOW_TOKEN_CSS = {
  none: 'none',
  subtle: 'var(--tf-shadow-sm)',
  medium: 'var(--tf-shadow)',
  elevated: 'var(--tf-shadow-lg)',
  floating: 'var(--tf-shadow-xl)',
  accent_glow: 'var(--tf-glow-accent)',
};

/// Applies BoxStyle (spec §1.5) to an element. `raw` is an int-keyed FieldMap:
/// 0=margin, 1=padding, 2=border, 3=background, 4=radius, 5=width, 6=height,
/// 7=min_width, 8=min_height, 9=max_width, 10=max_height, 11=overflow_x,
/// 12=overflow_y. Called AFTER the container token classes — inline style
/// overrides classes per the CSS cascade.
export function applyBoxStyle(el, raw, ctx) {
  if (raw === undefined) return;
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: BoxStyle must be FieldMap`);
  assertOnlyKnownFieldMapKeys(raw, BOX_STYLE_KEYS, ctx);

  const margin = readFieldMap(raw, 0);
  if (margin !== undefined) applyEdgeValues(el, margin, 'margin', `${ctx}.margin`);
  const padding = readFieldMap(raw, 1);
  if (padding !== undefined) applyEdgeValues(el, padding, 'padding', `${ctx}.padding`);
  const border = readFieldMap(raw, 2);
  if (border !== undefined) applyBorderEdges(el, border, `${ctx}.border`);
  const background = readFieldMap(raw, 3);
  if (background !== undefined) {
    const b = requireEnum(background, BACKGROUND_TOKENS, `${ctx}.background`);
    el.style.background = `var(--tf-bg-${b})`;
  }
  const radius = readFieldMap(raw, 4);
  if (radius !== undefined) applyCornerValues(el, radius, `${ctx}.radius`);

  const dims = [
    [5, 'width'], [6, 'height'], [7, 'minWidth'], [8, 'minHeight'],
    [9, 'maxWidth'], [10, 'maxHeight'],
  ];
  for (const [key, prop] of dims) {
    const v = readFieldMap(raw, key);
    if (v === undefined) continue;
    const t = parseDimensionToken(v, `${ctx}.${prop}`);
    el.style[prop] = t === 'full' ? '100%' : t === 'fit_content' ? 'fit-content' : t;
  }

  const overflowX = readFieldMap(raw, 11);
  if (overflowX !== undefined) {
    el.style.overflowX = requireEnum(overflowX, OVERFLOWS, `${ctx}.overflow_x`);
  }
  const overflowY = readFieldMap(raw, 12);
  if (overflowY !== undefined) {
    el.style.overflowY = requireEnum(overflowY, OVERFLOWS, `${ctx}.overflow_y`);
  }
  const shadow = readFieldMap(raw, 13);
  if (shadow !== undefined) {
    if (typeof shadow !== 'string' || !(shadow in SHADOW_TOKEN_CSS)) {
      throw new TypeError(`${ctx}.shadow: unknown ShadowToken ${JSON.stringify(shadow)}`);
    }
    el.style.boxShadow = SHADOW_TOKEN_CSS[shadow];
  }
}

// =============================================================================
// ResponsiveRule (spec inline.rs) — container-query driven layout adaptation.
// A container declares a list of overrides keyed by a `max_width`; the renderer
// generates `@container addon (max-width: Npx)` rules scoped by a stable
// `data-responsive="<hash>"` attribute set on the element. No per-addon CSS —
// the same declaration renders identically for every addon.
// =============================================================================

// Breakpoint token → px. Mirrors the mapping documented on `Breakpoint`
// (tokens.rs): 640/768/1024/1280/1536/1920.
const BREAKPOINT_PX = { xs: 640, sm: 768, md: 1024, lg: 1280, xl: 1536, xxl: 1920 };

const RESPONSIVE_RULE_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8]);

function requireI32(value, ctx) {
  if (typeof value === 'bigint') {
    if (value < -2147483648n || value > 2147483647n) {
      throw new TypeError(`${ctx}: expected i32, got ${value}`);
    }
    return Number(value);
  }
  if (!Number.isInteger(value) || value < -2147483648 || value > 2147483647) {
    throw new TypeError(`${ctx}: expected i32, got ${value}`);
  }
  return value;
}

/// FNV-1a 32-bit hex — a compact, stable content hash for the generated rules
/// so identical declarations dedup to one injected `@container` block.
function fnv1aHex(str) {
  let h = 0x811c9dc5;
  for (let i = 0; i < str.length; i += 1) {
    h ^= str.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return (h >>> 0).toString(16).padStart(8, '0');
}

/// ContainerWidth `{kind:'token'|'px', value}` → px number. Token maps through
/// the Breakpoint scale; px is taken verbatim (u16).
function containerWidthToPx(raw, ctx) {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) {
    throw new TypeError(`${ctx}: ContainerWidth must be {kind, value}`);
  }
  assertOnlyKnownObjectKeys(raw, new Set(['kind', 'value']), ctx);
  switch (raw.kind) {
    case 'token': {
      if (typeof raw.value !== 'string' || !(raw.value in BREAKPOINT_PX)) {
        throw new TypeError(`${ctx}.token: unknown Breakpoint ${JSON.stringify(raw.value)}`);
      }
      return BREAKPOINT_PX[raw.value];
    }
    case 'px':
      return requireU16(raw.value, `${ctx}.px`);
    default:
      throw new TypeError(`${ctx}.kind must be 'token'/'px', got ${raw.kind}`);
  }
}

/// Applies a container's `responsive: Vec<ResponsiveRule>` by generating scoped
/// `@container addon` rules. `rulesRaw` is an Array of int-keyed FieldMaps:
/// 0=max_width(ContainerWidth), 1=direction, 2=gap, 3=align, 4=justify,
/// 5=padding(EdgeValues), 6=min_height(DimensionToken), 7=order(i32), 8=hidden.
/// `direction`/`gap`/`align`/`justify`/`padding`/`min_height` retarget the
/// container's own flex layout at that width; `order`/`hidden` reposition or
/// hide the container within ITS parent (self-as-flex-child), so all fields
/// target the same `[data-responsive]` element.
export function applyResponsive(el, rulesRaw, ctx, name) {
  if (rulesRaw === undefined || rulesRaw === null) return;
  if (!Array.isArray(rulesRaw)) {
    throw new TypeError(`${name}.responsive: expected Array<ResponsiveRule>`);
  }
  if (rulesRaw.length === 0) return;

  const decls = [];
  for (let i = 0; i < rulesRaw.length; i += 1) {
    const rule = rulesRaw[i];
    const rctx = `${name}.responsive[${i}]`;
    if (!Array.isArray(rule)) throw new TypeError(`${rctx}: ResponsiveRule must be FieldMap`);
    assertOnlyKnownFieldMapKeys(rule, RESPONSIVE_RULE_KEYS, rctx);
    const maxWidthRaw = readFieldMap(rule, 0);
    if (maxWidthRaw === undefined) throw new TypeError(`${rctx}.max_width is required`);
    const maxPx = containerWidthToPx(maxWidthRaw, `${rctx}.max_width`);

    const props = [];
    const dir = readFieldMap(rule, 1);
    if (dir !== undefined) {
      props.push(['flex-direction', requireEnum(dir, FLEX_DIRECTIONS, `${rctx}.direction`).replace(/_/g, '-')]);
    }
    const gap = readFieldMap(rule, 2);
    if (gap !== undefined) {
      props.push(['gap', `var(--tf-space-${requireEnum(gap, SPACINGS, `${rctx}.gap`)})`]);
    }
    const align = readFieldMap(rule, 3);
    if (align !== undefined) {
      props.push(['align-items', flexAlignToCss(requireEnum(align, FLEX_ALIGNS, `${rctx}.align`))]);
    }
    const justify = readFieldMap(rule, 4);
    if (justify !== undefined) {
      props.push(['justify-content', flexJustifyToCss(requireEnum(justify, FLEX_JUSTIFIES, `${rctx}.justify`))]);
    }
    const padding = readFieldMap(rule, 5);
    if (padding !== undefined) {
      assertOnlyKnownFieldMapKeys(padding, EDGE_KEYS, `${rctx}.padding`);
      const sides = [['top', 0], ['right', 1], ['bottom', 2], ['left', 3]];
      for (const [sideName, k] of sides) {
        const sv = readFieldMap(padding, k);
        if (sv === undefined) continue;
        props.push([`padding-${sideName}`, spaceLikeToCss(sv, SPACINGS, 'tf-space', `${rctx}.padding.${sideName}`)]);
      }
    }
    const minHeight = readFieldMap(rule, 6);
    if (minHeight !== undefined) {
      const t = parseDimensionToken(minHeight, `${rctx}.min_height`);
      props.push(['min-height', t === 'full' ? '100%' : t === 'fit_content' ? 'fit-content' : t]);
    }
    const order = readFieldMap(rule, 7);
    if (order !== undefined) {
      props.push(['order', String(requireI32(order, `${rctx}.order`))]);
    }
    const hidden = readFieldMap(rule, 8);
    if (hidden !== undefined) {
      if (typeof hidden !== 'boolean') throw new TypeError(`${rctx}.hidden: expected bool`);
      if (hidden) props.push(['display', 'none']);
    }
    if (props.length === 0) continue; // max_width only — nothing to override
    decls.push({ maxPx, props });
  }
  if (decls.length === 0) return;

  // Hash from a canonical ascending order — deterministic and independent of
  // author order, so identical declarations always dedup to one injection.
  const ascending = [...decls].sort((a, b) => a.maxPx - b.maxPx);
  const canonical = ascending
    .map((d) => `${d.maxPx}{${d.props.map(([k, v]) => `${k}:${v}`).join(';')}}`)
    .join('|');
  const hash = fnv1aHex(canonical);
  el.setAttribute('data-responsive', hash);

  // Emit blocks in DESCENDING max_width order: at a narrow width several
  // `@container (max-width: N)` blocks match at once, and equal-specificity
  // rules resolve by source order (later wins). Putting the smaller breakpoint
  // LAST lets it override the wider one for the same property.
  //
  // Every property is `!important`: the base layout is applied as INLINE style
  // (e.g. `el.style.flexDirection`, `el.style.background`) by the container
  // renderers, and inline style beats any stylesheet rule — including
  // `@container` — without `!important`. This is the intended, deliberate
  // responsive override of the deliberately-inline base.
  const descending = [...decls].sort((a, b) => b.maxPx - a.maxPx);
  const sel = `[data-responsive="${hash}"]`;
  const cssText = `${descending
    .map((d) => `@container addon (max-width: ${d.maxPx}px){${sel}{${d.props
      .map(([k, v]) => `${k}:${v} !important`)
      .join(';')}}}`)
    .join('\n')}\n`;
  injectResponsiveCss(hash, cssText);
}

// =============================================================================
// Flex (0x0101)
// =============================================================================

export const FLEX_TAG = 0x0101;

const FLEX_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);

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
  applyBoxStyle(el, ctx.readField(component.fields, 9), 'Flex.style');
  applyResponsive(el, ctx.readField(component.fields, 10), ctx, 'Flex');

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

const GRID_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7]);

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
  applyBoxStyle(el, ctx.readField(component.fields, 7), 'Grid.style');

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

const STACK_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);

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
  // Klucz 4 (justify) opcjonalny — główna (pionowa) oś rozkładu dzieci.
  const justify = optionalEnum(
    ctx.readField(component.fields, 4),
    FLEX_JUSTIFIES,
    'Stack.justify'
  );

  const el = document.createElement('div');
  el.classList.add('tf-stack');
  el.classList.add(`tf-stack--gap-${gap}`);
  el.classList.add(`tf-stack--align-${align}`);
  if (padding) el.classList.add(`tf-stack--padding-${padding}`);
  if (justify) el.classList.add(`tf-stack--justify-${justify}`);
  applyBoxStyle(el, ctx.readField(component.fields, 5), 'Stack.style');
  applyResponsive(el, ctx.readField(component.fields, 6), ctx, 'Stack');

  for (const childComponent of children) {
    el.appendChild(ctx.renderChild(childComponent));
  }
  return el;
}

// =============================================================================
// Cluster (0x0104)
// =============================================================================

export const CLUSTER_TAG = 0x0104;

const CLUSTER_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

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
  // Klucz 4 (wrap) opcjonalny — undefined/true zachowuje domyślne zawijanie,
  // false wymusza jeden rząd (np. badge bez zawijania do nowej linii).
  const wrapRaw = ctx.readField(component.fields, 4);
  let wrap = true;
  if (wrapRaw !== undefined) {
    if (typeof wrapRaw !== 'boolean') {
      throw new TypeError(`Cluster.wrap: expected bool, got ${typeof wrapRaw}`);
    }
    wrap = wrapRaw;
  }

  const el = document.createElement('div');
  el.classList.add('tf-cluster');
  el.classList.add(`tf-cluster--gap-${gap}`);
  el.classList.add(`tf-cluster--align-${align}`);
  el.classList.add(`tf-cluster--justify-${justify}`);
  if (!wrap) el.classList.add('tf-cluster--nowrap');

  for (const childComponent of children) {
    el.appendChild(ctx.renderChild(childComponent));
  }
  return el;
}

// =============================================================================
// Split (0x0105)
// =============================================================================

export const SPLIT_TAG = 0x0105;

const SPLIT_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);
const SPLIT_ORIENTATIONS = new Set(['horizontal', 'vertical']);

function requireBool(value, ctx) {
  if (typeof value !== 'boolean') {
    throw new TypeError(`${ctx}: expected bool, got ${typeof value}`);
  }
  return value;
}

function requireNonEmptyString(value, ctx) {
  if (typeof value !== 'string' || value.length === 0) {
    throw new TypeError(`${ctx}: expected non-empty string, got ${JSON.stringify(value)}`);
  }
  return value;
}

/// Converts a `SplitSize` discriminated union (auto / px / percent) to a CSS
/// flex-basis value. Mirrors per-variant key whitelists from the Rust decoder
/// in `inline.rs` (SplitSize::decode), incl. the finite 0.0..=100.0 percent
/// range enforced on the wire.
function splitSizeToBasis(size, ctx) {
  if (!size || typeof size !== 'object' || Array.isArray(size)) {
    throw new TypeError(`${ctx}: SplitSize must be object`);
  }
  switch (size.kind) {
    case 'auto':
      assertOnlyKnownObjectKeys(size, new Set(['kind']), `${ctx}.auto`);
      return 'auto';
    case 'px': {
      assertOnlyKnownObjectKeys(size, new Set(['kind', 'value']), `${ctx}.px`);
      const v = requireU32(size.value, `${ctx}.px.value`);
      return `${v}px`;
    }
    case 'percent': {
      assertOnlyKnownObjectKeys(size, new Set(['kind', 'value']), `${ctx}.percent`);
      const v = size.value;
      if (typeof v !== 'number' || !Number.isFinite(v) || v < 0 || v > 100) {
        throw new TypeError(`${ctx}.percent.value must be finite 0.0..=100.0, got ${v}`);
      }
      return `${v}%`;
    }
    default:
      throw new TypeError(`${ctx}.kind must be 'auto'/'px'/'percent', got ${size.kind}`);
  }
}

export function renderSplit(component, ctx) {
  assertOnlyKnownFields(component.fields, SPLIT_FIELD_KEYS, 'Split');
  const orientation = requireEnum(
    ctx.readField(component.fields, 0),
    SPLIT_ORIENTATIONS,
    'Split.orientation'
  );
  const primaryBasis = splitSizeToBasis(
    ctx.readField(component.fields, 1),
    'Split.primary_size'
  );
  const minPrimary = requireU32(ctx.readField(component.fields, 2), 'Split.min_primary');
  const maxPrimary = requireU32(ctx.readField(component.fields, 3), 'Split.max_primary');
  if (minPrimary > maxPrimary) {
    throw new TypeError(
      `Split: min_primary (${minPrimary}) must be <= max_primary (${maxPrimary})`
    );
  }
  const resizable = requireBool(ctx.readField(component.fields, 4), 'Split.resizable');
  const primarySlot = requireNonEmptyString(
    ctx.readField(component.fields, 5),
    'Split.primary_slot'
  );
  const secondarySlot = requireNonEmptyString(
    ctx.readField(component.fields, 6),
    'Split.secondary_slot'
  );

  const horizontal = orientation === 'horizontal';
  const el = document.createElement('div');
  el.classList.add('tf-split');
  el.classList.add(`tf-split--${orientation}`);
  if (resizable) el.classList.add('tf-split--resizable');

  const primary = document.createElement('div');
  primary.classList.add('tf-split__pane', 'tf-split__pane--primary');
  primary.setAttribute('data-slot-id', primarySlot);
  // Data-driven sizing only goes inline: flex-basis from SplitSize and the
  // px clamp from min/max_primary. Structural flex layout comes from the
  // .tf-split* classes.
  primary.style.flexBasis = primaryBasis;
  if (horizontal) {
    primary.style.minWidth = `${minPrimary}px`;
    primary.style.maxWidth = `${maxPrimary}px`;
  } else {
    primary.style.minHeight = `${minPrimary}px`;
    primary.style.maxHeight = `${maxPrimary}px`;
  }

  const divider = document.createElement('div');
  divider.classList.add('tf-split__divider');
  divider.setAttribute('role', 'separator');
  // ARIA orientation describes the divider line itself: a horizontal split
  // (panes side by side) has a vertical divider, and vice versa.
  divider.setAttribute('aria-orientation', horizontal ? 'vertical' : 'horizontal');

  const secondary = document.createElement('div');
  secondary.classList.add('tf-split__pane', 'tf-split__pane--secondary');
  secondary.setAttribute('data-slot-id', secondarySlot);

  if (resizable) {
    // Pointer-drag resize: move/up listeners live on document so the drag
    // survives the pointer leaving the divider; both are released via
    // ctx.registerCleanup when the element is destroyed.
    let drag = null;
    const onPointerDown = (e) => {
      const rect = primary.getBoundingClientRect();
      drag = {
        startCoord: horizontal ? e.clientX : e.clientY,
        startSize: horizontal ? rect.width : rect.height,
      };
      if (typeof divider.setPointerCapture === 'function' && e.pointerId != null) {
        try { divider.setPointerCapture(e.pointerId); } catch {}
      }
      if (typeof e.preventDefault === 'function') e.preventDefault();
    };
    const onPointerMove = (e) => {
      if (!drag) return;
      const coord = horizontal ? e.clientX : e.clientY;
      const next = drag.startSize + (coord - drag.startCoord);
      const clamped = Math.min(maxPrimary, Math.max(minPrimary, next));
      primary.style.flexBasis = `${Math.round(clamped)}px`;
    };
    const onPointerUp = () => {
      drag = null;
    };
    divider.addEventListener('pointerdown', onPointerDown);
    document.addEventListener('pointermove', onPointerMove);
    document.addEventListener('pointerup', onPointerUp);
    document.addEventListener('pointercancel', onPointerUp);
    ctx.registerCleanup(() => {
      divider.removeEventListener('pointerdown', onPointerDown);
      document.removeEventListener('pointermove', onPointerMove);
      document.removeEventListener('pointerup', onPointerUp);
      document.removeEventListener('pointercancel', onPointerUp);
    });
  }

  el.appendChild(primary);
  el.appendChild(divider);
  el.appendChild(secondary);
  return el;
}

// =============================================================================
// Box (0x0115)
// =============================================================================

export const BOX_TAG = 0x0115;

const BOX_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);

// DimensionToken → CSS length. parseDimensionToken zwraca string-kind dla
// wariantów jednostkowych (auto/full/fit_content) i gotowy CSS dla wartości.
function boxDimensionToCss(raw, ctx) {
  const t = parseDimensionToken(raw, ctx);
  if (t === 'full') return '100%';
  if (t === 'auto') return 'auto';
  if (t === 'fit_content') return 'fit-content';
  return t;
}

function renderBox(component, ctx) {
  assertOnlyKnownFields(component.fields, BOX_FIELD_KEYS, 'Box');
  // Wszystkie pola opcjonalne — pusty Box to przezroczysty div.
  const widthRaw = ctx.readField(component.fields, 0);
  const widthCss = widthRaw === undefined
    ? null : boxDimensionToCss(widthRaw, 'Box.width');
  const growRaw = ctx.readField(component.fields, 1);
  let grow = false;
  if (growRaw !== undefined) {
    if (typeof growRaw !== 'boolean') {
      throw new TypeError(`Box.grow: expected bool, got ${typeof growRaw}`);
    }
    grow = growRaw;
  }
  const alignSelf = optionalEnum(
    ctx.readField(component.fields, 2),
    FLEX_ALIGNS,
    'Box.align_self'
  );
  const padding = optionalEnum(
    ctx.readField(component.fields, 3),
    SPACINGS,
    'Box.padding'
  );
  const margin = optionalEnum(
    ctx.readField(component.fields, 4),
    SPACINGS,
    'Box.margin'
  );
  const childrenRaw = ctx.readField(component.fields, 5);
  const children = childrenRaw === undefined ? [] : requireArray(childrenRaw, 'Box.children');
  // Keys 7-10: simple flex behavior for children — any of them enables
  // display:flex (mirror of Rust Box.direction/gap/align/justify).
  const direction = optionalEnum(
    ctx.readField(component.fields, 7),
    FLEX_DIRECTIONS,
    'Box.direction'
  );
  const gap = optionalEnum(
    ctx.readField(component.fields, 8),
    SPACINGS,
    'Box.gap'
  );
  const align = optionalEnum(
    ctx.readField(component.fields, 9),
    FLEX_ALIGNS,
    'Box.align'
  );
  const justify = optionalEnum(
    ctx.readField(component.fields, 10),
    FLEX_JUSTIFIES,
    'Box.justify'
  );

  const el = document.createElement('div');
  el.classList.add('tf-box');
  if (widthCss != null) el.style.width = widthCss;
  // grow=true → `flex: 1 1 0` (grow + basis 0), not just flex-grow. With the
  // default `flex-basis: auto`, siblings size from their content first, so two
  // grow children with different content end up unequal. Basis 0 makes them
  // split the free space equally regardless of content (design-system "fill").
  if (grow) { el.style.flexGrow = '1'; el.style.flexBasis = '0'; }
  if (alignSelf) el.style.alignSelf = flexAlignToCss(alignSelf);
  if (padding) el.classList.add(`tf-box--padding-${padding}`);
  if (margin) el.classList.add(`tf-box--margin-${margin}`);
  if (direction || gap || align || justify) {
    el.style.display = 'flex';
    el.style.flexDirection = direction ? direction.replace(/_/g, '-') : 'row';
    if (gap) el.style.gap = `var(--tf-space-${gap})`;
    if (align) el.style.alignItems = flexAlignToCss(align);
    if (justify) el.style.justifyContent = flexJustifyToCss(justify);
  }
  applyBoxStyle(el, ctx.readField(component.fields, 6), 'Box.style');
  applyResponsive(el, ctx.readField(component.fields, 11), ctx, 'Box');

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
  if (!lookupComponentRenderer(SPLIT_TAG)) registerComponentRenderer(SPLIT_TAG, renderSplit);
  if (!lookupComponentRenderer(BOX_TAG)) registerComponentRenderer(BOX_TAG, renderBox);
}
