// =============================================================================
// File: modules/motion.js
// Opis: Orkiestracja animacji JS dla rzeczy niemożliwych do zrobienia samym CSS:
//   - page transitions (panel-navigate fade-out → fetch → fade-in),
//   - number counter (Stat: 0 → wartość finalna, easeOutCubic),
//   - stagger enter dla list/timeline/table,
//   - sparkline draw (stroke-dashoffset z policzoną długością ścieżki),
//   - heatmap cascade (delay per cell),
//   - animowane usuwanie elementów (leaving class + remove po klatce).
// Wszystkie funkcje respektują prefers-reduced-motion — w trybie reduce skaczą
// natychmiast do stanu finalnego, bez animacji.
// =============================================================================

/**
 * Sprawdza czy użytkownik ma włączone prefers-reduced-motion (system OS).
 */
export function prefersReducedMotion() {
  try {
    return window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  } catch {
    return false;
  }
}

/**
 * Page transition pomiędzy panelami addonu. Fade-out aktualnego rootu, fetch
 * nowego contentu, fade-in. Zwraca nowy element (już wstawiony do DOM).
 *
 * @param {HTMLElement} currentEl  Aktualny element rootu panelu.
 * @param {() => Promise<HTMLElement>} fetchNewContent Callback fetchujący i
 *                                                    renderujący nowy content.
 * @returns {Promise<HTMLElement|null>}
 */
export async function panelTransition(currentEl, fetchNewContent) {
  if (!currentEl) {
    return fetchNewContent ? await fetchNewContent() : null;
  }
  if (prefersReducedMotion()) {
    const newContent = await fetchNewContent();
    if (newContent && currentEl.parentNode) currentEl.replaceWith(newContent);
    return newContent;
  }

  currentEl.classList.add('sdk-panel-leaving');
  await waitFor(150);
  const newContent = await fetchNewContent();
  if (!newContent) return null;
  newContent.classList.add('sdk-panel-entering');
  if (currentEl.parentNode) currentEl.replaceWith(newContent);
  return newContent;
}

/**
 * Animuje wartość numeryczną od `from` do `to` w czasie `duration` ms.
 * Easing: easeOutCubic. Sprawdza prefers-reduced-motion: jeśli włączone,
 * ustawia od razu wartość finalną.
 *
 * @param {Text|HTMLElement} target  Element którego .textContent będzie aktualizowany.
 * @param {number} from
 * @param {number} to
 * @param {number} [duration=800]
 * @param {(v: number) => string} [format]  Formatter wyświetlanej wartości.
 */
export function animateNumber(target, from, to, duration = 800, format) {
  const fmt = typeof format === 'function'
    ? format
    : (v) => Math.round(v).toLocaleString('pl-PL');
  if (!target) return;
  if (prefersReducedMotion() || duration < 50 || from === to) {
    target.textContent = fmt(to);
    return;
  }
  const start = performance.now();
  const delta = to - from;
  const ease = (t) => 1 - Math.pow(1 - t, 3);

  function frame(now) {
    const elapsed = now - start;
    const t = Math.min(elapsed / duration, 1);
    target.textContent = fmt(from + delta * ease(t));
    if (t < 1) requestAnimationFrame(frame);
  }
  requestAnimationFrame(frame);
}

/**
 * Helper: ekstraktuje wartość numeryczną z tekstu Statu (np. "1,234", "98.5%",
 * "12.3 ms") i zwraca { num, format } gdzie format zachowuje oryginalny zapis.
 * Zwraca null jeśli tekst nie zawiera liczby.
 */
export function parseStatValue(text) {
  if (typeof text !== 'string') return null;
  const match = text.match(/^(\s*)([+-]?\d[\d\s,.]*)(.*)$/);
  if (!match) return null;
  const prefix = match[1] || '';
  const numText = match[2];
  const suffix = match[3] || '';
  // Heurystyka: jeśli jest kropka po cyfrach -> traktuj jako separator dziesiętny,
  // a przecinki/spacje jako separatory tysięcy.
  const hasDecimal = /\.\d/.test(numText);
  const cleaned = numText.replace(/[\s,]/g, '');
  const num = Number(cleaned);
  if (!Number.isFinite(num)) return null;
  const decimals = hasDecimal ? (numText.split('.')[1] || '').length : 0;
  const format = (v) => {
    const fixed = v.toFixed(decimals);
    const [intPart, decPart] = fixed.split('.');
    const grouped = Number(intPart).toLocaleString('pl-PL');
    const out = decPart != null ? `${grouped},${decPart}` : grouped;
    return `${prefix}${out}${suffix}`;
  };
  return { num, format };
}

/**
 * Stagger enter — dla każdego dziecka kontenera ustawia animation-delay i klasę
 * sdk-animate-slide-in-up. Zwraca Promise rozwiązujący się po wszystkich
 * animacjach (z grubsza, plus baseDuration).
 */
export function staggerEnter(container, selector = ':scope > *', delayMs = 40, baseDurationMs = 250) {
  if (!container) return Promise.resolve();
  if (prefersReducedMotion()) return Promise.resolve();

  const items = container.querySelectorAll(selector);
  items.forEach((el, idx) => {
    el.style.animationDelay = `${idx * delayMs}ms`;
    el.classList.add('sdk-animate-slide-in-up');
  });
  return new Promise((resolve) => {
    setTimeout(resolve, items.length * delayMs + baseDurationMs);
  });
}

/**
 * Sparkline draw — liczy długość ścieżki SVG i ustawia stroke-dasharray /
 * dashoffset tak by animacja sdk-sparkline-draw poprawnie pokryła całą krzywą
 * niezależnie od jej kształtu.
 */
export function animateSparklinePath(pathEl) {
  if (!pathEl) return;
  if (prefersReducedMotion()) return;
  let length = 0;
  try {
    length = pathEl.getTotalLength();
  } catch {
    return;
  }
  if (!Number.isFinite(length) || length <= 0) return;
  pathEl.style.strokeDasharray = `${length}`;
  pathEl.style.strokeDashoffset = `${length}`;
  // Force reflow tak żeby przejście było widoczne (przeglądarka inaczej zbatchuje style).
  pathEl.getBoundingClientRect();
  pathEl.style.transition = 'stroke-dashoffset 1s var(--sdk-easing-decelerate)';
  pathEl.style.strokeDashoffset = '0';
}

/**
 * Cascade enter dla komórek heatmapy — każda komórka dostaje delay = idx * step.
 */
export function cascadeHeatmapCells(cells, delayPerCell = 4) {
  if (!cells) return;
  if (prefersReducedMotion()) return;
  for (let i = 0; i < cells.length; i++) {
    cells[i].style.animationDelay = `${i * delayPerCell}ms`;
  }
}

/**
 * Usuwa element z DOM po animacji wyjścia (dodanie klasy `leaving`).
 * W trybie reduce-motion usuwa od razu.
 */
export function animatedRemove(el, leavingClass = 'leaving', durationMs = 150) {
  if (!el) return Promise.resolve();
  if (prefersReducedMotion()) {
    el.remove();
    return Promise.resolve();
  }
  el.classList.add(leavingClass);
  return new Promise((resolve) => {
    setTimeout(() => {
      el.remove();
      resolve();
    }, durationMs);
  });
}

function waitFor(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
