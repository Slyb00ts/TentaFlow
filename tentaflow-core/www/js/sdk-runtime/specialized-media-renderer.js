// =============================================================================
// File: sdk-runtime/specialized-media-renderer.js
// Description: Renderers for specialized media/IO components: VideoStream
// (0x0604), LiveCameraTile (0x0605), MapView (0x0606), CodeEditor (0x0607),
// Terminal (0x0608), Audio (0x0609), IFrame (0x060A) — chunk 3.3g.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/specialized/{media,text_io,map,iframe}.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef, assertBindRef } from './bind-resolver.js';
import {
  requireEnum, requireBool, requireU8, requireU16, requireString,
  requirePath, assertOnlyKnownFields,
} from './data-chart-shared.js';
import { parseDimensionToken, parseAspectRatio } from './data-specialised-renderer.js';

// tf-video-stream is registered globally by the app component bundle; the
// renderer creates it via document.createElement (same pattern as tf-input in
// form-text-renderer) — no import here so the node test harness can load.

// A `camera:<id>` stream_id is a live subscribe ref served over the binary
// protocol via tf-video-stream (MSE) — NOT a media URL. It's the only subscribe
// scheme the core wires today (dispatch/stream.rs CAMERA_PREFIX); every other
// value stays on the plain <video src> path.
function isSubscribeStreamId(v) {
  return typeof v === 'string' && v.startsWith('camera:');
}

// Wpina nakladke detekcji na zywo nad kafelek tf-video-stream. Z `camera:<id>`
// wyciagamy camera_id i otwieramy BINARNY strumien detekcji (ApiBinary.subscribe
// 'cameraDetectionsSubscribeRequest') — zero REST, zero raw WebSocket. Modul
// ladujemy dynamicznie (import()) — node-owy harness testowy nie ma DOM canvasa,
// a renderer musi sie w nim ladowac. Cleanup zwalnia overlay (binarny unsubscribe
// + rAF cancel) gdy kafelek znika z DOM.
function attachLiveDetections(wrapper, tile, readStreamId, ctx) {
  if (typeof window === 'undefined' || typeof document === 'undefined') return;
  const streamId = readStreamId();
  if (typeof streamId !== 'string' || !streamId.startsWith('camera:')) return;
  const cameraId = streamId.slice('camera:'.length);
  if (!cameraId) return;

  let overlay = null;
  let cancelled = false;
  import('/js/modules/vision-detections-overlay.js')
    .then(({ attachDetectionsOverlay }) => {
      if (cancelled) return;
      overlay = attachDetectionsOverlay({
        video: tile,
        cameraId,
        videoResolver: () => tile.shadowRoot?.querySelector('video') || null,
      });
    })
    .catch((e) => {
      console.warn('[specialized-media] detections overlay load failed:', e?.message ?? e);
    });

  ctx.registerCleanup(() => {
    cancelled = true;
    if (overlay) {
      overlay.destroy();
      overlay = null;
    }
  });
}

// =============================================================================
// VideoStream (0x0604)
// =============================================================================

export const VIDEO_STREAM_TAG = 0x0604;
const VS_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7]);
const VIDEO_CONTROLS = new Set(['none', 'minimal', 'full']);
const IMAGE_FITS = new Set(['cover', 'contain', 'fill', 'none']);

