// =============================================================================
// Plik: sdk-runtime/form-wrappers-renderer.js
// Opis: Renderery container-typed komponentów Form — chunk 3.3c-8:
//   - FormField   (0x031A) — label+hint+error+required+child+layout
//   - FormGroup   (0x031B) — collapsible group z optional title/description
//   - FormSection (0x031C) — section z heavier heading + divider_top
//   - Form        (0x031D) — submit scope + validators + prevent_default_submit
//
// Wszystkie rekurencyjnie renderują child Component(s) przez ctx.renderChild
// — engine zapewnia cleanup propagation. Form intercepts native form submit
// dispatch + emit'uje `submit` event z scope_id (handlers per schema: submit,
// reset).
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/form/wrappers.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';

// =============================================================================
// Walidatory
// =============================================================================

const FORM_FIELD_LAYOUTS = new Set(['stacked', 'horizontal']);
const FORM_LAYOUTS = new Set(['stacked', 'horizontal', 'compact']);
const SPACING_TOKENS = new Set(['zero', 'xxs', 'xs', 'sm', 'md', 'lg', 'xl', 'xxl']);
const FORM_VALIDATOR_KINDS = new Set(['all_required', 'any_required', 'match', 'custom']);
// scope_id grammar: [a-z0-9_-]+ length 1..=64 (mirror test_id).
const SCOPE_ID_RE = /^[a-z0-9_-]+$/;
// field_id grammar: takie same ograniczenia (component id allowlist).
const FIELD_ID_RE = /^[a-z0-9_-]+$/;

function requireEnum(v, set, ctx) {
  if (typeof v !== 'string' || !set.has(v)) {
    throw new TypeError(`${ctx}: expected one of ${[...set].join('/')}, got ${JSON.stringify(v)}`);
  }
  return v;
}
function requireBool(v, ctx) {
  if (typeof v !== 'boolean') throw new TypeError(`${ctx}: expected boolean, got ${typeof v}`);
  return v;
}
function requireString(v, ctx) {
  if (typeof v !== 'string') throw new TypeError(`${ctx}: expected string`);
  return v;
}
function requireArray(v, ctx) {
  if (!Array.isArray(v)) throw new TypeError(`${ctx}: expected array`);
  return v;
}
function assertOnlyKnownFields(fields, allowedKeys, name) {
  for (const [k] of fields) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(`${name}: unknown field key ${k} (allowed: ${[...allowedKeys].join(',')})`);
    }
  }
}
function assertOnlyKnownObjectKeys(obj, allowedKeys, ctx) {
  for (const k of Object.keys(obj)) {
    if (!allowedKeys.has(k)) throw new TypeError(`${ctx}: unexpected key '${k}'`);
  }
}

