// =============================================================================
// File: sdk-runtime/feedback-overlay-renderer.js
// Description: Renderers for overlay/gate feedback components: Modal (0x0509),
// Drawer (0x050A), Popover (0x050B), Sheet (0x050C), GateScreen (0x050D),
// ConfirmationDialog (0x050E) — chunk 3.3e-3.
//
// Modal/Drawer/Sheet/ConfirmationDialog render through the <tf-modal> web
// component (variant/size attributes); ESC, backdrop click and the close
// button are handled by the component and bridged to the SDK 'dismiss'
// CustomEvent on the root element. Popover and GateScreen are non-dialog
// surfaces without a tf-* primitive and keep class-based markup. Slot
// containers carry data-slot-id for the slot manager (chunk 3.5).
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/feedback/overlay.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef, assertBindRef } from './bind-resolver.js';
import {
  TONES,
  requireEnum, requireBool, requireString,
  assertOnlyKnownFields,
} from './data-chart-shared.js';
import { renderIcon } from './icon-renderer.js';
import { BUTTON_TAG } from './action-button-renderer.js';

// Reactively mirrors a BindRef into an attribute on `el`.
function bindAttribute(el, attr, bindRef, ctx) {
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    el.setAttribute(attr, v == null ? '' : String(v));
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

// Bridges the tf-modal 'close' event (ESC / backdrop / close button) to the
// SDK 'dismiss' CustomEvent dispatched on the overlay root element.
function bridgeCloseToDismiss(el, ctx) {
  const onClose = (e) => {
    if (e.target !== el) return;
    el.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('dismiss', {
      bubbles: false,
    }));
  };
  el.addEventListener('close', onClose);
  ctx.registerCleanup(() => el.removeEventListener('close', onClose));
}

// Creates a slot container child consumed by tf-modal (`slot` attribute) and
// later filled by the slot manager (`data-slot-id`).
function makeSlotContainer(slotName, slotId) {
  const slotEl = document.createElement('div');
  slotEl.setAttribute('slot', slotName);
  slotEl.setAttribute('data-slot-id', slotId);
  return slotEl;
}

// =============================================================================
// Modal (0x0509) — <tf-modal variant="modal">
// =============================================================================

export const MODAL_TAG = 0x0509;
const MODAL_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8]);
const MODAL_SIZES = new Set(['xs', 'sm', 'md', 'lg', 'xl', 'fullscreen']);

function renderModal(component, ctx) {
  assertOnlyKnownFields(component.fields, MODAL_FIELD_KEYS, 'Modal');

  const titleBind = ctx.readField(component.fields, 0);
  if (titleBind == null) throw new TypeError('Modal.title is required (BindRef)');
  assertBindRef(titleBind, 'Modal.title');
  const subtitleBind = ctx.readField(component.fields, 1);
  if (subtitleBind != null) assertBindRef(subtitleBind, 'Modal.subtitle');
  const bodySlot = ctx.readField(component.fields, 2);
  if (bodySlot == null) throw new TypeError('Modal.body_slot is required');
  requireString(bodySlot, 'Modal.body_slot');
  const footerSlot = ctx.readField(component.fields, 3);
  if (footerSlot != null) requireString(footerSlot, 'Modal.footer_slot');
  const size = requireEnum(ctx.readField(component.fields, 4), MODAL_SIZES, 'Modal.size');
  const dismissible = requireBool(ctx.readField(component.fields, 5), 'Modal.dismissible');
  const preventScroll = requireBool(ctx.readField(component.fields, 6), 'Modal.prevent_scroll');
  const closable = requireBool(ctx.readField(component.fields, 7), 'Modal.closable');
  const iconRaw = ctx.readField(component.fields, 8);

  const el = document.createElement('tf-modal');
  el.setAttribute('variant', 'modal');
  el.setAttribute('size', size);
  if (!dismissible) el.setAttribute('no-dismiss', '');
  if (!closable) el.setAttribute('no-close', '');
  if (preventScroll) el.classList.add('tf-modal--prevent-scroll');

  bindAttribute(el, 'title', titleBind, ctx);
  if (subtitleBind != null) bindAttribute(el, 'subtitle', subtitleBind, ctx);

  // Optional header icon: a slotted child tf-modal moves into the header
  // before the title (mockup n03 share dialog).
  if (iconRaw != null) {
    const iconEl = renderIcon(iconRaw, 'Modal.icon');
    iconEl.setAttribute('slot', 'title-icon');
    iconEl.classList.add('tf-modal-title-icon');
    el.appendChild(iconEl);
  }

  el.appendChild(makeSlotContainer('body', bodySlot));
  if (footerSlot != null) el.appendChild(makeSlotContainer('footer', footerSlot));

  bridgeCloseToDismiss(el, ctx);
  // SDK overlays exist only while addon state shows them — open immediately.
  el.setAttribute('open', '');

  return el;
}

