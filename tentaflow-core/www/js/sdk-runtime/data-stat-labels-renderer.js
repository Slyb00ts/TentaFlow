// =============================================================================
// Plik: sdk-runtime/data-stat-labels-renderer.js
// Opis: Renderery stat + label komponentow §4 Data Display — chunk 3.3d-2:
//   - KeyValue  (0x0207) — 2-kolumnowa lista label:value z KvItem
//   - StatCard  (0x0208) — <tf-stat-card> web component z trend/footnote
//   - Stat      (0x0209) — compact stat bez kontenera
//   - Badge     (0x020A) — status/count pill z count overflow (max)
//   - Chip      (0x020B) — filter/tag chip z handlers click/remove
//   - Tag       (0x020C) — static read-only label
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/{stat,labels}.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef, formatValue } from './bind-resolver.js';
import { renderIcon } from './icon-renderer.js';

// =============================================================================
// Walidatory wspolne
// =============================================================================

const TONES = new Set(['neutral', 'primary', 'success', 'warning', 'critical', 'info', 'muted']);
const DENSITIES = new Set(['compact', 'default', 'comfortable']);
const KV_LAYOUTS = new Set(['stacked', 'horizontal', 'grid']);
const SPACING_TOKENS = new Set(['zero', 'xxs', 'xs', 'sm', 'md', 'lg', 'xl', 'xxl']);
const BADGE_VARIANTS = new Set(['solid', 'soft', 'outline', 'pulse', 'dot']);
const CHIP_VARIANTS = new Set(['solid', 'soft', 'outline', 'removable', 'selectable', 'toggle']);
const TAG_SIZES = new Set(['xs', 'sm', 'md']);
const STAT_SIZES = new Set(['sm', 'md', 'lg']);
const TREND_DIRECTIONS = new Set(['up', 'down', 'flat']);
const VALUE_FORMAT_KINDS = new Set([
  'number', 'currency', 'percent', 'bytes', 'duration',
  'date', 'time', 'datetime', 'relative', 'plain',
]);
const ACTION_ID_RE = /^[a-z0-9_-]{1,64}$/;
const KV_ITEM_KEYS = new Set([0, 1, 2, 3, 4, 5]);

// Map SDK tone names to tf-stat-card accent attribute values
const TONE_TO_ACCENT = {
  'success': 'success',
  'warning': 'warning',
  'critical': 'danger',
  'info': 'info',
};

// Map SDK trend direction to tf-stat-card delta-type
const TREND_TO_DELTA_TYPE = {
  'up': 'up',
  'down': 'down',
  'flat': 'neutral',
};

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
function requireU32(v, ctx) {
  if (!Number.isInteger(v) || v < 0 || v > 0xFFFFFFFF) {
    throw new TypeError(`${ctx}: expected u32, got ${v}`);
  }
  return v;
}
function requireF64(v, ctx) {
  if (typeof v !== 'number' || !Number.isFinite(v)) {
    throw new TypeError(`${ctx}: expected finite f64, got ${v}`);
  }
  return v;
}
function requireString(v, ctx) {
  if (typeof v !== 'string') throw new TypeError(`${ctx}: expected string`);
  return v;
}
function assertOnlyKnownFields(fields, allowedKeys, name) {
  for (const [k] of fields) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(`${name}: unknown field key ${k} (allowed: ${[...allowedKeys].join(',')})`);
    }
  }
}

/// Eager ValueFormat probe
function assertValueFormat(fmt, ctx, locale) {
  if (fmt == null) return;
  if (typeof fmt !== 'object' || Array.isArray(fmt)) {
    throw new TypeError(`${ctx}: ValueFormat must be object`);
  }
  if (typeof fmt.kind !== 'string' || !VALUE_FORMAT_KINDS.has(fmt.kind)) {
    throw new TypeError(`${ctx}: ValueFormat.kind invalid: ${fmt.kind}`);
  }
  try {
    formatValue(0, fmt, locale);
  } catch (err) {
    throw new TypeError(`${ctx}: invalid ValueFormat — ${err && err.message ? err.message : err}`);
  }
}

