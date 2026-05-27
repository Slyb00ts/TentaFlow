// =============================================================================
// File: sdk-runtime/feedback-overlay-renderer.js
// Description: Renderers for overlay/gate feedback components: Modal (0x0509),
// Drawer (0x050A), Popover (0x050B), Sheet (0x050C), GateScreen (0x050D),
// ConfirmationDialog (0x050E) — chunk 3.3e-3.
//
// Modal/Drawer/Popover/Sheet use data-slot-id containers that the slot manager
// (chunk 3.5) fills at runtime. Dismissible overlays dispatch a 'dismiss'
// CustomEvent on backdrop click and ESC key.
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

// =============================================================================
// Modal (0x0509)
// =============================================================================

export const MODAL_TAG = 0x0509;
const MODAL_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7]);
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

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-modal');
  wrapper.setAttribute('role', 'dialog');
  wrapper.setAttribute('aria-modal', 'true');

  const backdrop = document.createElement('div');
  backdrop.classList.add('tf-modal__backdrop');
  wrapper.appendChild(backdrop);

  const container = document.createElement('div');
  container.classList.add('tf-modal__container', `tf-modal--size-${size}`);

  if (preventScroll) wrapper.classList.add('tf-modal--prevent-scroll');

  // Header
  const header = document.createElement('div');
  header.classList.add('tf-modal__header');

  const titleEl = document.createElement('h2');
  titleEl.classList.add('tf-modal__title');
  const applyTitle = () => {
    const v = resolveBindRef(titleBind, ctx.store);
    titleEl.textContent = v == null ? '' : String(v);
  };
  applyTitle();
  ctx.registerCleanup(subscribeBindRef(titleBind, ctx.store, applyTitle));
  header.appendChild(titleEl);

  if (subtitleBind != null) {
    const subtitleEl = document.createElement('p');
    subtitleEl.classList.add('tf-modal__subtitle');
    const applySubtitle = () => {
      const v = resolveBindRef(subtitleBind, ctx.store);
      subtitleEl.textContent = v == null ? '' : String(v);
    };
    applySubtitle();
    ctx.registerCleanup(subscribeBindRef(subtitleBind, ctx.store, applySubtitle));
    header.appendChild(subtitleEl);
  }

  const dispatchDismiss = () => {
    wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('dismiss', {
      bubbles: false,
    }));
  };

  if (closable) {
    const closeBtn = document.createElement('button');
    closeBtn.type = 'button';
    closeBtn.classList.add('tf-modal__close');
    closeBtn.setAttribute('aria-label', 'Close');
    closeBtn.textContent = '×';
    closeBtn.addEventListener('click', dispatchDismiss);
    ctx.registerCleanup(() => closeBtn.removeEventListener('click', dispatchDismiss));
    header.appendChild(closeBtn);
  }

  container.appendChild(header);

  // Body slot
  const bodyEl = document.createElement('div');
  bodyEl.classList.add('tf-modal__body');
  bodyEl.setAttribute('data-slot-id', bodySlot);
  container.appendChild(bodyEl);

  // Footer slot
  if (footerSlot != null) {
    const footerEl = document.createElement('div');
    footerEl.classList.add('tf-modal__footer');
    footerEl.setAttribute('data-slot-id', footerSlot);
    container.appendChild(footerEl);
  }

  wrapper.appendChild(container);

  // Dismissible: backdrop click + ESC
  if (dismissible) {
    const onBackdrop = (e) => {
      if (e.target === backdrop) dispatchDismiss();
    };
    backdrop.addEventListener('click', onBackdrop);
    ctx.registerCleanup(() => backdrop.removeEventListener('click', onBackdrop));

    const onEsc = (e) => {
      if (e.key === 'Escape') dispatchDismiss();
    };
    document.addEventListener('keydown', onEsc);
    ctx.registerCleanup(() => document.removeEventListener('keydown', onEsc));
  }

  return wrapper;
}

