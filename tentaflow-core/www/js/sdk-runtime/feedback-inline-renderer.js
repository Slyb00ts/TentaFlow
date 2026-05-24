// =============================================================================
// File: sdk-runtime/feedback-inline-renderer.js
// Description: Renderers for inline feedback components: Alert (0x0501),
// Banner (0x0502), Callout (0x0503), Toast (0x0504), Hint (0x0505),
// OfflineBanner (0x050F) — chunk 3.3e-1.
//
// All components use tone-based BEM modifiers, reactive BindRef fields,
// optional IconRef, and (where applicable) recursive child rendering via
// ctx.renderChild(). Dismissible components dispatch a 'dismiss' CustomEvent.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/feedback/inline.rs.
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
// Alert (0x0501)
// =============================================================================

export const ALERT_TAG = 0x0501;
const ALERT_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);
const ALERT_VARIANTS = new Set(['default', 'filled', 'outlined', 'soft']);

function renderAlert(component, ctx) {
  assertOnlyKnownFields(component.fields, ALERT_FIELD_KEYS, 'Alert');

  const tone = requireEnum(ctx.readField(component.fields, 0), TONES, 'Alert.tone');
  const variant = requireEnum(ctx.readField(component.fields, 1), ALERT_VARIANTS, 'Alert.variant');
  const iconRaw = ctx.readField(component.fields, 2);
  const titleBind = ctx.readField(component.fields, 3);
  const messageBind = ctx.readField(component.fields, 4);
  if (messageBind == null) throw new TypeError('Alert.message is required (BindRef)');
  assertBindRef(messageBind, 'Alert.message');
  if (titleBind != null) assertBindRef(titleBind, 'Alert.title');
  const actionsRaw = ctx.readField(component.fields, 5);
  const dismissible = requireBool(ctx.readField(component.fields, 6), 'Alert.dismissible');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-alert', `tf-alert--tone-${tone}`, `tf-alert--variant-${variant}`);
  wrapper.setAttribute('role', 'alert');

  if (iconRaw != null) {
    const iconEl = renderIcon(iconRaw, 'Alert.icon');
    iconEl.classList.add('tf-alert__icon');
    wrapper.appendChild(iconEl);
  }

  const body = document.createElement('div');
  body.classList.add('tf-alert__body');

  if (titleBind != null) {
    const titleEl = document.createElement('div');
    titleEl.classList.add('tf-alert__title');
    const applyTitle = () => {
      const v = resolveBindRef(titleBind, ctx.store);
      titleEl.textContent = v == null ? '' : String(v);
    };
    applyTitle();
    ctx.registerCleanup(subscribeBindRef(titleBind, ctx.store, applyTitle));
    body.appendChild(titleEl);
  }

  const messageEl = document.createElement('div');
  messageEl.classList.add('tf-alert__message');
  const applyMessage = () => {
    const v = resolveBindRef(messageBind, ctx.store);
    messageEl.textContent = v == null ? '' : String(v);
  };
  applyMessage();
  ctx.registerCleanup(subscribeBindRef(messageBind, ctx.store, applyMessage));
  body.appendChild(messageEl);

  if (actionsRaw != null) {
    if (!Array.isArray(actionsRaw)) throw new TypeError('Alert.actions must be array');
    const actionsEl = document.createElement('div');
    actionsEl.classList.add('tf-alert__actions');
    for (const actionComp of actionsRaw) {
      if (!actionComp || actionComp.tag !== BUTTON_TAG) throw new TypeError(`Alert.actions: children must be Button (0x${BUTTON_TAG.toString(16)})`);
      actionsEl.appendChild(ctx.renderChild(actionComp));
    }
    body.appendChild(actionsEl);
  }

  wrapper.appendChild(body);

  if (dismissible) {
    const closeBtn = document.createElement('button');
    closeBtn.type = 'button';
    closeBtn.classList.add('tf-alert__close');
    closeBtn.setAttribute('aria-label', 'Dismiss');
    closeBtn.textContent = '×';
    const onDismiss = () => {
      wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('dismiss', {
        bubbles: false,
      }));
    };
    closeBtn.addEventListener('click', onDismiss);
    ctx.registerCleanup(() => closeBtn.removeEventListener('click', onDismiss));
    wrapper.appendChild(closeBtn);
  }

  return wrapper;
}