// =============================================================================
// Drawer (0x050A) — <tf-modal variant="drawer-{side}">
// =============================================================================

export const DRAWER_TAG = 0x050A;
const DRAWER_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);
const DRAWER_SIDES = new Set(['left', 'right', 'top', 'bottom']);
const DRAWER_SIZES = new Set(['xs', 'sm', 'md', 'lg', 'xl']);

function renderDrawer(component, ctx) {
  assertOnlyKnownFields(component.fields, DRAWER_FIELD_KEYS, 'Drawer');

  const side = requireEnum(ctx.readField(component.fields, 0), DRAWER_SIDES, 'Drawer.side');
  const size = requireEnum(ctx.readField(component.fields, 1), DRAWER_SIZES, 'Drawer.size');
  const titleBind = ctx.readField(component.fields, 2);
  if (titleBind != null) assertBindRef(titleBind, 'Drawer.title');
  const bodySlot = ctx.readField(component.fields, 3);
  if (bodySlot == null) throw new TypeError('Drawer.body_slot is required');
  requireString(bodySlot, 'Drawer.body_slot');
  const footerSlot = ctx.readField(component.fields, 4);
  if (footerSlot != null) requireString(footerSlot, 'Drawer.footer_slot');
  const dismissible = requireBool(ctx.readField(component.fields, 5), 'Drawer.dismissible');

  const el = document.createElement('tf-modal');
  el.setAttribute('variant', `drawer-${side}`);
  el.setAttribute('size', size);
  // Drawer spec has no closable flag — no header close button.
  el.setAttribute('no-close', '');
  if (!dismissible) el.setAttribute('no-dismiss', '');

  if (titleBind != null) bindAttribute(el, 'title', titleBind, ctx);

  el.appendChild(makeSlotContainer('body', bodySlot));
  if (footerSlot != null) el.appendChild(makeSlotContainer('footer', footerSlot));

  bridgeCloseToDismiss(el, ctx);
  el.setAttribute('open', '');

  return el;
}

// =============================================================================
// Popover (0x050B)
//
// No tf-* mapping: tf-tooltip is hover/focus-only and text-only, while a
// Popover is a click-anchored container filled by the slot manager and
// positioned by the host relative to data-anchor-id. Kept as class-based
// markup (shared CSS, no inline styles).
// =============================================================================

export const POPOVER_TAG = 0x050B;
const POPOVER_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);
const POPOVER_PLACEMENTS = new Set([
  'top', 'top_start', 'top_end',
  'bottom', 'bottom_start', 'bottom_end',
  'left', 'left_start', 'left_end',
  'right', 'right_start', 'right_end',
]);

function renderPopover(component, ctx) {
  assertOnlyKnownFields(component.fields, POPOVER_FIELD_KEYS, 'Popover');

  const anchorId = ctx.readField(component.fields, 0);
  if (anchorId == null) throw new TypeError('Popover.anchor_id is required');
  requireString(anchorId, 'Popover.anchor_id');
  const bodySlot = ctx.readField(component.fields, 1);
  if (bodySlot == null) throw new TypeError('Popover.body_slot is required');
  requireString(bodySlot, 'Popover.body_slot');
  const placement = requireEnum(ctx.readField(component.fields, 2), POPOVER_PLACEMENTS, 'Popover.placement');
  const dismissible = requireBool(ctx.readField(component.fields, 3), 'Popover.dismissible');
  const arrow = requireBool(ctx.readField(component.fields, 4), 'Popover.arrow');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-popover', `tf-popover--placement-${placement}`);
  wrapper.setAttribute('data-anchor-id', anchorId);

  const bodyEl = document.createElement('div');
  bodyEl.classList.add('tf-popover__body');
  bodyEl.setAttribute('data-slot-id', bodySlot);
  wrapper.appendChild(bodyEl);

  if (arrow) {
    const arrowEl = document.createElement('div');
    arrowEl.classList.add('tf-popover__arrow');
    wrapper.appendChild(arrowEl);
  }

  if (dismissible) {
    const onEsc = (e) => {
      if (e.key !== 'Escape') return;
      wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('dismiss', {
        bubbles: false,
      }));
    };
    document.addEventListener('keydown', onEsc);
    ctx.registerCleanup(() => document.removeEventListener('keydown', onEsc));
  }

  return wrapper;
}

