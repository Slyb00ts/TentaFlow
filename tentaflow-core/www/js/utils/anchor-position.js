// =============================================================================
// Plik: utils/anchor-position.js
// Opis: Anchor positioning dla Popover/Tooltip. Oblicza koordynaty
// fixed-positioned overlayu wzgledem anchora i robi auto-flip gdy nie miesci
// sie w viewport.
// =============================================================================

// Obliczenia pozycji dla kazdego placement. `a` to DOMRect anchora, `p` to
// {w, h} popovera. 8px to gap miedzy anchor a popover.
const PLACEMENT_TO_OFFSETS = {
  'top-start':    (a, p) => ({ left: a.left,                       top: a.top - p.h - 8 }),
  'top-end':      (a, p) => ({ left: a.right - p.w,                top: a.top - p.h - 8 }),
  'top':          (a, p) => ({ left: a.left + (a.width - p.w) / 2, top: a.top - p.h - 8 }),
  'bottom-start': (a, p) => ({ left: a.left,                       top: a.bottom + 8 }),
  'bottom-end':   (a, p) => ({ left: a.right - p.w,                top: a.bottom + 8 }),
  'bottom':       (a, p) => ({ left: a.left + (a.width - p.w) / 2, top: a.bottom + 8 }),
  'right-start':  (a, p) => ({ left: a.right + 8,                  top: a.top }),
  'right':        (a, p) => ({ left: a.right + 8,                  top: a.top + (a.height - p.h) / 2 }),
  'left-start':   (a, p) => ({ left: a.left - p.w - 8,             top: a.top }),
  'left':         (a, p) => ({ left: a.left - p.w - 8,             top: a.top + (a.height - p.h) / 2 }),
};

// Kolejnosc fallbackow gdy preferowany placement nie miesci sie w viewport.
const FLIP_FALLBACKS = {
  'top-start':    ['bottom-start', 'top-end',      'bottom-end'],
  'top-end':      ['bottom-end',   'top-start',    'bottom-start'],
  'top':          ['bottom',       'top-start',    'bottom-start'],
  'bottom-start': ['top-start',    'bottom-end',   'top-end'],
  'bottom-end':   ['top-end',      'bottom-start', 'top-start'],
  'bottom':       ['top',          'bottom-start', 'top-start'],
  'right-start':  ['left-start',   'right',        'left'],
  'right':        ['left',         'right-start',  'left-start'],
  'left-start':   ['right-start',  'left',         'right'],
  'left':         ['right',        'left-start',   'right-start'],
};

// Normalizacja placement z formatu backendu (TopStart/BottomEnd/...) lub
// kebab-case. Akceptuje rowniez snake_case (top_start).
function normalizePlacement(p) {
  if (!p || typeof p !== 'string') return 'bottom-start';
  if (PLACEMENT_TO_OFFSETS[p]) return p;
  const kebab = p
    .replace(/([a-z])([A-Z])/g, '$1-$2')
    .replace(/_/g, '-')
    .toLowerCase();
  return PLACEMENT_TO_OFFSETS[kebab] ? kebab : 'bottom-start';
}

/**
 * Pozycjonuje popover przy anchorze. Iteruje preferowany placement +
 * fallbacks i wybiera pierwszy ktory miesci sie w viewport. Jesli zaden -
 * clampuje do viewport zachowujac preferowany placement.
 *
 * Returns: { placement, left, top } w pikselach.
 */
export function computeAnchorPosition(anchorEl, popoverEl, preferredPlacement = 'bottom-start') {
  if (!anchorEl || !popoverEl) return null;

  const placement = normalizePlacement(preferredPlacement);
  const anchor = anchorEl.getBoundingClientRect();
  const popover = { w: popoverEl.offsetWidth, h: popoverEl.offsetHeight };
  const viewport = { w: window.innerWidth, h: window.innerHeight };

  const candidates = [placement, ...(FLIP_FALLBACKS[placement] || [])];

  for (const cand of candidates) {
    const fn = PLACEMENT_TO_OFFSETS[cand];
    if (!fn) continue;
    const pos = fn(anchor, popover);
    if (
      pos.left >= 8 &&
      pos.top >= 8 &&
      pos.left + popover.w <= viewport.w - 8 &&
      pos.top + popover.h <= viewport.h - 8
    ) {
      return { placement: cand, left: pos.left, top: pos.top };
    }
  }

  // Zaden wariant nie pasuje - clamp do viewport zachowujac preferowany.
  const fn = PLACEMENT_TO_OFFSETS[placement];
  const pos = fn(anchor, popover);
  return {
    placement,
    left: Math.max(8, Math.min(pos.left, viewport.w - popover.w - 8)),
    top: Math.max(8, Math.min(pos.top, viewport.h - popover.h - 8)),
  };
}

/**
 * Przyczepia popover do anchora po target_id i auto-reposition na scroll/resize.
 * Zwraca cleanup function ktora trzeba wywolac przy zamknieciu popovera, aby
 * zdjac listenery.
 */
export function attachPopover(popoverEl, anchorId, placement) {
  const anchorEl = anchorId ? document.getElementById(anchorId) : null;
  if (!anchorEl) {
    console.warn(`[popover] anchor #${anchorId} not found - centering`);
    // Brak anchora - centrujemy popover, nie ustawiamy listenerow.
    popoverEl.style.position = 'fixed';
    popoverEl.style.left = '50%';
    popoverEl.style.top = '50%';
    popoverEl.style.transform = 'translate(-50%, -50%)';
    return () => {};
  }

  const reposition = () => {
    const result = computeAnchorPosition(anchorEl, popoverEl, placement);
    if (!result) return;
    popoverEl.style.position = 'fixed';
    popoverEl.style.left = `${result.left}px`;
    popoverEl.style.top = `${result.top}px`;
    popoverEl.dataset.placement = result.placement;
  };

  // Pierwszy reposition - czekamy frame, zeby popover dostał wymiary po insercie.
  requestAnimationFrame(reposition);

  window.addEventListener('scroll', reposition, { passive: true, capture: true });
  window.addEventListener('resize', reposition);

  return () => {
    window.removeEventListener('scroll', reposition, { capture: true });
    window.removeEventListener('resize', reposition);
  };
}
