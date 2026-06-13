// =============================================================================
// Plik: sdk-runtime/layout-atomic-renderers.js
// Opis: Rendererzy 3 atomic komponentów Layout (Faza 6 Krok 3.3a-1):
//   - Divider  (tag 0x0108) — pozioma/pionowa linia z opcjonalnym labelem
//   - Spacer   (tag 0x0109) — puste miejsce w layoucie
//   - Tooltip  (tag 0x010F) — child + popup content na hover/focus
// Spec ref: `tentaflow-sdk-spec/src/protocol/ui/layout/atomic.rs`.
// Wszystkie komponenty są plain HTML elementami z CSS classami semantic-
// token-based — addony NIE wysyłają raw HTML/CSS; renderer mapuje token
// na klasę.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';

// =============================================================================
// Token validators
// =============================================================================

const DIVIDER_ORIENTATIONS = new Set(['horizontal', 'vertical']);
const DIVIDER_VARIANTS = new Set(['default', 'subtle', 'strong', 'dashed']);
const SPACINGS = new Set([
  'zero', 'xxs', 'xs', 'sm', 'md', 'lg', 'xl', 'xxl',
]);
const SPACER_AXES = new Set(['x', 'y', 'both']);
const DRAWER_SIDES = new Set(['left', 'right', 'top', 'bottom']);

function requireEnum(value, set, ctx) {
  if (typeof value !== 'string' || !set.has(value)) {
    throw new TypeError(
      `${ctx}: expected one of ${[...set].join('/')}, got ${JSON.stringify(value)}`
    );
  }
  return value;
}

function requireU16(value, ctx) {
  // u16 z wire'u może przyjść jako Number albo BigInt (dekoder CBOR oddaje
  // niektóre inty jako BigInt). Floaty / negatywne / > 65535 odrzucamy.
  if (typeof value === 'bigint') {
    if (value < 0n || value > 0xFFFFn) {
      throw new TypeError(`${ctx}: expected u16 integer, got ${value}`);
    }
    return Number(value);
  }
  if (!Number.isInteger(value) || value < 0 || value > 0xFFFF) {
    throw new TypeError(`${ctx}: expected u16 integer, got ${value}`);
  }
  return value;
}

// =============================================================================
// Divider (0x0108)
// =============================================================================

export const DIVIDER_TAG = 0x0108;

function renderDivider(component, ctx) {
  const orientation = requireEnum(
    ctx.readField(component.fields, 0),
    DIVIDER_ORIENTATIONS,
    'Divider.orientation'
  );
  const variant = requireEnum(
    ctx.readField(component.fields, 1),
    DIVIDER_VARIANTS,
    'Divider.variant'
  );
  const spacing = requireEnum(
    ctx.readField(component.fields, 2),
    SPACINGS,
    'Divider.spacing'
  );
  const labelBind = ctx.readField(component.fields, 3); // Option<BindRef>

  const el = document.createElement('div');
  el.classList.add('tf-divider-rule');
  el.classList.add(`tf-divider--${orientation}`);
  el.classList.add(`tf-divider--${variant}`);
  el.classList.add(`tf-divider--spacing-${spacing}`);
  el.setAttribute('role', 'separator');
  el.setAttribute(
    'aria-orientation',
    orientation === 'vertical' ? 'vertical' : 'horizontal'
  );

  if (labelBind != null) {
    const labelEl = document.createElement('span');
    labelEl.classList.add('tf-divider__label');
    el.appendChild(labelEl);
    const apply = () => {
      const v = resolveBindRef(labelBind, ctx.store);
      labelEl.textContent = v == null ? '' : String(v);
    };
    apply();
    const off = subscribeBindRef(labelBind, ctx.store, apply);
    ctx.registerCleanup(off);
  }

  return el;
}

// =============================================================================
// Spacer (0x0109)
// =============================================================================

export const SPACER_TAG = 0x0109;

function renderSpacer(component, ctx) {
  const size = requireEnum(
    ctx.readField(component.fields, 0),
    SPACINGS,
    'Spacer.size'
  );
  const axis = requireEnum(
    ctx.readField(component.fields, 1),
    SPACER_AXES,
    'Spacer.axis'
  );
  const el = document.createElement('div');
  el.classList.add('tf-spacer');
  el.classList.add(`tf-spacer--size-${size}`);
  el.classList.add(`tf-spacer--axis-${axis}`);
  el.setAttribute('aria-hidden', 'true');
  return el;
}