function applyTextBind(element, bindRef, ctx) {
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    element.textContent = v == null ? '' : String(v);
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

/// Reactive show/hide na BindRef boolowym. Brak BindRef → element zawsze
/// widoczny.
function applyVisibleBind(element, bindRef, ctx, defaultVisible) {
  if (bindRef == null) {
    element.hidden = !defaultVisible;
    return () => defaultVisible;
  }
  let visible = false;
  const apply = () => {
    visible = resolveBindRef(bindRef, ctx.store) === true;
    element.hidden = !visible;
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
  return () => visible;
}

// =============================================================================
// FormValidator parsing (tagged union)
// =============================================================================

function parseFormValidator(raw, ctx) {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) {
    throw new TypeError(`${ctx}: FormValidator must be object`);
  }
  if (typeof raw.kind !== 'string' || !FORM_VALIDATOR_KINDS.has(raw.kind)) {
    throw new TypeError(`${ctx}.kind unsupported: ${raw.kind}`);
  }
  switch (raw.kind) {
    case 'all_required': {
      assertOnlyKnownObjectKeys(raw, new Set(['kind', 'field_ids']), `${ctx}.all_required`);
      const ids = requireArray(raw.field_ids, `${ctx}.field_ids`);
      const out = ids.map((s, i) => requireString(s, `${ctx}.field_ids[${i}]`));
      for (let i = 0; i < out.length; i++) {
        if (!FIELD_ID_RE.test(out[i])) throw new TypeError(`${ctx}.field_ids[${i}]: invalid id`);
      }
      return { kind: 'all_required', field_ids: out };
    }
    case 'any_required': {
      assertOnlyKnownObjectKeys(raw, new Set(['kind', 'field_ids', 'error_message']), `${ctx}.any_required`);
      const ids = requireArray(raw.field_ids, `${ctx}.field_ids`);
      const out = ids.map((s, i) => requireString(s, `${ctx}.field_ids[${i}]`));
      for (let i = 0; i < out.length; i++) {
        if (!FIELD_ID_RE.test(out[i])) throw new TypeError(`${ctx}.field_ids[${i}]: invalid id`);
      }
      if (raw.error_message == null) {
        throw new TypeError(`${ctx}.error_message required (BindRef)`);
      }
      return { kind: 'any_required', field_ids: out, error_message: raw.error_message };
    }
    case 'match': {
      assertOnlyKnownObjectKeys(raw, new Set(['kind', 'field_a', 'field_b']), `${ctx}.match`);
      const a = requireString(raw.field_a, `${ctx}.field_a`);
      const b = requireString(raw.field_b, `${ctx}.field_b`);
      if (!FIELD_ID_RE.test(a)) throw new TypeError(`${ctx}.field_a: invalid id`);
      if (!FIELD_ID_RE.test(b)) throw new TypeError(`${ctx}.field_b: invalid id`);
      if (a === b) throw new TypeError(`${ctx}.match: field_a must differ from field_b`);
      return { kind: 'match', field_a: a, field_b: b };
    }
    case 'custom': {
      assertOnlyKnownObjectKeys(raw, new Set(['kind', 'id', 'params']), `${ctx}.custom`);
      const id = requireString(raw.id, `${ctx}.id`);
      if (!FIELD_ID_RE.test(id)) throw new TypeError(`${ctx}.id: invalid id`);
      const params = raw.params == null ? null : raw.params;
      return { kind: 'custom', id, params };
    }
  }
  throw new TypeError(`${ctx}: unreachable`);
}

/// Walidacja Component shape (rekurencyjne dziecko). Engine sam waliduje
/// pełniej przy render — tu sprawdzamy minimum że to obiekt z tag.
function assertComponentShape(c, ctx) {
  if (!c || typeof c !== 'object' || Array.isArray(c)) {
    throw new TypeError(`${ctx}: Component must be object`);
  }
  if (!Number.isInteger(c.tag)) throw new TypeError(`${ctx}.tag must be u16`);
}

// =============================================================================
// FormField (0x031A)
// =============================================================================

export const FORM_FIELD_TAG = 0x031A;
const FORM_FIELD_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);