function renderVideoStream(component, ctx) {
  assertOnlyKnownFields(component.fields, VS_FIELD_KEYS, 'VideoStream');

  const streamIdBind = ctx.readField(component.fields, 0);
  if (streamIdBind == null) throw new TypeError('VideoStream.stream_id is required (BindRef)');
  assertBindRef(streamIdBind, 'VideoStream.stream_id');
  const widthPxRaw = ctx.readField(component.fields, 1);
  const widthPx = widthPxRaw != null ? requireU16(widthPxRaw, 'VideoStream.width_px') : null;
  const aspectRatioRaw = ctx.readField(component.fields, 2);
  if (aspectRatioRaw == null) throw new TypeError('VideoStream.aspect_ratio is required');
  const aspectRatio = parseAspectRatio(aspectRatioRaw, 'VideoStream.aspect_ratio');
  const controls = requireEnum(ctx.readField(component.fields, 3), VIDEO_CONTROLS, 'VideoStream.controls');
  const autoplay = requireBool(ctx.readField(component.fields, 4), 'VideoStream.autoplay');
  const muted = requireBool(ctx.readField(component.fields, 5), 'VideoStream.muted');
  const objectFit = requireEnum(ctx.readField(component.fields, 6), IMAGE_FITS, 'VideoStream.object_fit');
  const posterRefRaw = ctx.readField(component.fields, 7);
  const posterRef = posterRefRaw != null ? requireString(posterRefRaw, 'VideoStream.poster_ref') : null;

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-video-stream');
  if (widthPx != null) wrapper.style.width = `${widthPx}px`;
  if (aspectRatio != null) wrapper.style.aspectRatio = aspectRatio;

  // Live subscribe stream (camera:…) → tf-video-stream (MediaSource over the
  // binary streamSubscribeRequest). A raw rtsp(s):// URL is NOT browser-playable;
  // forcing it into <video src> is what produced ERR_UNKNOWN_URL_SCHEME.
  if (isSubscribeStreamId(resolveBindRef(streamIdBind, ctx.store))) {
    const tile = document.createElement('tf-video-stream');
    tile.style.display = 'block';
    tile.style.width = '100%';
    tile.style.height = '100%';
    tile.style.setProperty('--tf-video-stream-height', '100%');
    const applyId = () => {
      const v = resolveBindRef(streamIdBind, ctx.store);
      if (typeof v === 'string' && v.length > 0) tile.setAttribute('stream-id', v);
      else tile.removeAttribute('stream-id');
    };
    applyId();
    ctx.registerCleanup(subscribeBindRef(streamIdBind, ctx.store, applyId));
    wrapper.appendChild(tile);
    attachLiveDetections(wrapper, tile, () => resolveBindRef(streamIdBind, ctx.store), ctx);
    return wrapper;
  }

  const video = document.createElement('video');
  video.classList.add('tf-video-stream__video');
  video.style.objectFit = objectFit;
  if (controls !== 'none') video.controls = true;
  if (autoplay) video.autoplay = true;
  if (muted) video.muted = true;
  if (posterRef != null) video.poster = posterRef;
  video.playsInline = true;

  const apply = () => {
    const src = resolveBindRef(streamIdBind, ctx.store);
    if (src == null || typeof src !== 'string') { video.removeAttribute('src'); return; }
    if (/^javascript:/i.test(src.trim())) { video.removeAttribute('src'); return; }
    video.src = src;
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(streamIdBind, ctx.store, apply));
  wrapper.appendChild(video);
  return wrapper;
}

// =============================================================================
// LiveCameraTile (0x0605)
// =============================================================================

export const LIVE_CAMERA_TILE_TAG = 0x0605;
const LCT_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);

