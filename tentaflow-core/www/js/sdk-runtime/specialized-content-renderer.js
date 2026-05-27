// =============================================================================
// File: sdk-runtime/specialized-content-renderer.js
// Description: Renderers for specialized content components: ImageGallery
// (0x060B), Carousel (0x060C), PdfViewer (0x060D), FpsCounter (0x060E),
// StepProgress (0x060F), Stopwatch (0x0610), VirtualizedLog (0x0611)
// — chunk 3.3g.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/specialized/{gallery,telemetry,wizard,log}.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef, assertBindRef } from './bind-resolver.js';
import {
  TONES,
  requireEnum, requireBool, requireU8, requireU16, requireString,
  requirePath, assertOnlyKnownFields,
} from './data-chart-shared.js';
import { parseDimensionToken, parseAspectRatio } from './data-specialised-renderer.js';

// =============================================================================
// ImageGallery (0x060B)
// =============================================================================

export const IMAGE_GALLERY_TAG = 0x060B;
const IG_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);
const SPACINGS = new Set(['zero', 'xxs', 'xs', 'sm', 'md', 'lg', 'xl', 'xxl']);

function renderImageGallery(component, ctx) {
  assertOnlyKnownFields(component.fields, IG_FIELD_KEYS, 'ImageGallery');

  const imagesPath = requirePath(ctx.readField(component.fields, 0), 'ImageGallery.images_path');
  const columns = requireU8(ctx.readField(component.fields, 1), 'ImageGallery.columns');
  const aspectRatioRaw = ctx.readField(component.fields, 2);
  if (aspectRatioRaw == null) throw new TypeError('ImageGallery.aspect_ratio is required');
  const aspectRatio = parseAspectRatio(aspectRatioRaw, 'ImageGallery.aspect_ratio');
  const gap = requireEnum(ctx.readField(component.fields, 3), SPACINGS, 'ImageGallery.gap');
  const lightbox = requireBool(ctx.readField(component.fields, 4), 'ImageGallery.lightbox');
  const lazyLoad = requireBool(ctx.readField(component.fields, 5), 'ImageGallery.lazy_load');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-image-gallery');
  wrapper.style.display = 'grid';
  wrapper.style.gridTemplateColumns = `repeat(${columns}, 1fr)`;
  wrapper.style.gap = `var(--tf-space-${gap})`;
  wrapper.setAttribute('role', 'list');

  const rebuild = () => {
    wrapper.replaceChildren();
    let images = [];
    try { const arr = ctx.store.read(imagesPath); if (Array.isArray(arr)) images = arr; } catch { /* no data yet */ }

    for (let i = 0; i < images.length; i++) {
      const item = images[i];
      if (item == null || typeof item !== 'object') continue;
      const cell = document.createElement('div');
      cell.classList.add('tf-image-gallery__cell');
      cell.setAttribute('role', 'listitem');
      if (aspectRatio != null) cell.style.aspectRatio = aspectRatio;

      const img = document.createElement('img');
      img.classList.add('tf-image-gallery__img');
      img.style.objectFit = 'cover';
      const src = typeof item === 'string' ? item : (item.src || item.url || '');
      if (typeof src === 'string' && !/^javascript:/i.test(src.trim())) img.src = src;
      img.alt = (item.alt != null ? String(item.alt) : `Image ${i + 1}`);
      if (lazyLoad) img.loading = 'lazy';

      if (lightbox) {
        cell.classList.add('tf-image-gallery__cell--clickable');
        cell.setAttribute('role', 'button');
        cell.tabIndex = 0;
        const idx = i;
        const onClick = () => {
          wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('lightbox_open', {
            bubbles: false,
            detail: { index: idx, src: img.src },
          }));
        };
        cell.addEventListener('click', onClick);
      }

      cell.appendChild(img);
      wrapper.appendChild(cell);
    }
  };
  rebuild();
  ctx.registerCleanup(ctx.store.subscribe(imagesPath, rebuild));
  return wrapper;
}

// =============================================================================
// Carousel (0x060C)
// =============================================================================