function renderFormField(component, ctx) {
  assertOnlyKnownFields(component.fields, FORM_FIELD_FIELD_KEYS, 'FormField');

  const labelBind = ctx.readField(component.fields, 0);
  if (labelBind == null) throw new TypeError('FormField.label is required (BindRef)');
  const hintBind = ctx.readField(component.fields, 1);
  const errorBind = ctx.readField(component.fields, 2);
  const required = requireBool(ctx.readField(component.fields, 3), 'FormField.required');
  const childRaw = ctx.readField(component.fields, 4);
  assertComponentShape(childRaw, 'FormField.child');
  const layout = requireEnum(ctx.readField(component.fields, 5), FORM_FIELD_LAYOUTS, 'FormField.layout');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-form-field');
  wrapper.classList.add(`tf-form-field--layout-${layout}`);
  if (required) wrapper.classList.add('tf-form-field--required');

  // Label + child IDs sterowane przez aria-labelledby (rzeczywiste
  // <label for=> wymagałoby znanego id child input'a; child może być dowolny
  // wrapper-element, więc fallbackiem na aria-labelledby).
  const labelEl = document.createElement('div');
  labelEl.classList.add('tf-form-field__label');
  const labelId = `tf-form-field-${component.id}-label`;
  labelEl.setAttribute('id', labelId);
  applyTextBind(labelEl, labelBind, ctx);

  if (required) {
    const star = document.createElement('span');
    star.classList.add('tf-form-field__required-mark');
    star.setAttribute('aria-hidden', 'true');
    star.textContent = '*';
    labelEl.appendChild(star);
  }

  wrapper.appendChild(labelEl);

  const childEl = ctx.renderChild(childRaw);
  childEl.classList.add('tf-form-field__child');
  childEl.setAttribute('aria-labelledby', labelId);
  if (required) childEl.setAttribute('aria-required', 'true');
  wrapper.appendChild(childEl);

  if (hintBind != null) {
    const hint = document.createElement('div');
    hint.classList.add('tf-form-field__hint');
    const hintId = `tf-form-field-${component.id}-hint`;
    hint.setAttribute('id', hintId);
    applyTextBind(hint, hintBind, ctx);
    wrapper.appendChild(hint);
    // Dodaj aria-describedby na child (jeśli child to interaktywny element).
    childEl.setAttribute('aria-describedby', hintId);
  }

  if (errorBind != null) {
    const err = document.createElement('div');
    err.classList.add('tf-form-field__error');
    err.setAttribute('role', 'alert');
    const errId = `tf-form-field-${component.id}-error`;
    err.setAttribute('id', errId);
    const apply = () => {
      const v = resolveBindRef(errorBind, ctx.store);
      const text = typeof v === 'string' && v.length > 0 ? v : null;
      // aria-describedby update — usuń errId (jeśli był) na clear,
      // dodaj na set. Operacja set-based żeby uniknąć duplikatów przy
      // wielokrotnych przejściach.
      const current = childEl.getAttribute('aria-describedby');
      const ids = current ? current.split(/\s+/).filter(Boolean) : [];
      const filtered = ids.filter((id) => id !== errId);
      if (text == null) {
        err.textContent = '';
        err.hidden = true;
        wrapper.classList.remove('tf-form-field--invalid');
        childEl.removeAttribute('aria-invalid');
        if (filtered.length > 0) {
          childEl.setAttribute('aria-describedby', filtered.join(' '));
        } else {
          childEl.removeAttribute('aria-describedby');
        }
      } else {
        err.textContent = text;
        err.hidden = false;
        wrapper.classList.add('tf-form-field--invalid');
        childEl.setAttribute('aria-invalid', 'true');
        filtered.push(errId);
        childEl.setAttribute('aria-describedby', filtered.join(' '));
      }
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(errorBind, ctx.store, apply));
    wrapper.appendChild(err);
  }

  return wrapper;
}

// =============================================================================
// FormGroup (0x031B)
// =============================================================================

export const FORM_GROUP_TAG = 0x031B;
const FORM_GROUP_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);