function applyReactiveText(element, bindRef, ctx, valueFormat) {
  const apply = () => {
    const raw = resolveBindRef(bindRef, ctx.store);
    if (raw == null) { element.textContent = ''; return; }
    if (valueFormat != null) {
      try { element.textContent = formatValue(raw, valueFormat, ctx.locale); }
      catch { element.textContent = String(raw); }
    } else {
      element.textContent = String(raw);
    }
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

function parseTrend(raw, ctx) {
  if (raw == null) return null;
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: Trend must be FieldMap`);
  const seen = new Set();
  const t = { direction: null, percent: null, label: null, tone: null };
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    switch (k) {
      case 0: t.direction = requireEnum(v, TREND_DIRECTIONS, `${ctx}.direction`); break;
      case 1: t.percent = requireF64(v, `${ctx}.percent`); break;
      case 2: if (v != null) t.label = v; break;
      case 3: if (v != null) t.tone = requireEnum(v, TONES, `${ctx}.tone`); break;
      default: throw new TypeError(`${ctx}: unknown Trend key ${k}`);
    }
  }
  if (t.direction == null) throw new TypeError(`${ctx}: direction required`);
  if (t.percent == null) throw new TypeError(`${ctx}: percent required`);
  return t;
}

function parseFootnote(raw, ctx) {
  if (raw == null) return null;
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: Footnote must be FieldMap`);
  const seen = new Set();
  const f = { tone: null, icon: null, content: null };
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    switch (k) {
      case 0: f.tone = requireEnum(v, TONES, `${ctx}.tone`); break;
      case 1: if (v != null) f.icon = v; break;
      case 2: f.content = v; break;
      default: throw new TypeError(`${ctx}: unknown Footnote key ${k}`);
    }
  }
  if (f.tone == null) throw new TypeError(`${ctx}: tone required`);
  if (f.content == null) throw new TypeError(`${ctx}: content required`);
  return f;
}

function parseKvItem(raw, ctx, locale) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: KvItem must be FieldMap`);
  const seen = new Set();
  const it = { label: null, value: null, hint: null, icon: null, action_id: null, format: null };
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!KV_ITEM_KEYS.has(k)) throw new TypeError(`${ctx}: unknown KvItem key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    switch (k) {
      case 0: it.label = v; break;
      case 1: it.value = v; break;
      case 2: if (v != null) it.hint = v; break;
      case 3: if (v != null) it.icon = v; break;
      case 4: if (v != null) {
        const aid = requireString(v, `${ctx}.action_id`);
        if (!ACTION_ID_RE.test(aid)) throw new TypeError(`${ctx}.action_id: invalid grammar`);
        it.action_id = aid;
      } break;
      case 5: if (v != null) {
        assertValueFormat(v, `${ctx}.format`, locale);
        it.format = v;
      } break;
    }
  }
  if (it.label == null) throw new TypeError(`${ctx}: label required`);
  if (it.value == null) throw new TypeError(`${ctx}: value required`);
  return it;
}

// =============================================================================
// KeyValue (0x0207)
// =============================================================================

export const KEY_VALUE_TAG = 0x0207;
const KEY_VALUE_FIELD_KEYS = new Set([0, 1, 2, 3]);

