// =============================================================================
// Plik: sdk-runtime/form-radio-groups-renderer.js
// Opis: Renderery RadioGroup (0x030D) + RadioCardGroup (0x030E) — chunk 3.3c-7.
//
// Oba używają `bind_path: StatePath` jako shared state dla wszystkich opcji.
// Wybór emit'uje `change` z `{ value, kind }` SelectValue (handlers spec
// data.rs deklaruje `change` dla obu). Reactive bind_path read-only;
// write-back chunk 3.6.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/form/groups.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';
import { renderIcon } from './icon-renderer.js';

// =============================================================================
// Walidatory
// =============================================================================

const SELECT_VALUE_KINDS = new Set(['tstr', 'u32', 'i32', 'bool']);
const SELECT_VALUE_KEYS = new Set(['kind', 'value']);
const RADIO_OPTION_KEYS = new Set([0, 1, 2, 3]);
const RADIO_CARD_OPTION_KEYS = new Set([0, 1, 2, 3, 4, 5]);
const RADIO_ORIENTATIONS = new Set(['horizontal', 'vertical']);
const RADIO_CARD_VARIANTS = new Set(['default', 'compact', 'feature']);
const DENSITIES = new Set(['compact', 'default', 'comfortable']);
const BADGE_VARIANTS = new Set(['solid', 'soft', 'outline', 'pulse', 'dot']);
const TONES = new Set(['neutral', 'primary', 'success', 'warning', 'critical', 'info', 'muted']);
const INLINE_BADGE_KEYS = new Set([0, 1, 2, 3, 4, 5]);

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
function requireU8(v, ctx) {
  if (!Number.isInteger(v) || v < 0 || v > 0xFF) throw new TypeError(`${ctx}: expected u8, got ${v}`);
  return v;
}
function requirePath(v, ctx) {
  if (!Array.isArray(v)) throw new TypeError(`${ctx}: expected StatePath`);
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

function parseSelectValue(sv, ctx) {
  if (!sv || typeof sv !== 'object') throw new TypeError(`${ctx}: SelectValue must be object`);
  assertOnlyKnownObjectKeys(sv, SELECT_VALUE_KEYS, ctx);
  if (!SELECT_VALUE_KINDS.has(sv.kind)) throw new TypeError(`${ctx}.kind unsupported: ${sv.kind}`);
  switch (sv.kind) {
    case 'tstr':
      if (typeof sv.value !== 'string') throw new TypeError(`${ctx}.value must be string`);
      return { tag: 'tstr', value: sv.value };
    case 'bool':
      if (typeof sv.value !== 'boolean') throw new TypeError(`${ctx}.value must be boolean`);
      return { tag: 'bool', value: sv.value };
    case 'u32':
      if (!Number.isInteger(sv.value) || sv.value < 0 || sv.value > 0xFFFFFFFF) {
        throw new TypeError(`${ctx}.value must be u32`);
      }
      return { tag: 'u32', value: sv.value };
    case 'i32':
      if (!Number.isInteger(sv.value) || sv.value < -0x80000000 || sv.value > 0x7FFFFFFF) {
        throw new TypeError(`${ctx}.value must be i32`);
      }
      return { tag: 'i32', value: sv.value };
  }
  throw new TypeError(`${ctx}.kind unsupported`);
}

function selectValueEquals(parsed, storeValue) {
  if (parsed.tag === 'tstr') return typeof storeValue === 'string' && storeValue === parsed.value;
  if (parsed.tag === 'bool') return typeof storeValue === 'boolean' && storeValue === parsed.value;
  if (parsed.tag === 'u32' || parsed.tag === 'i32') {
    if (typeof storeValue === 'number' && storeValue === parsed.value) return true;
    if (typeof storeValue === 'bigint' && storeValue === BigInt(parsed.value)) return true;
  }
  return false;
}

function parseRadioOption(raw, ctx) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: RadioOption must be FieldMap`);
  const seen = new Set();
  let value, label, hint = null, disabled;
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!RADIO_OPTION_KEYS.has(k)) throw new TypeError(`${ctx}: unknown RadioOption key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    switch (k) {
      case 0: value = parseSelectValue(v, `${ctx}.value`); break;
      case 1: label = v; break;
      case 2: if (v != null) hint = v; break;
      case 3: disabled = requireBool(v, `${ctx}.disabled`); break;
    }
  }
  if (value === undefined) throw new TypeError(`${ctx}: value required`);
  if (label === undefined) throw new TypeError(`${ctx}: label required`);
  if (disabled === undefined) throw new TypeError(`${ctx}: disabled required`);
  return { value, label, hint, disabled };
}