// =============================================================================
// Banner (0x0502)
// =============================================================================

export const BANNER_TAG = 0x0502;
const BANNER_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);
const BANNER_POSITIONS = new Set(['top', 'inline']);

function renderBanner(component, ctx) {
  assertOnlyKnownFields(component.fields, BANNER_FIELD_KEYS, 'Banner');

  const tone = requireEnum(ctx.readField(component.fields, 0), TONES, 'Banner.tone');
  const iconRaw = ctx.readField(component.fields, 1);
  const messageBind = ctx.readField(component.fields, 2);
  if (messageBind == null) throw new TypeError('Banner.message is required (BindRef)');
  assertBindRef(messageBind, 'Banner.message');
  const actionRaw = ctx.readField(component.fields, 3);
  const dismissible = requireBool(ctx.readField(component.fields, 4), 'Banner.dismissible');
  const position = requireEnum(ctx.readField(component.fields, 5), BANNER_POSITIONS, 'Banner.position');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-banner', `tf-banner--tone-${tone}`, `tf-banner--position-${position}`);
  wrapper.setAttribute('role', 'banner');

  if (iconRaw != null) {
    const iconEl = renderIcon(iconRaw, 'Banner.icon');
    iconEl.classList.add('tf-banner__icon');
    wrapper.appendChild(iconEl);
  }

  const messageEl = document.createElement('span');
  messageEl.classList.add('tf-banner__message');
  const applyMessage = () => {
    const v = resolveBindRef(messageBind, ctx.store);
    messageEl.textContent = v == null ? '' : String(v);
  };
  applyMessage();
  ctx.registerCleanup(subscribeBindRef(messageBind, ctx.store, applyMessage));
  wrapper.appendChild(messageEl);

  if (actionRaw != null) {
    if (!actionRaw || actionRaw.tag !== BUTTON_TAG) throw new TypeError(`Banner.action must be Button (0x${BUTTON_TAG.toString(16)})`);
    const actionEl = ctx.renderChild(actionRaw);
    actionEl.classList.add('tf-banner__action');
    wrapper.appendChild(actionEl);
  }

  if (dismissible) {
    const closeBtn = document.createElement('button');
    closeBtn.type = 'button';
    closeBtn.classList.add('tf-banner__close');
    closeBtn.setAttribute('aria-label', 'Dismiss');
    closeBtn.textContent = '×';
    const onDismiss = () => {
      wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('dismiss', {
        bubbles: false,
      }));
    };
    closeBtn.addEventListener('click', onDismiss);
    ctx.registerCleanup(() => closeBtn.removeEventListener('click', onDismiss));
    wrapper.appendChild(closeBtn);
  }

  return wrapper;
}

// =============================================================================
// Callout (0x0503)
// =============================================================================

export const CALLOUT_TAG = 0x0503;
const CALLOUT_FIELD_KEYS = new Set([0, 1, 2, 3]);

function renderCallout(component, ctx) {
  assertOnlyKnownFields(component.fields, CALLOUT_FIELD_KEYS, 'Callout');

  const tone = requireEnum(ctx.readField(component.fields, 0), TONES, 'Callout.tone');
  const iconRaw = ctx.readField(component.fields, 1);
  const titleBind = ctx.readField(component.fields, 2);
  if (titleBind != null) assertBindRef(titleBind, 'Callout.title');
  const contentRaw = ctx.readField(component.fields, 3);
  if (contentRaw == null || !Array.isArray(contentRaw)) {
    throw new TypeError('Callout.content is required (Vec<Component>)');
  }

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-callout', `tf-callout--tone-${tone}`);

  if (iconRaw != null) {
    const iconEl = renderIcon(iconRaw, 'Callout.icon');
    iconEl.classList.add('tf-callout__icon');
    wrapper.appendChild(iconEl);
  }

  const body = document.createElement('div');
  body.classList.add('tf-callout__body');

  if (titleBind != null) {
    const titleEl = document.createElement('div');
    titleEl.classList.add('tf-callout__title');
    const applyTitle = () => {
      const v = resolveBindRef(titleBind, ctx.store);
      titleEl.textContent = v == null ? '' : String(v);
    };
    applyTitle();
    ctx.registerCleanup(subscribeBindRef(titleBind, ctx.store, applyTitle));
    body.appendChild(titleEl);
  }

  const contentEl = document.createElement('div');
  contentEl.classList.add('tf-callout__content');
  for (const childComp of contentRaw) {
    contentEl.appendChild(ctx.renderChild(childComp));
  }
  body.appendChild(contentEl);

  wrapper.appendChild(body);
  return wrapper;
}