function renderKeyValue(component, ctx) {
  assertOnlyKnownFields(component.fields, KEY_VALUE_FIELD_KEYS, 'KeyValue');

  const itemsRaw = ctx.readField(component.fields, 0);
  const items = itemsRaw == null ? [] : (() => {
    if (!Array.isArray(itemsRaw)) throw new TypeError('KeyValue.items: expected Array<KvItem>');
    return itemsRaw.map((it, i) => parseKvItem(it, `KeyValue.items[${i}]`, ctx.locale));
  })();
  const density = requireEnum(ctx.readField(component.fields, 1), DENSITIES, 'KeyValue.density');
  const layout = requireEnum(ctx.readField(component.fields, 2), KV_LAYOUTS, 'KeyValue.layout');
  const labelWidthRaw = ctx.readField(component.fields, 3);
  const labelWidth = labelWidthRaw == null
    ? null
    : requireEnum(labelWidthRaw, SPACING_TOKENS, 'KeyValue.label_width');

  const wrapper = document.createElement('dl');
  wrapper.classList.add('tf-keyvalue');
  wrapper.classList.add(`tf-keyvalue--density-${density}`);
  wrapper.classList.add(`tf-keyvalue--layout-${layout}`);
  if (labelWidth) wrapper.setAttribute('data-label-width', labelWidth);

  for (let i = 0; i < items.length; i++) {
    const it = items[i];
    const row = document.createElement('div');
    row.classList.add('tf-keyvalue__row');

    const dt = document.createElement('dt');
    dt.classList.add('tf-keyvalue__label');
    if (it.icon) {
      const ic = renderIcon(it.icon, `KeyValue.items[${i}].icon`);
      ic.classList.add('tf-keyvalue__icon');
      dt.appendChild(ic);
    }
    const lblText = document.createElement('span');
    lblText.classList.add('tf-keyvalue__label-text');
    applyReactiveText(lblText, it.label, ctx, null);
    dt.appendChild(lblText);
    row.appendChild(dt);

    const dd = document.createElement('dd');
    dd.classList.add('tf-keyvalue__value');
    const valText = document.createElement('span');
    valText.classList.add('tf-keyvalue__value-text');
    applyReactiveText(valText, it.value, ctx, it.format);
    dd.appendChild(valText);

    if (it.action_id != null) {
      dd.setAttribute('role', 'button');
      dd.setAttribute('tabindex', '0');
      dd.classList.add('tf-keyvalue__value--clickable');
      const onClick = (e) => {
        e.preventDefault();
        wrapper.dispatchEvent(
          new (globalThis.CustomEvent || globalThis.Event)('item_click', {
            bubbles: false,
            detail: { action_id: it.action_id, item_index: i },
          })
        );
      };
      const onKeyDown = (e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onClick(e);
        }
      };
      dd.addEventListener('click', onClick);
      dd.addEventListener('keydown', onKeyDown);
      ctx.registerCleanup(() => {
        dd.removeEventListener('click', onClick);
        dd.removeEventListener('keydown', onKeyDown);
      });
    }

    if (it.hint != null) {
      const hint = document.createElement('span');
      hint.classList.add('tf-keyvalue__hint');
      applyReactiveText(hint, it.hint, ctx, null);
      dd.appendChild(hint);
    }
    row.appendChild(dd);
    wrapper.appendChild(row);
  }

  return wrapper;
}

// =============================================================================
// Trend + Footnote rendering helpers (shared by StatCard + Stat)
// =============================================================================

function renderTrendBadge(trend, ctx) {
  const el = document.createElement('span');
  el.classList.add('tf-trend');
  el.classList.add(`tf-trend--${trend.direction}`);
  if (trend.tone) el.classList.add(`tf-trend--tone-${trend.tone}`);
  const arrow = document.createElement('span');
  arrow.classList.add('tf-trend__arrow');
  arrow.setAttribute('aria-hidden', 'true');
  arrow.textContent = trend.direction === 'up' ? '▲' : trend.direction === 'down' ? '▼' : '→';
  el.appendChild(arrow);
  const pct = document.createElement('span');
  pct.classList.add('tf-trend__percent');
  pct.textContent = `${Number.isInteger(trend.percent) ? trend.percent : trend.percent.toFixed(1)}%`;
  el.appendChild(pct);
  if (trend.label != null) {
    const lbl = document.createElement('span');
    lbl.classList.add('tf-trend__label');
    applyReactiveText(lbl, trend.label, ctx, null);
    el.appendChild(lbl);
  }
  return el;
}

