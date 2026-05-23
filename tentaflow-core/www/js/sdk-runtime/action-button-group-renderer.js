// =============================================================================
// Plik: sdk-runtime/action-button-group-renderer.js
// Opis: Renderer ButtonGroup (tag 0x0403) — Faza 6 Krok 3.3b-4.
// Grupuje kilka Button-ów wizualnie (attached=true → bez gap'a między
// nimi, jak segmented control). Spec wymusza tag każdego dziecka =
// 0x0401 (Button) — egzekwujemy strict.
//
// Spec ref: `tentaflow-sdk-spec/src/protocol/ui/actions/buttons.rs`.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { BUTTON_TAG } from './action-button-renderer.js';

const BUTTON_GROUP_ORIENTATIONS = new Set(['horizontal', 'vertical']);

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
function requireArray(v, ctx) {
  if (!Array.isArray(v)) {
    throw new TypeError(`${ctx}: expected Array, got ${typeof v}`);
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

export const BUTTON_GROUP_TAG = 0x0403;
const BUTTON_GROUP_FIELD_KEYS = new Set([0, 1, 2]);

function renderButtonGroup(component, ctx) {
  assertOnlyKnownFields(component.fields, BUTTON_GROUP_FIELD_KEYS, 'ButtonGroup');
  const buttonsRaw = ctx.readField(component.fields, 0);
  if (buttonsRaw === undefined) {
    throw new TypeError('ButtonGroup.buttons is required');
  }
  const buttons = requireArray(buttonsRaw, 'ButtonGroup.buttons');
  const orientation = requireEnum(
    ctx.readField(component.fields, 1),
    BUTTON_GROUP_ORIENTATIONS,
    'ButtonGroup.orientation'
  );
  const attachedRaw = ctx.readField(component.fields, 2);
  if (attachedRaw === undefined) {
    throw new TypeError('ButtonGroup.attached is required');
  }
  const attached = requireBool(attachedRaw, 'ButtonGroup.attached');

  // Spec §6 0x0403: each child must be a Button (tag 0x0401). Mirror
  // Rust `ensure_ref_tag_encode/decode(Button::TAG)`.
  for (let i = 0; i < buttons.length; i++) {
    const b = buttons[i];
    if (!b || typeof b !== 'object' || b.tag !== BUTTON_TAG) {
      throw new TypeError(
        `ButtonGroup.buttons[${i}] must be Button (tag 0x0401), got 0x${
          (b && b.tag != null ? b.tag : 0).toString(16).padStart(4, '0')
        }`
      );
    }
  }

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-button-group');
  wrapper.classList.add(`tf-button-group--orientation-${orientation}`);
  if (attached) wrapper.classList.add('tf-button-group--attached');
  wrapper.setAttribute('role', 'group');

  for (const buttonComp of buttons) {
    wrapper.appendChild(ctx.renderChild(buttonComp));
  }
  return wrapper;
}

export function registerActionButtonGroupRenderer() {
  if (!lookupComponentRenderer(BUTTON_GROUP_TAG)) {
    registerComponentRenderer(BUTTON_GROUP_TAG, renderButtonGroup);
  }
}
