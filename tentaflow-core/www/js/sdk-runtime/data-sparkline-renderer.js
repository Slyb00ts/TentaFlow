// =============================================================================
// File: sdk-runtime/data-sparkline-renderer.js
// Description: Renderer Sparkline (0x0215) using <tf-sparkline> web component.
//              Maps data_path, variant, tone to tf-sparkline properties:
//              .points, .color, .fill, .showDots, .height.
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/charts.rs Sparkline.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef } from './bind-resolver.js';

const SPARKLINE_VARIANTS = new Set(['line', 'area', 'bar']);
const TONES = new Set(['neutral', 'primary', 'success', 'warning', 'critical', 'info', 'muted']);

// Map SDK tone to tf-sparkline color role names
const TONE_TO_COLOR = {
  neutral: 'primary',
  primary: 'primary',
  success: 'success',
  warning: 'warning',
  critical: 'danger',
  info: 'info',
  muted: 'primary',
};

function requireEnum(v, set, ctx) {
  if (typeof v !== 'string' || !set.has(v)) {
    throw new TypeError(`${ctx}: expected one of ${[...set].join('/')}, got ${JSON.stringify(v)}`);
  }
  return v;
}
function requireBool(v, ctx) {
  if (typeof v !== 'boolean') throw new TypeError(`${ctx}: expected boolean, got ${typeof v}`);
  return v;
}
function requireU16(v, ctx) {
  if (typeof v === 'bigint') {
    if (v < 0n || v > 0xFFFFn) throw new TypeError(`${ctx}: expected u16, got ${v}`);
    return Number(v);
  }
  if (!Number.isInteger(v) || v < 0 || v > 0xFFFF) throw new TypeError(`${ctx}: expected u16, got ${v}`);
  return v;
}
function requirePath(v, ctx) {
  if (!Array.isArray(v)) throw new TypeError(`${ctx}: expected StatePath`);
  return v;
}
function assertOnlyKnownFields(fields, allowedKeys, name) {
  for (const [k] of fields) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(`${name}: unknown field key ${k} (allowed: ${[...allowedKeys].join(',')})`);
    }
  }
}

// =============================================================================
// Sparkline (0x0215) — uses <tf-sparkline>
// =============================================================================

export const SPARKLINE_TAG = 0x0215;
const SPARKLINE_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);

function renderSparkline(component, ctx) {
  assertOnlyKnownFields(component.fields, SPARKLINE_FIELD_KEYS, 'Sparkline');

  const dataPath = requirePath(ctx.readField(component.fields, 0), 'Sparkline.data_path');
  const variant = requireEnum(ctx.readField(component.fields, 1), SPARKLINE_VARIANTS, 'Sparkline.variant');
  const tone = requireEnum(ctx.readField(component.fields, 2), TONES, 'Sparkline.tone');
  const widthPx = requireU16(ctx.readField(component.fields, 3), 'Sparkline.width_px');
  if (widthPx === 0) throw new TypeError('Sparkline.width_px must be > 0');
  const heightPx = requireU16(ctx.readField(component.fields, 4), 'Sparkline.height_px');
  if (heightPx === 0) throw new TypeError('Sparkline.height_px must be > 0');
  const showMinMax = requireBool(ctx.readField(component.fields, 5), 'Sparkline.show_min_max');

  const wrapper = document.createElement('span');
  wrapper.classList.add('tf-sparkline-wrapper');
  wrapper.classList.add(`tf-sparkline--variant-${variant}`);
  wrapper.classList.add(`tf-sparkline--tone-${tone}`);
  wrapper.style.display = 'inline-flex';
  wrapper.style.alignItems = 'center';
  wrapper.style.gap = '0.5em';

  // <tf-sparkline> web component
  const sparkline = document.createElement('tf-sparkline');
  sparkline.style.width = `${widthPx}px`;
  sparkline.color = TONE_TO_COLOR[tone] || 'primary';
  // `variant` drives the draw mode: 'bar' renders discrete bars, 'area' fills
  // under the line, 'line' is the plain stroke. `fill` is kept in sync so the
  // area path still fills.
  sparkline.variant = variant;
  sparkline.fill = (variant === 'area');
  sparkline.showDots = false;
  sparkline.height = heightPx;
  // Canvas has no intrinsic semantics — expose the chart to assistive tech as
  // an image with a descriptive label (a11y.label wins, else synthesized).
  sparkline.setAttribute('role', 'img');
  const a11yLabelRef = component.a11y != null ? component.a11y.label : null;
  wrapper.appendChild(sparkline);

  let minBadge = null;
  let maxBadge = null;
  if (showMinMax) {
    const statsWrap = document.createElement('span');
    statsWrap.classList.add('tf-sparkline__stats');
    minBadge = document.createElement('span');
    minBadge.classList.add('tf-sparkline__min');
    statsWrap.appendChild(minBadge);
    const sep = document.createElement('span');
    sep.classList.add('tf-sparkline__sep');
    sep.setAttribute('aria-hidden', 'true');
    sep.textContent = '/';
    statsWrap.appendChild(sep);
    maxBadge = document.createElement('span');
    maxBadge.classList.add('tf-sparkline__max');
    statsWrap.appendChild(maxBadge);
    wrapper.appendChild(statsWrap);
  }

  const readData = () => {
    let arr;
    try { arr = ctx.store.read(dataPath); } catch { arr = undefined; }
    if (!Array.isArray(arr)) return [];
    return arr.filter((n) => typeof n === 'number' && Number.isFinite(n));
  };

  const applyAriaLabel = (data, min, max) => {
    let text = null;
    if (a11yLabelRef != null) {
      const v = resolveBindRef(a11yLabelRef, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) text = v;
    }
    if (text == null) {
      text = data.length === 0
        ? `${variant} sparkline, no data`
        : `${variant} sparkline, ${data.length} points, min ${formatStat(min)}, max ${formatStat(max)}`;
    }
    sparkline.setAttribute('aria-label', text);
  };

  const rebuild = () => {
    const data = readData();
    sparkline.points = data;

    let min = null, max = null;
    if (data.length > 0) {
      min = data[0]; max = data[0];
      for (const n of data) { if (n < min) min = n; if (n > max) max = n; }
    }

    if (showMinMax) {
      if (data.length === 0) {
        minBadge.textContent = '';
        maxBadge.textContent = '';
      } else {
        minBadge.textContent = formatStat(min);
        maxBadge.textContent = formatStat(max);
      }
    }

    applyAriaLabel(data, min, max);
  };
  rebuild();
  ctx.registerCleanup(ctx.store.subscribe(dataPath, rebuild));

  return wrapper;
}

function formatStat(n) {
  if (!Number.isFinite(n)) return '';
  if (Number.isInteger(n)) return String(n);
  return n.toFixed(2);
}

// =============================================================================
// Registration
// =============================================================================

export function registerDataSparklineRenderer() {
  if (!lookupComponentRenderer(SPARKLINE_TAG)) registerComponentRenderer(SPARKLINE_TAG, renderSparkline);
}