export const CAROUSEL_TAG = 0x060C;
const CR_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7]);
const CAROUSEL_GESTURES = new Set(['swipe', 'arrows_only', 'none']);

function renderCarousel(component, ctx) {
  assertOnlyKnownFields(component.fields, CR_FIELD_KEYS, 'Carousel');

  const itemsPath = requirePath(ctx.readField(component.fields, 0), 'Carousel.items_path');
  const currentIndexPath = requirePath(ctx.readField(component.fields, 1), 'Carousel.current_index_path');
  const autoplay = requireBool(ctx.readField(component.fields, 2), 'Carousel.autoplay');
  const autoplayMs = requireU16(ctx.readField(component.fields, 3), 'Carousel.autoplay_ms');
  const loop = requireBool(ctx.readField(component.fields, 4), 'Carousel.loop');
  const showIndicators = requireBool(ctx.readField(component.fields, 5), 'Carousel.show_indicators');
  const showArrows = requireBool(ctx.readField(component.fields, 6), 'Carousel.show_arrows');
  const gestures = requireEnum(ctx.readField(component.fields, 7), CAROUSEL_GESTURES, 'Carousel.gestures');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-carousel');
  wrapper.setAttribute('role', 'region');
  wrapper.setAttribute('aria-roledescription', 'carousel');

  const viewport = document.createElement('div');
  viewport.classList.add('tf-carousel__viewport');

  const getItems = () => {
    try { const arr = ctx.store.read(itemsPath); return Array.isArray(arr) ? arr : []; } catch { return []; }
  };
  const getIndex = () => {
    try { const v = ctx.store.read(currentIndexPath); return typeof v === 'number' ? v : 0; } catch { return 0; }
  };
  const setIndex = (idx) => {
    wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('slide_change', {
      bubbles: false,
      detail: { index: idx },
    }));
  };

  let autoplayTimer = null;
  const clearAutoplay = () => { if (autoplayTimer != null) { clearInterval(autoplayTimer); autoplayTimer = null; } };
  ctx.registerCleanup(clearAutoplay);

  const navigate = (delta) => {
    const items = getItems();
    if (items.length === 0) return;
    let idx = getIndex() + delta;
    if (loop) { idx = ((idx % items.length) + items.length) % items.length; }
    else { idx = Math.max(0, Math.min(idx, items.length - 1)); }
    setIndex(idx);
  };

  const rebuild = () => {
    viewport.replaceChildren();
    const items = getItems();
    const currentIdx = getIndex();

    for (let i = 0; i < items.length; i++) {
      const slide = document.createElement('div');
      slide.classList.add('tf-carousel__slide');
      slide.setAttribute('aria-roledescription', 'slide');
      slide.setAttribute('aria-label', `Slide ${i + 1} of ${items.length}`);
      if (i !== currentIdx) slide.classList.add('tf-carousel__slide--hidden');

      const item = items[i];
      if (item != null && typeof item === 'object' && (item.src || item.url)) {
        const img = document.createElement('img');
        img.classList.add('tf-carousel__img');
        const src = item.src || item.url;
        if (typeof src === 'string' && !/^javascript:/i.test(src.trim())) img.src = src;
        img.alt = item.alt != null ? String(item.alt) : `Slide ${i + 1}`;
        slide.appendChild(img);
      } else if (typeof item === 'string') {
        const img = document.createElement('img');
        img.classList.add('tf-carousel__img');
        if (!/^javascript:/i.test(item.trim())) img.src = item;
        img.alt = `Slide ${i + 1}`;
        slide.appendChild(img);
      }
      viewport.appendChild(slide);
    }
  };
  rebuild();
  ctx.registerCleanup(ctx.store.subscribe(itemsPath, rebuild));
  ctx.registerCleanup(ctx.store.subscribe(currentIndexPath, rebuild));

  wrapper.appendChild(viewport);

  if (showArrows) {
    const prevBtn = document.createElement('button');
    prevBtn.type = 'button';
    prevBtn.classList.add('tf-carousel__arrow', 'tf-carousel__arrow--prev');
    prevBtn.setAttribute('aria-label', 'Previous slide');
    prevBtn.textContent = '‹';
    prevBtn.addEventListener('click', () => navigate(-1));

    const nextBtn = document.createElement('button');
    nextBtn.type = 'button';
    nextBtn.classList.add('tf-carousel__arrow', 'tf-carousel__arrow--next');
    nextBtn.setAttribute('aria-label', 'Next slide');
    nextBtn.textContent = '›';
    nextBtn.addEventListener('click', () => navigate(1));

    wrapper.appendChild(prevBtn);
    wrapper.appendChild(nextBtn);
  }

  if (showIndicators) {
    const indicatorBar = document.createElement('div');
    indicatorBar.classList.add('tf-carousel__indicators');

    const rebuildIndicators = () => {
      indicatorBar.replaceChildren();
      const items = getItems();
      const currentIdx = getIndex();
      for (let i = 0; i < items.length; i++) {
        const dot = document.createElement('button');
        dot.type = 'button';
        dot.classList.add('tf-carousel__indicator');
        if (i === currentIdx) dot.classList.add('tf-carousel__indicator--active');
        dot.setAttribute('aria-label', `Go to slide ${i + 1}`);
        const idx = i;
        dot.addEventListener('click', () => setIndex(idx));
        indicatorBar.appendChild(dot);
      }
    };
    rebuildIndicators();
    ctx.registerCleanup(ctx.store.subscribe(itemsPath, rebuildIndicators));
    ctx.registerCleanup(ctx.store.subscribe(currentIndexPath, rebuildIndicators));
    wrapper.appendChild(indicatorBar);
  }

  if (autoplay && autoplayMs > 0) {
    autoplayTimer = setInterval(() => navigate(1), autoplayMs);
  }

  return wrapper;
}