function renderFormGroup(component, ctx) {
  assertOnlyKnownFields(component.fields, FORM_GROUP_FIELD_KEYS, 'FormGroup');

  const titleBind = ctx.readField(component.fields, 0);
  const descriptionBind = ctx.readField(component.fields, 1);
  const collapsible = requireBool(ctx.readField(component.fields, 2), 'FormGroup.collapsible');
  const expandedBind = ctx.readField(component.fields, 3);
  const childrenRaw = ctx.readField(component.fields, 4);
  if (!Array.isArray(childrenRaw)) {
    throw new TypeError('FormGroup.children: expected Array<Component>');
  }
  if (childrenRaw.length === 0) {
    throw new TypeError('FormGroup.children must be non-empty');
  }
  childrenRaw.forEach((c, i) => assertComponentShape(c, `FormGroup.children[${i}]`));
  const spacing = requireEnum(ctx.readField(component.fields, 5), SPACING_TOKENS, 'FormGroup.spacing');

  // collapsible=false z expanded BindRef'em jest sprzeczne — expanded ma
  // sens tylko gdy collapsible=true.
  if (!collapsible && expandedBind != null) {
    throw new TypeError('FormGroup.expanded only valid when collapsible=true');
  }

  const wrapper = document.createElement('section');
  wrapper.classList.add('tf-form-group');
  wrapper.classList.add(`tf-form-group--spacing-${spacing}`);
  if (collapsible) wrapper.classList.add('tf-form-group--collapsible');

  let headerEl = null;
  let toggleBtn = null;
  let bodyEl = null;
  let isExpanded = true;

  if (titleBind != null || descriptionBind != null || collapsible) {
    headerEl = document.createElement('header');
    headerEl.classList.add('tf-form-group__header');

    if (collapsible) {
      toggleBtn = document.createElement('button');
      toggleBtn.setAttribute('type', 'button');
      toggleBtn.classList.add('tf-form-group__toggle');
      const tid = `tf-form-group-${component.id}-toggle`;
      toggleBtn.setAttribute('id', tid);
      headerEl.appendChild(toggleBtn);
    }

    if (titleBind != null) {
      const title = document.createElement('h3');
      title.classList.add('tf-form-group__title');
      applyTextBind(title, titleBind, ctx);
      if (toggleBtn) toggleBtn.appendChild(title);
      else headerEl.appendChild(title);
    }
    if (descriptionBind != null) {
      const desc = document.createElement('p');
      desc.classList.add('tf-form-group__description');
      applyTextBind(desc, descriptionBind, ctx);
      headerEl.appendChild(desc);
    }
    wrapper.appendChild(headerEl);
  }

  bodyEl = document.createElement('div');
  bodyEl.classList.add('tf-form-group__body');
  for (let i = 0; i < childrenRaw.length; i++) {
    const childEl = ctx.renderChild(childrenRaw[i]);
    bodyEl.appendChild(childEl);
  }
  wrapper.appendChild(bodyEl);

  if (collapsible) {
    if (toggleBtn) {
      toggleBtn.setAttribute('aria-controls', `tf-form-group-${component.id}-body`);
      bodyEl.setAttribute('id', `tf-form-group-${component.id}-body`);
    }
    const applyExpanded = () => {
      isExpanded = expandedBind == null
        ? true
        : resolveBindRef(expandedBind, ctx.store) !== false;
      bodyEl.hidden = !isExpanded;
      if (toggleBtn) toggleBtn.setAttribute('aria-expanded', isExpanded ? 'true' : 'false');
      if (isExpanded) wrapper.classList.add('tf-form-group--expanded');
      else wrapper.classList.remove('tf-form-group--expanded');
    };
    applyExpanded();
    if (expandedBind != null) {
      ctx.registerCleanup(subscribeBindRef(expandedBind, ctx.store, applyExpanded));
    }
    if (toggleBtn) {
      // Toggle bez BindRef'a → lokalny state (flip). Z BindRef'em →
      // emit 'change' (write-back przez chunk 3.6). Bez expandedBind po
      // prostu mutujemy DOM lokalnie.
      const onClick = (e) => {
        e.preventDefault();
        if (expandedBind != null) {
          // expandedBind przekazany w detail żeby chunk 3.6 / host wiedział
          // dokąd write-back (BindRef::Bound.path).
          wrapper.dispatchEvent(
            new (globalThis.CustomEvent || globalThis.Event)('toggle', {
              bubbles: false,
              detail: { value: !isExpanded, kind: 'bool', bind: expandedBind },
            })
          );
        } else {
          isExpanded = !isExpanded;
          bodyEl.hidden = !isExpanded;
          toggleBtn.setAttribute('aria-expanded', isExpanded ? 'true' : 'false');
          if (isExpanded) wrapper.classList.add('tf-form-group--expanded');
          else wrapper.classList.remove('tf-form-group--expanded');
        }
      };
      toggleBtn.addEventListener('click', onClick);
      ctx.registerCleanup(() => toggleBtn.removeEventListener('click', onClick));
    }
  }

  return wrapper;
}