// =============================================================================
// Sheet (0x050C) — <tf-modal variant="drawer-bottom"> with detent classes
// =============================================================================

export const SHEET_TAG = 0x050C;
const SHEET_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);
const SHEET_DETENTS = new Set(['small', 'medium', 'large', 'full']);

function renderSheet(component, ctx) {
  assertOnlyKnownFields(component.fields, SHEET_FIELD_KEYS, 'Sheet');

  const titleBind = ctx.readField(component.fields, 0);
  if (titleBind != null) assertBindRef(titleBind, 'Sheet.title');
  const bodySlot = ctx.readField(component.fields, 1);
  if (bodySlot == null) throw new TypeError('Sheet.body_slot is required');
  requireString(bodySlot, 'Sheet.body_slot');
  const footerSlot = ctx.readField(component.fields, 2);
  if (footerSlot != null) requireString(footerSlot, 'Sheet.footer_slot');
  const detentsRaw = ctx.readField(component.fields, 3);
  if (!Array.isArray(detentsRaw) || detentsRaw.length === 0) {
    throw new TypeError('Sheet.detents is required (non-empty Vec<String>)');
  }
  for (let i = 0; i < detentsRaw.length; i++) {
    requireEnum(detentsRaw[i], SHEET_DETENTS, `Sheet.detents[${i}]`);
  }
  const currentDetentBind = ctx.readField(component.fields, 4);
  if (currentDetentBind != null) assertBindRef(currentDetentBind, 'Sheet.current_detent');
  const dismissible = requireBool(ctx.readField(component.fields, 5), 'Sheet.dismissible');

  const el = document.createElement('tf-modal');
  el.setAttribute('variant', 'drawer-bottom');
  el.setAttribute('no-close', '');
  if (!dismissible) el.setAttribute('no-dismiss', '');
  el.classList.add('tf-sheet');

  // Current detent reflected as a host class for the host/sheet styling.
  const applyDetent = () => {
    for (const d of SHEET_DETENTS) el.classList.remove(`tf-sheet--detent-${d}`);
    let detent = detentsRaw[0];
    if (currentDetentBind != null) {
      const v = resolveBindRef(currentDetentBind, ctx.store);
      if (v != null && SHEET_DETENTS.has(String(v))) detent = String(v);
    }
    el.classList.add(`tf-sheet--detent-${detent}`);
  };
  applyDetent();
  if (currentDetentBind != null) {
    ctx.registerCleanup(subscribeBindRef(currentDetentBind, ctx.store, applyDetent));
  }

  if (titleBind != null) bindAttribute(el, 'title', titleBind, ctx);

  el.appendChild(makeSlotContainer('body', bodySlot));
  if (footerSlot != null) el.appendChild(makeSlotContainer('footer', footerSlot));

  bridgeCloseToDismiss(el, ctx);
  el.setAttribute('open', '');

  return el;
}

// =============================================================================
// GateScreen (0x050D)
//
// Full-screen blocking takeover (auth/permission/rate-limit/maintenance).
// No tf-* dialog mapping on purpose: a gate has no header/close/backdrop
// semantics and its actions are SDK Button children (rendered as <tf-button>
// by the Button renderer). Class-based markup, shared CSS only.
// =============================================================================

export const GATE_SCREEN_TAG = 0x050D;
const GATE_SCREEN_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);
const GATE_SCREEN_VARIANTS = new Set([
  'auth_required', 'permission_denied', 'rate_limited', 'maintenance',
]);

