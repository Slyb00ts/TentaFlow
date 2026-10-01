// ===== File: lib/actions/fields.js — one form field per kind for the shared action windows =====
//
// Every field is a small controller: `el` (append it), `read()` (the value the
// caller receives), `validate()` (marks the field and answers whether it is
// fine) and `focus()`. The windows only iterate over them, so a
// new kind is one entry here.

import { I18n } from '/js/i18n.js';
import '/js/components/tf-input.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-select.js';
import '/js/components/tf-date-field.js';
import '/js/components/tf-button.js';

export const t = (key, vars) => I18n.t(`actions.${key}`, vars);

const REQUIRED_MARK = ' *';

function labelText(field) {
  return field.required ? `${field.label}${REQUIRED_MARK}` : field.label;
}

// tf-input and tf-textarea render their own error line; the other controls get
// one line under them.
function wrap(field, control, ownError) {
  const el = document.createElement('div');
  el.className = 'tf-act__field';
  if (field.kind === 'area') el.classList.add('tf-act__field--wide');
  el.dataset.field = field.key;
  el.append(...control);
  const errorLine = document.createElement('div');
  errorLine.className = 'tf-act__field-error';
  errorLine.hidden = true;
  if (!ownError) el.append(errorLine);
  return { el, errorLine };
}

function setError(host, msg) {
  if (host.ownError) {
    if (msg) host.control.setAttribute('error', msg);
    else host.control.removeAttribute('error');
    return;
  }
  host.errorLine.textContent = msg;
  host.errorLine.hidden = !msg;
}

function textLike(field, { multiline }) {
  const control = document.createElement(multiline ? 'tf-textarea' : 'tf-input');
  control.setAttribute('label', labelText(field));
  if (multiline) control.setAttribute('rows', '3');
  if (field.kind === 'number') {
    control.setAttribute('type', 'number');
    control.setAttribute('inputmode', 'decimal');
    for (const attr of ['min', 'max', 'step']) if (field[attr] != null) control.setAttribute(attr, String(field[attr]));
  }
  if (field.placeholder) control.setAttribute('placeholder', field.placeholder);
  if (field.hint) control.setAttribute('hint', field.hint);
  if (field.maxLength) control.setAttribute('maxlength', String(field.maxLength));
  control.setAttribute('value', field.value == null ? '' : String(field.value));
  const { el } = wrap(field, [control], true);
  const host = { control, ownError: true };
  control.addEventListener('input', () => setError(host, ''));
  return { el, control, host };
}

function textField(field) {
  const { el, control, host } = textLike(field, { multiline: false });
  return {
    key: field.key,
    el,
    read: () => control.value.trim(),
    validate() {
      if (field.required && !control.value.trim()) { setError(host, t('required_field')); return false; }
      return true;
    },
    focus: () => control.focus(),
  };
}

function areaField(field) {
  const { el, control, host } = textLike(field, { multiline: true });
  return {
    key: field.key,
    el,
    read: () => control.value.trim(),
    validate() {
      if (field.required && !control.value.trim()) { setError(host, t('required_field')); return false; }
      return true;
    },
    focus: () => control.focus(),
  };
}

function numberField(field) {
  const { el, control, host } = textLike(field, { multiline: false });
  const read = () => {
    const raw = control.value.trim().replace(',', '.');
    return raw === '' ? null : Number(raw);
  };
  return {
    key: field.key,
    el,
    read,
    validate() {
      const n = read();
      if (n === null) {
        if (field.required) { setError(host, t('required_field')); return false; }
        return true;
      }
      if (!Number.isFinite(n)) { setError(host, t('invalid_number')); return false; }
      if (field.min != null && n < field.min) { setError(host, t('number_min', { min: field.min })); return false; }
      if (field.max != null && n > field.max) { setError(host, t('number_max', { max: field.max })); return false; }
      return true;
    },
    focus: () => control.focus(),
  };
}

function normalizeOptions(options) {
  return (options ?? []).map((o) => (typeof o === 'object'
    ? { value: String(o.value), label: o.label ?? String(o.value), disabled: o.disabled }
    : { value: String(o), label: String(o) }));
}

function selectControl(field, options) {
  const control = document.createElement('tf-select');
  control.setAttribute('label', labelText(field));
  if (field.hint) control.setAttribute('hint', field.hint);
  const current = field.value == null ? '' : String(field.value);
  const list = normalizeOptions(options);
  // A required field with no value would otherwise start on its first option,
  // and the user would submit a choice they never made.
  if (!field.required || !current) list.unshift({ value: '', label: field.required ? t('choose') : '—' });
  const { el, errorLine } = wrap(field, [control], false);
  const host = { control, ownError: false, errorLine };
  control.setOptions(list, current);
  control.addEventListener('change', () => setError(host, ''));
  return {
    key: field.key,
    el,
    read: () => (control.value === '' ? null : control.value),
    validate() {
      if (field.required && !control.value) { setError(host, t('required_field')); return false; }
      return true;
    },
    focus: () => control.focus(),
  };
}

// A day is typed in the UI language's format or picked from the calendar popup; the
// field's value is always ISO `YYYY-MM-DD`.
function dateField(field) {
  const control = document.createElement('tf-date-field');
  control.setAttribute('label', labelText(field));
  if (field.hint) control.setAttribute('hint', field.hint);
  if (field.min) control.setAttribute('min', String(field.min));
  if (field.max) control.setAttribute('max', String(field.max));
  control.setAttribute('value', field.value ? String(field.value) : '');
  const { el } = wrap(field, [control], true);
  const read = () => control.value || null;
  return {
    key: field.key,
    el,
    read,
    validate() {
      if (!control.value && control.text.trim() === '') {
        if (field.required) { control.setAttribute('error', t('required_field')); return false; }
        return true;
      }
      return control.validate();
    },
    focus: () => control.focus(),
  };
}

function personField(field, people) {
  return selectControl(field, (people ?? []).map((p) => ({ value: p.id, label: p.name, disabled: Boolean(p.disabled) })));
}

const BUILDERS = {
  text: textField,
  area: areaField,
  number: numberField,
  select: (field) => selectControl(field, field.options),
  date: dateField,
  person: (field, ctx) => personField(field, ctx.people),
};

/**
 * Builds the controller for one field.
 * `field` = { key, label, kind: text|area|select|date|person|number, value,
 * options, required, hint, placeholder, min, max, step, maxLength }.
 */
export function buildField(field, ctx = {}) {
  const build = BUILDERS[field.kind ?? 'text'];
  if (!build) throw new Error(`unknown action field kind '${field.kind}'`);
  return build(field, ctx);
}

/** Validates every field, focuses the first bad one and answers whether all passed. */
export function validateFields(fields) {
  let firstBad = null;
  for (const f of fields) {
    if (!f.validate() && !firstBad) firstBad = f;
  }
  firstBad?.focus();
  return firstBad === null;
}

/**
 * The element that really takes focus for `el`: a tf-button host is not focusable, its inner button is.
 * Anything else is returned as it is.
 */
export function focusTarget(el) {
  return el?.matches?.('tf-button') ? el.querySelector('button') ?? el : el;
}
