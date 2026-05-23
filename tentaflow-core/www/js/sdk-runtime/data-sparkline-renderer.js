// =============================================================================
// Plik: sdk-runtime/data-sparkline-renderer.js
// Opis: Renderer Sparkline (0x0215) — chunk 3.3d-7. Inline mini chart z
// prawdziwym SVG rendering: line (polyline), area (polygon z fill), bar
// (rectangles). data_path: StatePath → Array<finite number>. Reactive
// rebuild przy patch'u.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/charts.rs Sparkline.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';

const SPARKLINE_VARIANTS = new Set(['line', 'area', 'bar']);
const TONES = new Set(['neutral', 'primary', 'success', 'warning', 'critical', 'info', 'muted']);
const SVG_NS = 'http://www.w3.org/2000/svg';

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
function requireU16(v, ctx) {
  if (!Number.isInteger(v) || v < 0 || v > 0xFFFF) throw new TypeError(`${ctx}: expected u16, got ${v}`);
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

// =============================================================================
// Sparkline (0x0215)
// =============================================================================

export const SPARKLINE_TAG = 0x0215;
const SPARKLINE_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);

function renderSparkline(component, ctx) {
  assertOnlyKnownFields(component.fields, SPARKLINE_FIELD_KEYS, 'Sparkline');

  const dataPath = requirePath(ctx.readField(component.fields, 0), 'Sparkline.data_path');
  const variant = requireEnum(ctx.readField(component.fields, 1), SPARKLINE_VARIANTS, 'Sparkline.variant');
  const tone = requireEnum(ctx.readField(component.fields, 2), TONES, 'Sparkline.tone');
  const widthPx = requireU16(ctx.readField(component.fields, 3), 'Sparkline.width_px');
  if (widthPx === 0) throw new TypeError('Sparkline.width_px must be > 0');
  const heightPx = requireU16(ctx.readField(component.fields, 4), 'Sparkline.height_px');
  if (heightPx === 0) throw new TypeError('Sparkline.height_px must be > 0');
  const showMinMax = requireBool(ctx.readField(component.fields, 5), 'Sparkline.show_min_max');

  const wrapper = document.createElement('span');
  wrapper.classList.add('tf-sparkline');
  wrapper.classList.add(`tf-sparkline--variant-${variant}`);
  wrapper.classList.add(`tf-sparkline--tone-${tone}`);
  wrapper.style.display = 'inline-flex';
  wrapper.style.alignItems = 'center';
  wrapper.style.gap = '0.5em';

  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.classList.add('tf-sparkline__svg');
  svg.setAttribute('width', String(widthPx));
  svg.setAttribute('height', String(heightPx));
  svg.setAttribute('viewBox', `0 0 ${widthPx} ${heightPx}`);
  svg.setAttribute('preserveAspectRatio', 'none');
  svg.setAttribute('role', 'img');
  svg.setAttribute('aria-label', 'Sparkline chart');
  wrapper.appendChild(svg);

  let minBadge = null;
  let maxBadge = null;
  if (showMinMax) {
    const statsWrap = document.createElement('span');
    statsWrap.classList.add('tf-sparkline__stats');
    minBadge = document.createElement('span');
    minBadge.classList.add('tf-sparkline__min');
    statsWrap.appendChild(minBadge);
    const sep = document.createElement('span');
    sep.classList.add('tf-sparkline__sep');
    sep.setAttribute('aria-hidden', 'true');
    sep.textContent = '/';
    statsWrap.appendChild(sep);
    maxBadge = document.createElement('span');
    maxBadge.classList.add('tf-sparkline__max');
    statsWrap.appendChild(maxBadge);
    wrapper.appendChild(statsWrap);
  }

  const readData = () => {
    let arr;
    try { arr = ctx.store.read(dataPath); } catch { arr = undefined; }
    if (!Array.isArray(arr)) return [];
    // Filter finite numbers; nieliczbowe wpisy są ignorowane (renderer
    // jest defensywny — błędne dane nie crashują UI).
    return arr.filter((n) => typeof n === 'number' && Number.isFinite(n));
  };

  const rebuild = () => {
    svg.replaceChildren();
    const data = readData();
    if (data.length === 0) {
      if (minBadge) minBadge.textContent = '';
      if (maxBadge) maxBadge.textContent = '';
      return;
    }
    let min = data[0], max = data[0];
    for (const n of data) { if (n < min) min = n; if (n > max) max = n; }
    if (showMinMax) {
      minBadge.textContent = formatStat(min);
      maxBadge.textContent = formatStat(max);
    }
    // Range protection — gdy wszystkie wartości równe, użyj range=1 żeby
    // nie dzielić przez 0; punkt na środku SVG.
    const range = max - min === 0 ? 1 : max - min;
    const n = data.length;
    if (variant === 'bar') {
      // Bar variant: każdy słupek to <rect> wysokości proporcjonalnej do
      // (value - min) / range; szerokość = width/n.
      const barW = n > 0 ? widthPx / n : widthPx;
      for (let i = 0; i < n; i++) {
        const h = ((data[i] - min) / range) * heightPx;
        const rect = document.createElementNS(SVG_NS, 'rect');
        rect.setAttribute('x', String(i * barW));
        rect.setAttribute('y', String(heightPx - h));
        // Spec wymaga width/n bez sztucznej przerwy — wizualne odstępy są
        // domeną CSS (opcjonalny stroke z surface bg między barami).
        rect.setAttribute('width', String(barW));
        rect.setAttribute('height', String(h));
        rect.classList.add('tf-sparkline__bar');
        svg.appendChild(rect);
      }
      return;
    }
    // line/area: polyline po wszystkich punktach.
    const stepX = n > 1 ? widthPx / (n - 1) : 0;
    const points = data.map((v, i) => {
      const x = i * stepX;
      const y = heightPx - ((v - min) / range) * heightPx;
      return `${x},${y}`;
    }).join(' ');
    if (variant === 'area') {
      // Area: polygon z dolnymi rogami SVG zamknięty pod krzywą.
      const polygon = document.createElementNS(SVG_NS, 'polygon');
      polygon.setAttribute('points', `0,${heightPx} ${points} ${widthPx},${heightPx}`);
      polygon.classList.add('tf-sparkline__area');
      svg.appendChild(polygon);
      // Plus widoczna linia powyżej fill'u (lepsza czytelność).
      const line = document.createElementNS(SVG_NS, 'polyline');
      line.setAttribute('points', points);
      line.classList.add('tf-sparkline__line');
      svg.appendChild(line);
    } else {
      const line = document.createElementNS(SVG_NS, 'polyline');
      line.setAttribute('points', points);
      line.classList.add('tf-sparkline__line');
      svg.appendChild(line);
    }
  };
  rebuild();
  ctx.registerCleanup(ctx.store.subscribe(dataPath, rebuild));

  return wrapper;
}

/// Format min/max badge: liczby całkowite bez kropki, ułamki z 2 miejscami.
function formatStat(n) {
  if (!Number.isFinite(n)) return '';
  if (Number.isInteger(n)) return String(n);
  return n.toFixed(2);
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerDataSparklineRenderer() {
  if (!lookupComponentRenderer(SPARKLINE_TAG)) registerComponentRenderer(SPARKLINE_TAG, renderSparkline);
}