// =============================================================================
// FormSection (0x031C)
// =============================================================================

export const FORM_SECTION_TAG = 0x031C;
const FORM_SECTION_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderFormSection(component, ctx) {
  assertOnlyKnownFields(component.fields, FORM_SECTION_FIELD_KEYS, 'FormSection');

  const titleBind = ctx.readField(component.fields, 0);
  if (titleBind == null) throw new TypeError('FormSection.title is required');
  const descriptionBind = ctx.readField(component.fields, 1);
  const childrenRaw = ctx.readField(component.fields, 2);
  if (!Array.isArray(childrenRaw)) {
    throw new TypeError('FormSection.children: expected Array<Component>');
  }
  if (childrenRaw.length === 0) {
    throw new TypeError('FormSection.children must be non-empty');
  }
  childrenRaw.forEach((c, i) => assertComponentShape(c, `FormSection.children[${i}]`));
  // §5 0x031C defaults: spacing=Lg, divider_top=true.
  const spacingRaw = ctx.readField(component.fields, 3);
  const spacing = spacingRaw === undefined
    ? 'lg'
    : requireEnum(spacingRaw, SPACING_TOKENS, 'FormSection.spacing');
  const dividerTopRaw = ctx.readField(component.fields, 4);
  const dividerTop = dividerTopRaw === undefined
    ? true
    : requireBool(dividerTopRaw, 'FormSection.divider_top');

  const wrapper = document.createElement('section');
  wrapper.classList.add('tf-form-section');
  wrapper.classList.add(`tf-form-section--spacing-${spacing}`);
  if (dividerTop) wrapper.classList.add('tf-form-section--divider-top');

  const header = document.createElement('header');
  header.classList.add('tf-form-section__header');
  const title = document.createElement('h2');
  title.classList.add('tf-form-section__title');
  applyTextBind(title, titleBind, ctx);
  header.appendChild(title);
  if (descriptionBind != null) {
    const desc = document.createElement('p');
    desc.classList.add('tf-form-section__description');
    applyTextBind(desc, descriptionBind, ctx);
    header.appendChild(desc);
  }
  wrapper.appendChild(header);

  const body = document.createElement('div');
  body.classList.add('tf-form-section__body');
  for (let i = 0; i < childrenRaw.length; i++) {
    body.appendChild(ctx.renderChild(childrenRaw[i]));
  }
  wrapper.appendChild(body);

  return wrapper;
}

// =============================================================================
// Form (0x031D)
// =============================================================================

export const FORM_TAG = 0x031D;
const FORM_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);