// Aktualny timestamp HH:MM:SS do nakladki dolnej (mono).
function nowHms() {
  const d = new Date();
  const p = (n) => String(n).padStart(2, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

function renderLiveCameraTile(component, ctx) {
  assertOnlyKnownFields(component.fields, LCT_FIELD_KEYS, 'LiveCameraTile');

  const streamIdBind = ctx.readField(component.fields, 0);
  if (streamIdBind == null) throw new TypeError('LiveCameraTile.stream_id is required (BindRef)');
  assertBindRef(streamIdBind, 'LiveCameraTile.stream_id');
  const labelBind = ctx.readField(component.fields, 1);
  if (labelBind == null) throw new TypeError('LiveCameraTile.camera_label is required (BindRef)');
  assertBindRef(labelBind, 'LiveCameraTile.camera_label');
  const statusBind = ctx.readField(component.fields, 2);
  if (statusBind == null) throw new TypeError('LiveCameraTile.status is required (BindRef)');
  assertBindRef(statusBind, 'LiveCameraTile.status');
  const fpsBind = ctx.readField(component.fields, 3);
  if (fpsBind != null) assertBindRef(fpsBind, 'LiveCameraTile.fps');
  const showOverlay = requireBool(ctx.readField(component.fields, 4), 'LiveCameraTile.show_overlay');
  const showFullscreen = requireBool(ctx.readField(component.fields, 5), 'LiveCameraTile.show_fullscreen_button');
  const aspectRatioRaw = ctx.readField(component.fields, 6);
  if (aspectRatioRaw == null) throw new TypeError('LiveCameraTile.aspect_ratio is required');
  const aspectRatio = parseAspectRatio(aspectRatioRaw, 'LiveCameraTile.aspect_ratio');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-live-camera');
  if (aspectRatio != null) wrapper.style.aspectRatio = aspectRatio;

  // Zywy strumien subskrypcji (camera:<id>) idzie przez tf-video-stream (MSE nad
  // binarnym streamSubscribeRequest) — dokladnie jak w renderVideoStream — i
  // dostaje binarna nakladke detekcji (canvas). Wypelnia caly kafelek. Surowy
  // URL (media plik) zostaje na prostej sciezce <video src>, bez subskrypcji.
  if (isSubscribeStreamId(resolveBindRef(streamIdBind, ctx.store))) {
    const tile = document.createElement('tf-video-stream');
    tile.classList.add('tf-live-camera__stream');
    tile.style.display = 'block';
    tile.style.width = '100%';
    tile.style.height = '100%';
    tile.style.setProperty('--tf-video-stream-height', '100%');
    const applyId = () => {
      const v = resolveBindRef(streamIdBind, ctx.store);
      if (typeof v === 'string' && v.length > 0) tile.setAttribute('stream-id', v);
      else tile.removeAttribute('stream-id');
    };
    applyId();
    ctx.registerCleanup(subscribeBindRef(streamIdBind, ctx.store, applyId));
    wrapper.appendChild(tile);
    attachLiveDetections(wrapper, tile, () => resolveBindRef(streamIdBind, ctx.store), ctx);
  } else {
    const video = document.createElement('video');
    video.classList.add('tf-live-camera__video');
    video.autoplay = true;
    video.muted = true;
    video.playsInline = true;

    const applySrc = () => {
      const src = resolveBindRef(streamIdBind, ctx.store);
      if (src == null || typeof src !== 'string') { video.removeAttribute('src'); return; }
      if (/^javascript:/i.test(src.trim())) { video.removeAttribute('src'); return; }
      video.src = src;
    };
    applySrc();
    ctx.registerCleanup(subscribeBindRef(streamIdBind, ctx.store, applySrc));
    wrapper.appendChild(video);
  }

  if (showOverlay) {
    // Nakladki-etykiety leza w kontenerze (light DOM) NAD canvasem detekcji
    // (canvas ma z-index 20). Sa male i w rogach, wiec nie zaslaniaja bboxow.

    // Nakladka gorna: nazwa kamery (z ikona) po lewej, status po prawej.
    const overlay = document.createElement('div');
    overlay.classList.add('tf-live-camera__overlay');

    const title = document.createElement('span');
    title.classList.add('tf-live-camera__title');
    const icon = document.createElement('span');
    icon.classList.add('tf-live-camera__icon');
    icon.setAttribute('aria-hidden', 'true');
    title.appendChild(icon);
    const labelEl = document.createElement('span');
    labelEl.classList.add('tf-live-camera__label');
    const applyLabel = () => {
      const v = resolveBindRef(labelBind, ctx.store);
      labelEl.textContent = v == null ? '' : String(v);
    };
    applyLabel();
    ctx.registerCleanup(subscribeBindRef(labelBind, ctx.store, applyLabel));
    title.appendChild(labelEl);
    overlay.appendChild(title);

    const statusEl = document.createElement('span');
    statusEl.classList.add('tf-live-camera__status');
    const applyStatus = () => {
      const v = resolveBindRef(statusBind, ctx.store);
      const s = v == null ? '' : String(v);
      statusEl.textContent = s;
      statusEl.setAttribute('data-status', s);
    };
    applyStatus();
    ctx.registerCleanup(subscribeBindRef(statusBind, ctx.store, applyStatus));
    overlay.appendChild(statusEl);
    wrapper.appendChild(overlay);

    // Nakladka dolna: fps (opcjonalnie) po lewej, biezacy timestamp (mono) po prawej.
    const bottom = document.createElement('div');
    bottom.classList.add('tf-live-camera__overlay-bottom');

    if (fpsBind != null) {
      const fpsEl = document.createElement('span');
      fpsEl.classList.add('tf-live-camera__fps');
      const applyFps = () => {
        const v = resolveBindRef(fpsBind, ctx.store);
        fpsEl.textContent = v == null ? '' : `${v} fps`;
      };
      applyFps();
      ctx.registerCleanup(subscribeBindRef(fpsBind, ctx.store, applyFps));
      bottom.appendChild(fpsEl);
    }

    const timeEl = document.createElement('span');
    timeEl.classList.add('tf-live-camera__time');
    timeEl.textContent = nowHms();
    if (typeof window !== 'undefined' && typeof window.setInterval === 'function') {
      const timer = window.setInterval(() => { timeEl.textContent = nowHms(); }, 1000);
      ctx.registerCleanup(() => window.clearInterval(timer));
    }
    bottom.appendChild(timeEl);
    wrapper.appendChild(bottom);
  }

  if (showFullscreen) {
    const fsBtn = document.createElement('button');
    fsBtn.type = 'button';
    fsBtn.classList.add('tf-live-camera__fullscreen');
    fsBtn.setAttribute('aria-label', 'Fullscreen');
    fsBtn.textContent = '⛶';
    const onFs = () => {
      wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('fullscreen_request', {
        bubbles: false,
      }));
    };
    fsBtn.addEventListener('click', onFs);
    ctx.registerCleanup(() => fsBtn.removeEventListener('click', onFs));
    wrapper.appendChild(fsBtn);
  }

  return wrapper;
}

// =============================================================================
// MapView (0x0606)
// =============================================================================

export const MAP_VIEW_TAG = 0x0606;
const MV_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
const TILE_PROVIDERS = new Set(['osm', 'mapbox', 'tile_server']);

function renderMapView(component, ctx) {
  assertOnlyKnownFields(component.fields, MV_FIELD_KEYS, 'MapView');

  const centerPath = requirePath(ctx.readField(component.fields, 0), 'MapView.center_path');
  const zoomPath = requirePath(ctx.readField(component.fields, 1), 'MapView.zoom_path');
  const tileProvider = requireEnum(ctx.readField(component.fields, 2), TILE_PROVIDERS, 'MapView.tile_provider');
  const tileServerUrlRaw = ctx.readField(component.fields, 3);
  const tileServerUrl = tileServerUrlRaw != null ? requireString(tileServerUrlRaw, 'MapView.tile_server_url') : null;
  const heightRaw = ctx.readField(component.fields, 4);
  if (heightRaw == null) throw new TypeError('MapView.height is required');
  const height = parseDimensionToken(heightRaw, 'MapView.height');
  const markersPath = requirePath(ctx.readField(component.fields, 5), 'MapView.markers_path');
  const polygonsPathRaw = ctx.readField(component.fields, 6);
  const polygonsPath = polygonsPathRaw != null ? requirePath(polygonsPathRaw, 'MapView.polygons_path') : null;
  const heatmapPathRaw = ctx.readField(component.fields, 7);
  const heatmapPath = heatmapPathRaw != null ? requirePath(heatmapPathRaw, 'MapView.heatmap_path') : null;
  const interactive = requireBool(ctx.readField(component.fields, 8), 'MapView.interactive');
  const showAttribution = requireBool(ctx.readField(component.fields, 9), 'MapView.show_attribution');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-map-view');
  if (height != null) wrapper.style.height = height;
  wrapper.setAttribute('data-map-provider', tileProvider);
  wrapper.setAttribute('data-interactive', String(interactive));
  wrapper.setAttribute('data-show-attribution', String(showAttribution));
  if (tileServerUrl != null) wrapper.setAttribute('data-tile-server-url', tileServerUrl);

  // center/zoom/markers state paths stored as data-attrs for platform map adapter.
  wrapper.setAttribute('data-center-path', JSON.stringify(centerPath));
  wrapper.setAttribute('data-zoom-path', JSON.stringify(zoomPath));
  wrapper.setAttribute('data-markers-path', JSON.stringify(markersPath));
  if (polygonsPath != null) wrapper.setAttribute('data-polygons-path', JSON.stringify(polygonsPath));
  if (heatmapPath != null) wrapper.setAttribute('data-heatmap-path', JSON.stringify(heatmapPath));

  const placeholder = document.createElement('div');
  placeholder.classList.add('tf-map-view__placeholder');
  placeholder.textContent = 'Map';
  wrapper.appendChild(placeholder);

  return wrapper;
}

// =============================================================================
// CodeEditor (0x0607)
// =============================================================================

export const CODE_EDITOR_TAG = 0x0607;
const CE_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
const CE_THEMES = new Set(['auto', 'light', 'dark']);

function renderCodeEditor(component, ctx) {
  assertOnlyKnownFields(component.fields, CE_FIELD_KEYS, 'CodeEditor');

  const bindPath = requirePath(ctx.readField(component.fields, 0), 'CodeEditor.bind_path');
  const language = requireString(ctx.readField(component.fields, 1), 'CodeEditor.language');
  const readOnly = requireBool(ctx.readField(component.fields, 2), 'CodeEditor.read_only');
  const lineNumbers = requireBool(ctx.readField(component.fields, 3), 'CodeEditor.line_numbers');
  const wordWrap = requireBool(ctx.readField(component.fields, 4), 'CodeEditor.word_wrap');
  const theme = requireEnum(ctx.readField(component.fields, 5), CE_THEMES, 'CodeEditor.theme');
  const minHeightPx = requireU16(ctx.readField(component.fields, 6), 'CodeEditor.min_height_px');
  const maxHeightPxRaw = ctx.readField(component.fields, 7);
  const maxHeightPx = maxHeightPxRaw != null ? requireU16(maxHeightPxRaw, 'CodeEditor.max_height_px') : null;
  const tabSizeRaw = ctx.readField(component.fields, 8);
  const tabSize = tabSizeRaw != null ? requireU8(tabSizeRaw, 'CodeEditor.tab_size') : 2;
  const indentWithTabs = requireBool(ctx.readField(component.fields, 9), 'CodeEditor.indent_with_tabs');
  const bracketMatching = requireBool(ctx.readField(component.fields, 10), 'CodeEditor.bracket_matching');
  const autocomplete = requireBool(ctx.readField(component.fields, 11), 'CodeEditor.autocomplete');
  const lintingActionRaw = ctx.readField(component.fields, 12);
  const lintingActionId = lintingActionRaw != null ? requireString(lintingActionRaw, 'CodeEditor.linting_action_id') : null;

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-code-editor', `tf-code-editor--theme-${theme}`);
  wrapper.style.minHeight = `${minHeightPx}px`;
  if (maxHeightPx != null) wrapper.style.maxHeight = `${maxHeightPx}px`;
  wrapper.setAttribute('data-language', language);
  wrapper.setAttribute('data-tab-size', String(tabSize));
  if (readOnly) wrapper.setAttribute('data-readonly', 'true');
  if (lintingActionId != null) wrapper.setAttribute('data-linting-action', lintingActionId);

  const pre = document.createElement('pre');
  pre.classList.add('tf-code-editor__pre');
  pre.style.tabSize = String(tabSize);
  if (wordWrap) pre.style.whiteSpace = 'pre-wrap';

  const code = document.createElement('code');
  code.classList.add('tf-code-editor__code');
  code.setAttribute('data-language', language);

  let gutterEl = null;
  if (lineNumbers) {
    gutterEl = document.createElement('div');
    gutterEl.classList.add('tf-code-editor__gutter');
    wrapper.appendChild(gutterEl);
  }

  const apply = () => {
    let content = '';
    try { content = ctx.store.read(bindPath); } catch { /* no data yet */ }
    if (content == null) content = '';
    code.textContent = String(content);
    if (gutterEl != null) {
      const lines = String(content).split('\n');
      gutterEl.replaceChildren();
      for (let i = 0; i < lines.length; i++) {
        const ln = document.createElement('span');
        ln.classList.add('tf-code-editor__line-no');
        ln.textContent = String(i + 1);
        gutterEl.appendChild(ln);
      }
    }
  };
  apply();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, apply));

  pre.appendChild(code);
  wrapper.appendChild(pre);
  return wrapper;
}

