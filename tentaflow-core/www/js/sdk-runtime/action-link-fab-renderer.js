// =============================================================================
// Plik: sdk-runtime/action-link-fab-renderer.js
// Opis: Rendererzy LinkButton (0x0404), Link (0x0405), Fab (0x040C) —
// Faza 6 Krok 3.3b-3. Wszystkie używają `renderIcon()` helpera z chunka
// 3.3b-2. Nawigacja nie idzie przez raw `href` — addon dispatchuje
// `click` event do backend handler'a (spec §6 0x0405 komentarz: "No raw
// `href`; navigation flows through handlers").
//
// Spec ref: `tentaflow-sdk-spec/src/protocol/ui/actions/buttons.rs`.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';
import { renderIcon } from './icon-renderer.js';

const TONES = new Set([
  'neutral', 'primary', 'success', 'warning', 'critical', 'info', 'muted',
]);
const LINK_UNDERLINES = new Set(['always', 'hover', 'never']);
const FAB_SIZES = new Set(['sm', 'md', 'lg']);
const FAB_POSITIONS = new Set(['bottom_right', 'bottom_left', 'inline']);

function requireEnum(v, set, ctx) {
  if (typeof v !== 'string' || !set.has(v)) {
    throw new TypeError(
      `${ctx}: expected one of ${[...set].join('/')}, got ${JSON.stringify(v)}`
    );
  }
  return v;
}
function assertOnlyKnownFields(fields, allowedKeys, name) {
  for (const [k] of fields) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(
        `${name}: unknown field key ${k} (allowed: ${[...allowedKeys].join(',')})`
      );
    }
  }
}