// =============================================================================
// Drawer (0x050A)
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

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-drawer', `tf-drawer--side-${side}`, `tf-drawer--size-${size}`);
  wrapper.setAttribute('role', 'dialog');
  wrapper.setAttribute('aria-modal', 'true');

  const backdrop = document.createElement('div');
  backdrop.classList.add('tf-drawer__backdrop');
  wrapper.appendChild(backdrop);

  const container = document.createElement('div');
  container.classList.add('tf-drawer__container');

  const dispatchDismiss = () => {
    wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('dismiss', {
      bubbles: false,
    }));
  };

  // Header (with optional title)
  if (titleBind != null) {
    const header = document.createElement('div');
    header.classList.add('tf-drawer__header');

    const titleEl = document.createElement('h2');
    titleEl.classList.add('tf-drawer__title');
    const applyTitle = () => {
      const v = resolveBindRef(titleBind, ctx.store);
      titleEl.textContent = v == null ? '' : String(v);
    };
    applyTitle();
    ctx.registerCleanup(subscribeBindRef(titleBind, ctx.store, applyTitle));
    header.appendChild(titleEl);
    container.appendChild(header);
  }

  // Body slot
  const bodyEl = document.createElement('div');
  bodyEl.classList.add('tf-drawer__body');
  bodyEl.setAttribute('data-slot-id', bodySlot);
  container.appendChild(bodyEl);

  // Footer slot
  if (footerSlot != null) {
    const footerEl = document.createElement('div');
    footerEl.classList.add('tf-drawer__footer');
    footerEl.setAttribute('data-slot-id', footerSlot);
    container.appendChild(footerEl);
  }

  wrapper.appendChild(container);

  // Dismissible: backdrop click + ESC
  if (dismissible) {
    const onBackdrop = (e) => {
      if (e.target === backdrop) dispatchDismiss();
    };
    backdrop.addEventListener('click', onBackdrop);
    ctx.registerCleanup(() => backdrop.removeEventListener('click', onBackdrop));

    const onEsc = (e) => {
      if (e.key === 'Escape') dispatchDismiss();
    };
    document.addEventListener('keydown', onEsc);
    ctx.registerCleanup(() => document.removeEventListener('keydown', onEsc));
  }

  return wrapper;
}

// =============================================================================
// Popover (0x050B)
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

  const dispatchDismiss = () => {
    wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('dismiss', {
      bubbles: false,
    }));
  };

  if (dismissible) {
    const onEsc = (e) => {
      if (e.key === 'Escape') dispatchDismiss();
    };
    document.addEventListener('keydown', onEsc);
    ctx.registerCleanup(() => document.removeEventListener('keydown', onEsc));
  }

  return wrapper;
}

// =============================================================================
// Sheet (0x050C)
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

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-sheet');
  wrapper.setAttribute('role', 'dialog');
  wrapper.setAttribute('aria-modal', 'true');

  const backdrop = document.createElement('div');
  backdrop.classList.add('tf-sheet__backdrop');
  wrapper.appendChild(backdrop);

  const container = document.createElement('div');
  container.classList.add('tf-sheet__container');

  // Apply current detent class reactively
  const applyDetent = () => {
    for (const d of SHEET_DETENTS) container.classList.remove(`tf-sheet--detent-${d}`);
    let detent = detentsRaw[0];
    if (currentDetentBind != null) {
      const v = resolveBindRef(currentDetentBind, ctx.store);
      if (v != null && SHEET_DETENTS.has(String(v))) detent = String(v);
    }
    container.classList.add(`tf-sheet--detent-${detent}`);
  };
  applyDetent();
  if (currentDetentBind != null) {
    ctx.registerCleanup(subscribeBindRef(currentDetentBind, ctx.store, applyDetent));
  }

  // Handle
  const handle = document.createElement('div');
  handle.classList.add('tf-sheet__handle');
  container.appendChild(handle);

  // Title
  if (titleBind != null) {
    const header = document.createElement('div');
    header.classList.add('tf-sheet__header');
    const titleEl = document.createElement('h2');
    titleEl.classList.add('tf-sheet__title');
    const applyTitle = () => {
      const v = resolveBindRef(titleBind, ctx.store);
      titleEl.textContent = v == null ? '' : String(v);
    };
    applyTitle();
    ctx.registerCleanup(subscribeBindRef(titleBind, ctx.store, applyTitle));
    header.appendChild(titleEl);
    container.appendChild(header);
  }

  // Body slot
  const bodyEl = document.createElement('div');
  bodyEl.classList.add('tf-sheet__body');
  bodyEl.setAttribute('data-slot-id', bodySlot);
  container.appendChild(bodyEl);

  // Footer slot
  if (footerSlot != null) {
    const footerEl = document.createElement('div');
    footerEl.classList.add('tf-sheet__footer');
    footerEl.setAttribute('data-slot-id', footerSlot);
    container.appendChild(footerEl);
  }

  wrapper.appendChild(container);

  const dispatchDismiss = () => {
    wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('dismiss', {
      bubbles: false,
    }));
  };

  if (dismissible) {
    const onBackdrop = (e) => {
      if (e.target === backdrop) dispatchDismiss();
    };
    backdrop.addEventListener('click', onBackdrop);
    ctx.registerCleanup(() => backdrop.removeEventListener('click', onBackdrop));

    const onEsc = (e) => {
      if (e.key === 'Escape') dispatchDismiss();
    };
    document.addEventListener('keydown', onEsc);
    ctx.registerCleanup(() => document.removeEventListener('keydown', onEsc));
  }

  return wrapper;
}