// =============================================================================
// Terminal (0x0608)
// =============================================================================

export const TERMINAL_TAG = 0x0608;
const TM_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);
const TM_THEMES = new Set(['default', 'high_contrast', 'dim']);

function renderTerminal(component, ctx) {
  assertOnlyKnownFields(component.fields, TM_FIELD_KEYS, 'Terminal');

  const streamIdBind = ctx.readField(component.fields, 0);
  if (streamIdBind == null) throw new TypeError('Terminal.stream_id is required (BindRef)');
  assertBindRef(streamIdBind, 'Terminal.stream_id');
  const rows = requireU16(ctx.readField(component.fields, 1), 'Terminal.rows');
  const cols = requireU16(ctx.readField(component.fields, 2), 'Terminal.cols');
  const theme = requireEnum(ctx.readField(component.fields, 3), TM_THEMES, 'Terminal.theme');
  const searchable = requireBool(ctx.readField(component.fields, 4), 'Terminal.searchable');
  const copyable = requireBool(ctx.readField(component.fields, 5), 'Terminal.copyable');
  const maxBufferRaw = ctx.readField(component.fields, 6);
  let maxBufferLines = 10000;
  if (maxBufferRaw != null) {
    if (typeof maxBufferRaw === 'bigint') {
      if (maxBufferRaw < 0n || maxBufferRaw > 0xFFFFFFFFn) {
        throw new TypeError('Terminal.max_buffer_lines must be u32');
      }
      maxBufferLines = Number(maxBufferRaw);
    } else {
      if (typeof maxBufferRaw !== 'number' || !Number.isInteger(maxBufferRaw) || maxBufferRaw < 0 || maxBufferRaw > 0xFFFFFFFF) {
        throw new TypeError('Terminal.max_buffer_lines must be u32');
      }
      maxBufferLines = maxBufferRaw;
    }
  }

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-terminal', `tf-terminal--theme-${theme}`);
  wrapper.setAttribute('data-rows', String(rows));
  wrapper.setAttribute('data-cols', String(cols));

  const pre = document.createElement('pre');
  pre.classList.add('tf-terminal__output');
  // rows/cols sizing via ch/lh units.
  pre.style.width = `${cols}ch`;
  pre.style.height = `${rows}lh`;

  const lines = [];

  const apply = () => {
    const v = resolveBindRef(streamIdBind, ctx.store);
    if (v == null) return;
    const text = String(v);
    lines.push(text);
    while (lines.length > maxBufferLines) lines.shift();
    pre.textContent = lines.join('\n');
    pre.scrollTop = pre.scrollHeight;
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(streamIdBind, ctx.store, apply));

  wrapper.appendChild(pre);

  if (copyable) wrapper.setAttribute('data-copyable', 'true');
  if (searchable) wrapper.setAttribute('data-searchable', 'true');

  return wrapper;
}

