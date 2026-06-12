// =============================================================================
// Plik: sdk-runtime/form-file-color-renderer.js
// Opis: Renderery FileInput (0x0318) + ColorPicker (0x0319) — chunk 3.3c-6.
//
// FileInput: <input type="file"> z opcjonalnym drag-drop area, walidacja
// accept/max_size_bytes/max_files po wyborze, emit 'change' z listą plików
// (metadata only — actual upload to host przez upload_action_id),
// 'reject' gdy walidacja nie przeszła.
//
// ColorPicker: 4 warianty:
//   - swatch        — siatka kolorów (allowed_tokens lub default palette)
//   - wheel         — <input type="color"> native
//   - compact       — chip + small dropdown swatches
//   - tokens_only   — pure semantic token list (allowed_tokens required)
//
// Wszystkie używają reactive bind_path (read-only). Spec ref:
// tentaflow-sdk-spec/src/protocol/ui/form/file_color.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';

// =============================================================================
// Walidatory
// =============================================================================

const FILE_CAPTURES = new Set(['user', 'environment']);
const COLOR_VARIANTS = new Set(['swatch', 'wheel', 'compact', 'tokens_only']);
const COLOR_TOKENS = new Set([
  'background_default', 'background_subtle', 'background_muted',
  'surface_default', 'surface_raised', 'surface_overlay',
  'border_default', 'border_strong', 'border_subtle',
  'text_default', 'text_muted', 'text_inverse',
  'accent_primary', 'accent_secondary',
  'tone_neutral', 'tone_success', 'tone_warning', 'tone_critical', 'tone_info',
]);
// Hex color #RGB / #RRGGBB / #RRGGBBAA — używane gdy variant != tokens_only
// i user wybiera dowolny kolor wheel'em / swatch hex'em.
const HEX_COLOR_RE = /^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$/;
// Default palette dla swatch gdy brak allowed_tokens — 16 sensownych
// kolorów (HSL-spaced).
const DEFAULT_SWATCH_PALETTE = Object.freeze([
  '#000000', '#ffffff', '#9ca3af', '#374151',
  '#ef4444', '#f97316', '#eab308', '#84cc16',
  '#22c55e', '#14b8a6', '#06b6d4', '#3b82f6',
  '#6366f1', '#8b5cf6', '#a855f7', '#ec4899',
]);

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
function requireU64(v, ctx) {
  // JS Number bezpieczne do 2^53; spec używa u64 ale realnie pliki >2^53 są
  // niespotykane. Wartości bigint są też akceptowane (host może wysłać u64
  // jako bigint).
  if (typeof v === 'bigint') {
    if (v < 0n) throw new TypeError(`${ctx}: expected u64, got ${v}`);
    return v;
  }
  if (!Number.isInteger(v) || v < 0) throw new TypeError(`${ctx}: expected u64, got ${v}`);
  return v;
}
function requirePath(v, ctx) {
  if (!Array.isArray(v)) throw new TypeError(`${ctx}: expected StatePath`);
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

function applyTextBind(element, bindRef, ctx) {
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    element.textContent = v == null ? '' : String(v);
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

/// Czy plik pasuje do accept pattern'a (mime/wildcard/extension).
/// Akceptujemy:
///   "image/png"     → exact MIME
///   "image/\*"      → MIME family
///   ".pdf"          → extension match (case-insensitive)
function matchesAcceptPattern(file, pattern) {
  if (pattern === '*' || pattern === '*/*') return true;
  if (pattern.startsWith('.')) {
    const name = (file.name || '').toLowerCase();
    return name.endsWith(pattern.toLowerCase());
  }
  if (pattern.endsWith('/*')) {
    const family = pattern.slice(0, -2);
    const ftype = file.type || '';
    return ftype.startsWith(`${family}/`);
  }
  return (file.type || '') === pattern;
}

function fileMatchesAnyAccept(file, patterns) {
  if (patterns.length === 0) return true;
  return patterns.some((p) => matchesAcceptPattern(file, p));
}

// =============================================================================
// FileInput (0x0318)
// =============================================================================

export const FILE_INPUT_TAG = 0x0318;
const FILE_INPUT_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);

function renderFileInput(component, ctx) {
  assertOnlyKnownFields(component.fields, FILE_INPUT_FIELD_KEYS, 'FileInput');

  const bindPath = requirePath(ctx.readField(component.fields, 0), 'FileInput.bind_path');
  const acceptRaw = ctx.readField(component.fields, 1);
  if (!Array.isArray(acceptRaw)) {
    throw new TypeError('FileInput.accept: expected Array<string>');
  }
  const accept = acceptRaw.map((s, i) => requireString(s, `FileInput.accept[${i}]`));
  const maxSizeBytes = requireU64(ctx.readField(component.fields, 2), 'FileInput.max_size_bytes');
  const maxFiles = requireU8(ctx.readField(component.fields, 3), 'FileInput.max_files');
  if (maxFiles === 0) throw new TypeError('FileInput.max_files must be > 0');
  const multiple = requireBool(ctx.readField(component.fields, 4), 'FileInput.multiple');
  const dragAndDrop = requireBool(ctx.readField(component.fields, 5), 'FileInput.drag_and_drop');
  const captureRaw = ctx.readField(component.fields, 6);
  const capture = captureRaw == null ? null : requireEnum(captureRaw, FILE_CAPTURES, 'FileInput.capture');
  const uploadActionId = requireString(
    ctx.readField(component.fields, 7), 'FileInput.upload_action_id'
  );
  const labelBind = ctx.readField(component.fields, 8);
  const hintBind = ctx.readField(component.fields, 9);

  // multiple=false ALE max_files>1 jest sprzeczne — wymuszamy spójność.
  if (!multiple && maxFiles > 1) {
    throw new TypeError('FileInput.multiple=false requires max_files=1');
  }

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-file-input');
  if (dragAndDrop) wrapper.classList.add('tf-file-input--dnd');

  let labelEl = null;
  if (labelBind != null) {
    labelEl = document.createElement('label');
    labelEl.classList.add('tf-file-input__label');
    applyTextBind(labelEl, labelBind, ctx);
    wrapper.appendChild(labelEl);
  }

  // Drop zone albo prosty button trigger.
  const dropzone = document.createElement('div');
  dropzone.classList.add('tf-file-input__dropzone');
  if (dragAndDrop) dropzone.classList.add('tf-file-input__dropzone--dnd');
  dropzone.setAttribute('role', 'button');
  dropzone.setAttribute('tabindex', '0');

  const trigger = document.createElement('span');
  trigger.classList.add('tf-file-input__trigger');
  trigger.textContent = multiple ? 'Wybierz pliki' : 'Wybierz plik';
  dropzone.appendChild(trigger);

  if (dragAndDrop) {
    const dndHint = document.createElement('span');
    dndHint.classList.add('tf-file-input__dnd-hint');
    dndHint.textContent = 'lub upuść tutaj';
    dropzone.appendChild(dndHint);
  }

  const input = document.createElement('input');
  input.setAttribute('type', 'file');
  input.classList.add('tf-file-input__input');
  const inputId = `tf-file-input-${component.id}`;
  input.setAttribute('id', inputId);
  if (labelEl) labelEl.setAttribute('for', inputId);
  if (accept.length > 0) input.setAttribute('accept', accept.join(','));
  if (multiple) input.setAttribute('multiple', '');
  if (capture) input.setAttribute('capture', capture);
  // Ukryty natywnie — dropzone wywołuje click na input'cie.
  input.classList.add('tf-file-input__input--hidden');

  if (labelBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError('FileInput without label requires Component.a11y.label');
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError('FileInput.a11y.label must resolve to non-blank string');
    }
    const applyAria = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) {
        input.setAttribute('aria-label', v);
        dropzone.setAttribute('aria-label', v);
      } else {
        input.removeAttribute('aria-label');
        dropzone.removeAttribute('aria-label');
      }
    };
    applyAria();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAria));
  }

  wrapper.appendChild(input);
  wrapper.appendChild(dropzone);

  if (hintBind != null) {
    const hint = document.createElement('span');
    hint.classList.add('tf-file-input__hint');
    applyTextBind(hint, hintBind, ctx);
    wrapper.appendChild(hint);
  }

  // Lista wybranych plików — wyświetla się po valid selection.
  const fileList = document.createElement('ul');
  fileList.classList.add('tf-file-input__list');
  wrapper.appendChild(fileList);

  // Sync z store: store value to lista metadata (po upload). Bez wsparcia
  // dla wstawiania natywnych File obiektów w input.files (security), więc
  // tylko renderujemy nazwy z store i ewentualnie clear przyciskiem.
  const renderStoreFiles = () => {
    fileList.innerHTML = '';
    let v;
    try { v = ctx.store.read(bindPath); } catch { v = undefined; }
    if (!Array.isArray(v)) return;
    for (const meta of v) {
      const li = document.createElement('li');
      li.classList.add('tf-file-input__item');
      const name = meta && typeof meta === 'object' && typeof meta.name === 'string'
        ? meta.name
        : String(meta);
      const nameEl = document.createElement('span');
      nameEl.classList.add('tf-file-input__item-name');
      nameEl.textContent = name;
      li.appendChild(nameEl);
      if (meta && typeof meta === 'object' && (typeof meta.size === 'number' || typeof meta.size === 'bigint')) {
        const sizeEl = document.createElement('span');
        sizeEl.classList.add('tf-file-input__item-size');
        sizeEl.textContent = formatBytes(meta.size);
        li.appendChild(sizeEl);
      }
      fileList.appendChild(li);
    }
  };
  renderStoreFiles();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, renderStoreFiles));

  // Walidacja FileList → emit 'change' z metadata lub 'reject' z reason.
  const validateAndEmit = (files) => {
    if (!files || files.length === 0) return;
    const arr = Array.from(files);
    if (arr.length > maxFiles) {
      wrapper.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('reject', {
          bubbles: false,
          detail: { reason: 'max_files', count: arr.length, max: maxFiles },
        })
      );
      return;
    }
    for (const f of arr) {
      if (!fileMatchesAnyAccept(f, accept)) {
        wrapper.dispatchEvent(
          new (globalThis.CustomEvent || globalThis.Event)('reject', {
            bubbles: false,
            detail: { reason: 'accept', name: f.name, type: f.type },
          })
        );
        return;
      }
      const fSize = typeof f.size === 'bigint' ? f.size : BigInt(f.size || 0);
      const maxBig = typeof maxSizeBytes === 'bigint' ? maxSizeBytes : BigInt(maxSizeBytes);
      if (fSize > maxBig) {
        wrapper.dispatchEvent(
          new (globalThis.CustomEvent || globalThis.Event)('reject', {
            bubbles: false,
            detail: { reason: 'max_size', name: f.name, size: f.size, max: maxSizeBytes },
          })
        );
        return;
      }
    }
    const meta = arr.map((f) => ({
      name: f.name,
      size: f.size,
      type: f.type || '',
      last_modified: f.lastModified || 0,
    }));
    // Spec FileInput schema (schema/data.rs:1139) deklaruje
    // handlers=["files_selected", "upload_progress", "upload_complete",
    // "upload_error"]. Renderer emit'uje `files_selected` przy
    // poprawnym wyborze; upload_* eventy emit'uje host po faktycznym
    // upload'zie (renderer nie zajmuje się siecią).
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('files_selected', {
        bubbles: false,
        detail: {
          value: meta,
          kind: 'files',
          upload_action_id: uploadActionId,
        },
      })
    );
  };

  const onInputChange = () => validateAndEmit(input.files);
  input.addEventListener('change', onInputChange);
  ctx.registerCleanup(() => input.removeEventListener('change', onInputChange));

  const onDropzoneClick = (e) => {
    e.preventDefault();
    try { input.click(); } catch {}
  };
  const onDropzoneKey = (e) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      try { input.click(); } catch {}
    }
  };
  dropzone.addEventListener('click', onDropzoneClick);
  dropzone.addEventListener('keydown', onDropzoneKey);
  ctx.registerCleanup(() => {
    dropzone.removeEventListener('click', onDropzoneClick);
    dropzone.removeEventListener('keydown', onDropzoneKey);
  });

  if (dragAndDrop) {
    const onDragOver = (e) => {
      e.preventDefault();
      dropzone.classList.add('tf-file-input__dropzone--over');
    };
    const onDragLeave = () => {
      dropzone.classList.remove('tf-file-input__dropzone--over');
    };
    const onDrop = (e) => {
      e.preventDefault();
      dropzone.classList.remove('tf-file-input__dropzone--over');
      if (e.dataTransfer && e.dataTransfer.files) {
        validateAndEmit(e.dataTransfer.files);
      }
    };
    dropzone.addEventListener('dragover', onDragOver);
    dropzone.addEventListener('dragleave', onDragLeave);
    dropzone.addEventListener('drop', onDrop);
    ctx.registerCleanup(() => {
      dropzone.removeEventListener('dragover', onDragOver);
      dropzone.removeEventListener('dragleave', onDragLeave);
      dropzone.removeEventListener('drop', onDrop);
    });
  }

  return wrapper;
}