function parseInlineBadge(raw, ctx) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: InlineBadge must be FieldMap`);
  const seen = new Set();
  let variant, tone, label = null, count = null, icon = null, pulse;
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!INLINE_BADGE_KEYS.has(k)) throw new TypeError(`${ctx}: unknown InlineBadge key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    switch (k) {
      case 0: variant = requireEnum(v, BADGE_VARIANTS, `${ctx}.variant`); break;
      case 1: tone = requireEnum(v, TONES, `${ctx}.tone`); break;
      case 2: if (v != null) label = v; break;
      case 3: if (v != null) count = v; break;
      case 4: if (v != null) icon = v; break;
      case 5: pulse = requireBool(v, `${ctx}.pulse`); break;
    }
  }
  if (variant === undefined) throw new TypeError(`${ctx}: variant required`);
  if (tone === undefined) throw new TypeError(`${ctx}: tone required`);
  if (pulse === undefined) throw new TypeError(`${ctx}: pulse required`);
  return { variant, tone, label, count, icon, pulse };
}

function parseRadioCardOption(raw, ctx) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: RadioCardOption must be FieldMap`);
  const seen = new Set();
  let value, icon, title, description = null, badge = null, disabled;
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!RADIO_CARD_OPTION_KEYS.has(k)) throw new TypeError(`${ctx}: unknown RadioCardOption key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    switch (k) {
      case 0: value = parseSelectValue(v, `${ctx}.value`); break;
      case 1: icon = v; break;  // IconRef (validation in renderIcon)
      case 2: title = v; break;
      case 3: if (v != null) description = v; break;
      case 4: if (v != null) badge = parseInlineBadge(v, `${ctx}.badge`); break;
      case 5: disabled = requireBool(v, `${ctx}.disabled`); break;
    }
  }
  if (value === undefined) throw new TypeError(`${ctx}: value required`);
  if (icon === undefined || icon === null) throw new TypeError(`${ctx}: icon required`);
  if (title === undefined) throw new TypeError(`${ctx}: title required`);
  if (disabled === undefined) throw new TypeError(`${ctx}: disabled required`);
  return { value, icon, title, description, badge, disabled };
}

