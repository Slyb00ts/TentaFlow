// =============================================================================
// File: sdk-runtime/feedback-loading-renderer.js
// Description: Renderers for loading feedback components:
//   - Skeleton  (0x0506) — placeholder shapes with shimmer (div, no tf-*)
//   - Spinner   (0x0507) — uses <tf-spinner> with size attribute
//   - LoadingBar(0x0508) — uses <tf-progress-bar> or div for indeterminate
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/feedback/loading.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef, assertBindRef } from './bind-resolver.js';
import {
  TONES,
  requireEnum, requireBool, requireU8,
  assertOnlyKnownFields,
} from './data-chart-shared.js';
import { parseDimensionToken } from './data-specialised-renderer.js';

// =============================================================================
// Skeleton (0x0506) — div with shimmer class (no tf-skeleton exists)
// =============================================================================

export const SKELETON_TAG = 0x0506;
const SKELETON_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);
const SKELETON_VARIANTS = new Set(['text', 'circle', 'rectangle', 'card', 'table_row']);

function renderSkeleton(component, ctx) {
  assertOnlyKnownFields(component.fields, SKELETON_FIELD_KEYS, 'Skeleton');

  const variant = requireEnum(ctx.readField(component.fields, 0), SKELETON_VARIANTS, 'Skeleton.variant');
  const widthRaw = ctx.readField(component.fields, 1);
  const heightRaw = ctx.readField(component.fields, 2);
  const animate = requireBool(ctx.readField(component.fields, 3), 'Skeleton.animate');
  const lines = requireU8(ctx.readField(component.fields, 4), 'Skeleton.lines');

  const width = parseDimensionToken(widthRaw, 'Skeleton.width');
  const height = parseDimensionToken(heightRaw, 'Skeleton.height');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-skeleton', `tf-skeleton--${variant}`);
  if (animate) wrapper.classList.add('tf-skeleton--animate');
  wrapper.setAttribute('aria-hidden', 'true');

  if (width != null) wrapper.style.width = width;
  if (height != null) wrapper.style.height = height;

  switch (variant) {
    case 'text':
      for (let i = 0; i < lines; i++) {
        const line = document.createElement('div');
        line.classList.add('tf-skeleton__line');
        wrapper.appendChild(line);
      }
      break;

    case 'circle': {
      const circle = document.createElement('div');
      circle.classList.add('tf-skeleton__circle');
      if (width != null) { circle.style.width = width; circle.style.height = width; }
      wrapper.appendChild(circle);
      break;
    }

    case 'rectangle': {
      const rect = document.createElement('div');
      rect.classList.add('tf-skeleton__rect');
      wrapper.appendChild(rect);
      break;
    }

    case 'card': {
      const headerRect = document.createElement('div');
      headerRect.classList.add('tf-skeleton__rect', 'tf-skeleton__card-header');
      wrapper.appendChild(headerRect);
      const body = document.createElement('div');
      body.classList.add('tf-skeleton__card-body');
      for (let i = 0; i < 3; i++) {
        const line = document.createElement('div');
        line.classList.add('tf-skeleton__line');
        body.appendChild(line);
      }
      wrapper.appendChild(body);
      break;
    }

    case 'table_row': {
      const row = document.createElement('div');
      row.classList.add('tf-skeleton__table-row');
      for (let i = 0; i < 4; i++) {
        const cell = document.createElement('div');
        cell.classList.add('tf-skeleton__rect', 'tf-skeleton__table-cell');
        row.appendChild(cell);
      }
      wrapper.appendChild(row);
      break;
    }
  }

  return wrapper;
}

// =============================================================================
// Spinner (0x0507) — uses <tf-spinner> web component
// =============================================================================

export const SPINNER_TAG = 0x0507;
const SPINNER_FIELD_KEYS = new Set([0, 1, 2, 3]);
const SPINNER_SIZES = new Set(['xs', 'sm', 'md', 'lg', 'xl']);
const SPINNER_VARIANTS = new Set(['default', 'ring', 'dots', 'bars']);