// =============================================================================
// PdfViewer (0x060D)
// =============================================================================

export const PDF_VIEWER_TAG = 0x060D;
const PV_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);
const PDF_ZOOM_MODES = new Set(['fit_width', 'fit_height', 'actual', 'custom']);

function renderPdfViewer(component, ctx) {
  assertOnlyKnownFields(component.fields, PV_FIELD_KEYS, 'PdfViewer');

  const srcRef = requireString(ctx.readField(component.fields, 0), 'PdfViewer.src_ref');
  const pagePathRaw = ctx.readField(component.fields, 1);
  const pagePath = pagePathRaw != null ? requirePath(pagePathRaw, 'PdfViewer.page_path') : null;
  const heightRaw = ctx.readField(component.fields, 2);
  if (heightRaw == null) throw new TypeError('PdfViewer.height is required');
  const height = parseDimensionToken(heightRaw, 'PdfViewer.height');
  const zoomMode = requireEnum(ctx.readField(component.fields, 3), PDF_ZOOM_MODES, 'PdfViewer.zoom_mode');
  const searchable = requireBool(ctx.readField(component.fields, 4), 'PdfViewer.searchable');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-pdf-viewer');
  if (height != null) wrapper.style.height = height;
  wrapper.setAttribute('data-zoom-mode', zoomMode);
  wrapper.setAttribute('data-searchable', String(searchable));
  if (pagePath != null) wrapper.setAttribute('data-page-path', JSON.stringify(pagePath));

  // PDF rendering is delegated to platform adapter; embed via <object>.
  if (/^javascript:/i.test(srcRef.trim())) {
    const err = document.createElement('div');
    err.classList.add('tf-pdf-viewer__blocked');
    err.textContent = 'Blocked: invalid PDF source.';
    wrapper.appendChild(err);
    return wrapper;
  }

  const obj = document.createElement('object');
  obj.classList.add('tf-pdf-viewer__object');
  obj.type = 'application/pdf';
  obj.data = srcRef;
  obj.style.width = '100%';
  obj.style.height = '100%';

  const fallback = document.createElement('p');
  fallback.classList.add('tf-pdf-viewer__fallback');
  fallback.textContent = 'PDF cannot be displayed.';
  obj.appendChild(fallback);
  wrapper.appendChild(obj);
  return wrapper;
}

// =============================================================================
// FpsCounter (0x060E)
// =============================================================================

