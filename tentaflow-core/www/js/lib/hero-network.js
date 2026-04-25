// =============================================================================
// Plik: lib/hero-network.js
// Opis: Animowana siatka particles + linie do canvasu w hero Dashboardu.
//       80 punktow dryfuje wolno, miedzy parami < THRESHOLD rysuja sie linie
//       (alpha proporcjonalna do dystansu). RAF pauzuje gdy zakladka schowana,
//       resize observer dostosowuje DPI. Eksport: mount(canvas) / unmount().
// =============================================================================

const POINT_COUNT = 90;
const LINK_DISTANCE = 140;       // px: max odleglosc rysowania linii miedzy punktami
const POINT_SPEED = 0.18;        // px/frame max
const POINT_RADIUS = 1.4;
const FILL = 'rgba(167, 139, 250, 0.55)';   // accent-2
const LINE = 'rgba(99, 102, 241, ';          // accent-1 + dynamic alpha

let rafId = null;
let resizeObserver = null;
let visibilityHandler = null;

export function mount(canvas) {
  if (!canvas || !(canvas instanceof HTMLCanvasElement)) return;

  const ctx = canvas.getContext('2d', { alpha: true });
  let dpr = Math.min(window.devicePixelRatio || 1, 2);
  let width = 0;
  let height = 0;
  let points = [];

  const resize = () => {
    const rect = canvas.getBoundingClientRect();
    width = Math.max(1, Math.round(rect.width));
    height = Math.max(1, Math.round(rect.height));
    canvas.width = width * dpr;
    canvas.height = height * dpr;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    if (points.length === 0) {
      seedPoints();
    } else {
      // utrzymaj istniejace punkty po resize (clamp do nowej powierzchni)
      points.forEach((p) => {
        if (p.x > width) p.x = width;
        if (p.y > height) p.y = height;
      });
    }
  };

  const seedPoints = () => {
    points = [];
    for (let i = 0; i < POINT_COUNT; i++) {
      points.push({
        x: Math.random() * width,
        y: Math.random() * height,
        vx: (Math.random() - 0.5) * POINT_SPEED,
        vy: (Math.random() - 0.5) * POINT_SPEED,
      });
    }
  };

  const tick = () => {
    ctx.clearRect(0, 0, width, height);

    // Update positions z odbiciem od krawedzi
    for (const p of points) {
      p.x += p.vx;
      p.y += p.vy;
      if (p.x < 0 || p.x > width) p.vx = -p.vx;
      if (p.y < 0 || p.y > height) p.vy = -p.vy;
    }

    // Linie miedzy punktami w zasiegu (alpha = (1 - d/THRESHOLD) * 0.6)
    ctx.lineWidth = 0.6;
    for (let i = 0; i < points.length; i++) {
      const a = points[i];
      for (let j = i + 1; j < points.length; j++) {
        const b = points[j];
        const dx = a.x - b.x;
        const dy = a.y - b.y;
        const d2 = dx * dx + dy * dy;
        if (d2 > LINK_DISTANCE * LINK_DISTANCE) continue;
        const d = Math.sqrt(d2);
        const alpha = (1 - d / LINK_DISTANCE) * 0.6;
        ctx.strokeStyle = LINE + alpha.toFixed(3) + ')';
        ctx.beginPath();
        ctx.moveTo(a.x, a.y);
        ctx.lineTo(b.x, b.y);
        ctx.stroke();
      }
    }

    // Punkty (na wierzchu linii)
    ctx.fillStyle = FILL;
    for (const p of points) {
      ctx.beginPath();
      ctx.arc(p.x, p.y, POINT_RADIUS, 0, Math.PI * 2);
      ctx.fill();
    }

    rafId = requestAnimationFrame(tick);
  };

  resize();
  rafId = requestAnimationFrame(tick);

  // Resize obserwer (responsive); pauza gdy zakladka schowana.
  if (window.ResizeObserver) {
    resizeObserver = new ResizeObserver(resize);
    resizeObserver.observe(canvas);
  } else {
    window.addEventListener('resize', resize);
  }
  visibilityHandler = () => {
    if (document.hidden) {
      if (rafId) { cancelAnimationFrame(rafId); rafId = null; }
    } else if (!rafId) {
      rafId = requestAnimationFrame(tick);
    }
  };
  document.addEventListener('visibilitychange', visibilityHandler);
}

export function unmount() {
  if (rafId) { cancelAnimationFrame(rafId); rafId = null; }
  if (resizeObserver) { resizeObserver.disconnect(); resizeObserver = null; }
  if (visibilityHandler) { document.removeEventListener('visibilitychange', visibilityHandler); visibilityHandler = null; }
}