function renderFootnoteBlock(fn, ctx) {
  const el = document.createElement('div');
  el.classList.add('tf-footnote');
  el.classList.add(`tf-footnote--tone-${fn.tone}`);
  if (fn.icon) {
    const ic = renderIcon(fn.icon, 'Footnote.icon');
    ic.classList.add('tf-footnote__icon');
    el.appendChild(ic);
  }
  const content = document.createElement('span');
  content.classList.add('tf-footnote__content');
  applyReactiveText(content, fn.content, ctx, null);
  el.appendChild(content);
  return el;
}

// =============================================================================
// StatCard (0x0208) — uses <tf-stat-card> web component
// =============================================================================

export const STAT_CARD_TAG = 0x0208;
const STAT_CARD_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8]);

function renderStatCard(component, ctx) {
  assertOnlyKnownFields(component.fields, STAT_CARD_FIELD_KEYS, 'StatCard');

  const label = ctx.readField(component.fields, 0);
  if (label == null) throw new TypeError('StatCard.label is required');
  const iconRaw = ctx.readField(component.fields, 1);
  const value = ctx.readField(component.fields, 2);
  if (value == null) throw new TypeError('StatCard.value is required');
  const valueSuffix = ctx.readField(component.fields, 3);
  const format = ctx.readField(component.fields, 4);
  assertValueFormat(format, 'StatCard.format', ctx.locale);
  const trendRaw = ctx.readField(component.fields, 5);
  const trend = parseTrend(trendRaw, 'StatCard.trend');
  const footnoteRaw = ctx.readField(component.fields, 6);
  const footnote = parseFootnote(footnoteRaw, 'StatCard.footnote');
  const accentRaw = ctx.readField(component.fields, 7);
  const accent = accentRaw == null ? null : requireEnum(accentRaw, TONES, 'StatCard.accent');
  const clickable = requireBool(ctx.readField(component.fields, 8), 'StatCard.clickable');

  // Create <tf-stat-card> web component
  const el = document.createElement('tf-stat-card');
  if (clickable) {
    el.classList.add('tf-stat-card--clickable');
    el.setAttribute('role', 'button');
    el.setAttribute('tabindex', '0');
  }

  // Map SDK accent tone to tf-stat-card accent attribute
  if (accent) {
    const mappedAccent = TONE_TO_ACCENT[accent] || accent;
    el.setAttribute('accent', mappedAccent);
  }

  // Reactive icon attribute
  if (iconRaw != null) {
    const iconName = typeof iconRaw === 'string' ? iconRaw
      : (iconRaw && typeof iconRaw === 'object' ? (iconRaw.name || '') : '');
    if (iconName) el.setAttribute('icon', iconName);
  }

  // Reactive label attribute
  const applyLabel = () => {
    const v = resolveBindRef(label, ctx.store);
    el.setAttribute('label', v == null ? '' : String(v));
  };
  applyLabel();
  ctx.registerCleanup(subscribeBindRef(label, ctx.store, applyLabel));

  // Reactive value attribute
  const applyValue = () => {
    const raw = resolveBindRef(value, ctx.store);
    if (raw == null) { el.setAttribute('value', ''); return; }
    if (format != null) {
      try { el.setAttribute('value', formatValue(raw, format, ctx.locale)); }
      catch { el.setAttribute('value', String(raw)); }
    } else {
      el.setAttribute('value', String(raw));
    }
  };
  applyValue();
  ctx.registerCleanup(subscribeBindRef(value, ctx.store, applyValue));

  // Reactive suffix attribute
  if (valueSuffix != null) {
    const applySuffix = () => {
      const v = resolveBindRef(valueSuffix, ctx.store);
      if (v != null) el.setAttribute('suffix', String(v));
      else el.removeAttribute('suffix');
    };
    applySuffix();
    ctx.registerCleanup(subscribeBindRef(valueSuffix, ctx.store, applySuffix));
  }

  // Trend mapped to delta/delta-type attributes
  if (trend) {
    const deltaType = TREND_TO_DELTA_TYPE[trend.direction] || 'neutral';
    el.setAttribute('delta-type', deltaType);
    const pctText = `${Number.isInteger(trend.percent) ? trend.percent : trend.percent.toFixed(1)}%`;
    el.setAttribute('delta', pctText);
  }

  // Footnote rendered as child element (tf-stat-card renders children via
  // light DOM, so we append extra elements after the component builds)
  if (footnote) {
    const fnEl = renderFootnoteBlock(footnote, ctx);
    el.appendChild(fnEl);
  }

  return el;
}

