// =============================================================================
// File: sdk-runtime/action-icon-button-renderer.js
// Description: Renderer IconButton (tag 0x0402) using <tf-button> in icon-only
//              mode. Maps SDK variant/tone/size to tf-button attributes.
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/actions/buttons.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';
import { renderIcon } from './icon-renderer.js';

const BUTTON_VARIANTS = new Set([
  'primary', 'secondary', 'tertiary', 'ghost', 'destructive', 'link',
]);
const TONES = new Set([
  'neutral', 'primary', 'success', 'warning', 'critical', 'info', 'muted',
]);
const BUTTON_SIZES = new Set(['xs', 'sm', 'md', 'lg']);

// SDK variant -> tf-button variant attribute
const VARIANT_MAP = {
  primary: 'primary',
  secondary: 'secondary',
  tertiary: 'ghost',
  ghost: 'ghost',
  destructive: 'danger',
  link: 'ghost',
};
// SDK size -> tf-button size attribute
const SIZE_MAP = { xs: 'sm', sm: 'sm', md: null, lg: null };

function requireEnum(v, set, ctx) {
  if (typeof v !== 'string' || !set.has(v)) {
    throw new TypeError(
      `${ctx}: expected one of ${[...set].join('/')}, got ${JSON.stringify(v)}`
    );
  }
  return v;
}
function requireString(v, ctx) {
  if (typeof v !== 'string') {
    throw new TypeError(`${ctx}: expected string, got ${typeof v}`);
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

export const ICON_BUTTON_TAG = 0x0402;
const ICON_BUTTON_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);

function renderIconButton(component, ctx) {
  assertOnlyKnownFields(component.fields, ICON_BUTTON_FIELD_KEYS, 'IconButton');
  const iconRaw = ctx.readField(component.fields, 0);
  if (iconRaw == null) {
    throw new TypeError('IconButton.icon is required (IconRef)');
  }
  const variant = requireEnum(
    ctx.readField(component.fields, 1),
    BUTTON_VARIANTS,
    'IconButton.variant'
  );
  const tone = requireEnum(
    ctx.readField(component.fields, 2),
    TONES,
    'IconButton.tone'
  );
  const size = requireEnum(
    ctx.readField(component.fields, 3),
    BUTTON_SIZES,
    'IconButton.size'
  );
  const ariaLabelRaw = ctx.readField(component.fields, 4);
  if (ariaLabelRaw === undefined) {
    throw new TypeError('IconButton.aria_label is required');
  }
  const ariaLabel = requireString(ariaLabelRaw, 'IconButton.aria_label');
  if (ariaLabel.trim().length === 0) {
    throw new TypeError('IconButton.aria_label must be non-blank (a11y)');
  }
  const disabledBind = ctx.readField(component.fields, 5);
  const loadingBind = ctx.readField(component.fields, 6);

  // Resolve icon name from IconRef for tf-button icon attribute
  const iconName = (iconRaw && iconRaw.kind === 'named') ? iconRaw.name : null;

  const btn = document.createElement('tf-button');
  btn.setAttribute('variant', VARIANT_MAP[variant] || 'primary');
  btn.setAttribute('tone', tone);
  const mappedSize = SIZE_MAP[size];
  if (mappedSize) btn.setAttribute('size', mappedSize);
  if (iconName) btn.setAttribute('icon', iconName);
  btn.setAttribute('aria-label', ariaLabel);

  // If icon cannot be set via attribute, render and append manually
  if (!iconName) {
    const iconEl = renderIcon(iconRaw, 'IconButton.icon');
    btn.appendChild(iconEl);
  }

  // Reactive disabled/loading
  let isDisabledLogical = false;
  let isLoadingLogical = false;
  const updateDisabledAttr = () => {
    if (isDisabledLogical || isLoadingLogical) {
      btn.setAttribute('disabled', '');
    } else {
      btn.removeAttribute('disabled');
    }
  };
  if (disabledBind != null) {
    const apply = () => {
      isDisabledLogical = resolveBindRef(disabledBind, ctx.store) === true;
      updateDisabledAttr();
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(disabledBind, ctx.store, apply));
  }
  if (loadingBind != null) {
    const apply = () => {
      isLoadingLogical = resolveBindRef(loadingBind, ctx.store) === true;
      if (isLoadingLogical) {
        btn.setAttribute('aria-busy', 'true');
      } else {
        btn.removeAttribute('aria-busy');
      }
      updateDisabledAttr();
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(loadingBind, ctx.store, apply));
  }
  return btn;
}

export function registerActionIconButtonRenderer() {
  if (!lookupComponentRenderer(ICON_BUTTON_TAG)) {
    registerComponentRenderer(ICON_BUTTON_TAG, renderIconButton);
  }
}
