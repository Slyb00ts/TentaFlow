// =============================================================================
// Plik: sdk-runtime/data-avatar-lists-renderer.js
// Opis: Renderery §4 Data Display avatar + list — chunk 3.3d-3:
//   - Avatar      (0x020D) — single avatar z size/shape/status presence dot
//   - AvatarGroup (0x020E) — stack avatarów z max_visible overflow indicator
//   - BulletList  (0x020F) — bullet/numbered/check/icon list z density
//   - Timeline    (0x0210) — chronological items z action_id 'item_click'
//
// Avatar/AvatarGroup używają tej samej tagged-union AvatarRef shape co Chip
// (chunk 3.3d-2), ale przez full Component z parsowaniem FieldMap. Timeline
// items mają unique id, ts_ms (Unix millis), title BindRef, optional
// description/icon/tone/action_id; show_dates renderuje grup po dniu.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/{avatar,lists}.rs +
// inline.rs (TimelineItem, AvatarRef).
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

const TONES = new Set(['neutral', 'primary', 'success', 'warning', 'critical', 'info', 'muted']);
const AVATAR_SIZES = new Set(['xs', 'sm', 'md', 'lg', 'xl']);
const AVATAR_SHAPES = new Set(['circle', 'rounded', 'square']);
const AVATAR_STATUSES = new Set(['online', 'offline', 'busy', 'away']);
const AVATAR_OVERLAPS = new Set(['tight', 'default', 'loose']);
const AVATAR_REF_KINDS = new Set(['image', 'initials', 'icon']);
const BULLET_LIST_VARIANTS = new Set(['bullet', 'numbered', 'check', 'icon']);
const TIMELINE_ORIENTATIONS = new Set(['vertical', 'horizontal']);
const DENSITIES = new Set(['compact', 'default', 'comfortable']);
const SAFE_AVATAR_SRC = /^(https:\/\/|data:image\/(png|jpeg|gif|webp|svg\+xml);)/;
// Timeline item id grammar + action_id: [a-z0-9_-]+ length 1..=64.
const ID_RE = /^[a-z0-9_-]{1,64}$/;
const TIMELINE_ITEM_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);

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
  if (typeof v === 'bigint') {
    if (v < 0n || v > 0xFFn) throw new TypeError(`${ctx}: expected u8, got ${v}`);
    return Number(v);
  }
  if (!Number.isInteger(v) || v < 0 || v > 0xFF) throw new TypeError(`${ctx}: expected u8, got ${v}`);
  return v;
}
function requireI64Ms(v, ctx) {
  // Accept number or bigint. Wymagamy timestamp ±8.64e15 ms (Date TimeClip).
  if (typeof v === 'bigint') {
    if (v < -8_640_000_000_000_000n || v > 8_640_000_000_000_000n) {
      throw new TypeError(`${ctx}: ts_ms out of TimeClip range`);
    }
    return Number(v);
  }
  if (typeof v !== 'number' || !Number.isInteger(v)) {
    throw new TypeError(`${ctx}: expected i64 integer (Unix ms), got ${v}`);
  }
  if (v < -8_640_000_000_000_000 || v > 8_640_000_000_000_000) {
    throw new TypeError(`${ctx}: ts_ms out of TimeClip range`);
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

// =============================================================================
// AvatarRef rendering (shared by Avatar + AvatarGroup)
// =============================================================================

// Number of deterministic initials-avatar color buckets. Backed by the
// .tf-avatar-source--auto-N palette in controls.css.
const AVATAR_AUTO_COLOR_COUNT = 6;

/// Stable hash of the initials text → palette bucket. FNV-1a keeps the same
/// person the same color across renders without any host round-trip (B2).
function avatarAutoColorClass(text) {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  const bucket = (h >>> 0) % AVATAR_AUTO_COLOR_COUNT;
  return `tf-avatar-source--auto-${bucket}`;
}

/// Parsuje + renderuje AvatarRef tagged union. Zwraca <span class=tf-avatar-source>.
/// `autoColorInitials` (default true) tints an initials source with a
/// deterministic palette bucket derived from the initials text — used when the
/// Avatar declares no explicit tone (B2). Set false to keep the neutral fill.
function renderAvatarSource(ref, ctx, sizeClass, autoColorInitials = false) {
  if (typeof ref !== 'object' || ref == null || Array.isArray(ref)) {
    throw new TypeError(`${ctx}: AvatarRef must be object`);
  }
  if (typeof ref.kind !== 'string' || !AVATAR_REF_KINDS.has(ref.kind)) {
    throw new TypeError(`${ctx}.kind must be image/initials/icon, got ${ref.kind}`);
  }
  const wrap = document.createElement('span');
  wrap.classList.add('tf-avatar-source');
  if (sizeClass) wrap.classList.add(sizeClass);
  if (ref.kind === 'image') {
    assertOnlyKnownObjectKeys(ref, new Set(['kind', 'ref']), `${ctx}.image`);
    if (typeof ref.ref !== 'string' || ref.ref.length === 0) {
      throw new TypeError(`${ctx}.ref required`);
    }
    if (!SAFE_AVATAR_SRC.test(ref.ref)) {
      throw new TypeError(`${ctx}.ref: only https:// or data:image/* allowed`);
    }
    const img = document.createElement('img');
    img.setAttribute('src', ref.ref);
    img.setAttribute('alt', '');
    img.setAttribute('loading', 'lazy');
    img.classList.add('tf-avatar-source__img');
    wrap.appendChild(img);
  } else if (ref.kind === 'initials') {
    assertOnlyKnownObjectKeys(ref, new Set(['kind', 'initials']), `${ctx}.initials`);
    if (typeof ref.initials !== 'string' || ref.initials.length === 0) {
      throw new TypeError(`${ctx}.initials required`);
    }
    const txt = document.createElement('span');
    txt.classList.add('tf-avatar-source__initials');
    txt.textContent = ref.initials.slice(0, 3);
    if (autoColorInitials) wrap.classList.add(avatarAutoColorClass(ref.initials));
    wrap.appendChild(txt);
  } else {
    assertOnlyKnownObjectKeys(ref, new Set(['kind', 'icon']), `${ctx}.icon`);
    if (ref.icon == null) throw new TypeError(`${ctx}.icon required`);
    const ic = renderIcon(ref.icon, `${ctx}.icon`);
    ic.classList.add('tf-avatar-source__icon');
    wrap.appendChild(ic);
  }
  return wrap;
}

// =============================================================================
// Avatar (0x020D)
// =============================================================================

export const AVATAR_TAG = 0x020D;
const AVATAR_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderAvatar(component, ctx) {
  assertOnlyKnownFields(component.fields, AVATAR_FIELD_KEYS, 'Avatar');

  const sourceRaw = ctx.readField(component.fields, 0);
  if (sourceRaw == null) throw new TypeError('Avatar.source is required');
  const size = requireEnum(ctx.readField(component.fields, 1), AVATAR_SIZES, 'Avatar.size');
  const shape = requireEnum(ctx.readField(component.fields, 2), AVATAR_SHAPES, 'Avatar.shape');
  const statusRaw = ctx.readField(component.fields, 3);
  const status = statusRaw == null ? null : requireEnum(statusRaw, AVATAR_STATUSES, 'Avatar.status');
  const toneRaw = ctx.readField(component.fields, 4);
  const tone = toneRaw == null ? null : requireEnum(toneRaw, TONES, 'Avatar.tone');

  const wrapper = document.createElement('span');
  wrapper.classList.add('tf-avatar-block');
  wrapper.classList.add(`tf-avatar--size-${size}`);
  wrapper.classList.add(`tf-avatar--shape-${shape}`);
  if (tone) wrapper.classList.add(`tf-avatar--tone-${tone}`);

  // An explicit tone wins; without one, initials sources get a deterministic
  // color from their text (B2) so people read as distinct chips, not mono.
  const source = renderAvatarSource(sourceRaw, 'Avatar.source', undefined, tone == null);
  source.classList.add('tf-avatar__source');
  wrapper.appendChild(source);

  if (status) {
    const dot = document.createElement('span');
    dot.classList.add('tf-avatar__status');
    dot.classList.add(`tf-avatar__status--${status}`);
    dot.setAttribute('role', 'status');
    dot.setAttribute('aria-label', status);
    wrapper.appendChild(dot);
  }
  return wrapper;
}

/// Pełna walidacja Component shape dla Avatar (lustro
/// `ComponentRenderer.assertComponent`, ale lokalne — engine'owy validator
/// uruchamiamy dopiero przy renderChild dla widocznych entries, więc
/// overflow entries musi sprawdzić sam parser). Wymóg: tag=0x020D, id
/// string, fields Array<[u8, Value]>, handlers/bind/visibility null lub
/// odpowiedni shape (zostawiamy engine'owi pełną walidację dla widocznych).
function assertAvatarComponentShape(c, ctx) {
  if (!c || typeof c !== 'object' || Array.isArray(c)) {
    throw new TypeError(`${ctx}: Component must be object`);
  }
  if (c.tag !== AVATAR_TAG) {
    throw new TypeError(`${ctx}: expected Avatar (0x020D), got tag 0x${(c.tag || 0).toString(16)}`);
  }
  if (typeof c.id !== 'string' || c.id.length === 0) {
    throw new TypeError(`${ctx}.id must be non-empty string`);
  }
  if (!Array.isArray(c.fields)) {
    throw new TypeError(`${ctx}.fields must be Array<[u8, Value]>`);
  }
  // Avatar wymaga pola 0 (source), 1 (size), 2 (shape) — szybki check
  // czy są obecne (full walidacja pól odbywa się przy render visible
  // slice'a; overflow entries muszą przynajmniej spełnić minimum spec'u).
  let hasSource = false, hasSize = false, hasShape = false;
  for (const entry of c.fields) {
    if (!Array.isArray(entry) || entry.length !== 2) {
      throw new TypeError(`${ctx}.fields entry must be [u8, Value]`);
    }
    const [k] = entry;
    if (!Number.isInteger(k) || k < 0 || k > 0xFF) {
      throw new TypeError(`${ctx}.fields key must be u8`);
    }
    if (k === 0) hasSource = true;
    if (k === 1) hasSize = true;
    if (k === 2) hasShape = true;
  }
  if (!hasSource) throw new TypeError(`${ctx}: missing required field source (key 0)`);
  if (!hasSize) throw new TypeError(`${ctx}: missing required field size (key 1)`);
  if (!hasShape) throw new TypeError(`${ctx}: missing required field shape (key 2)`);
}

// =============================================================================
// AvatarGroup (0x020E)
// =============================================================================

export const AVATAR_GROUP_TAG = 0x020E;
const AVATAR_GROUP_FIELD_KEYS = new Set([0, 1, 2, 3]);

function renderAvatarGroup(component, ctx) {
  assertOnlyKnownFields(component.fields, AVATAR_GROUP_FIELD_KEYS, 'AvatarGroup');

  const avatarsRaw = ctx.readField(component.fields, 0);
  const avatars = avatarsRaw == null ? [] : (() => {
    if (!Array.isArray(avatarsRaw)) throw new TypeError('AvatarGroup.avatars: expected Array<Component>');
    for (let i = 0; i < avatarsRaw.length; i++) {
      assertAvatarComponentShape(avatarsRaw[i], `AvatarGroup.avatars[${i}]`);
    }
    return avatarsRaw;
  })();
  const maxVisible = requireU8(ctx.readField(component.fields, 1), 'AvatarGroup.max_visible');
  if (maxVisible === 0) throw new TypeError('AvatarGroup.max_visible must be > 0');
  const overlap = requireEnum(ctx.readField(component.fields, 2), AVATAR_OVERLAPS, 'AvatarGroup.overlap');
  const size = requireEnum(ctx.readField(component.fields, 3), AVATAR_SIZES, 'AvatarGroup.size');

  const wrapper = document.createElement('span');
  wrapper.classList.add('tf-avatar-group');
  wrapper.classList.add(`tf-avatar-group--overlap-${overlap}`);
  wrapper.classList.add(`tf-avatar-group--size-${size}`);

  const visible = avatars.slice(0, maxVisible);
  for (const av of visible) {
    const childEl = ctx.renderChild(av);
    childEl.classList.add('tf-avatar-group__item');
    wrapper.appendChild(childEl);
  }
  const overflow = avatars.length - visible.length;
  if (overflow > 0) {
    const more = document.createElement('span');
    more.classList.add('tf-avatar-group__more');
    more.classList.add('tf-avatar-block');
    more.classList.add(`tf-avatar--size-${size}`);
    more.classList.add('tf-avatar--shape-circle');
    more.setAttribute('aria-label', `${overflow} more`);
    more.textContent = `+${overflow}`;
    wrapper.appendChild(more);
  }
  return wrapper;
}

// =============================================================================
// BulletList (0x020F)
// =============================================================================

export const BULLET_LIST_TAG = 0x020F;
const BULLET_LIST_FIELD_KEYS = new Set([0, 1, 2, 3]);

function renderBulletList(component, ctx) {
  assertOnlyKnownFields(component.fields, BULLET_LIST_FIELD_KEYS, 'BulletList');

  const itemsRaw = ctx.readField(component.fields, 0);
  const items = itemsRaw == null ? [] : (() => {
    if (!Array.isArray(itemsRaw)) throw new TypeError('BulletList.items: expected Array<BindRef>');
    return itemsRaw;
  })();
  const variant = requireEnum(ctx.readField(component.fields, 1), BULLET_LIST_VARIANTS, 'BulletList.variant');
  const toneRaw = ctx.readField(component.fields, 2);
  const tone = toneRaw == null ? null : requireEnum(toneRaw, TONES, 'BulletList.tone');
  const density = requireEnum(ctx.readField(component.fields, 3), DENSITIES, 'BulletList.density');

  // variant=numbered → <ol>; inaczej <ul> z custom markers.
  const wrapper = document.createElement(variant === 'numbered' ? 'ol' : 'ul');
  wrapper.classList.add('tf-bullet-list');
  wrapper.classList.add(`tf-bullet-list--variant-${variant}`);
  wrapper.classList.add(`tf-bullet-list--density-${density}`);
  if (tone) wrapper.classList.add(`tf-bullet-list--tone-${tone}`);

  items.forEach((item, idx) => {
    const li = document.createElement('li');
    li.classList.add('tf-bullet-list__item');
    if (variant === 'check') {
      const mark = document.createElement('span');
      mark.classList.add('tf-bullet-list__check');
      mark.setAttribute('aria-hidden', 'true');
      mark.textContent = '✓';
      li.appendChild(mark);
    } else if (variant === 'icon') {
      // Icon variant: per-list jeden icon symbol (•) — addon nie ma per-item
      // icon w spec; renderujemy general placeholder.
      const mark = document.createElement('span');
      mark.classList.add('tf-bullet-list__icon-mark');
      mark.setAttribute('aria-hidden', 'true');
      mark.textContent = '◆';
      li.appendChild(mark);
    }
    const txt = document.createElement('span');
    txt.classList.add('tf-bullet-list__text');
    applyTextBind(txt, item, ctx);
    li.appendChild(txt);
    wrapper.appendChild(li);
  });
  return wrapper;
}

// =============================================================================
// Timeline (0x0210)
// =============================================================================

export const TIMELINE_TAG = 0x0210;
const TIMELINE_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function parseTimelineItem(raw, ctx) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: TimelineItem must be FieldMap`);
  const seen = new Set();
  const it = { id: null, ts_ms: null, title: null, description: null, icon: null, tone: null, action_id: null };
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!TIMELINE_ITEM_KEYS.has(k)) throw new TypeError(`${ctx}: unknown TimelineItem key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    switch (k) {
      case 0: {
        const id = requireString(v, `${ctx}.id`);
        if (!ID_RE.test(id)) throw new TypeError(`${ctx}.id: invalid grammar`);
        it.id = id;
        break;
      }
      case 1: it.ts_ms = requireI64Ms(v, `${ctx}.ts_ms`); break;
      case 2: it.title = v; break;
      case 3: if (v != null) it.description = v; break;
      case 4: if (v != null) it.icon = v; break;
      case 5: if (v != null) it.tone = requireEnum(v, TONES, `${ctx}.tone`); break;
      case 6: if (v != null) {
        const a = requireString(v, `${ctx}.action_id`);
        if (!ID_RE.test(a)) throw new TypeError(`${ctx}.action_id: invalid grammar`);
        it.action_id = a;
      } break;
    }
  }
  if (it.id == null) throw new TypeError(`${ctx}: id required`);
  if (it.ts_ms == null) throw new TypeError(`${ctx}: ts_ms required`);
  if (it.title == null) throw new TypeError(`${ctx}: title required`);
  return it;
}