// =============================================================================
// Stat (0x0209)
// =============================================================================

export const STAT_TAG = 0x0209;
const STAT_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderStat(component, ctx) {
  assertOnlyKnownFields(component.fields, STAT_FIELD_KEYS, 'Stat');

  const label = ctx.readField(component.fields, 0);
  if (label == null) throw new TypeError('Stat.label is required');
  const value = ctx.readField(component.fields, 1);
  if (value == null) throw new TypeError('Stat.value is required');
  const format = ctx.readField(component.fields, 2);
  assertValueFormat(format, 'Stat.format', ctx.locale);
  const trend = parseTrend(ctx.readField(component.fields, 3), 'Stat.trend');
  const size = requireEnum(ctx.readField(component.fields, 4), STAT_SIZES, 'Stat.size');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-stat');
  wrapper.classList.add(`tf-stat--size-${size}`);

  const lblEl = document.createElement('span');
  lblEl.classList.add('tf-stat__label');
  applyReactiveText(lblEl, label, ctx, null);
  wrapper.appendChild(lblEl);

  const valueRow = document.createElement('div');
  valueRow.classList.add('tf-stat__value-row');
  const valEl = document.createElement('span');
  valEl.classList.add('tf-stat__value');
  applyReactiveText(valEl, value, ctx, format);
  valueRow.appendChild(valEl);
  if (trend) valueRow.appendChild(renderTrendBadge(trend, ctx));
  wrapper.appendChild(valueRow);

  return wrapper;
}

// =============================================================================
// Badge (0x020A)
// =============================================================================

export const BADGE_TAG = 0x020A;
const BADGE_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);

function renderBadge(component, ctx) {
  assertOnlyKnownFields(component.fields, BADGE_FIELD_KEYS, 'Badge');

  const variant = requireEnum(ctx.readField(component.fields, 0), BADGE_VARIANTS, 'Badge.variant');
  const tone = requireEnum(ctx.readField(component.fields, 1), TONES, 'Badge.tone');
  const label = ctx.readField(component.fields, 2);
  if (label == null) throw new TypeError('Badge.label is required');
  const iconRaw = ctx.readField(component.fields, 3);
  const count = ctx.readField(component.fields, 4);
  const max = requireU32(ctx.readField(component.fields, 5), 'Badge.max');
  if (max === 0) throw new TypeError('Badge.max must be > 0');
  const pulse = requireBool(ctx.readField(component.fields, 6), 'Badge.pulse');

  const wrapper = document.createElement('span');
  wrapper.classList.add('tf-badge');
  wrapper.classList.add(`tf-badge--variant-${variant}`);
  wrapper.classList.add(`tf-badge--tone-${tone}`);
  if (pulse) wrapper.classList.add('tf-badge--pulse');

  if (iconRaw != null) {
    const ic = renderIcon(iconRaw, 'Badge.icon');
    ic.classList.add('tf-badge__icon');
    wrapper.appendChild(ic);
  }
  if (variant !== 'dot') {
    const lblEl = document.createElement('span');
    lblEl.classList.add('tf-badge__label');
    applyReactiveText(lblEl, label, ctx, null);
    wrapper.appendChild(lblEl);
    if (count != null) {
      const cntEl = document.createElement('span');
      cntEl.classList.add('tf-badge__count');
      const apply = () => {
        const raw = resolveBindRef(count, ctx.store);
        const n = typeof raw === 'number' ? raw : Number(raw);
        if (!Number.isFinite(n)) { cntEl.textContent = ''; return; }
        cntEl.textContent = n > max ? `${max}+` : String(n);
      };
      apply();
      ctx.registerCleanup(subscribeBindRef(count, ctx.store, apply));
      wrapper.appendChild(cntEl);
    }
  } else {
    wrapper.setAttribute('role', 'status');
    const sr = document.createElement('span');
    sr.classList.add('tf-visually-hidden');
    applyReactiveText(sr, label, ctx, null);
    wrapper.appendChild(sr);
  }

  return wrapper;
}

