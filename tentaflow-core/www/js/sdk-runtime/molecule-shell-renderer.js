// =============================================================================
// File: sdk-runtime/molecule-shell-renderer.js
// Description: Renderers for shell/empty molecule components: AppShell (0x0006),
// LoginShell (0x0007), WizardShell (0x000B), EmptyState (0x0003),
// ErrorBoundary (0x0008), WelcomeHero (0x0009) — chunk 3.3f.
//
// EmptyState renders the dashboard <tf-empty-state> web component; the
// remaining entries are page-level structural shells without tf-* equivalents
// (interactive children still render as tf-* components, e.g. <tf-button>).
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/molecules/shell.rs,
//           tentaflow-sdk-spec/src/protocol/ui/molecules/empty.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef, assertBindRef } from './bind-resolver.js';
import {
  requireEnum, requireBool, requireString,
  assertOnlyKnownFields,
} from './data-chart-shared.js';
import { renderIcon } from './icon-renderer.js';
import { BUTTON_TAG } from './action-button-renderer.js';

const SPACINGS = new Set(['zero', 'xxs', 'xs', 'sm', 'md', 'lg', 'xl', 'xxl']);
const EMPTY_STATE_VARIANTS = new Set(['default', 'compact', 'illustrated']);

function assertComponentTag(c, expectedTag, parent, field) {
  if (!c || typeof c !== 'object' || Array.isArray(c)) {
    throw new TypeError(`${parent}.${field}: expected Component`);
  }
  if (c.tag !== expectedTag) {
    throw new TypeError(
      `${parent}.${field}: expected tag 0x${expectedTag.toString(16)}, got 0x${(c.tag || 0).toString(16)}`
    );
  }
}

function assertComponentArrayTag(arr, expectedTag, parent, field) {
  if (!Array.isArray(arr)) return;
  for (const c of arr) assertComponentTag(c, expectedTag, parent, field);
}