// =============================================================================
// GateScreen (0x050D)
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
// ConfirmationDialog (0x050E)
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

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-confirm-dialog', `tf-confirm-dialog--tone-${tone}`);
  wrapper.setAttribute('role', 'alertdialog');
  wrapper.setAttribute('aria-modal', 'true');

  const backdrop = document.createElement('div');
  backdrop.classList.add('tf-confirm-dialog__backdrop');
  wrapper.appendChild(backdrop);

  const container = document.createElement('div');
  container.classList.add('tf-confirm-dialog__container');

  // Icon
  if (iconRaw != null) {
    const iconEl = renderIcon(iconRaw, 'ConfirmationDialog.icon');
    iconEl.classList.add('tf-confirm-dialog__icon');
    container.appendChild(iconEl);
  }

  // Title
  const titleEl = document.createElement('h2');
  titleEl.classList.add('tf-confirm-dialog__title');
  const applyTitle = () => {
    const v = resolveBindRef(titleBind, ctx.store);
    titleEl.textContent = v == null ? '' : String(v);
  };
  applyTitle();
  ctx.registerCleanup(subscribeBindRef(titleBind, ctx.store, applyTitle));
  container.appendChild(titleEl);

  // Message
  const messageEl = document.createElement('p');
  messageEl.classList.add('tf-confirm-dialog__message');
  const applyMessage = () => {
    const v = resolveBindRef(messageBind, ctx.store);
    messageEl.textContent = v == null ? '' : String(v);
  };
  applyMessage();
  ctx.registerCleanup(subscribeBindRef(messageBind, ctx.store, applyMessage));
  container.appendChild(messageEl);

  // Require typing input
  let typingInput = null;
  if (requireTyping != null) {
    const typingWrap = document.createElement('div');
    typingWrap.classList.add('tf-confirm-dialog__typing');

    const typingLabel = document.createElement('label');
    typingLabel.classList.add('tf-confirm-dialog__typing-label');
    typingLabel.textContent = requireTyping;
    typingWrap.appendChild(typingLabel);

    typingInput = document.createElement('input');
    typingInput.type = 'text';
    typingInput.classList.add('tf-confirm-dialog__typing-input');
    typingInput.setAttribute('autocomplete', 'off');
    typingWrap.appendChild(typingInput);

    container.appendChild(typingWrap);
  }

  // Actions
  const actionsEl = document.createElement('div');
  actionsEl.classList.add('tf-confirm-dialog__actions');

  const cancelBtn = document.createElement('button');
  cancelBtn.type = 'button';
  cancelBtn.classList.add('tf-confirm-dialog__cancel');
  const applyCancelLabel = () => {
    const v = resolveBindRef(cancelLabelBind, ctx.store);
    cancelBtn.textContent = v == null ? '' : String(v);
  };
  applyCancelLabel();
  ctx.registerCleanup(subscribeBindRef(cancelLabelBind, ctx.store, applyCancelLabel));
  const onCancel = () => {
    wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('dismiss', {
      bubbles: false,
    }));
  };
  cancelBtn.addEventListener('click', onCancel);
  ctx.registerCleanup(() => cancelBtn.removeEventListener('click', onCancel));
  actionsEl.appendChild(cancelBtn);

  const confirmBtn = document.createElement('button');
  confirmBtn.type = 'button';
  confirmBtn.classList.add('tf-confirm-dialog__confirm');
  if (destructive) confirmBtn.classList.add('tf-confirm-dialog__confirm--destructive');
  const applyConfirmLabel = () => {
    const v = resolveBindRef(confirmLabelBind, ctx.store);
    confirmBtn.textContent = v == null ? '' : String(v);
  };
  applyConfirmLabel();
  ctx.registerCleanup(subscribeBindRef(confirmLabelBind, ctx.store, applyConfirmLabel));
  const onConfirm = () => {
    if (requireTyping != null && typingInput.value !== requireTyping) return;
    wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('confirm', {
      bubbles: false,
    }));
  };
  confirmBtn.addEventListener('click', onConfirm);
  ctx.registerCleanup(() => confirmBtn.removeEventListener('click', onConfirm));

  // When require_typing is set, confirm is disabled until input matches
  if (requireTyping != null) {
    confirmBtn.setAttribute('disabled', '');
    const onInput = () => {
      if (typingInput.value === requireTyping) {
        confirmBtn.removeAttribute('disabled');
      } else {
        confirmBtn.setAttribute('disabled', '');
      }
    };
    typingInput.addEventListener('input', onInput);
    ctx.registerCleanup(() => typingInput.removeEventListener('input', onInput));
  }

  actionsEl.appendChild(confirmBtn);
  container.appendChild(actionsEl);

  wrapper.appendChild(container);

  // ESC dispatches dismiss
  const onEsc = (e) => {
    if (e.key === 'Escape') onCancel();
  };
  document.addEventListener('keydown', onEsc);
  ctx.registerCleanup(() => document.removeEventListener('keydown', onEsc));

  return wrapper;
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
