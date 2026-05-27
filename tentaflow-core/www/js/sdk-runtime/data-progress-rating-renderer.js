// =============================================================================
// Plik: sdk-runtime/data-progress-rating-renderer.js
// Opis: Renderery ProgressBar (0x021D) + RatingDisplay (0x021E) — chunk 3.3d-13.
//
// ProgressBar: uses <tf-progress-bar> web component with reactive value/tone/
// size/label attributes. Variants: `default` (solid fill), `striped` (CSS
// striped pattern), `indeterminate` (animated bar). Tone maps to tf-progress-bar
// tone attribute. show_label renders % label.
//
// RatingDisplay: max symbols (stars/hearts/circles) rendered as SVG, or
// variant=numeric as text. precision: `full` (int), `half` (half-fill),
// `decimal` (partial fill).
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/progress.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef, assertBindRef } from './bind-resolver.js';
import {
  SVG_NS, TONES,
  requireEnum, requireBool, requireU8, requireF64,
  assertOnlyKnownFields,
} from './data-chart-shared.js';

// Map SDK tones to tf-progress-bar tone attribute values
const TONE_MAP = {
  'primary': 'accent',
  'success': 'success',
  'warning': 'warning',
  'critical': 'danger',
  'info': 'accent',
  'neutral': 'accent',
  'muted': 'accent',
};

// =============================================================================
// ProgressBar (0x021D) — uses <tf-progress-bar> web component
// =============================================================================

export const PROGRESS_BAR_TAG = 0x021D;
const PROGRESS_BAR_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);
const PROGRESS_VARIANTS = new Set(['default', 'striped', 'indeterminate']);
const PROGRESS_SIZES = new Set(['xs', 'sm', 'md', 'lg']);

function renderProgressBar(component, ctx) {
  assertOnlyKnownFields(component.fields, PROGRESS_BAR_FIELD_KEYS, 'ProgressBar');

  const valueBind = ctx.readField(component.fields, 0);
  if (valueBind == null) throw new TypeError('ProgressBar.value is required (BindRef)');
  assertBindRef(valueBind, 'ProgressBar.value');
  let max = ctx.readField(component.fields, 1);
  if (max == null) max = 1.0;
  max = requireF64(max, 'ProgressBar.max');
  if (!(max > 0)) throw new TypeError('ProgressBar.max must be > 0');
  const variant = requireEnum(ctx.readField(component.fields, 2), PROGRESS_VARIANTS, 'ProgressBar.variant');
  const tone = requireEnum(ctx.readField(component.fields, 3), TONES, 'ProgressBar.tone');
  const showLabel = requireBool(ctx.readField(component.fields, 4), 'ProgressBar.show_label');
  const labelBind = ctx.readField(component.fields, 5);
  if (labelBind != null) assertBindRef(labelBind, 'ProgressBar.label');
  const size = requireEnum(ctx.readField(component.fields, 6), PROGRESS_SIZES, 'ProgressBar.size');

  // Create <tf-progress-bar> web component
  const el = document.createElement('tf-progress-bar');
  el.setAttribute('tone', TONE_MAP[tone] || 'accent');
  el.setAttribute('size', size);

  // Apply variant CSS class for striped/indeterminate
  if (variant === 'striped') el.classList.add('tf-progress-bar--striped');
  if (variant === 'indeterminate') el.classList.add('tf-progress-bar--indeterminate');

  // ARIA attributes
  el.setAttribute('role', 'progressbar');
  el.setAttribute('aria-valuemin', '0');
  el.setAttribute('aria-valuemax', String(max));

  const apply = () => {
    if (variant === 'indeterminate') {
      el.removeAttribute('aria-valuenow');
      el.setAttribute('value', '0');
      if (showLabel && labelBind != null) {
        const v = resolveBindRef(labelBind, ctx.store);
        el.setAttribute('label', v == null ? '' : String(v));
      } else {
        el.removeAttribute('label');
      }
      return;
    }
    const raw = resolveBindRef(valueBind, ctx.store);
    const invalid = (raw == null) || typeof raw !== 'number' || !Number.isFinite(raw);
    if (invalid) {
      el.setAttribute('value', '0');
      el.removeAttribute('aria-valuenow');
      if (raw != null) el.setAttribute('aria-invalid', 'true');
      else el.removeAttribute('aria-invalid');
      if (showLabel) el.setAttribute('label', '—');
      else el.removeAttribute('label');
      return;
    }
    el.removeAttribute('aria-invalid');
    const clamped = Math.max(0, Math.min(max, raw));
    const pct = (clamped / max) * 100;
    el.setAttribute('value', String(pct.toFixed(2)));
    el.setAttribute('aria-valuenow', String(clamped));
    if (showLabel) {
      if (labelBind != null) {
        const v = resolveBindRef(labelBind, ctx.store);
        el.setAttribute('label', v == null ? `${pct.toFixed(0)}%` : String(v));
      } else {
        el.setAttribute('label', `${pct.toFixed(0)}%`);
      }
    } else {
      el.removeAttribute('label');
    }
  };
  apply();
  if (variant !== 'indeterminate') {
    ctx.registerCleanup(subscribeBindRef(valueBind, ctx.store, apply));
  }
  if (labelBind != null && showLabel) {
    ctx.registerCleanup(subscribeBindRef(labelBind, ctx.store, apply));
  }

  return el;
}