function renderGateScreen(component, ctx) {
  assertOnlyKnownFields(component.fields, GATE_SCREEN_FIELD_KEYS, 'GateScreen');

  const iconRaw = ctx.readField(component.fields, 0);
  if (iconRaw == null) throw new TypeError('GateScreen.icon is required (IconRef)');
  const titleBind = ctx.readField(component.fields, 1);
  if (titleBind == null) throw new TypeError('GateScreen.title is required (BindRef)');
  assertBindRef(titleBind, 'GateScreen.title');
  const messageBind = ctx.readField(component.fields, 2);
  if (messageBind == null) throw new TypeError('GateScreen.message is required (BindRef)');
  assertBindRef(messageBind, 'GateScreen.message');
  const actionsRaw = ctx.readField(component.fields, 3);
  if (!Array.isArray(actionsRaw)) throw new TypeError('GateScreen.actions must be array');
  for (const actionComp of actionsRaw) {
    if (!actionComp || actionComp.tag !== BUTTON_TAG) {
      throw new TypeError(`GateScreen.actions: children must be Button (0x${BUTTON_TAG.toString(16)})`);
    }
  }
  const variant = requireEnum(ctx.readField(component.fields, 4), GATE_SCREEN_VARIANTS, 'GateScreen.variant');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-gate-screen', `tf-gate-screen--variant-${variant}`);

  const iconEl = renderIcon(iconRaw, 'GateScreen.icon');
  iconEl.classList.add('tf-gate-screen__icon');
  wrapper.appendChild(iconEl);

  const titleEl = document.createElement('h1');
  titleEl.classList.add('tf-gate-screen__title');
  const applyTitle = () => {
    const v = resolveBindRef(titleBind, ctx.store);
    titleEl.textContent = v == null ? '' : String(v);
  };
  applyTitle();
  ctx.registerCleanup(subscribeBindRef(titleBind, ctx.store, applyTitle));
  wrapper.appendChild(titleEl);

  const messageEl = document.createElement('p');
  messageEl.classList.add('tf-gate-screen__message');
  const applyMessage = () => {
    const v = resolveBindRef(messageBind, ctx.store);
    messageEl.textContent = v == null ? '' : String(v);
  };
  applyMessage();
  ctx.registerCleanup(subscribeBindRef(messageBind, ctx.store, applyMessage));
  wrapper.appendChild(messageEl);

  if (actionsRaw.length > 0) {
    const actionsEl = document.createElement('div');
    actionsEl.classList.add('tf-gate-screen__actions');
    for (const actionComp of actionsRaw) {
      actionsEl.appendChild(ctx.renderChild(actionComp));
    }
    wrapper.appendChild(actionsEl);
  }

  return wrapper;
}

// =============================================================================
// ConfirmationDialog (0x050E) — <tf-modal> with tf-button/tf-input content
// =============================================================================

export const CONFIRMATION_DIALOG_TAG = 0x050E;
const CONFIRMATION_DIALOG_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7]);