function applyTextBind(element, bindRef, ctx) {
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    element.textContent = v == null ? '' : String(v);
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

// Reactive BindRef → attribute mirror (web components re-render through
// attributeChangedCallback, e.g. <tf-empty-state title/message>).
function applyAttrBind(element, attrName, bindRef, ctx) {
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    element.setAttribute(attrName, v == null ? '' : String(v));
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

// =============================================================================
// AppShell (0x0006) — 5 fields
// =============================================================================

export const APP_SHELL_TAG = 0x0006;
const APP_SHELL_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderAppShell(component, ctx) {
  assertOnlyKnownFields(component.fields, APP_SHELL_FIELD_KEYS, 'AppShell');

  const sidebarSlot = requireString(ctx.readField(component.fields, 0), 'AppShell.sidebar_slot');
  const contentSlot = requireString(ctx.readField(component.fields, 1), 'AppShell.content_slot');
  const headerSlotRaw = ctx.readField(component.fields, 2);
  const sidebarWidthRaw = ctx.readField(component.fields, 3);
  const sidebarWidth = sidebarWidthRaw == null ? 'xl' : requireEnum(sidebarWidthRaw, SPACINGS, 'AppShell.sidebar_width');
  const collapsibleSidebar = requireBool(ctx.readField(component.fields, 4), 'AppShell.collapsible_sidebar');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-app-shell');

  if (headerSlotRaw != null) {
    requireString(headerSlotRaw, 'AppShell.header_slot');
    const headerEl = document.createElement('div');
    headerEl.classList.add('tf-app-shell__header');
    headerEl.setAttribute('data-slot-id', headerSlotRaw);
    wrapper.appendChild(headerEl);
  }

  const body = document.createElement('div');
  body.classList.add('tf-app-shell__body');

  const sidebar = document.createElement('aside');
  sidebar.classList.add('tf-app-shell__sidebar', `tf-app-shell__sidebar--width-${sidebarWidth}`);
  sidebar.setAttribute('data-slot-id', sidebarSlot);

  if (collapsibleSidebar) {
    const toggle = document.createElement('tf-button');
    toggle.classList.add('tf-app-shell__sidebar-toggle');
    toggle.setAttribute('variant', 'ghost');
    toggle.setAttribute('size', 'sm');
    toggle.setAttribute('label', '☰');
    toggle.setAttribute('aria-label', 'Toggle sidebar');
    toggle.addEventListener('click', () => {
      const collapsed = wrapper.classList.toggle('tf-app-shell--sidebar-collapsed');
      toggle.setAttribute('aria-expanded', String(!collapsed));
    });
    toggle.setAttribute('aria-expanded', 'true');
    sidebar.appendChild(toggle);
  }

  body.appendChild(sidebar);

  const content = document.createElement('main');
  content.classList.add('tf-app-shell__content');
  content.setAttribute('data-slot-id', contentSlot);
  body.appendChild(content);

  wrapper.appendChild(body);

  return wrapper;
}

// =============================================================================
// LoginShell (0x0007) — 5 fields
// =============================================================================

export const LOGIN_SHELL_TAG = 0x0007;
const LOGIN_SHELL_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderLoginShell(component, ctx) {
  assertOnlyKnownFields(component.fields, LOGIN_SHELL_FIELD_KEYS, 'LoginShell');

  const logoRaw = ctx.readField(component.fields, 0);
  if (logoRaw == null) throw new TypeError('LoginShell.logo is required (IconRef)');
  const titleBind = ctx.readField(component.fields, 1);
  if (titleBind == null) throw new TypeError('LoginShell.title is required (BindRef)');
  assertBindRef(titleBind, 'LoginShell.title');
  const subtitleBind = ctx.readField(component.fields, 2);
  if (subtitleBind != null) assertBindRef(subtitleBind, 'LoginShell.subtitle');
  const contentSlot = requireString(ctx.readField(component.fields, 3), 'LoginShell.content_slot');
  const footerSlotRaw = ctx.readField(component.fields, 4);

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-login-shell');

  const card = document.createElement('div');
  card.classList.add('tf-login-shell__card');

  const logoEl = renderIcon(logoRaw, 'LoginShell.logo');
  logoEl.classList.add('tf-login-shell__logo');
  card.appendChild(logoEl);

  const titleEl = document.createElement('h1');
  titleEl.classList.add('tf-login-shell__title');
  applyTextBind(titleEl, titleBind, ctx);
  card.appendChild(titleEl);

  if (subtitleBind != null) {
    const subtitleEl = document.createElement('p');
    subtitleEl.classList.add('tf-login-shell__subtitle');
    applyTextBind(subtitleEl, subtitleBind, ctx);
    card.appendChild(subtitleEl);
  }

  const body = document.createElement('div');
  body.classList.add('tf-login-shell__content');
  body.setAttribute('data-slot-id', contentSlot);
  card.appendChild(body);

  if (footerSlotRaw != null) {
    requireString(footerSlotRaw, 'LoginShell.footer_slot');
    const footer = document.createElement('div');
    footer.classList.add('tf-login-shell__footer');
    footer.setAttribute('data-slot-id', footerSlotRaw);
    card.appendChild(footer);
  }

  wrapper.appendChild(card);

  return wrapper;
}

// =============================================================================
// WizardShell (0x000B) — 5 fields
// =============================================================================

export const WIZARD_SHELL_TAG = 0x000B;
const WIZARD_SHELL_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderWizardShell(component, ctx) {
  assertOnlyKnownFields(component.fields, WIZARD_SHELL_FIELD_KEYS, 'WizardShell');

  const stepsRaw = ctx.readField(component.fields, 0) || [];
  const currentStepBind = ctx.readField(component.fields, 1);
  if (currentStepBind == null) throw new TypeError('WizardShell.current_step_id is required (BindRef)');
  assertBindRef(currentStepBind, 'WizardShell.current_step_id');
  const contentSlot = requireString(ctx.readField(component.fields, 2), 'WizardShell.content_slot');
  const footerSlot = requireString(ctx.readField(component.fields, 3), 'WizardShell.footer_slot');
  const cancellable = requireBool(ctx.readField(component.fields, 4), 'WizardShell.cancellable');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-wizard-shell');

  // Steps indicator
  if (Array.isArray(stepsRaw) && stepsRaw.length > 0) {
    const nav = document.createElement('nav');
    nav.classList.add('tf-wizard-shell__steps');
    nav.setAttribute('aria-label', 'Wizard steps');

    const applyStepHighlight = () => {
      const currentId = resolveBindRef(currentStepBind, ctx.store);
      const items = nav.querySelectorAll('.tf-wizard-shell__step');
      for (const item of items) {
        item.classList.toggle('tf-wizard-shell__step--current', item.dataset.stepId === String(currentId));
      }
    };

    for (const step of stepsRaw) {
      if (step == null || typeof step !== 'object') continue;
      const stepEl = document.createElement('div');
      stepEl.classList.add('tf-wizard-shell__step');
      // WizardStep is an inline struct — [key, value] pairs on the wire.
      const stepIdRaw = ctx.readField(step, 0);
      stepEl.dataset.stepId = typeof stepIdRaw === 'string' ? stepIdRaw : '';
      if (ctx.readField(step, 2) === true) stepEl.classList.add('tf-wizard-shell__step--optional');

      const label = ctx.readField(step, 1);
      if (label != null) {
        const labelEl = document.createElement('span');
        labelEl.classList.add('tf-wizard-shell__step-label');
        applyTextBind(labelEl, label, ctx);
        stepEl.appendChild(labelEl);
      }

      const description = ctx.readField(step, 4);
      if (description != null) {
        const descEl = document.createElement('span');
        descEl.classList.add('tf-wizard-shell__step-desc');
        applyTextBind(descEl, description, ctx);
        stepEl.appendChild(descEl);
      }

      nav.appendChild(stepEl);
    }

    applyStepHighlight();
    ctx.registerCleanup(subscribeBindRef(currentStepBind, ctx.store, applyStepHighlight));

    wrapper.appendChild(nav);
  }

  const body = document.createElement('div');
  body.classList.add('tf-wizard-shell__content');
  body.setAttribute('data-slot-id', contentSlot);
  wrapper.appendChild(body);

  const footer = document.createElement('div');
  footer.classList.add('tf-wizard-shell__footer');
  footer.setAttribute('data-slot-id', footerSlot);
  wrapper.appendChild(footer);

  // State marker only — controls.css defines no cancellable modifier, so this
  // stays a data attribute instead of an unstyled class.
  if (cancellable) wrapper.setAttribute('data-cancellable', 'true');

  return wrapper;
}

// =============================================================================
// EmptyState (0x0003) — 6 fields, renders the dashboard <tf-empty-state>
// web component (icon goes in via slot="icon", CTA buttons as light-DOM
// children that the component sweeps into its actions area).
// =============================================================================

export const EMPTY_STATE_TAG = 0x0003;
const EMPTY_STATE_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);

function renderEmptyState(component, ctx) {
  assertOnlyKnownFields(component.fields, EMPTY_STATE_FIELD_KEYS, 'EmptyState');

  const iconRaw = ctx.readField(component.fields, 0);
  if (iconRaw == null) throw new TypeError('EmptyState.icon is required (IconRef)');
  const headingBind = ctx.readField(component.fields, 1);
  if (headingBind == null) throw new TypeError('EmptyState.heading is required (BindRef)');
  assertBindRef(headingBind, 'EmptyState.heading');
  const messageBind = ctx.readField(component.fields, 2);
  if (messageBind != null) assertBindRef(messageBind, 'EmptyState.message');
  const primaryActionRaw = ctx.readField(component.fields, 3);
  const secondaryActionRaw = ctx.readField(component.fields, 4);
  const variant = requireEnum(ctx.readField(component.fields, 5), EMPTY_STATE_VARIANTS, 'EmptyState.variant');

  if (primaryActionRaw != null) {
    assertComponentTag(primaryActionRaw, BUTTON_TAG, 'EmptyState', 'primary_action');
  }
  if (secondaryActionRaw != null) {
    assertComponentTag(secondaryActionRaw, BUTTON_TAG, 'EmptyState', 'secondary_action');
  }

  const wrapper = document.createElement('tf-empty-state');
  // 'default' carries no modifier — base component styling applies.
  if (variant !== 'default') {
    wrapper.classList.add(`tf-empty-state--variant-${variant}`);
  }
  wrapper.setAttribute('role', 'status');

  // renderIcon keeps full IconRef validation (named + asset) and size/tone
  // classes; the component honours a slot="icon" child over its icon attr.
  const iconSlot = document.createElement('span');
  iconSlot.setAttribute('slot', 'icon');
  iconSlot.appendChild(renderIcon(iconRaw, 'EmptyState.icon'));
  wrapper.appendChild(iconSlot);

  applyAttrBind(wrapper, 'title', headingBind, ctx);
  if (messageBind != null) {
    applyAttrBind(wrapper, 'message', messageBind, ctx);
  }

  if (primaryActionRaw != null) {
    wrapper.appendChild(ctx.renderChild(primaryActionRaw));
  }
  if (secondaryActionRaw != null) {
    wrapper.appendChild(ctx.renderChild(secondaryActionRaw));
  }

  return wrapper;
}

// =============================================================================
// ErrorBoundary (0x0008) — 5 fields
// =============================================================================

export const ERROR_BOUNDARY_TAG = 0x0008;
const ERROR_BOUNDARY_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderErrorBoundary(component, ctx) {
  assertOnlyKnownFields(component.fields, ERROR_BOUNDARY_FIELD_KEYS, 'ErrorBoundary');

  const errorCodeBind = ctx.readField(component.fields, 0);
  if (errorCodeBind != null) assertBindRef(errorCodeBind, 'ErrorBoundary.error_code');
  const titleBind = ctx.readField(component.fields, 1);
  if (titleBind == null) throw new TypeError('ErrorBoundary.title is required (BindRef)');
  assertBindRef(titleBind, 'ErrorBoundary.title');
  const messageBind = ctx.readField(component.fields, 2);
  if (messageBind != null) assertBindRef(messageBind, 'ErrorBoundary.message');
  const actionsRaw = ctx.readField(component.fields, 3) || [];
  const technicalDetailsBind = ctx.readField(component.fields, 4);
  if (technicalDetailsBind != null) assertBindRef(technicalDetailsBind, 'ErrorBoundary.technical_details');

  assertComponentArrayTag(actionsRaw, BUTTON_TAG, 'ErrorBoundary', 'actions');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-error-boundary');
  wrapper.setAttribute('role', 'alert');

  if (errorCodeBind != null) {
    const codeEl = document.createElement('span');
    codeEl.classList.add('tf-error-boundary__code');
    applyTextBind(codeEl, errorCodeBind, ctx);
    wrapper.appendChild(codeEl);
  }

  const titleEl = document.createElement('h2');
  titleEl.classList.add('tf-error-boundary__title');
  applyTextBind(titleEl, titleBind, ctx);
  wrapper.appendChild(titleEl);

  if (messageBind != null) {
    const msg = document.createElement('p');
    msg.classList.add('tf-error-boundary__message');
    applyTextBind(msg, messageBind, ctx);
    wrapper.appendChild(msg);
  }

  if (Array.isArray(actionsRaw) && actionsRaw.length > 0) {
    const actionsEl = document.createElement('div');
    actionsEl.classList.add('tf-error-boundary__actions');
    for (const actionComp of actionsRaw) {
      actionsEl.appendChild(ctx.renderChild(actionComp));
    }
    wrapper.appendChild(actionsEl);
  }

  if (technicalDetailsBind != null) {
    const details = document.createElement('details');
    details.classList.add('tf-error-boundary__details');
    const summary = document.createElement('summary');
    summary.textContent = 'Technical details';
    details.appendChild(summary);
    const pre = document.createElement('pre');
    pre.classList.add('tf-error-boundary__details-text');
    applyTextBind(pre, technicalDetailsBind, ctx);
    details.appendChild(pre);
    wrapper.appendChild(details);
  }

  return wrapper;
}

// =============================================================================
// WelcomeHero (0x0009) — 7 fields (illustration + title + subtitle + features +
// primary_action + secondary_action)
// =============================================================================

export const WELCOME_HERO_TAG = 0x0009;
const WELCOME_HERO_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);