export const FPS_COUNTER_TAG = 0x060E;
const FPS_FIELD_KEYS = new Set([0, 1, 2]);
const FPS_VARIANTS = new Set(['minimal', 'detailed']);

function renderFpsCounter(component, ctx) {
  assertOnlyKnownFields(component.fields, FPS_FIELD_KEYS, 'FpsCounter');

  const sourcePath = requirePath(ctx.readField(component.fields, 0), 'FpsCounter.source_path');
  const variant = requireEnum(ctx.readField(component.fields, 1), FPS_VARIANTS, 'FpsCounter.variant');
  const historySecs = requireU8(ctx.readField(component.fields, 2), 'FpsCounter.history_secs');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-fps-counter', `tf-fps-counter--${variant}`);
  wrapper.setAttribute('data-history-secs', String(historySecs));

  const valueEl = document.createElement('span');
  valueEl.classList.add('tf-fps-counter__value');
  wrapper.appendChild(valueEl);

  if (variant === 'detailed') {
    const labelEl = document.createElement('span');
    labelEl.classList.add('tf-fps-counter__label');
    labelEl.textContent = 'FPS';
    wrapper.appendChild(labelEl);
  }

  const apply = () => {
    let v = null;
    try { v = ctx.store.read(sourcePath); } catch { /* no data yet */ }
    valueEl.textContent = v != null ? String(v) : '—';
  };
  apply();
  ctx.registerCleanup(ctx.store.subscribe(sourcePath, apply));
  return wrapper;
}

// =============================================================================
// StepProgress (0x060F)
// =============================================================================

export const STEP_PROGRESS_TAG = 0x060F;
const SP_FIELD_KEYS = new Set([0, 1, 2, 3]);
const SP_VARIANTS = new Set(['horizontal', 'vertical', 'compact']);
const STEP_STATUSES = new Set(['pending', 'current', 'complete', 'error', 'skipped']);

