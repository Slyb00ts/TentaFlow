// =============================================================================
// File: sdk-runtime/action-link-fab-renderer.js
// Description: Renderers for LinkButton (0x0404), Link (0x0405), Fab (0x040C)
//              using <tf-button> web component. LinkButton/Link use ghost
//              variant, Fab uses primary variant with icon.
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/actions/buttons.rs.
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

// Reactive label binding on tf-button's label attribute.
function bindLabelAttr(btn, bindRef, ctx) {
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    btn.setAttribute('label', v == null ? '' : String(v));
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

// =============================================================================
// LinkButton (0x0404) — link-styled button using <tf-button variant="ghost">
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

  const btn = document.createElement('tf-button');
  btn.setAttribute('variant', 'ghost');
  btn.setAttribute('tone', tone);
  btn.classList.add(`tf-link-button--underline-${underline}`);

  // Leading icon via tf-button icon attribute or rendered element
  if (iconLeadingRaw != null) {
    const iconName = (iconLeadingRaw.kind === 'named') ? iconLeadingRaw.name : null;
    if (iconName) {
      btn.setAttribute('icon', iconName);
    } else {
      const icon = renderIcon(iconLeadingRaw, 'LinkButton.icon_leading');
      btn.appendChild(icon);
    }
  }

  bindLabelAttr(btn, labelBind, ctx);

  // Trailing icon appended after label
  if (iconTrailingRaw != null) {
    const icon = renderIcon(iconTrailingRaw, 'LinkButton.icon_trailing');
    btn.appendChild(icon);
  }

  return btn;
}

// =============================================================================
// Link (0x0405) — text link, no raw href, navigation via handlers
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

  const btn = document.createElement('tf-button');
  btn.setAttribute('variant', 'ghost');
  btn.setAttribute('tone', tone);
  btn.classList.add(`tf-link--underline-${underline}`);
  btn.setAttribute('role', 'link');

  if (leadingIconRaw != null) {
    const iconName = (leadingIconRaw.kind === 'named') ? leadingIconRaw.name : null;
    if (iconName) {
      btn.setAttribute('icon', iconName);
    } else {
      const icon = renderIcon(leadingIconRaw, 'Link.leading_icon');
      btn.appendChild(icon);
    }
  }

  bindLabelAttr(btn, labelBind, ctx);

  if (trailingIconRaw != null) {
    const icon = renderIcon(trailingIconRaw, 'Link.trailing_icon');
    btn.appendChild(icon);
  }

  return btn;
}

// =============================================================================
// Fab (0x040C) — floating action button using <tf-button variant="primary">
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
  const labelBind = ctx.readField(component.fields, 4);

  const btn = document.createElement('tf-button');
  btn.setAttribute('variant', 'primary');
  btn.setAttribute('tone', tone);
  if (size === 'sm') btn.setAttribute('size', 'sm');
  btn.classList.add('tf-fab');
  btn.classList.add(`tf-fab--size-${size}`);
  btn.classList.add(`tf-fab--position-${position}`);

  // Icon
  const iconName = (iconRaw.kind === 'named') ? iconRaw.name : null;
  if (iconName) {
    btn.setAttribute('icon', iconName);
  } else {
    const iconEl = renderIcon(iconRaw, 'Fab.icon');
    btn.appendChild(iconEl);
  }

  if (labelBind != null) {
    // Extended FAB with label
    btn.classList.add('tf-fab--extended');
    bindLabelAttr(btn, labelBind, ctx);
  } else {
    // Icon-only FAB needs accessible name via a11y.label
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
// Registration
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