/// Doczepia reaktywny label do elementu — pattern jak Button/IconButton.
function bindLabel(element, bindRef, ctx) {
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    element.textContent = v == null ? '' : String(v);
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

// =============================================================================
// LinkButton (0x0404) — link-styled button (klik → handler, NIE href)
// =============================================================================

export const LINK_BUTTON_TAG = 0x0404;
const LINK_BUTTON_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderLinkButton(component, ctx) {
  assertOnlyKnownFields(component.fields, LINK_BUTTON_FIELD_KEYS, 'LinkButton');
  const labelBind = ctx.readField(component.fields, 0);
  if (labelBind == null) {
    throw new TypeError('LinkButton.label must be BindRef');
  }
  const iconLeadingRaw = ctx.readField(component.fields, 1);
  const iconTrailingRaw = ctx.readField(component.fields, 2);
  const tone = requireEnum(
    ctx.readField(component.fields, 3),
    TONES,
    'LinkButton.tone'
  );
  const underline = requireEnum(
    ctx.readField(component.fields, 4),
    LINK_UNDERLINES,
    'LinkButton.underline'
  );

  // LinkButton emituje 'click', nie ma navigation URL'a → używamy <button>
  // z type="button". Wygląd "link-style" ustala CSS przez tf-link-button.
  const btn = document.createElement('button');
  btn.setAttribute('type', 'button');
  btn.classList.add('tf-link-button');
  btn.classList.add(`tf-link-button--tone-${tone}`);
  btn.classList.add(`tf-link-button--underline-${underline}`);

  if (iconLeadingRaw != null) {
    const icon = renderIcon(iconLeadingRaw, 'LinkButton.icon_leading');
    icon.classList.add('tf-link-button__icon', 'tf-link-button__icon--leading');
    btn.appendChild(icon);
  }
  const labelEl = document.createElement('span');
  labelEl.classList.add('tf-link-button__label');
  btn.appendChild(labelEl);
  bindLabel(labelEl, labelBind, ctx);
  if (iconTrailingRaw != null) {
    const icon = renderIcon(iconTrailingRaw, 'LinkButton.icon_trailing');
    icon.classList.add('tf-link-button__icon', 'tf-link-button__icon--trailing');
    btn.appendChild(icon);
  }
  return btn;
}

// =============================================================================
// Link (0x0405) — standardowy text link
// =============================================================================

export const LINK_TAG = 0x0405;
const LINK_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderLink(component, ctx) {
  assertOnlyKnownFields(component.fields, LINK_FIELD_KEYS, 'Link');
  const labelBind = ctx.readField(component.fields, 0);
  if (labelBind == null) {
    throw new TypeError('Link.label must be BindRef');
  }
  const underline = requireEnum(
    ctx.readField(component.fields, 1),
    LINK_UNDERLINES,
    'Link.underline'
  );
  const tone = requireEnum(
    ctx.readField(component.fields, 2),
    TONES,
    'Link.tone'
  );
  const leadingIconRaw = ctx.readField(component.fields, 3);
  const trailingIconRaw = ctx.readField(component.fields, 4);

  // Spec §6 0x0405: "No raw href; navigation flows through handlers".
  // Renderujemy <a> z href="#" + preventDefault na click (engine handler
  // dispatchuje wire event), żeby semantyka role=link była zachowana
  // dla screen reader'ów ALE bez faktycznej nawigacji URL'em.
  const a = document.createElement('a');
  a.setAttribute('href', '#');
  a.setAttribute('role', 'link');
  a.classList.add('tf-link');
  a.classList.add(`tf-link--tone-${tone}`);
  a.classList.add(`tf-link--underline-${underline}`);
  // Blokuj default navigation (anchor href="#" inaczej scrollnie na top).
  const onClickGuard = (e) => e.preventDefault();
  a.addEventListener('click', onClickGuard);
  ctx.registerCleanup(() => a.removeEventListener('click', onClickGuard));

  if (leadingIconRaw != null) {
    const icon = renderIcon(leadingIconRaw, 'Link.leading_icon');
    icon.classList.add('tf-link__icon', 'tf-link__icon--leading');
    a.appendChild(icon);
  }
  const labelEl = document.createElement('span');
  labelEl.classList.add('tf-link__label');
  a.appendChild(labelEl);
  bindLabel(labelEl, labelBind, ctx);
  if (trailingIconRaw != null) {
    const icon = renderIcon(trailingIconRaw, 'Link.trailing_icon');
    icon.classList.add('tf-link__icon', 'tf-link__icon--trailing');
    a.appendChild(icon);
  }
  return a;
}

// =============================================================================
// Fab (0x040C) — floating action button
// =============================================================================

export const FAB_TAG = 0x040C;
const FAB_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderFab(component, ctx) {
  assertOnlyKnownFields(component.fields, FAB_FIELD_KEYS, 'Fab');
  const iconRaw = ctx.readField(component.fields, 0);
  if (iconRaw == null) {
    throw new TypeError('Fab.icon is required (IconRef)');
  }
  const tone = requireEnum(
    ctx.readField(component.fields, 1),
    TONES,
    'Fab.tone'
  );
  const size = requireEnum(
    ctx.readField(component.fields, 2),
    FAB_SIZES,
    'Fab.size'
  );
  const position = requireEnum(
    ctx.readField(component.fields, 3),
    FAB_POSITIONS,
    'Fab.position'
  );
  const labelBind = ctx.readField(component.fields, 4); // Option<BindRef>

  const btn = document.createElement('button');
  btn.setAttribute('type', 'button');
  btn.classList.add('tf-fab');
  btn.classList.add(`tf-fab--tone-${tone}`);
  btn.classList.add(`tf-fab--size-${size}`);
  btn.classList.add(`tf-fab--position-${position}`);

  // Icon (mandatory).
  const iconEl = renderIcon(iconRaw, 'Fab.icon');
  iconEl.classList.add('tf-fab__icon');
  btn.appendChild(iconEl);

  if (labelBind != null) {
    // Extended FAB — wyświetla label obok ikonki. Variant `extended` CSS
    // jest aktywowany przez obecność span'a z labelem.
    btn.classList.add('tf-fab--extended');
    const labelEl = document.createElement('span');
    labelEl.classList.add('tf-fab__label');
    btn.appendChild(labelEl);
    bindLabel(labelEl, labelBind, ctx);
  } else {
    // FAB bez visual label'a — semantyka button'a wymaga accessible name.
    // Spec §6 0x040C: addon musi dostarczyć albo `label` (BindRef) albo
    // `Component.a11y.label` z NIEPUSTĄ wartością. Engine wpisze go
    // później w `applyAccessibility` jako aria-label, ale tu walidujemy
    // że INITIAL wartość jest sensowna — inaczej button jest anonymous
    // dla screen reader'ów do czasu pierwszego patcha BindRef'a.
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError(
        'Fab without `label` field requires `Component.a11y.label` for accessible name'
      );
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError(
        'Fab.a11y.label must resolve to a non-blank string at initial render (accessible name)'
      );
    }
  }
  return btn;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerActionLinkFabRenderers() {
  if (!lookupComponentRenderer(LINK_BUTTON_TAG)) {
    registerComponentRenderer(LINK_BUTTON_TAG, renderLinkButton);
  }
  if (!lookupComponentRenderer(LINK_TAG)) {
    registerComponentRenderer(LINK_TAG, renderLink);
  }
  if (!lookupComponentRenderer(FAB_TAG)) {
    registerComponentRenderer(FAB_TAG, renderFab);
  }
}