// =============================================================================
// Tooltip (0x010F)
// =============================================================================

export const TOOLTIP_TAG = 0x010F;

function renderTooltip(component, ctx) {
  const childComponent = ctx.readField(component.fields, 0);
  if (childComponent == null || typeof childComponent !== 'object') {
    throw new TypeError('Tooltip.child must be Component object');
  }
  const contentBind = ctx.readField(component.fields, 1);
  if (contentBind == null) {
    throw new TypeError('Tooltip.content must be BindRef');
  }
  const side = requireEnum(
    ctx.readField(component.fields, 2),
    DRAWER_SIDES,
    'Tooltip.side'
  );
  const maxWidthPx = requireU16(
    ctx.readField(component.fields, 3),
    'Tooltip.max_width_px'
  );

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-tooltip-wrapper');
  wrapper.classList.add(`tf-tooltip-wrapper--side-${side}`);

  const childEl = ctx.renderChild(childComponent);
  wrapper.appendChild(childEl);

  const tooltipEl = document.createElement('div');
  tooltipEl.classList.add('tf-tooltip');
  tooltipEl.classList.add(`tf-tooltip--side-${side}`);
  tooltipEl.setAttribute('role', 'tooltip');
  tooltipEl.setAttribute('hidden', '');
  tooltipEl.style.maxWidth = `${maxWidthPx}px`;
  wrapper.appendChild(tooltipEl);

  const applyContent = () => {
    const v = resolveBindRef(contentBind, ctx.store);
    tooltipEl.textContent = v == null ? '' : String(v);
  };
  applyContent();
  const offContent = subscribeBindRef(contentBind, ctx.store, applyContent);
  ctx.registerCleanup(offContent);

  // ARIA: dziecku doklejamy `aria-describedby` do istniejących id-ref'ów
  // (jeśli child miał własny described_by przez a11y.described_by, łączymy
  // tokeny space-separated zgodnie z ARIA spec).
  const tooltipId = `tf-tooltip-${component.id}-tip`;
  tooltipEl.setAttribute('id', tooltipId);
  const existingDescBy = childEl.getAttribute('aria-describedby');
  const mergedDescBy = existingDescBy
    ? `${existingDescBy} ${tooltipId}`
    : tooltipId;
  childEl.setAttribute('aria-describedby', mergedDescBy);

  // Hover + focus pokazują tooltip. Pointer events są standardowe i
  // nie wymagają eventDispatcher'a — tooltip to UI affordance, addon nie
  // dostaje notify on-show.
  const show = () => {
    tooltipEl.removeAttribute('hidden');
  };
  const hide = () => {
    tooltipEl.setAttribute('hidden', '');
  };
  childEl.addEventListener('mouseenter', show);
  childEl.addEventListener('mouseleave', hide);
  childEl.addEventListener('focus', show);
  childEl.addEventListener('blur', hide);
  ctx.registerCleanup(() => {
    childEl.removeEventListener('mouseenter', show);
    childEl.removeEventListener('mouseleave', hide);
    childEl.removeEventListener('focus', show);
    childEl.removeEventListener('blur', hide);
  });

  return wrapper;
}

// =============================================================================
// Rejestracja
// =============================================================================

/// Rejestruje 3 atomic rendererów w globalnym registry component-renderer.
/// Wywoływane raz w bootstrap'ie panelu (lub w testach — z czystym
/// registry). Idempotentnie skip'uje już zarejestrowane tagi, żeby modul
/// dało się załadować dwukrotnie bez efektu ubocznego.
export function registerLayoutAtomicRenderers() {
  if (!lookupComponentRenderer(DIVIDER_TAG)) {
    registerComponentRenderer(DIVIDER_TAG, renderDivider);
  }
  if (!lookupComponentRenderer(SPACER_TAG)) {
    registerComponentRenderer(SPACER_TAG, renderSpacer);
  }
  if (!lookupComponentRenderer(TOOLTIP_TAG)) {
    registerComponentRenderer(TOOLTIP_TAG, renderTooltip);
  }
}
