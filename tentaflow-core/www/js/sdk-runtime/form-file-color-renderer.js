// =============================================================================
// File: sdk-runtime/form-file-color-renderer.js
// Description: FileInput (0x0318) + ColorPicker (0x0319) renderers.
//
// FileInput renders through the <tf-file-input> web component (dropzone +
// hidden native input live inside the component). The renderer validates the
// component's selection against accept/max_size_bytes/max_files and emits
// 'files_selected' with file metadata (actual upload goes to the host via
// upload_action_id) or 'reject' when validation fails.
//
// ColorPicker has 4 variants:
//   - swatch        — color grid (allowed_tokens or default palette)
//   - wheel         — <tf-color-input> web component
//   - compact       — swatch grid + <tf-input> hex field for fine-tuning
//   - tokens_only   — pure semantic token list (allowed_tokens required)
//
// All variants use reactive bind_path (read-only). Spec ref:
// tentaflow-sdk-spec/src/protocol/ui/form/file_color.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';
import { ApiBinary } from '../protocol/api-binary-shim.js';

// Fragment uploadu (256 KiB) — mieści się z zapasem w pojedynczej ramce WS i
// pasuje do rozmiaru kawałka odczytu w document store host-fn.
const UPLOAD_CHUNK_SIZE = 256 * 1024;

// =============================================================================
// Validators
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
// Hex color #RGB / #RRGGBB / #RRGGBBAA — used when variant != tokens_only and
// the user picks an arbitrary color via the wheel / hex field.
const HEX_COLOR_RE = /^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$/;
// Default swatch palette when allowed_tokens is absent — 16 sensible colors.
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
  // JS Number is safe up to 2^53; the spec uses u64 but files above 2^53 are
  // unrealistic. BigInt is also accepted (the host may send u64 as bigint).
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

