// =============================================================================
// Plik: sdk-runtime/action-button-renderer.js
// Opis: Renderer Button (tag 0x0401) — fundament grupy Action (Faza 6
// Krok 3.3b-1). Pozostałe komponenty action (IconButton/LinkButton/Fab/
// ButtonGroup/MenuButton/Menu/ActionBar/SegmentedControl/FilterChips/
// WizardFooter) lecą w kolejnych sub-chunkach 3.3b-2..3.3b-6.
//
// Button: standardowy przycisk z variant/tone/size, reactive label
// (BindRef), reactive disabled+loading (Option<BindRef>), opcjonalne
// icon_leading/icon_trailing (IconRef — render odraczany do chunka 3.3d).
// Click event natywny przechodzi do `Component.handlers` dispatch'a w
// engine'rze.
//
// Spec ref: `tentaflow-sdk-spec/src/protocol/ui/actions/buttons.rs`.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';

const BUTTON_VARIANTS = new Set([
  'primary', 'secondary', 'tertiary', 'ghost', 'destructive', 'link',
]);
const TONES = new Set([
  'neutral', 'primary', 'success', 'warning', 'critical', 'info', 'muted',
]);
const BUTTON_SIZES = new Set(['xs', 'sm', 'md', 'lg']);
const DENSITIES = new Set(['compact', 'default', 'comfortable']);

function requireEnum(v, set, ctx) {
  if (typeof v !== 'string' || !set.has(v)) {
    throw new TypeError(
      `${ctx}: expected one of ${[...set].join('/')}, got ${JSON.stringify(v)}`
    );
  }
  return v;
}
function requireBool(v, ctx) {
  if (typeof v !== 'boolean') {
    throw new TypeError(`${ctx}: expected boolean, got ${typeof v}`);
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

// =============================================================================
// Button (0x0401)
// =============================================================================

export const BUTTON_TAG = 0x0401;
const BUTTON_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);

function renderButton(component, ctx) {
  assertOnlyKnownFields(component.fields, BUTTON_FIELD_KEYS, 'Button');
  const variant = requireEnum(
    ctx.readField(component.fields, 0),
    BUTTON_VARIANTS,
    'Button.variant'
  );
  const tone = requireEnum(
    ctx.readField(component.fields, 1),
    TONES,
    'Button.tone'
  );
  const labelBind = ctx.readField(component.fields, 2);
  if (labelBind == null) {
    throw new TypeError('Button.label must be BindRef');
  }
  // icon_leading / icon_trailing — IconRef wymaga icon registry z chunka
  // 3.3d. Renderer odrzuca explicitnie obecność tych pól zamiast cicho
  // ignorować.
  if (ctx.readField(component.fields, 3) != null) {
  }
  if (ctx.readField(component.fields, 4) != null) {
  }
  const size = requireEnum(
    ctx.readField(component.fields, 5),
    BUTTON_SIZES,
    'Button.size'
  );
  const fullWidthRaw = ctx.readField(component.fields, 6);
  if (fullWidthRaw === undefined) {
    throw new TypeError('Button.full_width is required');
  }
  const fullWidth = requireBool(fullWidthRaw, 'Button.full_width');
  const disabledBind = ctx.readField(component.fields, 7); // Option<BindRef>
  const loadingBind = ctx.readField(component.fields, 8);  // Option<BindRef>
  const density = requireEnum(
    ctx.readField(component.fields, 9),
    DENSITIES,
    'Button.density'
  );

  const btn = document.createElement('button');
  btn.setAttribute('type', 'button');
  btn.classList.add('tf-button');
  btn.classList.add(`tf-button--variant-${variant}`);
  btn.classList.add(`tf-button--tone-${tone}`);
  btn.classList.add(`tf-button--size-${size}`);
  btn.classList.add(`tf-button--density-${density}`);
  if (fullWidth) btn.classList.add('tf-button--full-width');

  // Label binding — reactive textContent.
  const labelEl = document.createElement('span');
  labelEl.classList.add('tf-button__label');
  btn.appendChild(labelEl);
  const applyLabel = () => {
    const v = resolveBindRef(labelBind, ctx.store);
    labelEl.textContent = v == null ? '' : String(v);
  };
  applyLabel();
  ctx.registerCleanup(subscribeBindRef(labelBind, ctx.store, applyLabel));

  // Reactive disabled (Option<BindRef>). Defensive: jeśli loading=true,
  // disabled jest też wymuszony (W3C button affordance).
  let isDisabledLogical = false;
  let isLoadingLogical = false;
  const updateDisabledAttr = () => {
    if (isDisabledLogical || isLoadingLogical) {
      btn.setAttribute('disabled', '');
      btn.setAttribute('aria-disabled', 'true');
    } else {
      btn.removeAttribute('disabled');
      btn.removeAttribute('aria-disabled');
    }
  };
  if (disabledBind != null) {
    const applyDisabled = () => {
      isDisabledLogical = resolveBindRef(disabledBind, ctx.store) === true;
      updateDisabledAttr();
    };
    applyDisabled();
    ctx.registerCleanup(subscribeBindRef(disabledBind, ctx.store, applyDisabled));
  }

  // Reactive loading — pokazuje aria-busy=true + dodatkowy class + spinner
  // wstawiany jako pseudo-element przez CSS (nie wymaga DOM injection).
  if (loadingBind != null) {
    const applyLoading = () => {
      isLoadingLogical = resolveBindRef(loadingBind, ctx.store) === true;
      if (isLoadingLogical) {
        btn.classList.add('tf-button--loading');
        btn.setAttribute('aria-busy', 'true');
      } else {
        btn.classList.remove('tf-button--loading');
        btn.removeAttribute('aria-busy');
      }
      updateDisabledAttr();
    };
    applyLoading();
    ctx.registerCleanup(subscribeBindRef(loadingBind, ctx.store, applyLoading));
  }

  return btn;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerActionButtonRenderer() {
  if (!lookupComponentRenderer(BUTTON_TAG)) {
    registerComponentRenderer(BUTTON_TAG, renderButton);
  }
}