// =============================================================================
// Chip (0x020B)
// =============================================================================

export const CHIP_TAG = 0x020B;
const CHIP_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);

function renderChip(component, ctx) {
  assertOnlyKnownFields(component.fields, CHIP_FIELD_KEYS, 'Chip');

  const variant = requireEnum(ctx.readField(component.fields, 0), CHIP_VARIANTS, 'Chip.variant');
  const tone = requireEnum(ctx.readField(component.fields, 1), TONES, 'Chip.tone');
  const label = ctx.readField(component.fields, 2);
  if (label == null) throw new TypeError('Chip.label is required');
  const iconRaw = ctx.readField(component.fields, 3);
  const avatarRaw = ctx.readField(component.fields, 4);
  if (iconRaw != null && avatarRaw != null) {
    throw new TypeError('Chip: icon and avatar are mutually exclusive');
  }
  const selectedBind = ctx.readField(component.fields, 5);
  const removable = requireBool(ctx.readField(component.fields, 6), 'Chip.removable');

  const wrapper = document.createElement('span');
  wrapper.classList.add('tf-chip');
  wrapper.classList.add(`tf-chip--variant-${variant}`);
  wrapper.classList.add(`tf-chip--tone-${tone}`);
  if (variant === 'toggle' || variant === 'selectable') {
    wrapper.setAttribute('role', variant === 'toggle' ? 'button' : 'option');
    wrapper.setAttribute('tabindex', '0');
    wrapper.setAttribute('aria-pressed', 'false');
    const onKey = (e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        wrapper.click();
      }
    };
    wrapper.addEventListener('keydown', onKey);
    ctx.registerCleanup(() => wrapper.removeEventListener('keydown', onKey));
  }

  if (iconRaw != null) {
    const ic = renderIcon(iconRaw, 'Chip.icon');
    ic.classList.add('tf-chip__icon');
    wrapper.appendChild(ic);
  } else if (avatarRaw != null) {
    const av = renderAvatarRef(avatarRaw, 'Chip.avatar');
    av.classList.add('tf-chip__avatar');
    wrapper.appendChild(av);
  }

  const lblEl = document.createElement('span');
  lblEl.classList.add('tf-chip__label');
  applyReactiveText(lblEl, label, ctx, null);
  wrapper.appendChild(lblEl);

  if (selectedBind != null) {
    const apply = () => {
      const sel = resolveBindRef(selectedBind, ctx.store) === true;
      if (sel) {
        wrapper.classList.add('tf-chip--selected');
        wrapper.setAttribute('aria-pressed', 'true');
      } else {
        wrapper.classList.remove('tf-chip--selected');
        if (wrapper.hasAttribute('aria-pressed')) wrapper.setAttribute('aria-pressed', 'false');
      }
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(selectedBind, ctx.store, apply));
  }

  if (removable) {
    const rm = document.createElement('button');
    rm.setAttribute('type', 'button');
    rm.setAttribute('aria-label', 'Remove');
    rm.classList.add('tf-chip__remove');
    rm.textContent = '×';
    const onRemove = (e) => {
      e.preventDefault();
      e.stopPropagation();
      wrapper.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('remove', {
          bubbles: false,
          detail: null,
        })
      );
    };
    rm.addEventListener('click', onRemove);
    ctx.registerCleanup(() => rm.removeEventListener('click', onRemove));
    wrapper.appendChild(rm);
  }

  return wrapper;
}