function renderWelcomeHero(component, ctx) {
  assertOnlyKnownFields(component.fields, WELCOME_HERO_FIELD_KEYS, 'WelcomeHero');

  const illustrationRaw = ctx.readField(component.fields, 0);
  if (illustrationRaw == null) throw new TypeError('WelcomeHero.illustration is required (IconRef)');
  const titleBind = ctx.readField(component.fields, 1);
  if (titleBind == null) throw new TypeError('WelcomeHero.title is required (BindRef)');
  assertBindRef(titleBind, 'WelcomeHero.title');
  const subtitleBind = ctx.readField(component.fields, 2);
  if (subtitleBind == null) throw new TypeError('WelcomeHero.subtitle is required (BindRef)');
  assertBindRef(subtitleBind, 'WelcomeHero.subtitle');
  const featuresRaw = ctx.readField(component.fields, 3) || [];
  const primaryActionRaw = ctx.readField(component.fields, 4);
  if (primaryActionRaw == null) throw new TypeError('WelcomeHero.primary_action is required');
  assertComponentTag(primaryActionRaw, BUTTON_TAG, 'WelcomeHero', 'primary_action');
  const secondaryActionRaw = ctx.readField(component.fields, 5);
  if (secondaryActionRaw != null) {
    assertComponentTag(secondaryActionRaw, BUTTON_TAG, 'WelcomeHero', 'secondary_action');
  }

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-welcome-hero');

  const illustrationEl = renderIcon(illustrationRaw, 'WelcomeHero.illustration');
  illustrationEl.classList.add('tf-welcome-hero__illustration');
  wrapper.appendChild(illustrationEl);

  const titleEl = document.createElement('h1');
  titleEl.classList.add('tf-welcome-hero__title');
  applyTextBind(titleEl, titleBind, ctx);
  wrapper.appendChild(titleEl);

  const subtitleEl = document.createElement('p');
  subtitleEl.classList.add('tf-welcome-hero__subtitle');
  applyTextBind(subtitleEl, subtitleBind, ctx);
  wrapper.appendChild(subtitleEl);

  if (Array.isArray(featuresRaw) && featuresRaw.length > 0) {
    const featuresList = document.createElement('ul');
    featuresList.classList.add('tf-welcome-hero__features');
    for (const feat of featuresRaw) {
      if (feat == null || typeof feat !== 'object') continue;
      const li = document.createElement('li');
      li.classList.add('tf-welcome-hero__feature');

      // FeatureItem is an inline struct — decoded as [key, value] pairs,
      // read via ctx.readField like every other inline struct.
      const featIcon = ctx.readField(feat, 0);
      if (featIcon != null) {
        const iconEl = renderIcon(featIcon, 'FeatureItem.icon');
        iconEl.classList.add('tf-welcome-hero__feature-icon');
        li.appendChild(iconEl);
      }

      const featContent = document.createElement('div');
      featContent.classList.add('tf-welcome-hero__feature-content');
      const featTitle = ctx.readField(feat, 1);
      if (featTitle != null) {
        const ftEl = document.createElement('strong');
        ftEl.classList.add('tf-welcome-hero__feature-title');
        applyTextBind(ftEl, featTitle, ctx);
        featContent.appendChild(ftEl);
      }
      const featDesc = ctx.readField(feat, 2);
      if (featDesc != null) {
        const fdEl = document.createElement('span');
        fdEl.classList.add('tf-welcome-hero__feature-desc');
        applyTextBind(fdEl, featDesc, ctx);
        featContent.appendChild(fdEl);
      }
      li.appendChild(featContent);
      featuresList.appendChild(li);
    }
    wrapper.appendChild(featuresList);
  }

  const actionsEl = document.createElement('div');
  actionsEl.classList.add('tf-welcome-hero__actions');
  actionsEl.appendChild(ctx.renderChild(primaryActionRaw));
  if (secondaryActionRaw != null) {
    actionsEl.appendChild(ctx.renderChild(secondaryActionRaw));
  }
  wrapper.appendChild(actionsEl);

  return wrapper;
}

// =============================================================================
// Registration
// =============================================================================

export function registerMoleculeShellRenderers() {
  if (!lookupComponentRenderer(APP_SHELL_TAG)) {
    registerComponentRenderer(APP_SHELL_TAG, renderAppShell);
  }
  if (!lookupComponentRenderer(LOGIN_SHELL_TAG)) {
    registerComponentRenderer(LOGIN_SHELL_TAG, renderLoginShell);
  }
  if (!lookupComponentRenderer(WIZARD_SHELL_TAG)) {
    registerComponentRenderer(WIZARD_SHELL_TAG, renderWizardShell);
  }
  // EmptyState registered here replaces the temporary one in data-list-renderer.
  if (!lookupComponentRenderer(EMPTY_STATE_TAG)) {
    registerComponentRenderer(EMPTY_STATE_TAG, renderEmptyState);
  }
  if (!lookupComponentRenderer(ERROR_BOUNDARY_TAG)) {
    registerComponentRenderer(ERROR_BOUNDARY_TAG, renderErrorBoundary);
  }
  if (!lookupComponentRenderer(WELCOME_HERO_TAG)) {
    registerComponentRenderer(WELCOME_HERO_TAG, renderWelcomeHero);
  }
}