function renderStepProgress(component, ctx) {
  assertOnlyKnownFields(component.fields, SP_FIELD_KEYS, 'StepProgress');

  const stepsRaw = ctx.readField(component.fields, 0);
  if (stepsRaw == null || !Array.isArray(stepsRaw)) throw new TypeError('StepProgress.steps must be array');
  const currentIdPath = requirePath(ctx.readField(component.fields, 1), 'StepProgress.current_id_path');
  const variant = requireEnum(ctx.readField(component.fields, 2), SP_VARIANTS, 'StepProgress.variant');
  const clickableCompleted = requireBool(ctx.readField(component.fields, 3), 'StepProgress.clickable_completed');

  // Parse steps: each is {0: id, 1: label BindRef, 2: optional bool, 3?: status BindRef, 4?: description BindRef}.
  const steps = [];
  for (let i = 0; i < stepsRaw.length; i++) {
    const raw = stepsRaw[i];
    if (raw == null || typeof raw !== 'object') throw new TypeError(`StepProgress.steps[${i}] must be object`);

    let id, label, optional, statusBind, descBind;
    // Support both map-key (0,1,2,3,4) and named-key (id,label,...) formats.
    if ('0' in raw || 0 in raw) {
      id = raw[0] ?? raw['0'];
      label = raw[1] ?? raw['1'];
      optional = raw[2] ?? raw['2'] ?? false;
      statusBind = raw[3] ?? raw['3'] ?? null;
      descBind = raw[4] ?? raw['4'] ?? null;
    } else {
      id = raw.id;
      label = raw.label;
      optional = raw.optional ?? false;
      statusBind = raw.status ?? null;
      descBind = raw.description ?? null;
    }
    if (typeof id !== 'string') throw new TypeError(`StepProgress.steps[${i}].id must be string`);
    if (label == null) throw new TypeError(`StepProgress.steps[${i}].label is required`);
    assertBindRef(label, `StepProgress.steps[${i}].label`);
    steps.push({ id, label, optional, statusBind, descBind });
  }

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-step-progress', `tf-step-progress--${variant}`);
  wrapper.setAttribute('role', 'list');

  const rebuild = () => {
    wrapper.replaceChildren();
    let currentId = '';
    try { currentId = ctx.store.read(currentIdPath); } catch { /* no data yet */ }
    if (typeof currentId !== 'string') currentId = '';

    for (let i = 0; i < steps.length; i++) {
      const step = steps[i];
      const stepEl = document.createElement('div');
      stepEl.classList.add('tf-step-progress__step');
      stepEl.setAttribute('role', 'listitem');
      stepEl.setAttribute('data-step-id', step.id);

      // Resolve status from BindRef or derive from position.
      let status = 'pending';
      if (step.statusBind != null) {
        const sv = resolveBindRef(step.statusBind, ctx.store);
        if (typeof sv === 'string' && STEP_STATUSES.has(sv)) status = sv;
      }
      if (step.id === currentId) status = 'current';
      stepEl.setAttribute('data-status', status);
      stepEl.classList.add(`tf-step-progress__step--${status}`);

      const marker = document.createElement('span');
      marker.classList.add('tf-step-progress__marker');
      marker.textContent = status === 'complete' ? '✓' : status === 'error' ? '!' : String(i + 1);
      stepEl.appendChild(marker);

      const labelEl = document.createElement('span');
      labelEl.classList.add('tf-step-progress__label');
      const labelVal = resolveBindRef(step.label, ctx.store);
      labelEl.textContent = labelVal == null ? '' : String(labelVal);
      stepEl.appendChild(labelEl);

      if (step.descBind != null) {
        const descEl = document.createElement('span');
        descEl.classList.add('tf-step-progress__desc');
        const descVal = resolveBindRef(step.descBind, ctx.store);
        descEl.textContent = descVal == null ? '' : String(descVal);
        stepEl.appendChild(descEl);
      }

      if (step.optional) stepEl.classList.add('tf-step-progress__step--optional');

      if (clickableCompleted && status === 'complete') {
        stepEl.classList.add('tf-step-progress__step--clickable');
        stepEl.setAttribute('role', 'button');
        stepEl.tabIndex = 0;
        const sid = step.id;
        const onClick = () => {
          wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('step_click', {
            bubbles: false,
            detail: { step_id: sid },
          }));
        };
        stepEl.addEventListener('click', onClick);
      }

      // Connector between steps (not after last).
      if (i < steps.length - 1) {
        const connector = document.createElement('span');
        connector.classList.add('tf-step-progress__connector');
        stepEl.appendChild(connector);
      }

      wrapper.appendChild(stepEl);
    }
  };
  rebuild();
  ctx.registerCleanup(ctx.store.subscribe(currentIdPath, rebuild));
  // Subscribe to step status BindRefs.
  for (const step of steps) {
    if (step.statusBind != null) {
      ctx.registerCleanup(subscribeBindRef(step.statusBind, ctx.store, rebuild));
    }
  }
  return wrapper;
}

// =============================================================================
// Stopwatch (0x0610)
// =============================================================================

export const STOPWATCH_TAG = 0x0610;
const SW_FIELD_KEYS = new Set([0, 1, 2]);
const SW_VARIANTS = new Set(['seconds', 'minutes', 'hours', 'full']);

