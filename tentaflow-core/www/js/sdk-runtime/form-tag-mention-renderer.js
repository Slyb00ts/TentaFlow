// =============================================================================
// File: sdk-runtime/form-tag-mention-renderer.js
// Description: TagInput (0x0308) + MentionInput (0x0309) renderers. TagInput
// renders through <tf-tag-input> (removable chips + free-text entry);
// MentionInput renders through <tf-mention-input> (textarea + @-trigger
// suggestion popover).
//
// Both renderers treat the store as the source of truth and are read-only with
// respect to it — component mutations are intercepted, validated and re-emitted
// in SDK shape (the `__tfReemit` pattern, same as the Combobox/MultiSelect
// renderers) so the dispatcher never sees component-internal payloads. The host
// applies the corresponding state patch; the store subscription then re-feeds
// the component.
//
// TagInput store value is an ARRAY of strings (values_path). It emits:
//   add    { value, tags }   — a tag was added (tags = resulting array)
//   remove { value, index, tags }
//   change { tags, kind: 'array' }
// MentionInput store value is a string (bind_path); mentions_path is an array
// of suggestion objects {id, label} fed to the popover. It emits:
//   search  { trigger, query, action_id }
//   mention { id, label, trigger }
//   change  { value, kind: 'tstr' }
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/form/inputs.rs TagInput +
// MentionInput.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';

// =============================================================================
// Validators
// =============================================================================

function requireBool(v, ctx) {
  if (typeof v !== 'boolean') throw new TypeError(`${ctx}: expected boolean, got ${typeof v}`);
  return v;
}
function requirePath(v, ctx) {
  if (!Array.isArray(v)) throw new TypeError(`${ctx}: expected StatePath (Array<PathSegment>)`);
  return v;
}
function requireString(v, ctx) {
  if (typeof v !== 'string') throw new TypeError(`${ctx}: expected string`);
  return v;
}
function requireU32(v, ctx) {
  if (typeof v === 'bigint') {
    if (v < 0n || v > 0xFFFFFFFFn) throw new TypeError(`${ctx}: expected u32, got ${v}`);
    return Number(v);
  }
  if (!Number.isInteger(v) || v < 0 || v > 0xFFFFFFFF) {
    throw new TypeError(`${ctx}: expected u32, got ${v}`);
  }
  return v;
}
function requireStringArray(v, ctx) {
  if (!Array.isArray(v)) throw new TypeError(`${ctx}: expected Array<string>`);
  for (let i = 0; i < v.length; i++) {
    if (typeof v[i] !== 'string') throw new TypeError(`${ctx}[${i}]: expected string`);
  }
  return v;
}
function assertOnlyKnownFields(fields, allowedKeys, name) {
  for (const [k] of fields) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(`${name}: unknown field key ${k} (allowed: ${[...allowedKeys].join(',')})`);
    }
  }
}

function applyPlaceholderReactive(el, bindRef, ctx) {
  if (bindRef == null) return;
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    if (v == null || v === '') el.removeAttribute('placeholder');
    else el.setAttribute('placeholder', String(v));
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

// =============================================================================
// TagInput (0x0308)
// =============================================================================

export const TAGINPUT_TAG = 0x0308;
const TAGINPUT_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);