// =============================================================================
// Audio (0x0609)
// =============================================================================

export const AUDIO_TAG = 0x0609;
const AU_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);
const AUDIO_CONTROLS = new Set(['none', 'minimal', 'full']);
const AUDIO_VARIANTS = new Set(['default', 'compact', 'waveform']);

function renderAudio(component, ctx) {
  assertOnlyKnownFields(component.fields, AU_FIELD_KEYS, 'Audio');

  const srcBind = ctx.readField(component.fields, 0);
  if (srcBind == null) throw new TypeError('Audio.src_ref is required (BindRef)');
  assertBindRef(srcBind, 'Audio.src_ref');
  const controls = requireEnum(ctx.readField(component.fields, 1), AUDIO_CONTROLS, 'Audio.controls');
  const autoplay = requireBool(ctx.readField(component.fields, 2), 'Audio.autoplay');
  const loop = requireBool(ctx.readField(component.fields, 3), 'Audio.loop');
  const variant = requireEnum(ctx.readField(component.fields, 4), AUDIO_VARIANTS, 'Audio.variant');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-audio', `tf-audio--variant-${variant}`);

  const audio = document.createElement('audio');
  audio.classList.add('tf-audio__player');
  if (controls !== 'none') audio.controls = true;
  if (autoplay) audio.autoplay = true;
  if (loop) audio.loop = true;

  const apply = () => {
    const src = resolveBindRef(srcBind, ctx.store);
    if (src == null || typeof src !== 'string') { audio.removeAttribute('src'); return; }
    if (/^javascript:/i.test(src.trim())) { audio.removeAttribute('src'); return; }
    audio.src = src;
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(srcBind, ctx.store, apply));
  wrapper.appendChild(audio);
  return wrapper;
}