/// Whether a file matches an accept pattern (mime/wildcard/extension):
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

  // multiple=false but max_files>1 is contradictory — enforce consistency.
  if (!multiple && maxFiles > 1) {
    throw new TypeError('FileInput.multiple=false requires max_files=1');
  }

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-file-input-field');
  if (dragAndDrop) wrapper.classList.add('tf-file-input-field--dnd');

  // Dropzone + hidden native input live inside the web component.
  const fileEl = document.createElement('tf-file-input');
  if (accept.length > 0) fileEl.setAttribute('accept', accept.join(','));
  if (multiple) fileEl.setAttribute('multiple', '');
  if (capture) fileEl.setAttribute('capture', capture);
  if (!dragAndDrop) fileEl.setAttribute('no-drop', '');

  if (labelBind != null) {
    const applyLabel = () => {
      const v = resolveBindRef(labelBind, ctx.store);
      fileEl.setAttribute('label', v == null ? '' : String(v));
    };
    applyLabel();
    ctx.registerCleanup(subscribeBindRef(labelBind, ctx.store, applyLabel));
  } else {
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
        fileEl.setAttribute('aria-label', v);
      } else {
        fileEl.removeAttribute('aria-label');
      }
    };
    applyAria();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAria));
  }

  wrapper.appendChild(fileEl);

  if (hintBind != null) {
    const hint = document.createElement('span');
    hint.classList.add('tf-file-input__hint');
    applyTextBind(hint, hintBind, ctx);
    wrapper.appendChild(hint);
  }

  // Selected-file list — rendered from the store (metadata after upload).
  const fileList = document.createElement('ul');
  fileList.classList.add('tf-file-input__list');
  wrapper.appendChild(fileList);

  // Store sync: the store value is a metadata list (post-upload). Native File
  // objects cannot be injected back into the input (security), so only names
  // and sizes from the store are rendered.
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

  // FileList validation → emit 'files_selected' with metadata or 'reject'.
  // Zwraca tablicę zwalidowanych `File` (do uploadu) albo `null` przy odrzuceniu.
  const validateAndEmit = (files) => {
    if (!files || files.length === 0) return null;
    const arr = Array.from(files);
    if (arr.length > maxFiles) {
      wrapper.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('reject', {
          bubbles: false,
          detail: { reason: 'max_files', count: arr.length, max: maxFiles },
        })
      );
      return null;
    }
    for (const f of arr) {
      if (!fileMatchesAnyAccept(f, accept)) {
        wrapper.dispatchEvent(
          new (globalThis.CustomEvent || globalThis.Event)('reject', {
            bubbles: false,
            detail: { reason: 'accept', name: f.name, type: f.type },
          })
        );
        return null;
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
        return null;
      }
    }
    const meta = arr.map((f) => ({
      name: f.name,
      size: f.size,
      type: f.type || '',
      last_modified: f.lastModified || 0,
    }));
    // The FileInput spec schema (schema/data.rs:1139) declares
    // handlers=["files_selected", "upload_progress", "upload_complete",
    // "upload_error"]. The renderer emits `files_selected` on a valid pick;
    // upload_* events are emitted by the host AFTER the actual chunked upload
    // (see uploadFiles below).
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
    return arr;
  };

  // Addon_id panelu — per-instancja. Host uploaduje do document store tej
  // instancji; serwer i tak waliduje własność (org z sesji + otwarty panel).
  const addonId = ctx.store && ctx.store.addon_id;

  // Token przerwania bieżącej sekwencji uploadu. Tworzymy nowy obiekt na każdą
  // sekwencję `uploadFiles`; ustawienie `.aborted` (cleanup renderera albo nowy
  // wybór plików) przerywa pętlę MIĘDZY fragmentami — nie wysyłamy kolejnych i
  // nie startujemy następnego pliku. Współdzielony obiekt (nie goła zmienna), bo
  // stara sekwencja musi widzieć przerwanie nawet gdy ruszyła już nowa.
  let activeUpload = null;

  // Generyczny chunked upload JEDNEGO pliku do document store addona. Czytamy
  // PER FRAGMENT (`file.slice(start, end).arrayBuffer()`) — nigdy nie buforujemy
  // całego pliku w RAM, więc upload setek MiB nie wywala karty (anty-OOM). Każdy
  // fragment 256 KiB → AddonDocumentUploadChunkRequest. Emituje `upload_progress`
  // (pasek), a na końcu `upload_complete` z `doc_ref` (eventDispatcher przekaże
  // to do addona jako action params {doc_ref, filename, mime, name, size}) albo
  // `upload_error`. Bajtów NIE wkładamy w detail eventu.
  const uploadOne = async (file, token) => {
    const filename = file.name || 'plik';
    const mime = file.type || 'application/octet-stream';
    const total = typeof file.size === 'number' ? file.size : Number(file.size || 0);
    const uploadId = (globalThis.crypto && globalThis.crypto.randomUUID
      && globalThis.crypto.randomUUID())
      || `up-${Date.now()}-${Math.floor(Math.random() * 1e9)}`;
    // total_chunks = ceil(size/CHUNK); pusty plik to wciąż jeden fragment (0 B),
    // żeby serwer dostał i sfinalizował blob.
    const totalChunks = Math.max(1, Math.ceil(total / UPLOAD_CHUNK_SIZE));
    try {
      let docRef = null;
      for (let seq = 0; seq < totalChunks; seq += 1) {
        if (token.aborted) return false;
        const start = seq * UPLOAD_CHUNK_SIZE;
        const end = Math.min(start + UPLOAD_CHUNK_SIZE, total);
        // Odczyt JEDNEGO wycinka — slice jest leniwy (widok na plik), arrayBuffer
        // materializuje tylko ten fragment. `slice` zwalniany po wysłaniu.
        let slice;
        try {
          slice = new Uint8Array(await file.slice(start, end).arrayBuffer());
        } catch (err) {
          if (token.aborted) return false;
          wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('upload_error', {
            bubbles: false,
            detail: { filename, name: filename, mime, reason: 'read_failed', message: String(err && err.message) },
          }));
          return false;
        }
        if (token.aborted) return false;
        const resp = await ApiBinary.one('addonDocumentUploadChunkRequest', {
          addonId,
          uploadId,
          filename,
          mime,
          seq,
          totalChunks,
          bytes: slice,
        });
        if (token.aborted) return false;
        const sent = end;
        wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('upload_progress', {
          bubbles: false,
          detail: {
            filename, name: filename, mime,
            sent_bytes: sent, total_bytes: total,
            percent: total > 0 ? Math.round((sent / total) * 100) : 100,
          },
        }));
        if (resp && (resp.docRef != null || resp.doc_ref != null)) {
          docRef = resp.docRef ?? resp.doc_ref;
        }
      }
      if (docRef == null) {
        wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('upload_error', {
          bubbles: false,
          detail: { filename, name: filename, mime, reason: 'no_doc_ref' },
        }));
        return false;
      }
      // upload_complete — detail trafia do addona jako action params przez
      // eventDispatcher (component-renderer applyEventHandlers nasłuchuje tej
      // nazwy EventKind na wrapperze). Kształt zgodny z kontraktem addona.
      wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('upload_complete', {
        bubbles: false,
        detail: {
          doc_ref: docRef,
          filename,
          mime,
          name: filename,
          size: total,
        },
      }));
      return true;
    } catch (err) {
      if (token.aborted) return false;
      wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('upload_error', {
        bubbles: false,
        detail: { filename, name: filename, mime, reason: 'upload_failed', message: String(err && err.message) },
      }));
      return false;
    }
  };

  // Sekwencyjny upload listy plików: jeden plik na raz (await), łatwiejszy postęp
  // i mniejsze ryzyko przepełnienia bufora WS. Nowy wybór plików abortuje
  // poprzednią sekwencję (patrz onChange). Błąd JEDNEGO pliku PRZERYWA sekwencję
  // (już wyemitował `upload_error`) — świadomy wybór: kolejne pliki często zależą
  // od poprzednich (np. komplet dokumentów), a ciche kontynuowanie po błędzie
  // ukryłoby częściowy upload przed addonem.
  const uploadFiles = async (arr) => {
    if (!addonId) {
      wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('upload_error', {
        bubbles: false,
        detail: { reason: 'no_addon_context' },
      }));
      return;
    }
    // Przerwij ewentualną trwającą sekwencję przed startem nowej.
    if (activeUpload) activeUpload.aborted = true;
    const token = { aborted: false };
    activeUpload = token;
    for (const f of arr) {
      if (token.aborted) return;
      const ok = await uploadOne(f, token);
      if (!ok) return;
    }
  };

  // tf-file-input emits a bubbling 'change' with detail {files}. Block the
  // raw event (its shape is not the SDK contract) and run validation; the
  // SDK-shaped 'files_selected'/'reject' events are dispatched on the wrapper.
  // After a valid pick the host performs the chunked upload (upload_* events).
  const onChange = (e) => {
    e.stopImmediatePropagation();
    // Nowy wybór plików abortuje trwającą sekwencję (uploadFiles też to robi, ale
    // ustawiamy tu od razu, gdyby walidacja odrzuciła nowy wybór — stary upload
    // i tak musi paść, bo użytkownik zmienił intencję).
    if (activeUpload) activeUpload.aborted = true;
    const validated = validateAndEmit(e.detail && e.detail.files);
    if (validated && validated.length > 0) void uploadFiles(validated);
  };
  fileEl.addEventListener('change', onChange);
  ctx.registerCleanup(() => fileEl.removeEventListener('change', onChange));
  // Cleanup renderera (panel zamknięty / przerenderowany) przerywa upload —
  // pętla nie wyśle kolejnych fragmentów po zwolnieniu kontekstu.
  ctx.registerCleanup(() => {
    if (activeUpload) activeUpload.aborted = true;
  });

  return wrapper;
}