function applyTextBind(element, bindRef, ctx) {
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    element.textContent = v == null ? '' : String(v);
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

/// Deterministyczna serializacja StatePath → string używana w `name=`
/// attribute dla natywnej grupy radio'sów. Copy z form-atomic-renderer
/// (Radio 0x030C) — radio'sy dzielące bind_path muszą mieć ten sam `name`
/// dla natywnej eksklusywności.
function pathToName(path) {
  const json = JSON.stringify(path.map((s) => [s.kind, s.value]));
  const utf8 = unescape(encodeURIComponent(json));
  const b64 = (typeof btoa === 'function' ? btoa : (s) => Buffer.from(s, 'binary').toString('base64'))(utf8);
  return b64.replace(/=+$/, '').replace(/\+/g, '-').replace(/\//g, '_');
}

// =============================================================================
// RadioGroup (0x030D)
// =============================================================================

export const RADIO_GROUP_TAG = 0x030D;
const RADIO_GROUP_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderRadioGroup(component, ctx) {
  assertOnlyKnownFields(component.fields, RADIO_GROUP_FIELD_KEYS, 'RadioGroup');

  const bindPath = requirePath(ctx.readField(component.fields, 0), 'RadioGroup.bind_path');
  const optionsRaw = ctx.readField(component.fields, 1);
  if (!Array.isArray(optionsRaw)) {
    throw new TypeError('RadioGroup.options: expected Array<RadioOption>');
  }
  if (optionsRaw.length === 0) {
    throw new TypeError('RadioGroup.options must be non-empty');
  }
  const options = optionsRaw.map((o, i) => parseRadioOption(o, `RadioGroup.options[${i}]`));
  // Duplicate value detection (po canonical {kind,value} key).
  const seenVals = new Set();
  for (const opt of options) {
    const key = `${opt.value.tag}:${opt.value.value}`;
    if (seenVals.has(key)) {
      throw new TypeError(`RadioGroup.options contains duplicate value ${key}`);
    }
    seenVals.add(key);
  }
  const orientation = requireEnum(ctx.readField(component.fields, 2), RADIO_ORIENTATIONS, 'RadioGroup.orientation');
  const labelBind = ctx.readField(component.fields, 3);
  const density = requireEnum(ctx.readField(component.fields, 4), DENSITIES, 'RadioGroup.density');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-radio-group');
  wrapper.classList.add(`tf-radio-group--${orientation}`);
  wrapper.classList.add(`tf-radio-group--density-${density}`);
  wrapper.setAttribute('role', 'radiogroup');

  let labelDomId = null;
  if (labelBind != null) {
    labelDomId = `tf-radio-group-${component.id}-label`;
    const lbl = document.createElement('div');
    lbl.classList.add('tf-radio-group__label');
    lbl.setAttribute('id', labelDomId);
    applyTextBind(lbl, labelBind, ctx);
    wrapper.appendChild(lbl);
    wrapper.setAttribute('aria-labelledby', labelDomId);
  } else {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError('RadioGroup without label requires Component.a11y.label');
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError('RadioGroup.a11y.label must resolve to non-blank string');
    }
    const applyAria = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) wrapper.setAttribute('aria-label', v);
      else wrapper.removeAttribute('aria-label');
    };
    applyAria();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAria));
  }

  const listEl = document.createElement('div');
  listEl.classList.add('tf-radio-group__list');
  wrapper.appendChild(listEl);

  const radioName = `tf-radio-group-${pathToName(bindPath)}`;
  const radioNodes = [];

  options.forEach((opt, idx) => {
    const itemWrap = document.createElement('label');
    itemWrap.classList.add('tf-radio-group__item');
    if (opt.disabled) itemWrap.classList.add('tf-radio-group__item--disabled');

    const input = document.createElement('input');
    input.setAttribute('type', 'radio');
    input.setAttribute('name', radioName);
    input.classList.add('tf-radio-group__input');
    input.setAttribute('id', `tf-radio-group-${component.id}-opt-${idx}`);
    if (opt.disabled) input.setAttribute('disabled', '');

    const labelText = document.createElement('span');
    labelText.classList.add('tf-radio-group__item-label');
    applyTextBind(labelText, opt.label, ctx);

    itemWrap.appendChild(input);
    itemWrap.appendChild(labelText);

    if (opt.hint != null) {
      const hint = document.createElement('span');
      hint.classList.add('tf-radio-group__item-hint');
      applyTextBind(hint, opt.hint, ctx);
      itemWrap.appendChild(hint);
    }

    listEl.appendChild(itemWrap);
    radioNodes.push({ input, opt, idx });
  });

  // Reactive checked sync ze store.
  const syncChecked = () => {
    let current;
    try { current = ctx.store.read(bindPath); } catch { current = undefined; }
    for (const n of radioNodes) {
      n.input.checked = selectValueEquals(n.opt.value, current);
    }
  };
  syncChecked();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, syncChecked));

  // Per-radio change handler — emit `change` na wrapper z SelectValue.
  for (const n of radioNodes) {
    const onChange = (e) => {
      // stopPropagation chroni przed bubbling'iem `change` z radio
      // do wrappera — applyEventHandlers może mieć handler 'change' na
      // wrapperze i bez stopPropagation otrzymałby duplikat.
      e.stopPropagation();
      if (n.opt.disabled) return;
      if (!n.input.checked) return;
      wrapper.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('change', {
          bubbles: false,
          detail: { value: n.opt.value.value, kind: n.opt.value.tag },
        })
      );
    };
    n.input.addEventListener('change', onChange);
    ctx.registerCleanup(() => n.input.removeEventListener('change', onChange));
  }

  return wrapper;
}