// =============================================================================
// IFrame (0x060A)
// =============================================================================

export const IFRAME_TAG = 0x060A;
const IF_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);
const IFRAME_SANDBOX_TOKENS = new Set(['allow-scripts', 'allow-forms', 'allow-popups', 'allow-modals']);
const IFRAME_REFERRER_POLICIES = new Set([
  'no-referrer', 'no-referrer-when-downgrade', 'origin', 'origin-when-cross-origin',
  'same-origin', 'strict-origin', 'strict-origin-when-cross-origin', 'unsafe-url',
]);

function validateIFrameSrc(src) {
  if (typeof src !== 'string') return false;
  const trimmed = src.trim();
  // Only allow http: and https: protocols.
  if (/^https?:\/\//i.test(trimmed)) return true;
  return false;
}

function renderIFrame(component, ctx) {
  assertOnlyKnownFields(component.fields, IF_FIELD_KEYS, 'IFrame');

  const src = requireString(ctx.readField(component.fields, 0), 'IFrame.src');
  const sandboxRaw = ctx.readField(component.fields, 1);
  if (sandboxRaw == null || !Array.isArray(sandboxRaw)) throw new TypeError('IFrame.sandbox must be array');
  for (const tok of sandboxRaw) {
    if (!IFRAME_SANDBOX_TOKENS.has(tok)) throw new TypeError(`IFrame.sandbox: unknown token '${tok}'`);
  }
  const widthRaw = ctx.readField(component.fields, 2);
  if (widthRaw == null) throw new TypeError('IFrame.width is required');
  const width = parseDimensionToken(widthRaw, 'IFrame.width');
  const heightRaw = ctx.readField(component.fields, 3);
  if (heightRaw == null) throw new TypeError('IFrame.height is required');
  const height = parseDimensionToken(heightRaw, 'IFrame.height');
  const title = requireString(ctx.readField(component.fields, 4), 'IFrame.title');
  const referrerPolicy = requireEnum(ctx.readField(component.fields, 5), IFRAME_REFERRER_POLICIES, 'IFrame.referrer_policy');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-iframe');
  if (width != null) wrapper.style.width = width;
  if (height != null) wrapper.style.height = height;

  if (!validateIFrameSrc(src)) {
    const err = document.createElement('div');
    err.classList.add('tf-iframe__blocked');
    err.textContent = 'Blocked: only http/https URLs are allowed.';
    wrapper.appendChild(err);
    return wrapper;
  }

  const iframe = document.createElement('iframe');
  iframe.classList.add('tf-iframe__frame');
  iframe.src = src;
  iframe.title = title;
  iframe.referrerPolicy = referrerPolicy;
  // Build sandbox attribute: always sandbox; add individual allow-* tokens.
  const sandboxValue = sandboxRaw.length > 0 ? sandboxRaw.join(' ') : '';
  iframe.setAttribute('sandbox', sandboxValue);
  iframe.style.width = '100%';
  iframe.style.height = '100%';
  iframe.style.border = 'none';

  wrapper.appendChild(iframe);
  return wrapper;
}

// =============================================================================
// Registration
// =============================================================================

export function registerSpecializedMediaRenderers() {
  if (!lookupComponentRenderer(VIDEO_STREAM_TAG)) registerComponentRenderer(VIDEO_STREAM_TAG, renderVideoStream);
  if (!lookupComponentRenderer(LIVE_CAMERA_TILE_TAG)) registerComponentRenderer(LIVE_CAMERA_TILE_TAG, renderLiveCameraTile);
  if (!lookupComponentRenderer(MAP_VIEW_TAG)) registerComponentRenderer(MAP_VIEW_TAG, renderMapView);
  if (!lookupComponentRenderer(CODE_EDITOR_TAG)) registerComponentRenderer(CODE_EDITOR_TAG, renderCodeEditor);
  if (!lookupComponentRenderer(TERMINAL_TAG)) registerComponentRenderer(TERMINAL_TAG, renderTerminal);
  if (!lookupComponentRenderer(AUDIO_TAG)) registerComponentRenderer(AUDIO_TAG, renderAudio);
  if (!lookupComponentRenderer(IFRAME_TAG)) registerComponentRenderer(IFRAME_TAG, renderIFrame);
}