/// Human-readable size (1024-based).
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

  if (labelBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError('ColorPicker without label requires Component.a11y.label');
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError('ColorPicker.a11y.label must resolve to non-blank string');
    }
  }

  // Current value read + change emit.
  const readCurrent = () => {
    let v;
    try { v = ctx.store.read(bindPath); } catch { v = undefined; }
    return typeof v === 'string' ? v : '';
  };
  // The ColorPicker spec schema (schema/data.rs:1153) declares handlers=[] —
  // ColorPicker mutates state ONLY through bind_path write-back, with no
  // public handlers dispatched to the addon. A dedicated internal DOM event
  // `tf-bind-write` (absent from `EVENT_KIND_WIRE`) is used, so the addon
  // cannot attach a handler under that name —
  // `ComponentRenderer.applyEventHandlers` rejects it via the wire-kind
  // validator. Chunk 3.6 listens for this signal locally.
  const emitChange = (val, isToken) => {
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('tf-bind-write', {
        bubbles: false,
        detail: { value: val, kind: isToken ? 'token' : 'hex' },
      })
    );
  };

  if (variant === 'wheel') {
    // <tf-color-input> web component (swatch trigger + native color input).
    // No alpha — HTML5 color input does not support it.
    const colorEl = document.createElement('tf-color-input');
    colorEl.classList.add('tf-color-picker__wheel');
    if (labelBind != null) {
      const applyLabel = () => {
        const v = resolveBindRef(labelBind, ctx.store);
        colorEl.setAttribute('label', v == null ? '' : String(v));
      };
      applyLabel();
      ctx.registerCleanup(subscribeBindRef(labelBind, ctx.store, applyLabel));
    } else {
      const applyAria = () => {
        const v = resolveBindRef(component.a11y.label, ctx.store);
        if (typeof v === 'string' && v.trim().length > 0) {
          colorEl.setAttribute('aria-label', v);
        } else {
          colorEl.removeAttribute('aria-label');
        }
      };
      applyAria();
      ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAria));
    }
    // Store → component value sync. The native picker takes only #RRGGBB, so
    // #RGB is expanded and alpha is stripped; invalid values fall back to
    // #000000 (same as the previous native-input behavior).
    const sync = () => {
      const v = readCurrent();
      let normalized = '#000000';
      if (HEX_COLOR_RE.test(v)) {
        normalized = v.length === 4
          ? `#${v[1]}${v[1]}${v[2]}${v[2]}${v[3]}${v[3]}`
          : v.slice(0, 7);
      }
      // Property write — applies to the inner input/swatch directly.
      colorEl.value = normalized;
    };
    sync();
    ctx.registerCleanup(ctx.store.subscribe(bindPath, sync));
    // tf-color-input emits a bubbling 'change' {value}. Block it (the schema
    // allows no public handlers) and route through the internal bind-write.
    const onChange = (e) => {
      e.stopImmediatePropagation();
      emitChange(e.detail && typeof e.detail.value === 'string' ? e.detail.value : colorEl.value, false);
    };
    colorEl.addEventListener('change', onChange);
    ctx.registerCleanup(() => colorEl.removeEventListener('change', onChange));
    wrapper.appendChild(colorEl);
    return wrapper;
  }

  // swatch / compact / tokens_only — swatch grid (no tf-* component covers a
  // semantic-token swatch grid; buttons are the correct primitive here).
  let labelEl = null;
  if (labelBind != null) {
    labelEl = document.createElement('label');
    labelEl.classList.add('tf-color-picker__label');
    applyTextBind(labelEl, labelBind, ctx);
    wrapper.appendChild(labelEl);
  }

  const grid = document.createElement('div');
  grid.classList.add('tf-color-picker__grid');
  grid.setAttribute('role', 'radiogroup');
  if (labelBind == null) {
    const aria = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof aria === 'string') grid.setAttribute('aria-label', aria);
  }

  const swatchSources = (() => {
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
      // CSS maps data-token → background-color (semantic tokens resolved by
      // tf-theme.css).
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

  // Compact also gets a hex text field for fine-tuning — <tf-input>.
  if (variant === 'compact') {
    const hexEl = document.createElement('tf-input');
    hexEl.classList.add('tf-color-picker__hex');
    hexEl.setAttribute('placeholder', '#rrggbb');
    hexEl.setAttribute('maxlength', showAlpha ? '9' : '7');
    if (labelBind == null) {
      const aria = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof aria === 'string') hexEl.setAttribute('aria-label', `${aria} (hex)`);
    }
    const applyHex = (force) => {
      // Never clobber the field while the user is typing in it — unless this
      // is an explicit revert after an invalid commit.
      if (!force && hexEl.contains(document.activeElement)) return;
      const cur = readCurrent();
      hexEl.value = HEX_COLOR_RE.test(cur) ? cur : '';
    };
    applyHex(false);
    ctx.registerCleanup(ctx.store.subscribe(bindPath, () => applyHex(false)));
    // tf-input emits a bubbling CustomEvent 'change' {value}; the native
    // 'change' of its inner input bubbles through as well. Block both (the
    // schema allows no public handlers) but validate only on the CustomEvent
    // so a single edit produces a single bind-write.
    const onChange = (e) => {
      e.stopImmediatePropagation();
      if (!e.detail || typeof e.detail.value !== 'string') return;
      const v = e.detail.value.trim();
      if (v === '') {
        emitChange('', false);
        return;
      }
      if (!HEX_COLOR_RE.test(v)) {
        applyHex(true);
        return;
      }
      if (!showAlpha && v.length === 9) {
        applyHex(true);
        return;
      }
      emitChange(v, false);
    };
    hexEl.addEventListener('change', onChange);
    ctx.registerCleanup(() => hexEl.removeEventListener('change', onChange));
    wrapper.appendChild(hexEl);
  }

  return wrapper;
}

// =============================================================================
// Registration
// =============================================================================

export function registerFormFileColorRenderers() {
  if (!lookupComponentRenderer(FILE_INPUT_TAG)) registerComponentRenderer(FILE_INPUT_TAG, renderFileInput);
  if (!lookupComponentRenderer(COLOR_PICKER_TAG)) registerComponentRenderer(COLOR_PICKER_TAG, renderColorPicker);
}