function renderStopwatch(component, ctx) {
  assertOnlyKnownFields(component.fields, SW_FIELD_KEYS, 'Stopwatch');

  const startedAtPath = requirePath(ctx.readField(component.fields, 0), 'Stopwatch.started_at_path');
  const variant = requireEnum(ctx.readField(component.fields, 1), SW_VARIANTS, 'Stopwatch.variant');
  const tone = requireEnum(ctx.readField(component.fields, 2), TONES, 'Stopwatch.tone');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-stopwatch', `tf-stopwatch--tone-${tone}`);

  const timeEl = document.createElement('span');
  timeEl.classList.add('tf-stopwatch__time');
  wrapper.appendChild(timeEl);

  let intervalId = null;
  const updateIntervalMs = variant === 'full' ? 50 : 1000;

  const formatElapsed = (elapsedMs) => {
    const totalSecs = Math.floor(elapsedMs / 1000);
    const hrs = Math.floor(totalSecs / 3600);
    const mins = Math.floor((totalSecs % 3600) / 60);
    const secs = totalSecs % 60;
    switch (variant) {
      case 'seconds': return `${totalSecs}s`;
      case 'minutes': return `${mins}:${String(secs).padStart(2, '0')}`;
      case 'hours': return `${hrs}:${String(mins).padStart(2, '0')}:${String(secs).padStart(2, '0')}`;
      case 'full': {
        const ms = Math.floor((elapsedMs % 1000) / 10);
        return `${hrs}:${String(mins).padStart(2, '0')}:${String(secs).padStart(2, '0')}.${String(ms).padStart(2, '0')}`;
      }
      default: return `${totalSecs}s`;
    }
  };

  const tick = () => {
    let startedAt = null;
    try { startedAt = ctx.store.read(startedAtPath); } catch { /* no data yet */ }
    if (startedAt == null) { timeEl.textContent = '—'; return; }
    const startMs = typeof startedAt === 'number' ? startedAt : Date.parse(startedAt);
    if (isNaN(startMs)) { timeEl.textContent = '—'; return; }
    const elapsed = Math.max(0, Date.now() - startMs);
    timeEl.textContent = formatElapsed(elapsed);
  };

  const startInterval = () => {
    if (intervalId != null) clearInterval(intervalId);
    tick();
    intervalId = setInterval(tick, updateIntervalMs);
  };
  startInterval();

  ctx.registerCleanup(() => { if (intervalId != null) clearInterval(intervalId); });
  ctx.registerCleanup(ctx.store.subscribe(startedAtPath, startInterval));
  return wrapper;
}

// =============================================================================
// VirtualizedLog (0x0611)
// =============================================================================

export const VIRTUALIZED_LOG_TAG = 0x0611;
const VL_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
const LOG_VARIANTS = new Set(['compact', 'default', 'expanded']);
const LOG_LEVELS = new Set(['trace', 'debug', 'info', 'warn', 'error', 'fatal']);
const DENSITIES = new Set(['compact', 'default', 'comfortable']);