function renderConfirmationDialog(component, ctx) {
  assertOnlyKnownFields(component.fields, CONFIRMATION_DIALOG_FIELD_KEYS, 'ConfirmationDialog');

  const titleBind = ctx.readField(component.fields, 0);
  if (titleBind == null) throw new TypeError('ConfirmationDialog.title is required (BindRef)');
  assertBindRef(titleBind, 'ConfirmationDialog.title');
  const messageBind = ctx.readField(component.fields, 1);
  if (messageBind == null) throw new TypeError('ConfirmationDialog.message is required (BindRef)');
  assertBindRef(messageBind, 'ConfirmationDialog.message');
  const iconRaw = ctx.readField(component.fields, 2);
  const tone = requireEnum(ctx.readField(component.fields, 3), TONES, 'ConfirmationDialog.tone');
  const confirmLabelBind = ctx.readField(component.fields, 4);
  if (confirmLabelBind == null) throw new TypeError('ConfirmationDialog.confirm_label is required (BindRef)');
  assertBindRef(confirmLabelBind, 'ConfirmationDialog.confirm_label');
  const cancelLabelBind = ctx.readField(component.fields, 5);
  if (cancelLabelBind == null) throw new TypeError('ConfirmationDialog.cancel_label is required (BindRef)');
  assertBindRef(cancelLabelBind, 'ConfirmationDialog.cancel_label');
  const destructive = requireBool(ctx.readField(component.fields, 6), 'ConfirmationDialog.destructive');
  const requireTyping = ctx.readField(component.fields, 7);
  if (requireTyping != null) requireString(requireTyping, 'ConfirmationDialog.require_typing');

  const el = document.createElement('tf-modal');
  el.setAttribute('variant', 'modal');
  el.setAttribute('size', 'sm');
  el.setAttribute('no-close', '');
  el.classList.add('tf-confirm-dialog', `tf-confirm-dialog--tone-${tone}`);

  bindAttribute(el, 'title', titleBind, ctx);

  // Body: optional icon + message + optional confirmation input.
  const bodyEl = document.createElement('div');
  bodyEl.setAttribute('slot', 'body');

  if (iconRaw != null) {
    const iconEl = renderIcon(iconRaw, 'ConfirmationDialog.icon');
    iconEl.classList.add('tf-confirm-dialog__icon');
    bodyEl.appendChild(iconEl);
  }

  const messageEl = document.createElement('p');
  messageEl.classList.add('tf-confirm-dialog__message');
  const applyMessage = () => {
    const v = resolveBindRef(messageBind, ctx.store);
    messageEl.textContent = v == null ? '' : String(v);
  };
  applyMessage();
  ctx.registerCleanup(subscribeBindRef(messageBind, ctx.store, applyMessage));
  bodyEl.appendChild(messageEl);

  let typingInput = null;
  if (requireTyping != null) {
    typingInput = document.createElement('tf-input');
    typingInput.classList.add('tf-confirm-dialog__typing');
    typingInput.setAttribute('label', requireTyping);
    typingInput.setAttribute('autocomplete', 'off');
    bodyEl.appendChild(typingInput);
  }

  el.appendChild(bodyEl);

  // tf-input emits CustomEvent('input', {detail:{value}}); fall back to the
  // element value property when the event carries no detail.
  const typedValue = (e) => {
    if (e && e.detail && typeof e.detail.value === 'string') return e.detail.value;
    return typingInput && typingInput.value != null ? String(typingInput.value) : '';
  };

  // Footer: cancel + confirm tf-buttons.
  const footerEl = document.createElement('div');
  footerEl.setAttribute('slot', 'footer');
  footerEl.classList.add('tf-confirm-dialog__actions');

  const dispatchDismiss = () => {
    el.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('dismiss', {
      bubbles: false,
    }));
  };

  const cancelBtn = document.createElement('tf-button');
  cancelBtn.setAttribute('variant', 'ghost');
  cancelBtn.classList.add('tf-confirm-dialog__cancel');
  bindAttribute(cancelBtn, 'label', cancelLabelBind, ctx);
  cancelBtn.addEventListener('click', dispatchDismiss);
  ctx.registerCleanup(() => cancelBtn.removeEventListener('click', dispatchDismiss));
  footerEl.appendChild(cancelBtn);

  const confirmBtn = document.createElement('tf-button');
  confirmBtn.setAttribute('variant', destructive ? 'danger-solid' : 'primary');
  confirmBtn.classList.add('tf-confirm-dialog__confirm');
  bindAttribute(confirmBtn, 'label', confirmLabelBind, ctx);

  let typedMatches = requireTyping == null;
  const onConfirm = () => {
    if (!typedMatches) return;
    el.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('confirm', {
      bubbles: false,
    }));
  };
  confirmBtn.addEventListener('click', onConfirm);
  ctx.registerCleanup(() => confirmBtn.removeEventListener('click', onConfirm));

  // Confirm stays disabled until the typed value matches require_typing.
  if (requireTyping != null) {
    confirmBtn.setAttribute('disabled', '');
    const onInput = (e) => {
      typedMatches = typedValue(e) === requireTyping;
      if (typedMatches) {
        confirmBtn.removeAttribute('disabled');
      } else {
        confirmBtn.setAttribute('disabled', '');
      }
    };
    typingInput.addEventListener('input', onInput);
    ctx.registerCleanup(() => typingInput.removeEventListener('input', onInput));
  }

  footerEl.appendChild(confirmBtn);
  el.appendChild(footerEl);

  // ESC / backdrop come from tf-modal → bridge to 'dismiss'.
  bridgeCloseToDismiss(el, ctx);
  el.setAttribute('open', '');

  return el;
}

// =============================================================================
// Registration
// =============================================================================

export function registerFeedbackOverlayRenderers() {
  if (!lookupComponentRenderer(MODAL_TAG)) registerComponentRenderer(MODAL_TAG, renderModal);
  if (!lookupComponentRenderer(DRAWER_TAG)) registerComponentRenderer(DRAWER_TAG, renderDrawer);
  if (!lookupComponentRenderer(POPOVER_TAG)) registerComponentRenderer(POPOVER_TAG, renderPopover);
  if (!lookupComponentRenderer(SHEET_TAG)) registerComponentRenderer(SHEET_TAG, renderSheet);
  if (!lookupComponentRenderer(GATE_SCREEN_TAG)) registerComponentRenderer(GATE_SCREEN_TAG, renderGateScreen);
  if (!lookupComponentRenderer(CONFIRMATION_DIALOG_TAG)) registerComponentRenderer(CONFIRMATION_DIALOG_TAG, renderConfirmationDialog);
}