// =============================================================================
// RadioCardGroup (0x030E)
// =============================================================================

export const RADIO_CARD_GROUP_TAG = 0x030E;
const RADIO_CARD_GROUP_FIELD_KEYS = new Set([0, 1, 2, 3]);

function renderRadioCardGroup(component, ctx) {
  assertOnlyKnownFields(component.fields, RADIO_CARD_GROUP_FIELD_KEYS, 'RadioCardGroup');

  const bindPath = requirePath(ctx.readField(component.fields, 0), 'RadioCardGroup.bind_path');
  const optionsRaw = ctx.readField(component.fields, 1);
  if (!Array.isArray(optionsRaw)) {
    throw new TypeError('RadioCardGroup.options: expected Array<RadioCardOption>');
  }
  if (optionsRaw.length === 0) {
    throw new TypeError('RadioCardGroup.options must be non-empty');
  }
  const options = optionsRaw.map((o, i) => parseRadioCardOption(o, `RadioCardGroup.options[${i}]`));
  const seenVals = new Set();
  for (const opt of options) {
    const key = `${opt.value.tag}:${opt.value.value}`;
    if (seenVals.has(key)) {
      throw new TypeError(`RadioCardGroup.options contains duplicate value ${key}`);
    }
    seenVals.add(key);
  }
  const columns = requireU8(ctx.readField(component.fields, 2), 'RadioCardGroup.columns');
  if (columns === 0) throw new TypeError('RadioCardGroup.columns must be > 0');
  const variant = requireEnum(ctx.readField(component.fields, 3), RADIO_CARD_VARIANTS, 'RadioCardGroup.variant');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-radio-card-group');
  wrapper.classList.add(`tf-radio-card-group--variant-${variant}`);
  wrapper.setAttribute('role', 'radiogroup');
  wrapper.style.setProperty('--tf-radio-card-cols', String(columns));

  // RadioCardGroup nie ma label field — a11y.label wymagany.
  if (component.a11y == null || component.a11y.label == null) {
    throw new TypeError('RadioCardGroup requires Component.a11y.label (no label field in spec)');
  }
  const initial = resolveBindRef(component.a11y.label, ctx.store);
  if (typeof initial !== 'string' || initial.trim().length === 0) {
    throw new TypeError('RadioCardGroup.a11y.label must resolve to non-blank string');
  }
  const applyAria = () => {
    const v = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof v === 'string' && v.trim().length > 0) wrapper.setAttribute('aria-label', v);
    else wrapper.removeAttribute('aria-label');
  };
  applyAria();
  ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAria));

  const radioName = `tf-radio-card-group-${pathToName(bindPath)}`;
  const cardNodes = [];

  options.forEach((opt, idx) => {
    const card = document.createElement('label');
    card.classList.add('tf-radio-card-group__card');
    if (opt.disabled) card.classList.add('tf-radio-card-group__card--disabled');

    const input = document.createElement('input');
    input.setAttribute('type', 'radio');
    input.setAttribute('name', radioName);
    input.classList.add('tf-radio-card-group__input');
    input.setAttribute('id', `tf-radio-card-group-${component.id}-opt-${idx}`);
    if (opt.disabled) input.setAttribute('disabled', '');

    const iconEl = renderIcon(opt.icon, `RadioCardGroup.options[${idx}].icon`);
    iconEl.classList.add('tf-radio-card-group__icon');

    const body = document.createElement('div');
    body.classList.add('tf-radio-card-group__body');
    const title = document.createElement('span');
    title.classList.add('tf-radio-card-group__title');
    applyTextBind(title, opt.title, ctx);
    body.appendChild(title);
    if (opt.description != null) {
      const desc = document.createElement('span');
      desc.classList.add('tf-radio-card-group__description');
      applyTextBind(desc, opt.description, ctx);
      body.appendChild(desc);
    }

    card.appendChild(input);
    card.appendChild(iconEl);
    card.appendChild(body);

    if (opt.badge != null) {
      const badgeEl = renderInlineBadge(opt.badge, ctx);
      badgeEl.classList.add('tf-radio-card-group__badge');
      card.appendChild(badgeEl);
    }

    wrapper.appendChild(card);
    cardNodes.push({ input, opt, idx, card });
  });

  const syncChecked = () => {
    let current;
    try { current = ctx.store.read(bindPath); } catch { current = undefined; }
    for (const n of cardNodes) {
      const isSel = selectValueEquals(n.opt.value, current);
      n.input.checked = isSel;
      if (isSel) n.card.classList.add('tf-radio-card-group__card--selected');
      else n.card.classList.remove('tf-radio-card-group__card--selected');
    }
  };
  syncChecked();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, syncChecked));

  for (const n of cardNodes) {
    const onChange = (e) => {
      e.stopPropagation();
      if (n.opt.disabled) return;
      if (!n.input.checked) return;
      wrapper.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('change', {
          bubbles: false,
          detail: { value: n.opt.value.value, kind: n.opt.value.tag },
        })
      );
    };
    n.input.addEventListener('change', onChange);
    ctx.registerCleanup(() => n.input.removeEventListener('change', onChange));
  }

  return wrapper;
}