function renderTimeline(component, ctx) {
  assertOnlyKnownFields(component.fields, TIMELINE_FIELD_KEYS, 'Timeline');

  const itemsRaw = ctx.readField(component.fields, 0);
  const items = itemsRaw == null ? [] : (() => {
    if (!Array.isArray(itemsRaw)) throw new TypeError('Timeline.items: expected Array<TimelineItem>');
    return itemsRaw.map((it, i) => parseTimelineItem(it, `Timeline.items[${i}]`));
  })();
  // Duplicate id detection.
  const idSet = new Set();
  for (const it of items) {
    if (idSet.has(it.id)) throw new TypeError(`Timeline.items: duplicate id '${it.id}'`);
    idSet.add(it.id);
  }
  const orientation = requireEnum(ctx.readField(component.fields, 1), TIMELINE_ORIENTATIONS, 'Timeline.orientation');
  const density = requireEnum(ctx.readField(component.fields, 2), DENSITIES, 'Timeline.density');
  const showDates = requireBool(ctx.readField(component.fields, 3), 'Timeline.show_dates');
  const groupByDay = requireBool(ctx.readField(component.fields, 4), 'Timeline.group_by_day');

  const wrapper = document.createElement('ol');
  wrapper.classList.add('tf-timeline-track');
  wrapper.classList.add(`tf-timeline--${orientation}`);
  wrapper.classList.add(`tf-timeline--density-${density}`);

  const fmtDay = (ms) => {
    try {
      return new Intl.DateTimeFormat(ctx.locale, {
        year: 'numeric', month: 'short', day: 'numeric',
      }).format(new Date(ms));
    } catch { return ''; }
  };
  const fmtTime = (ms) => {
    try {
      return new Intl.DateTimeFormat(ctx.locale, {
        hour: '2-digit', minute: '2-digit',
      }).format(new Date(ms));
    } catch { return ''; }
  };

  let lastDayKey = null;
  for (let i = 0; i < items.length; i++) {
    const it = items[i];
    if (groupByDay) {
      const dayKey = new Date(it.ts_ms).toISOString().slice(0, 10);
      if (dayKey !== lastDayKey) {
        lastDayKey = dayKey;
        const header = document.createElement('li');
        header.classList.add('tf-timeline__day-header');
        header.setAttribute('role', 'presentation');
        header.textContent = fmtDay(it.ts_ms);
        wrapper.appendChild(header);
      }
    }
    const li = document.createElement('li');
    li.classList.add('tf-timeline__item');
    li.setAttribute('data-item-id', it.id);
    if (it.tone) li.classList.add(`tf-timeline__item--tone-${it.tone}`);

    const marker = document.createElement('span');
    marker.classList.add('tf-timeline__marker');
    marker.setAttribute('aria-hidden', 'true');
    if (it.icon != null) {
      const ic = renderIcon(it.icon, `Timeline.items[${i}].icon`);
      ic.classList.add('tf-timeline__icon');
      marker.appendChild(ic);
    } else {
      marker.textContent = '●';
    }
    li.appendChild(marker);

    const body = document.createElement('div');
    body.classList.add('tf-timeline__body');
    const titleEl = document.createElement('div');
    titleEl.classList.add('tf-timeline__title');
    applyTextBind(titleEl, it.title, ctx);
    body.appendChild(titleEl);
    if (showDates) {
      const time = document.createElement('time');
      time.classList.add('tf-timeline__time');
      time.setAttribute('datetime', new Date(it.ts_ms).toISOString());
      time.textContent = fmtTime(it.ts_ms);
      body.appendChild(time);
    }
    if (it.description != null) {
      const desc = document.createElement('div');
      desc.classList.add('tf-timeline__description');
      applyTextBind(desc, it.description, ctx);
      body.appendChild(desc);
    }
    li.appendChild(body);

    if (it.action_id != null) {
      li.classList.add('tf-timeline__item--clickable');
      li.setAttribute('role', 'button');
      li.setAttribute('tabindex', '0');
      const onClick = (e) => {
        e.preventDefault();
        wrapper.dispatchEvent(
          new (globalThis.CustomEvent || globalThis.Event)('item_click', {
            bubbles: false,
            detail: { item_id: it.id, action_id: it.action_id, ts_ms: it.ts_ms },
          })
        );
      };
      const onKey = (e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onClick(e);
        }
      };
      li.addEventListener('click', onClick);
      li.addEventListener('keydown', onKey);
      ctx.registerCleanup(() => {
        li.removeEventListener('click', onClick);
        li.removeEventListener('keydown', onKey);
      });
    }

    wrapper.appendChild(li);
  }
  return wrapper;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerDataAvatarListsRenderers() {
  if (!lookupComponentRenderer(AVATAR_TAG)) registerComponentRenderer(AVATAR_TAG, renderAvatar);
  if (!lookupComponentRenderer(AVATAR_GROUP_TAG)) registerComponentRenderer(AVATAR_GROUP_TAG, renderAvatarGroup);
  if (!lookupComponentRenderer(BULLET_LIST_TAG)) registerComponentRenderer(BULLET_LIST_TAG, renderBulletList);
  if (!lookupComponentRenderer(TIMELINE_TAG)) registerComponentRenderer(TIMELINE_TAG, renderTimeline);
}