function renderVirtualizedLog(component, ctx) {
  assertOnlyKnownFields(component.fields, VL_FIELD_KEYS, 'VirtualizedLog');

  const eventsPath = requirePath(ctx.readField(component.fields, 0), 'VirtualizedLog.events_path');
  const variant = requireEnum(ctx.readField(component.fields, 1), LOG_VARIANTS, 'VirtualizedLog.variant');
  const maxBufferRaw = ctx.readField(component.fields, 2);
  let maxBufferEvents = 10000;
  if (maxBufferRaw != null) {
    if (typeof maxBufferRaw !== 'number' || !Number.isInteger(maxBufferRaw) || maxBufferRaw < 0 || maxBufferRaw > 0xFFFFFFFF) {
      throw new TypeError('VirtualizedLog.max_buffer_events must be u32');
    }
    maxBufferEvents = maxBufferRaw;
  }
  const autoScroll = requireBool(ctx.readField(component.fields, 3), 'VirtualizedLog.auto_scroll');
  const searchable = requireBool(ctx.readField(component.fields, 4), 'VirtualizedLog.searchable');
  const filterLevelsRaw = ctx.readField(component.fields, 5);
  if (filterLevelsRaw == null || !Array.isArray(filterLevelsRaw)) throw new TypeError('VirtualizedLog.filter_levels must be array');
  for (const lvl of filterLevelsRaw) {
    if (!LOG_LEVELS.has(lvl)) throw new TypeError(`VirtualizedLog.filter_levels: unknown level '${lvl}'`);
  }
  const filterLevels = new Set(filterLevelsRaw);
  const showTimestamps = requireBool(ctx.readField(component.fields, 6), 'VirtualizedLog.show_timestamps');
  const showSource = requireBool(ctx.readField(component.fields, 7), 'VirtualizedLog.show_source');
  const copyable = requireBool(ctx.readField(component.fields, 8), 'VirtualizedLog.copyable');
  const heightRaw = ctx.readField(component.fields, 9);
  const height = heightRaw != null ? parseDimensionToken(heightRaw, 'VirtualizedLog.height') : '100%';
  const maxHeightRaw = ctx.readField(component.fields, 10);
  const maxHeight = maxHeightRaw != null ? parseDimensionToken(maxHeightRaw, 'VirtualizedLog.max_height') : null;
  const density = requireEnum(ctx.readField(component.fields, 11), DENSITIES, 'VirtualizedLog.density');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-log-viewer', `tf-log-viewer--${variant}`, `tf-log-viewer--density-${density}`);
  if (height != null) wrapper.style.height = height;
  if (maxHeight != null) wrapper.style.maxHeight = maxHeight;
  wrapper.style.overflow = 'auto';

  if (copyable) wrapper.setAttribute('data-copyable', 'true');
  if (searchable) wrapper.setAttribute('data-searchable', 'true');

  const listEl = document.createElement('div');
  listEl.classList.add('tf-log-viewer__list');
  listEl.setAttribute('role', 'log');

  const rebuild = () => {
    listEl.replaceChildren();
    let events = [];
    try { const arr = ctx.store.read(eventsPath); if (Array.isArray(arr)) events = arr; } catch { /* no data yet */ }

    // Trim to max buffer.
    if (events.length > maxBufferEvents) events = events.slice(events.length - maxBufferEvents);

    for (const ev of events) {
      if (ev == null || typeof ev !== 'object') continue;
      const level = ev.level || 'info';
      // Apply filter: show only if filterLevels is empty or includes the level.
      if (filterLevels.size > 0 && !filterLevels.has(level)) continue;

      const row = document.createElement('div');
      row.classList.add('tf-log-viewer__entry', `tf-log-viewer__entry--${level}`);

      if (showTimestamps && ev.timestamp != null) {
        const tsEl = document.createElement('span');
        tsEl.classList.add('tf-log-viewer__ts');
        tsEl.textContent = String(ev.timestamp);
        row.appendChild(tsEl);
      }

      const levelEl = document.createElement('span');
      levelEl.classList.add('tf-log-viewer__level');
      levelEl.textContent = level.toUpperCase();
      row.appendChild(levelEl);

      if (showSource && ev.source != null) {
        const srcEl = document.createElement('span');
        srcEl.classList.add('tf-log-viewer__source');
        srcEl.textContent = String(ev.source);
        row.appendChild(srcEl);
      }

      const msgEl = document.createElement('span');
      msgEl.classList.add('tf-log-viewer__msg');
      msgEl.textContent = ev.message != null ? String(ev.message) : '';
      row.appendChild(msgEl);

      listEl.appendChild(row);
    }

    if (autoScroll) wrapper.scrollTop = wrapper.scrollHeight;
  };
  rebuild();
  ctx.registerCleanup(ctx.store.subscribe(eventsPath, rebuild));

  wrapper.appendChild(listEl);
  return wrapper;
}

// =============================================================================
// Registration
// =============================================================================

export function registerSpecializedContentRenderers() {
  if (!lookupComponentRenderer(IMAGE_GALLERY_TAG)) registerComponentRenderer(IMAGE_GALLERY_TAG, renderImageGallery);
  if (!lookupComponentRenderer(CAROUSEL_TAG)) registerComponentRenderer(CAROUSEL_TAG, renderCarousel);
  if (!lookupComponentRenderer(PDF_VIEWER_TAG)) registerComponentRenderer(PDF_VIEWER_TAG, renderPdfViewer);
  if (!lookupComponentRenderer(FPS_COUNTER_TAG)) registerComponentRenderer(FPS_COUNTER_TAG, renderFpsCounter);
  if (!lookupComponentRenderer(STEP_PROGRESS_TAG)) registerComponentRenderer(STEP_PROGRESS_TAG, renderStepProgress);
  if (!lookupComponentRenderer(STOPWATCH_TAG)) registerComponentRenderer(STOPWATCH_TAG, renderStopwatch);
  if (!lookupComponentRenderer(VIRTUALIZED_LOG_TAG)) registerComponentRenderer(VIRTUALIZED_LOG_TAG, renderVirtualizedLog);
}