function renderTagInput(component, ctx) {
  assertOnlyKnownFields(component.fields, TAGINPUT_FIELD_KEYS, 'TagInput');

  const valuesPath = requirePath(ctx.readField(component.fields, 0), 'TagInput.values_path');
  const placeholderBind = ctx.readField(component.fields, 1);
  const validatorsRaw = ctx.readField(component.fields, 2);
  if (!Array.isArray(validatorsRaw)) {
    throw new TypeError('TagInput.validators: expected Array<ValidationRule>');
  }
  const maxTagsRaw = ctx.readField(component.fields, 3);
  const maxTags = maxTagsRaw == null ? null : requireU32(maxTagsRaw, 'TagInput.max_tags');
  if (maxTags != null && maxTags === 0) {
    throw new TypeError('TagInput.max_tags must be > 0 if set');
  }
  const separator = requireStringArray(ctx.readField(component.fields, 4), 'TagInput.separator');
  const dedupe = requireBool(ctx.readField(component.fields, 5), 'TagInput.dedupe');

  // ValidationRule.kind === 'min_length'/'max_length' constrain a single tag's
  // length; 'pattern' constrains its shape. Other kinds (required, etc.) are
  // not meaningful per-tag and are ignored here.
  let minTagLen = null;
  let maxTagLen = null;
  let pattern = null;
  for (const rule of validatorsRaw) {
    if (!rule || typeof rule !== 'object') continue;
    if (rule.kind === 'min_length' && rule.value != null) {
      minTagLen = requireU32(rule.value, 'TagInput.validators.min_length');
    } else if (rule.kind === 'max_length' && rule.value != null) {
      maxTagLen = requireU32(rule.value, 'TagInput.validators.max_length');
    } else if (rule.kind === 'pattern' && typeof rule.value === 'string') {
      pattern = new RegExp(rule.value);
    }
  }
  const tagAllowed = (tag) => {
    if (minTagLen != null && tag.length < minTagLen) return false;
    if (maxTagLen != null && tag.length > maxTagLen) return false;
    if (pattern != null && !pattern.test(tag)) return false;
    return true;
  };

  const el = document.createElement('tf-tag-input');
  if (dedupe) el.setAttribute('dedupe', '');
  if (maxTags != null) el.setAttribute('max-tags', String(maxTags));
  el.separators = separator;

  if (placeholderBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError(
        'TagInput without `placeholder` requires Component.a11y.label for accessible name'
      );
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError('TagInput.a11y.label must resolve to non-blank string at initial render');
    }
  }
  if (component.a11y != null && component.a11y.label != null) {
    const applyAria = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) el.setAttribute('aria-label', v);
      else el.removeAttribute('aria-label');
    };
    applyAria();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAria));
  }
  applyPlaceholderReactive(el, placeholderBind, ctx);

  // Store → component tags (the store is the source of truth).
  const readTags = () => {
    let arr;
    try { arr = ctx.store.read(valuesPath); } catch { arr = undefined; }
    return Array.isArray(arr) ? arr.map((t) => String(t)) : [];
  };
  const syncFromStore = () => { el.tags = readTags(); };
  syncFromStore();
  ctx.registerCleanup(ctx.store.subscribe(valuesPath, syncFromStore));

  // tf-tag-input 'add' {tag}: validate, then re-emit SDK 'add'. On rejection
  // restore the component to the store tags (drops the optimistic chip).
  const onAdd = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    if (!e.detail || typeof e.detail.tag !== 'string') return;
    const tag = e.detail.tag;
    const current = readTags();
    if (!tagAllowed(tag)
      || (dedupe && current.includes(tag))
      || (maxTags != null && current.length >= maxTags)) {
      syncFromStore();
      return;
    }
    const tags = [...current, tag];
    const add = new CustomEvent('add', { bubbles: false, detail: { value: tag, tags } });
    add.__tfReemit = true;
    el.dispatchEvent(add);
    const change = new CustomEvent('change', { bubbles: false, detail: { tags, kind: 'array' } });
    change.__tfReemit = true;
    el.dispatchEvent(change);
  };
  el.addEventListener('add', onAdd);
  ctx.registerCleanup(() => el.removeEventListener('add', onAdd));

  // tf-tag-input 'remove' {tag, index}: re-emit SDK 'remove' + 'change'.
  const onRemove = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    if (!e.detail || typeof e.detail.tag !== 'string' || typeof e.detail.index !== 'number') return;
    const current = readTags();
    const index = e.detail.index;
    if (index < 0 || index >= current.length) { syncFromStore(); return; }
    const tags = current.slice();
    tags.splice(index, 1);
    const remove = new CustomEvent('remove', {
      bubbles: false,
      detail: { value: e.detail.tag, index, tags },
    });
    remove.__tfReemit = true;
    el.dispatchEvent(remove);
    const change = new CustomEvent('change', { bubbles: false, detail: { tags, kind: 'array' } });
    change.__tfReemit = true;
    el.dispatchEvent(change);
  };
  el.addEventListener('remove', onRemove);
  ctx.registerCleanup(() => el.removeEventListener('remove', onRemove));

  // The component fires its own 'change' too; it is redundant with the SDK
  // change we re-emit from add/remove. Block the raw one (only ours passes).
  const onChange = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
  };
  el.addEventListener('change', onChange);
  ctx.registerCleanup(() => el.removeEventListener('change', onChange));

  return el;
}

// =============================================================================
// MentionInput (0x0309)
// =============================================================================