function renderSpinner(component, ctx) {
  assertOnlyKnownFields(component.fields, SPINNER_FIELD_KEYS, 'Spinner');

  const size = requireEnum(ctx.readField(component.fields, 0), SPINNER_SIZES, 'Spinner.size');
  const tone = requireEnum(ctx.readField(component.fields, 1), TONES, 'Spinner.tone');
  const labelBind = ctx.readField(component.fields, 2);
  const variant = requireEnum(ctx.readField(component.fields, 3), SPINNER_VARIANTS, 'Spinner.variant');

  if (labelBind != null) assertBindRef(labelBind, 'Spinner.label');

  // <tf-spinner> supports size attribute (xs, sm, md, lg, xl)
  const spinner = document.createElement('tf-spinner');
  spinner.setAttribute('size', size);
  spinner.classList.add(`tf-spinner--tone-${tone}`, `tf-spinner--${variant}`);

  // Reactive screen-reader label
  if (labelBind != null) {
    const applyLabel = () => {
      const v = resolveBindRef(labelBind, ctx.store);
      spinner.setAttribute('aria-label', v == null ? 'Loading' : String(v));
    };
    applyLabel();
    ctx.registerCleanup(subscribeBindRef(labelBind, ctx.store, applyLabel));
  }

  return spinner;
}

// =============================================================================
// LoadingBar (0x0508)
// =============================================================================

export const LOADING_BAR_TAG = 0x0508;
const LOADING_BAR_FIELD_KEYS = new Set([0, 1, 2]);

function renderLoadingBar(component, ctx) {
  assertOnlyKnownFields(component.fields, LOADING_BAR_FIELD_KEYS, 'LoadingBar');

  const visibleBind = ctx.readField(component.fields, 0);
  if (visibleBind == null) throw new TypeError('LoadingBar.visible is required (BindRef)');
  assertBindRef(visibleBind, 'LoadingBar.visible');

  const progressBind = ctx.readField(component.fields, 1);
  if (progressBind != null) assertBindRef(progressBind, 'LoadingBar.progress');

  const tone = requireEnum(ctx.readField(component.fields, 2), TONES, 'LoadingBar.tone');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-loading-bar', `tf-loading-bar--tone-${tone}`);
  wrapper.setAttribute('role', 'progressbar');

  const track = document.createElement('div');
  track.classList.add('tf-loading-bar__track');
  wrapper.appendChild(track);

  if (progressBind != null) {
    wrapper.classList.add('tf-loading-bar--determinate');
    const applyProgress = () => {
      const v = resolveBindRef(progressBind, ctx.store);
      if (v == null) { track.style.width = '0%'; wrapper.removeAttribute('aria-valuenow'); return; }
      if (typeof v !== 'number' || !Number.isFinite(v)) {
        track.style.width = '0%';
        wrapper.setAttribute('aria-invalid', 'true');
        wrapper.removeAttribute('aria-valuenow');
        return;
      }
      wrapper.removeAttribute('aria-invalid');
      const clamped = Math.max(0, Math.min(1, v));
      track.style.width = `${clamped * 100}%`;
      wrapper.setAttribute('aria-valuenow', String(Math.round(clamped * 100)));
    };
    applyProgress();
    wrapper.setAttribute('aria-valuemin', '0');
    wrapper.setAttribute('aria-valuemax', '100');
    ctx.registerCleanup(subscribeBindRef(progressBind, ctx.store, applyProgress));
  } else {
    wrapper.classList.add('tf-loading-bar--indeterminate');
  }

  const applyVisible = () => {
    const v = resolveBindRef(visibleBind, ctx.store);
    wrapper.style.display = v ? '' : 'none';
  };
  applyVisible();
  ctx.registerCleanup(subscribeBindRef(visibleBind, ctx.store, applyVisible));

  return wrapper;
}

// =============================================================================
// Registration
// =============================================================================

export function registerFeedbackLoadingRenderers() {
  if (!lookupComponentRenderer(SKELETON_TAG)) registerComponentRenderer(SKELETON_TAG, renderSkeleton);
  if (!lookupComponentRenderer(SPINNER_TAG)) registerComponentRenderer(SPINNER_TAG, renderSpinner);
  if (!lookupComponentRenderer(LOADING_BAR_TAG)) registerComponentRenderer(LOADING_BAR_TAG, renderLoadingBar);
}