/// Format human-readable size (1024-based).
function formatBytes(bytes) {
  const n = typeof bytes === 'bigint' ? Number(bytes) : Number(bytes);
  if (!Number.isFinite(n) || n < 0) return '';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

// =============================================================================
// ColorPicker (0x0319)
// =============================================================================

export const COLOR_PICKER_TAG = 0x0319;
const COLOR_PICKER_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderColorPicker(component, ctx) {
  assertOnlyKnownFields(component.fields, COLOR_PICKER_FIELD_KEYS, 'ColorPicker');

  const bindPath = requirePath(ctx.readField(component.fields, 0), 'ColorPicker.bind_path');
  const variant = requireEnum(ctx.readField(component.fields, 1), COLOR_VARIANTS, 'ColorPicker.variant');
  const allowedTokensRaw = ctx.readField(component.fields, 2);
  const allowedTokens = allowedTokensRaw == null ? null : (() => {
    if (!Array.isArray(allowedTokensRaw)) {
      throw new TypeError('ColorPicker.allowed_tokens: expected Array<ColorToken>');
    }
    if (allowedTokensRaw.length === 0) {
      throw new TypeError('ColorPicker.allowed_tokens cannot be empty array');
    }
    return allowedTokensRaw.map((t, i) => requireEnum(t, COLOR_TOKENS, `ColorPicker.allowed_tokens[${i}]`));
  })();
  if (variant === 'tokens_only' && allowedTokens == null) {
    throw new TypeError('ColorPicker.variant=tokens_only requires allowed_tokens');
  }
  const showAlpha = requireBool(ctx.readField(component.fields, 3), 'ColorPicker.show_alpha');
  const labelBind = ctx.readField(component.fields, 4);

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-color-picker');
  wrapper.classList.add(`tf-color-picker--variant-${variant}`);
  if (showAlpha) wrapper.classList.add('tf-color-picker--alpha');

  let labelEl = null;
  if (labelBind != null) {
    labelEl = document.createElement('label');
    labelEl.classList.add('tf-color-picker__label');
    applyTextBind(labelEl, labelBind, ctx);
    wrapper.appendChild(labelEl);
  }

  if (labelBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError('ColorPicker without label requires Component.a11y.label');
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError('ColorPicker.a11y.label must resolve to non-blank string');
    }
  }

  // Czytanie aktualnej wartości i emit'owanie change.
  const readCurrent = () => {
    let v;
    try { v = ctx.store.read(bindPath); } catch { v = undefined; }
    return typeof v === 'string' ? v : '';
  };
  // Spec ColorPicker schema (schema/data.rs:1153) deklaruje handlers=[] —
  // ColorPicker mutuje stan WYŁĄCZNIE przez bind_path write-back, bez
  // public handler'ów dispatchowanych do addona. Używamy dedykowanego
  // wewnętrznego DOM event'u `tf-bind-write` (NIEobecnego w
  // `EVENT_KIND_WIRE`), więc addon NIE może attach'ować handler'a o tej
  // nazwie — `ComponentRenderer.applyEventHandlers` odrzuci to przez
  // walidator wire kind'a. Chunk 3.6 zlistenuje na ten sygnał lokalnie.
  const emitChange = (val, isToken) => {
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('tf-bind-write', {
        bubbles: false,
        detail: { value: val, kind: isToken ? 'token' : 'hex' },
      })
    );
  };

  if (variant === 'wheel') {
    // Native color input. Bez alpha — HTML5 nie wspiera.
    const input = document.createElement('input');
    input.setAttribute('type', 'color');
    input.classList.add('tf-color-picker__wheel');
    const inputId = `tf-color-picker-${component.id}`;
    input.setAttribute('id', inputId);
    if (labelEl) labelEl.setAttribute('for', inputId);
    // Sync value ze store.
    const sync = () => {
      const v = readCurrent();
      if (!HEX_COLOR_RE.test(v)) {
        // Native picker akceptuje tylko #RRGGBB — domyślnie ustaw #000000.
        if (document.activeElement !== input) input.value = '#000000';
        return;
      }
      // Trim do 6 hex (input type=color nie obsługuje #RGB ani alpha).
      const normalized = v.length === 4
        ? `#${v[1]}${v[1]}${v[2]}${v[2]}${v[3]}${v[3]}`
        : v.slice(0, 7);
      if (document.activeElement !== input) input.value = normalized;
    };
    sync();
    ctx.registerCleanup(ctx.store.subscribe(bindPath, sync));
    if (labelBind == null) {
      const aria = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof aria === 'string') input.setAttribute('aria-label', aria);
    }
    // stopPropagation chroni przed bubbling'iem natywnego `change`
    // do wrappera, gdzie ComponentRenderer.applyEventHandlers mógłby
    // dispatch'ować to do addon handlera — schema ColorPicker handlers=[]
    // tego nie pozwala.
    const onChange = (e) => {
      e.stopPropagation();
      emitChange(input.value, false);
    };
    input.addEventListener('change', onChange);
    ctx.registerCleanup(() => input.removeEventListener('change', onChange));
    wrapper.appendChild(input);
    return wrapper;
  }

  // swatch / compact / tokens_only — siatka swatches.
  const grid = document.createElement('div');
  grid.classList.add('tf-color-picker__grid');
  grid.setAttribute('role', 'radiogroup');
  if (labelBind == null) {
    const aria = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof aria === 'string') grid.setAttribute('aria-label', aria);
  }

  const swatchSources = (() => {
    if (variant === 'tokens_only') {
      return allowedTokens.map((t) => ({ token: t, value: t, isToken: true }));
    }
    if (allowedTokens != null) {
      return allowedTokens.map((t) => ({ token: t, value: t, isToken: true }));
    }
    return DEFAULT_SWATCH_PALETTE.map((hex) => ({ token: null, value: hex, isToken: false }));
  })();

  const swatchButtons = [];
  for (const sw of swatchSources) {
    const btn = document.createElement('button');
    btn.setAttribute('type', 'button');
    btn.setAttribute('role', 'radio');
    btn.classList.add('tf-color-picker__swatch');
    btn.setAttribute('data-value', sw.value);
    if (sw.isToken) {
      btn.setAttribute('data-token', sw.token);
      // CSS musi mapować data-token → background-color (semantic tokens
      // resolve'owane przez tf-theme.css).
      btn.classList.add(`tf-color-picker__swatch--token-${sw.token}`);
    } else {
      btn.style.backgroundColor = sw.value;
    }
    btn.setAttribute('aria-label', sw.isToken ? sw.token : sw.value);
    const onClick = (e) => {
      e.preventDefault();
      emitChange(sw.value, sw.isToken);
    };
    btn.addEventListener('click', onClick);
    ctx.registerCleanup(() => btn.removeEventListener('click', onClick));
    grid.appendChild(btn);
    swatchButtons.push({ btn, sw });
  }

  const syncSelected = () => {
    const cur = readCurrent();
    for (const { btn, sw } of swatchButtons) {
      const isSel = sw.value === cur;
      if (isSel) {
        btn.setAttribute('aria-checked', 'true');
        btn.classList.add('tf-color-picker__swatch--selected');
      } else {
        btn.removeAttribute('aria-checked');
        btn.classList.remove('tf-color-picker__swatch--selected');
      }
    }
  };
  syncSelected();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, syncSelected));

  wrapper.appendChild(grid);

  // Compact dodaje też hex-text-input dla fine-tuningu.
  if (variant === 'compact') {
    const hexInput = document.createElement('input');
    hexInput.setAttribute('type', 'text');
    hexInput.setAttribute('placeholder', '#rrggbb');
    hexInput.setAttribute('maxlength', showAlpha ? '9' : '7');
    hexInput.classList.add('tf-color-picker__hex');
    if (labelBind == null) {
      const aria = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof aria === 'string') hexInput.setAttribute('aria-label', `${aria} (hex)`);
    }
    const syncHex = () => {
      const cur = readCurrent();
      if (HEX_COLOR_RE.test(cur)) {
        if (document.activeElement !== hexInput) hexInput.value = cur;
      } else if (document.activeElement !== hexInput) {
        hexInput.value = '';
      }
    };
    syncHex();
    ctx.registerCleanup(ctx.store.subscribe(bindPath, syncHex));
    const onChange = (e) => {
      // stopPropagation — patrz wheel onChange.
      e.stopPropagation();
      const v = hexInput.value.trim();
      if (v === '') {
        emitChange('', false);
        return;
      }
      if (!HEX_COLOR_RE.test(v)) {
        syncHex();
        return;
      }
      if (!showAlpha && v.length === 9) {
        syncHex();
        return;
      }
      emitChange(v, false);
    };
    hexInput.addEventListener('change', onChange);
    ctx.registerCleanup(() => hexInput.removeEventListener('change', onChange));
    wrapper.appendChild(hexInput);
  }

  return wrapper;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerFormFileColorRenderers() {
  if (!lookupComponentRenderer(FILE_INPUT_TAG)) registerComponentRenderer(FILE_INPUT_TAG, renderFileInput);
  if (!lookupComponentRenderer(COLOR_PICKER_TAG)) registerComponentRenderer(COLOR_PICKER_TAG, renderColorPicker);
}