export const MENTIONINPUT_TAG = 0x0309;
const MENTIONINPUT_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderMentionInput(component, ctx) {
  assertOnlyKnownFields(component.fields, MENTIONINPUT_FIELD_KEYS, 'MentionInput');

  const bindPath = requirePath(ctx.readField(component.fields, 0), 'MentionInput.bind_path');
  const mentionsPath = requirePath(ctx.readField(component.fields, 1), 'MentionInput.mentions_path');
  const triggerChars = requireStringArray(
    ctx.readField(component.fields, 2), 'MentionInput.trigger_chars'
  );
  if (triggerChars.length === 0) {
    throw new TypeError('MentionInput.trigger_chars must be non-empty');
  }
  for (const t of triggerChars) {
    if (t.length !== 1) throw new TypeError('MentionInput.trigger_chars entries must be single characters');
  }
  const mentionActionId = requireString(
    ctx.readField(component.fields, 3), 'MentionInput.mention_action_id'
  );
  const placeholderBind = ctx.readField(component.fields, 4);

  const el = document.createElement('tf-mention-input');
  el.triggers = triggerChars;

  if (placeholderBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError(
        'MentionInput without `placeholder` requires Component.a11y.label for accessible name'
      );
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError('MentionInput.a11y.label must resolve to non-blank string at initial render');
    }
  }
  if (component.a11y != null && component.a11y.label != null) {
    const applyAria = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) el.setAttribute('aria-label', v);
      else el.removeAttribute('aria-label');
    };
    applyAria();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAria));
  }
  applyPlaceholderReactive(el, placeholderBind, ctx);

  // Store → component text (source of truth, skips writes while focused).
  const syncTextFromStore = () => {
    let v;
    try { v = ctx.store.read(bindPath); } catch { v = undefined; }
    const next = v == null ? '' : String(v);
    if (el.getAttribute('value') !== next) el.value = next;
  };
  syncTextFromStore();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, syncTextFromStore));

  // Store → component suggestions. Each entry is {id, label} (id may be an
  // object key carrying a numeric/string id; only the string forms are fed).
  const readSuggestions = () => {
    let arr;
    try { arr = ctx.store.read(mentionsPath); } catch { arr = undefined; }
    if (!Array.isArray(arr)) return [];
    const out = [];
    for (const s of arr) {
      if (!s || typeof s !== 'object') continue;
      if (s.id == null || s.label == null) continue;
      out.push({ id: String(s.id), label: String(s.label) });
    }
    return out;
  };
  const syncSuggestionsFromStore = () => { el.suggestions = readSuggestions(); };
  syncSuggestionsFromStore();
  ctx.registerCleanup(ctx.store.subscribe(mentionsPath, syncSuggestionsFromStore));

  // tf-mention-input 'search' {trigger, query} → SDK 'search' carrying the
  // mention_action_id so the host knows which addon action to invoke.
  const onSearch = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    if (!e.detail || typeof e.detail.query !== 'string') return;
    const ce = new CustomEvent('search', {
      bubbles: false,
      detail: { trigger: e.detail.trigger, query: e.detail.query, action_id: mentionActionId },
    });
    ce.__tfReemit = true;
    el.dispatchEvent(ce);
  };
  el.addEventListener('search', onSearch);
  ctx.registerCleanup(() => el.removeEventListener('search', onSearch));

  // tf-mention-input 'mention' {id, label, trigger} → SDK 'mention' (passthrough
  // shape; the host links it to mention_action_id).
  const onMention = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    if (!e.detail || typeof e.detail.id !== 'string') return;
    const ce = new CustomEvent('mention', {
      bubbles: false,
      detail: {
        id: e.detail.id,
        label: e.detail.label,
        trigger: e.detail.trigger,
        action_id: mentionActionId,
      },
    });
    ce.__tfReemit = true;
    el.dispatchEvent(ce);
  };
  el.addEventListener('mention', onMention);
  ctx.registerCleanup(() => el.removeEventListener('mention', onMention));

  // tf-mention-input 'change' {value} → SDK 'change' {value, kind: 'tstr'}.
  const onChange = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    if (!e.detail || typeof e.detail.value !== 'string') return;
    const ce = new CustomEvent('change', {
      bubbles: false,
      detail: { value: e.detail.value, kind: 'tstr' },
    });
    ce.__tfReemit = true;
    el.dispatchEvent(ce);
  };
  el.addEventListener('change', onChange);
  ctx.registerCleanup(() => el.removeEventListener('change', onChange));

  return el;
}

// =============================================================================
// Registration
// =============================================================================

export function registerFormTagMentionRenderers() {
  if (!lookupComponentRenderer(TAGINPUT_TAG)) {
    registerComponentRenderer(TAGINPUT_TAG, renderTagInput);
  }
  if (!lookupComponentRenderer(MENTIONINPUT_TAG)) {
    registerComponentRenderer(MENTIONINPUT_TAG, renderMentionInput);
  }
}
