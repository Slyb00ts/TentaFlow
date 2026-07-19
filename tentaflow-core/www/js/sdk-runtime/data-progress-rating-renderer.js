// =============================================================================
// File: sdk-runtime/data-progress-rating-renderer.js
// Description: Renderers for ProgressBar (0x021D) using <tf-progress-bar> and
// RatingDisplay (0x021E) using <tf-rating> web components. The renderers only
// validate the CBOR FieldMap and map BindRefs to component attributes; all
// drawing lives in the components.
//
// ProgressBar: variants `default` (solid fill), `striped`, `indeterminate`.
// RatingDisplay: max symbols (stars/hearts/circles) or variant=numeric text;
// precision: `full` (int), `half` (half-fill), `decimal` (partial fill).
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/progress.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef, assertBindRef } from './bind-resolver.js';
import {
  TONES,
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
const PROGRESS_BAR_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7]);
const PROGRESS_VARIANTS = new Set(['default', 'striped', 'indeterminate']);
const PROGRESS_SIZES = new Set(['xs', 'sm', 'md', 'lg']);
const PROGRESS_ORIENTATIONS = new Set(['horizontal', 'vertical']);

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
  // Orientation is optional; absent → horizontal (byte-omitted on the wire).
  const orientationRaw = ctx.readField(component.fields, 7);
  const orientation = orientationRaw == null
    ? 'horizontal'
    : requireEnum(orientationRaw, PROGRESS_ORIENTATIONS, 'ProgressBar.orientation');

  // Create <tf-progress-bar> web component
  const el = document.createElement('tf-progress-bar');
  el.setAttribute('tone', TONE_MAP[tone] || 'accent');
  el.setAttribute('size', size);
  if (orientation === 'vertical') el.setAttribute('orientation', 'vertical');

  // Apply variant CSS class for striped/indeterminate
  if (variant === 'striped') el.classList.add('tf-progress-bar--variant-striped');
  if (variant === 'indeterminate') el.classList.add('tf-progress-bar--variant-indeterminate');

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
// RatingDisplay (0x021E) — uses <tf-rating> web component
// =============================================================================

export const RATING_DISPLAY_TAG = 0x021E;
const RATING_DISPLAY_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);
const RATING_VARIANTS = new Set(['stars', 'hearts', 'circles', 'numeric']);
const RATING_PRECISIONS = new Set(['full', 'half', 'decimal']);

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

  // <tf-rating> web component — renderer only maps validated fields onto it.
  const el = document.createElement('tf-rating');
  el.setAttribute('max', String(max));
  el.setAttribute('variant', variant);
  el.setAttribute('precision', precision);
  if (showValue) el.setAttribute('show-value', '');

  const apply = () => {
    const raw = resolveBindRef(valueBind, ctx.store);
    if (raw == null) {
      // Absent attribute = unknown state (em dash, no aria-invalid).
      el.removeAttribute('value');
    } else if (typeof raw !== 'number' || !Number.isFinite(raw)) {
      // Non-finite attribute = invalid state (aria-invalid).
      el.setAttribute('value', 'NaN');
    } else {
      el.setAttribute('value', String(raw));
    }
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(valueBind, ctx.store, apply));

  return el;
}

// =============================================================================
// Registration
// =============================================================================

export function registerDataProgressRatingRenderers() {
  if (!lookupComponentRenderer(PROGRESS_BAR_TAG)) registerComponentRenderer(PROGRESS_BAR_TAG, renderProgressBar);
  if (!lookupComponentRenderer(RATING_DISPLAY_TAG)) registerComponentRenderer(RATING_DISPLAY_TAG, renderRatingDisplay);
}
