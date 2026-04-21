// =============================================================================
// Plik: lib/block-zoom.js
// Opis: Blokuje globalne pinch-to-zoom i ctrl+wheel zoom dla calej strony.
//       Pozwala tylko w kontenerach opt-in: `[data-allow-pinch]` lub
//       wewnatrz elementu z `touch-action: none`. Potrzebne bo iOS Safari
//       od iOS 10 ignoruje `<meta viewport user-scalable=no>` ze wzgledow
//       accessibility; blokada przez JS jest jedyna metoda.
// =============================================================================

function isInAllowedZone(target) {
  if (!target || !(target instanceof Element)) return false;
  return !!target.closest('[data-allow-pinch]');
}

// iOS Safari: gesture events (gesturestart/gesturechange/gestureend).
// Dla iOS 13+ gesturestart jest jedyna skuteczna blokada pinch.
const onGesture = (e) => {
  if (isInAllowedZone(e.target)) return;
  e.preventDefault();
};
document.addEventListener('gesturestart', onGesture, { passive: false });
document.addEventListener('gesturechange', onGesture, { passive: false });
document.addEventListener('gestureend', onGesture, { passive: false });

// Ctrl+wheel zoom na desktop (Chrome/Firefox/Safari). Blokujemy globalnie
// poza data-allow-pinch. Uwaga: zwykle wheel scroll przechodzi dalej.
document.addEventListener('wheel', (e) => {
  if (!e.ctrlKey && !e.metaKey) return;
  if (isInAllowedZone(e.target)) return;
  e.preventDefault();
}, { passive: false });

// Double-tap zoom na iOS — wykryj dwa tap'y w < 300ms w tym samym miejscu.
let lastTapTime = 0;
let lastTapX = 0;
let lastTapY = 0;
document.addEventListener('touchend', (e) => {
  if (e.changedTouches.length !== 1) return;
  const t = e.changedTouches[0];
  const now = Date.now();
  const dt = now - lastTapTime;
  const dist = Math.hypot(t.clientX - lastTapX, t.clientY - lastTapY);
  if (dt < 300 && dist < 24) {
    if (!isInAllowedZone(e.target)) e.preventDefault();
    lastTapTime = 0;
  } else {
    lastTapTime = now;
    lastTapX = t.clientX;
    lastTapY = t.clientY;
  }
}, { passive: false });

// Ctrl/Cmd + '+' / '-' / '0' klawiatura — blokuje zoom przez skroty.
window.addEventListener('keydown', (e) => {
  if (!(e.ctrlKey || e.metaKey)) return;
  if (e.key === '+' || e.key === '-' || e.key === '=' || e.key === '0') {
    e.preventDefault();
  }
});