function renderForm(component, ctx) {
  assertOnlyKnownFields(component.fields, FORM_FIELD_KEYS, 'Form');

  const childrenRaw = ctx.readField(component.fields, 0);
  if (!Array.isArray(childrenRaw)) {
    throw new TypeError('Form.children: expected Array<Component>');
  }
  if (childrenRaw.length === 0) {
    throw new TypeError('Form.children must be non-empty');
  }
  childrenRaw.forEach((c, i) => assertComponentShape(c, `Form.children[${i}]`));
  const scopeId = requireString(ctx.readField(component.fields, 1), 'Form.scope_id');
  if (!SCOPE_ID_RE.test(scopeId) || scopeId.length > 64) {
    throw new TypeError('Form.scope_id must match [a-z0-9_-]+ length 1..=64');
  }
  const validatorsRaw = ctx.readField(component.fields, 2);
  if (!Array.isArray(validatorsRaw)) {
    throw new TypeError('Form.validators: expected Array<FormValidator>');
  }
  const validators = validatorsRaw.map((v, i) => parseFormValidator(v, `Form.validators[${i}]`));
  const preventDefaultSubmit = requireBool(
    ctx.readField(component.fields, 3), 'Form.prevent_default_submit'
  );
  const layout = requireEnum(ctx.readField(component.fields, 4), FORM_LAYOUTS, 'Form.layout');
  const disabledBind = ctx.readField(component.fields, 5);

  const wrapper = document.createElement('form');
  wrapper.classList.add('tf-form');
  wrapper.classList.add(`tf-form--layout-${layout}`);
  wrapper.setAttribute('data-scope-id', scopeId);
  // Natywne submit blokujemy gdy prevent_default_submit=true; user musi
  // wywołać submit przez handler dispatch'a — Form NIE robi natywnego POST.
  wrapper.setAttribute('novalidate', '');

  // Render children FIRST — disabledBind apply() musi widzieć już-podpięte
  // input/button/select/textarea żeby ustawić im disabled.
  for (let i = 0; i < childrenRaw.length; i++) {
    wrapper.appendChild(ctx.renderChild(childrenRaw[i]));
  }

  let disabledActive = false;
  if (disabledBind != null) {
    // Marker `data-tf-form-disabled` znaczy, że to MY (Form) ustawiliśmy
    // disabled — przy flip OFF zdejmiemy TYLKO te wpisy, nie ruszając
    // disabled ustawionego przez child renderery (per-pole binding'i).
    const apply = () => {
      disabledActive = resolveBindRef(disabledBind, ctx.store) === true;
      if (disabledActive) {
        wrapper.setAttribute('aria-disabled', 'true');
        wrapper.setAttribute('data-disabled', '');
      } else {
        wrapper.removeAttribute('aria-disabled');
        wrapper.removeAttribute('data-disabled');
      }
      const inputs = wrapper.querySelectorAll('input, button, select, textarea');
      inputs.forEach((i) => {
        if (disabledActive) {
          // Nie nadpisujemy istniejącego disabled — gdy child ma własny
          // disabled BindRef, nie chcemy go zniwelować przy flip OFF.
          if (!i.hasAttribute('disabled')) {
            i.setAttribute('disabled', '');
            i.setAttribute('data-tf-form-disabled', '');
          } else if (!i.hasAttribute('data-tf-form-disabled')) {
            // child już disabled przez własny binding — nie tagujemy.
          }
        } else if (i.hasAttribute('data-tf-form-disabled')) {
          i.removeAttribute('disabled');
          i.removeAttribute('data-tf-form-disabled');
        }
      });
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(disabledBind, ctx.store, apply));
  }

  const onSubmit = (e) => {
    if (preventDefaultSubmit) e.preventDefault();
    if (disabledActive) {
      e.preventDefault();
      return;
    }
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('submit_form', {
        bubbles: false,
        detail: { scope_id: scopeId, validators: validators.map((v) => v.kind) },
      })
    );
  };
  wrapper.addEventListener('submit', onSubmit);
  ctx.registerCleanup(() => wrapper.removeEventListener('submit', onSubmit));

  const onReset = (e) => {
    if (disabledActive) {
      e.preventDefault();
      return;
    }
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('reset_form', {
        bubbles: false,
        detail: { scope_id: scopeId },
      })
    );
  };
  wrapper.addEventListener('reset', onReset);
  ctx.registerCleanup(() => wrapper.removeEventListener('reset', onReset));

  // Expose validators metadata na data-* żeby chunk 3.6 / host mógł
  // sięgnąć bez ponownego parsing'u.
  wrapper.setAttribute('data-validators-count', String(validators.length));
  for (let i = 0; i < validators.length; i++) {
    wrapper.setAttribute(`data-validator-${i}`, validators[i].kind);
  }

  return wrapper;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerFormWrappersRenderers() {
  if (!lookupComponentRenderer(FORM_FIELD_TAG)) registerComponentRenderer(FORM_FIELD_TAG, renderFormField);
  if (!lookupComponentRenderer(FORM_GROUP_TAG)) registerComponentRenderer(FORM_GROUP_TAG, renderFormGroup);
  if (!lookupComponentRenderer(FORM_SECTION_TAG)) registerComponentRenderer(FORM_SECTION_TAG, renderFormSection);
  if (!lookupComponentRenderer(FORM_TAG)) registerComponentRenderer(FORM_TAG, renderForm);
}