// =============================================================================
// RatingDisplay (0x021E)
// =============================================================================

export const RATING_DISPLAY_TAG = 0x021E;
const RATING_DISPLAY_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);
const RATING_VARIANTS = new Set(['stars', 'hearts', 'circles', 'numeric']);
const RATING_PRECISIONS = new Set(['full', 'half', 'decimal']);

const RATING_PATHS = {
  stars: 'M12 2.5l2.95 6.55 7.05.65-5.3 4.85 1.55 6.95L12 17.85 5.75 21.5 7.3 14.55 2 9.7l7.05-.65L12 2.5z',
  hearts: 'M12 21s-7-4.35-7-10a4 4 0 0 1 7-2.65A4 4 0 0 1 19 11c0 5.65-7 10-7 10z',
  circles: 'M12 4a8 8 0 1 0 0 16 8 8 0 0 0 0-16z',
};

function renderRatingDisplay(component, ctx) {
  assertOnlyKnownFields(component.fields, RATING_DISPLAY_FIELD_KEYS, 'RatingDisplay');

  const valueBind = ctx.readField(component.fields, 0);
  if (valueBind == null) throw new TypeError('RatingDisplay.value is required (BindRef)');
  assertBindRef(valueBind, 'RatingDisplay.value');
  let max = ctx.readField(component.fields, 1);
  if (max == null) max = 5;
  max = requireU8(max, 'RatingDisplay.max');
  if (max === 0) throw new TypeError('RatingDisplay.max must be > 0');
  const variant = requireEnum(ctx.readField(component.fields, 2), RATING_VARIANTS, 'RatingDisplay.variant');
  const showValue = requireBool(ctx.readField(component.fields, 3), 'RatingDisplay.show_value');
  const precision = requireEnum(ctx.readField(component.fields, 4), RATING_PRECISIONS, 'RatingDisplay.precision');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-rating');
  wrapper.classList.add(`tf-rating--variant-${variant}`);
  wrapper.classList.add(`tf-rating--precision-${precision}`);
  wrapper.setAttribute('role', 'img');

  if (variant === 'numeric') {
    const txt = document.createElement('span');
    txt.classList.add('tf-rating__numeric');
    wrapper.appendChild(txt);
    const apply = () => {
      const raw = resolveBindRef(valueBind, ctx.store);
      if (raw == null) {
        txt.textContent = `— / ${max}`;
        wrapper.removeAttribute('aria-invalid');
        wrapper.setAttribute('aria-label', `unknown of ${max}`);
        return;
      }
      if (typeof raw !== 'number' || !Number.isFinite(raw)) {
        txt.textContent = `— / ${max}`;
        wrapper.setAttribute('aria-invalid', 'true');
        wrapper.setAttribute('aria-label', `invalid rating`);
        return;
      }
      wrapper.removeAttribute('aria-invalid');
      const clamped = Math.max(0, Math.min(max, raw));
      const formatted = precision === 'full' ? Math.round(clamped).toString()
        : precision === 'half' ? (Math.round(clamped * 2) / 2).toString()
        : clamped.toFixed(1);
      txt.textContent = `${formatted} / ${max}`;
      wrapper.setAttribute('aria-label', `${formatted} of ${max}`);
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(valueBind, ctx.store, apply));
    return wrapper;
  }

  const path = RATING_PATHS[variant];
  const clipPrefix = `tf-rating-clip-${Math.random().toString(36).slice(2, 10)}`;
  const iconsRoot = document.createElement('div');
  iconsRoot.classList.add('tf-rating__icons');
  wrapper.appendChild(iconsRoot);

  const fillRects = [];
  for (let i = 0; i < max; i++) {
    const svg = document.createElementNS(SVG_NS, 'svg');
    svg.setAttribute('viewBox', '0 0 24 24');
    svg.setAttribute('class', `tf-rating__icon tf-rating__icon--${variant}`);
    const trackEl = document.createElementNS(SVG_NS, 'path');
    trackEl.setAttribute('d', path);
    trackEl.setAttribute('class', 'tf-rating__icon-track');
    svg.appendChild(trackEl);
    const defs = document.createElementNS(SVG_NS, 'defs');
    const clip = document.createElementNS(SVG_NS, 'clipPath');
    const clipId = `${clipPrefix}-${i}`;
    clip.setAttribute('id', clipId);
    const rect = document.createElementNS(SVG_NS, 'rect');
    rect.setAttribute('x', '0');
    rect.setAttribute('y', '0');
    rect.setAttribute('width', '0');
    rect.setAttribute('height', '24');
    clip.appendChild(rect);
    defs.appendChild(clip);
    svg.appendChild(defs);
    const fillEl = document.createElementNS(SVG_NS, 'path');
    fillEl.setAttribute('d', path);
    fillEl.setAttribute('class', 'tf-rating__icon-fill');
    fillEl.setAttribute('clip-path', `url(#${clipId})`);
    svg.appendChild(fillEl);
    iconsRoot.appendChild(svg);
    fillRects.push(rect);
  }

  let valueLabel = null;
  if (showValue) {
    valueLabel = document.createElement('span');
    valueLabel.classList.add('tf-rating__value');
    wrapper.appendChild(valueLabel);
  }

  const apply = () => {
    const raw = resolveBindRef(valueBind, ctx.store);
    let clamped;
    let invalid = false;
    if (raw == null) {
      clamped = 0;
    } else if (typeof raw !== 'number' || !Number.isFinite(raw)) {
      clamped = 0;
      invalid = true;
    } else {
      clamped = Math.max(0, Math.min(max, raw));
    }
    if (invalid) wrapper.setAttribute('aria-invalid', 'true');
    else wrapper.removeAttribute('aria-invalid');

    let display;
    if (precision === 'full') display = Math.round(clamped);
    else if (precision === 'half') display = Math.round(clamped * 2) / 2;
    else display = clamped;

    for (let i = 0; i < fillRects.length; i++) {
      const slotValue = Math.max(0, Math.min(1, display - i));
      fillRects[i].setAttribute('width', String(24 * slotValue));
    }
    const ariaText = invalid ? 'invalid rating'
      : raw == null ? `unknown of ${max}`
      : `${display} of ${max}`;
    wrapper.setAttribute('aria-label', ariaText);
    if (valueLabel) {
      valueLabel.textContent = raw == null || invalid
        ? '—'
        : (precision === 'decimal' ? clamped.toFixed(1) : String(display));
    }
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(valueBind, ctx.store, apply));

  return wrapper;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerDataProgressRatingRenderers() {
  if (!lookupComponentRenderer(PROGRESS_BAR_TAG)) registerComponentRenderer(PROGRESS_BAR_TAG, renderProgressBar);
  if (!lookupComponentRenderer(RATING_DISPLAY_TAG)) registerComponentRenderer(RATING_DISPLAY_TAG, renderRatingDisplay);
}