// =============================================================================
// Toast (0x0504)
// =============================================================================

export const TOAST_TAG = 0x0504;
const TOAST_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);

function renderToast(component, ctx) {
  assertOnlyKnownFields(component.fields, TOAST_FIELD_KEYS, 'Toast');

  const tone = requireEnum(ctx.readField(component.fields, 0), TONES, 'Toast.tone');
  const titleBind = ctx.readField(component.fields, 1);
  if (titleBind == null) throw new TypeError('Toast.title is required (BindRef)');
  assertBindRef(titleBind, 'Toast.title');
  const bodyBind = ctx.readField(component.fields, 2);
  if (bodyBind != null) assertBindRef(bodyBind, 'Toast.body');
  const iconRaw = ctx.readField(component.fields, 3);
  const actionLabelRaw = ctx.readField(component.fields, 4);
  const actionIdRaw = ctx.readField(component.fields, 5);
  if (actionLabelRaw != null) requireString(actionLabelRaw, 'Toast.action_label');
  if (actionIdRaw != null) requireString(actionIdRaw, 'Toast.action_id');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-toast', `tf-toast--tone-${tone}`);
  wrapper.setAttribute('role', 'status');
  wrapper.setAttribute('aria-live', 'polite');

  if (iconRaw != null) {
    const iconEl = renderIcon(iconRaw, 'Toast.icon');
    iconEl.classList.add('tf-toast__icon');
    wrapper.appendChild(iconEl);
  }

  const body = document.createElement('div');
  body.classList.add('tf-toast__body');

  const titleEl = document.createElement('div');
  titleEl.classList.add('tf-toast__title');
  const applyTitle = () => {
    const v = resolveBindRef(titleBind, ctx.store);
    titleEl.textContent = v == null ? '' : String(v);
  };
  applyTitle();
  ctx.registerCleanup(subscribeBindRef(titleBind, ctx.store, applyTitle));
  body.appendChild(titleEl);

  if (bodyBind != null) {
    const bodyEl = document.createElement('div');
    bodyEl.classList.add('tf-toast__message');
    const applyBody = () => {
      const v = resolveBindRef(bodyBind, ctx.store);
      bodyEl.textContent = v == null ? '' : String(v);
    };
    applyBody();
    ctx.registerCleanup(subscribeBindRef(bodyBind, ctx.store, applyBody));
    body.appendChild(bodyEl);
  }

  wrapper.appendChild(body);

  if (actionLabelRaw != null && actionIdRaw != null) {
    const actionBtn = document.createElement('button');
    actionBtn.type = 'button';
    actionBtn.classList.add('tf-toast__action');
    actionBtn.textContent = actionLabelRaw;
    const onAction = () => {
      wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('toast_action', {
        bubbles: false,
        detail: { action_id: actionIdRaw },
      }));
    };
    actionBtn.addEventListener('click', onAction);
    ctx.registerCleanup(() => actionBtn.removeEventListener('click', onAction));
    wrapper.appendChild(actionBtn);
  }

  return wrapper;
}

// =============================================================================
// Hint (0x0505)
// =============================================================================

export const HINT_TAG = 0x0505;
const HINT_FIELD_KEYS = new Set([0, 1, 2]);

function renderHint(component, ctx) {
  assertOnlyKnownFields(component.fields, HINT_FIELD_KEYS, 'Hint');

  const contentBind = ctx.readField(component.fields, 0);
  if (contentBind == null) throw new TypeError('Hint.content is required (BindRef)');
  assertBindRef(contentBind, 'Hint.content');
  const iconRaw = ctx.readField(component.fields, 1);
  const tone = requireEnum(ctx.readField(component.fields, 2), TONES, 'Hint.tone');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-hint');
  wrapper.classList.add(`tf-hint--tone-${tone}`);

  if (iconRaw != null) {
    const iconEl = renderIcon(iconRaw, 'Hint.icon');
    iconEl.classList.add('tf-hint__icon');
    wrapper.appendChild(iconEl);
  }

  const contentEl = document.createElement('span');
  contentEl.classList.add('tf-hint__content');
  const applyContent = () => {
    const v = resolveBindRef(contentBind, ctx.store);
    contentEl.textContent = v == null ? '' : String(v);
  };
  applyContent();
  ctx.registerCleanup(subscribeBindRef(contentBind, ctx.store, applyContent));
  wrapper.appendChild(contentEl);

  return wrapper;
}