/// AvatarRef inline render
const SAFE_AVATAR_SRC = /^(https:\/\/|data:image\/(png|jpeg|gif|webp|svg\+xml);)/;
const AVATAR_REF_KINDS = new Set(['image', 'initials', 'icon']);
function renderAvatarRef(ref, ctx) {
  if (typeof ref !== 'object' || ref == null || Array.isArray(ref)) {
    throw new TypeError(`${ctx}: AvatarRef must be object`);
  }
  if (typeof ref.kind !== 'string' || !AVATAR_REF_KINDS.has(ref.kind)) {
    throw new TypeError(`${ctx}.kind must be image/initials/icon, got ${ref.kind}`);
  }
  const wrap = document.createElement('span');
  wrap.classList.add('tf-avatar-ref');
  if (ref.kind === 'image') {
    for (const k of Object.keys(ref)) {
      if (k !== 'kind' && k !== 'ref') throw new TypeError(`${ctx}: unexpected key '${k}' for image`);
    }
    if (typeof ref.ref !== 'string' || ref.ref.length === 0) {
      throw new TypeError(`${ctx}.ref required for image`);
    }
    if (!SAFE_AVATAR_SRC.test(ref.ref)) {
      throw new TypeError(`${ctx}.ref: only https:// or data:image/* allowed`);
    }
    const img = document.createElement('img');
    img.setAttribute('src', ref.ref);
    img.setAttribute('alt', '');
    img.setAttribute('loading', 'lazy');
    img.classList.add('tf-avatar-ref__img');
    wrap.appendChild(img);
  } else if (ref.kind === 'initials') {
    for (const k of Object.keys(ref)) {
      if (k !== 'kind' && k !== 'initials') throw new TypeError(`${ctx}: unexpected key '${k}' for initials`);
    }
    if (typeof ref.initials !== 'string' || ref.initials.length === 0) {
      throw new TypeError(`${ctx}.initials required`);
    }
    const ini = document.createElement('span');
    ini.classList.add('tf-avatar-ref__initials');
    ini.textContent = ref.initials.slice(0, 3);
    wrap.appendChild(ini);
  } else {
    for (const k of Object.keys(ref)) {
      if (k !== 'kind' && k !== 'icon') throw new TypeError(`${ctx}: unexpected key '${k}' for icon`);
    }
    if (ref.icon == null) throw new TypeError(`${ctx}.icon required`);
    const iconEl = renderIcon(ref.icon, `${ctx}.icon`);
    iconEl.classList.add('tf-avatar-ref__icon');
    wrap.appendChild(iconEl);
  }
  return wrap;
}

// =============================================================================
// Tag (0x020C)
// =============================================================================

export const TAG_TAG = 0x020C;
const TAG_FIELD_KEYS = new Set([0, 1, 2]);

function renderTag(component, ctx) {
  assertOnlyKnownFields(component.fields, TAG_FIELD_KEYS, 'Tag');

  const tone = requireEnum(ctx.readField(component.fields, 0), TONES, 'Tag.tone');
  const label = ctx.readField(component.fields, 1);
  if (label == null) throw new TypeError('Tag.label is required');
  const size = requireEnum(ctx.readField(component.fields, 2), TAG_SIZES, 'Tag.size');

  const wrapper = document.createElement('span');
  wrapper.classList.add('tf-tag');
  wrapper.classList.add(`tf-tag--tone-${tone}`);
  wrapper.classList.add(`tf-tag--size-${size}`);
  applyReactiveText(wrapper, label, ctx, null);
  return wrapper;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerDataStatLabelsRenderers() {
  if (!lookupComponentRenderer(KEY_VALUE_TAG)) registerComponentRenderer(KEY_VALUE_TAG, renderKeyValue);
  if (!lookupComponentRenderer(STAT_CARD_TAG)) registerComponentRenderer(STAT_CARD_TAG, renderStatCard);
  if (!lookupComponentRenderer(STAT_TAG)) registerComponentRenderer(STAT_TAG, renderStat);
  if (!lookupComponentRenderer(BADGE_TAG)) registerComponentRenderer(BADGE_TAG, renderBadge);
  if (!lookupComponentRenderer(CHIP_TAG)) registerComponentRenderer(CHIP_TAG, renderChip);
  if (!lookupComponentRenderer(TAG_TAG)) registerComponentRenderer(TAG_TAG, renderTag);
}