/// Minimalny render InlineBadge wewnątrz RadioCard. Pełny komponent
/// `tf-badge` (0x020A) zostanie wpięty w grupie data-display; tutaj
/// odtwarzamy wizualnie z inline-meta — wystarcza dla RadioCard.
function renderInlineBadge(badge, ctx) {
  const el = document.createElement('span');
  el.classList.add('tf-inline-badge');
  el.classList.add(`tf-inline-badge--variant-${badge.variant}`);
  el.classList.add(`tf-inline-badge--tone-${badge.tone}`);
  if (badge.pulse) el.classList.add('tf-inline-badge--pulse');

  if (badge.icon != null) {
    const ic = renderIcon(badge.icon, 'InlineBadge.icon');
    ic.classList.add('tf-inline-badge__icon');
    el.appendChild(ic);
  }
  if (badge.label != null) {
    const lbl = document.createElement('span');
    lbl.classList.add('tf-inline-badge__label');
    applyTextBind(lbl, badge.label, ctx);
    el.appendChild(lbl);
  }
  if (badge.count != null) {
    const cnt = document.createElement('span');
    cnt.classList.add('tf-inline-badge__count');
    applyTextBind(cnt, badge.count, ctx);
    el.appendChild(cnt);
  }
  return el;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerFormRadioGroupsRenderers() {
  if (!lookupComponentRenderer(RADIO_GROUP_TAG)) {
    registerComponentRenderer(RADIO_GROUP_TAG, renderRadioGroup);
  }
  if (!lookupComponentRenderer(RADIO_CARD_GROUP_TAG)) {
    registerComponentRenderer(RADIO_CARD_GROUP_TAG, renderRadioCardGroup);
  }
}