// =============================================================================
// OfflineBanner (0x050F)
// =============================================================================

export const OFFLINE_BANNER_TAG = 0x050F;
const OFFLINE_BANNER_FIELD_KEYS = new Set([0, 1, 2]);

function renderOfflineBanner(component, ctx) {
  assertOnlyKnownFields(component.fields, OFFLINE_BANNER_FIELD_KEYS, 'OfflineBanner');

  const messageBind = ctx.readField(component.fields, 0);
  if (messageBind == null) throw new TypeError('OfflineBanner.message is required (BindRef)');
  assertBindRef(messageBind, 'OfflineBanner.message');
  const actionLabelBind = ctx.readField(component.fields, 1);
  if (actionLabelBind != null) assertBindRef(actionLabelBind, 'OfflineBanner.action_label');
  const reconnectingBind = ctx.readField(component.fields, 2);
  if (reconnectingBind == null) throw new TypeError('OfflineBanner.reconnecting is required (BindRef)');
  assertBindRef(reconnectingBind, 'OfflineBanner.reconnecting');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-offline-banner');
  wrapper.setAttribute('role', 'status');

  const spinnerEl = document.createElement('span');
  spinnerEl.classList.add('tf-offline-banner__spinner');
  wrapper.appendChild(spinnerEl);

  const messageEl = document.createElement('span');
  messageEl.classList.add('tf-offline-banner__message');
  wrapper.appendChild(messageEl);

  let actionBtn = null;
  if (actionLabelBind != null) {
    actionBtn = document.createElement('button');
    actionBtn.type = 'button';
    actionBtn.classList.add('tf-offline-banner__action');
    const onAction = () => {
      wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('offline_action', {
        bubbles: false,
      }));
    };
    actionBtn.addEventListener('click', onAction);
    ctx.registerCleanup(() => actionBtn.removeEventListener('click', onAction));
    wrapper.appendChild(actionBtn);

    const applyActionLabel = () => {
      const v = resolveBindRef(actionLabelBind, ctx.store);
      actionBtn.textContent = v == null ? '' : String(v);
    };
    applyActionLabel();
    ctx.registerCleanup(subscribeBindRef(actionLabelBind, ctx.store, applyActionLabel));
  }

  const applyMessage = () => {
    const v = resolveBindRef(messageBind, ctx.store);
    messageEl.textContent = v == null ? '' : String(v);
  };
  applyMessage();
  ctx.registerCleanup(subscribeBindRef(messageBind, ctx.store, applyMessage));

  const applyReconnecting = () => {
    const v = resolveBindRef(reconnectingBind, ctx.store);
    const isReconnecting = !!v;
    wrapper.classList.toggle('tf-offline-banner--reconnecting', isReconnecting);
  };
  applyReconnecting();
  ctx.registerCleanup(subscribeBindRef(reconnectingBind, ctx.store, applyReconnecting));

  return wrapper;
}

// =============================================================================
// Registration
// =============================================================================

export function registerFeedbackInlineRenderers() {
  if (!lookupComponentRenderer(ALERT_TAG)) registerComponentRenderer(ALERT_TAG, renderAlert);
  if (!lookupComponentRenderer(BANNER_TAG)) registerComponentRenderer(BANNER_TAG, renderBanner);
  if (!lookupComponentRenderer(CALLOUT_TAG)) registerComponentRenderer(CALLOUT_TAG, renderCallout);
  if (!lookupComponentRenderer(TOAST_TAG)) registerComponentRenderer(TOAST_TAG, renderToast);
  if (!lookupComponentRenderer(HINT_TAG)) registerComponentRenderer(HINT_TAG, renderHint);
  if (!lookupComponentRenderer(OFFLINE_BANNER_TAG)) registerComponentRenderer(OFFLINE_BANNER_TAG, renderOfflineBanner);
}
