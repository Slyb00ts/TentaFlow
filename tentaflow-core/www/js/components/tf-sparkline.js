// =============================================================================
// Plik: components/tf-sparkline.js
// Opis: Mikro wykres linii (Specialized::Sparkline). Property `points` jako
// Array<number>; bez osi, bez legendy.
// =============================================================================

class TfSparkline extends HTMLElement {
  constructor() {
    super();
    this._points = [];
    this._height = 32;
    this._color = null;
    this._fill = false;
    this._showDots = false;
    this._variant = 'line';
    this._canvas = null;
  }

  connectedCallback() {
    if (!this._canvas) {
      this._canvas = document.createElement('canvas');
      this._canvas.style.display = 'block';
      this.appendChild(this._canvas);
      // Delikatny fade-in przy pierwszym pojawieniu sie — canvas-line draw
      // animation wymagalaby wlasnej petli rAF, ktora bije sie z update'ami.
      this.classList.add('sdk-animate-fade-in');
    }
    this._render();
  }

  set points(value) { this._points = Array.isArray(value) ? value.map(Number).filter(Number.isFinite) : []; if (this.isConnected) this._render(); }
  get points() { return this._points; }
  set color(value) { this._color = value || null; if (this.isConnected) this._render(); }
  get color() { return this._color; }
  set fill(value) { this._fill = Boolean(value); if (this.isConnected) this._render(); }
  get fill() { return this._fill; }
  set showDots(value) { this._showDots = Boolean(value); if (this.isConnected) this._render(); }
  get showDots() { return this._showDots; }
  set height(value) { const n = Number(value); if (Number.isFinite(n) && n > 0) this._height = n; if (this.isConnected) this._render(); }
  get height() { return this._height; }
  set variant(value) { this._variant = (value === 'bar' || value === 'area') ? value : 'line'; if (this.isConnected) this._render(); }
  get variant() { return this._variant; }

  _resolveColor() {
    const role = this._color || 'primary';
    const map = { primary: '--tf-accent-1', success: '--tf-success', warning: '--tf-warning', danger: '--tf-danger', info: '--tf-info', accent: '--tf-accent-2' };
    const v = map[role];
    if (!v) return '#6366f1';
    return getComputedStyle(document.documentElement).getPropertyValue(v).trim() || '#6366f1';
  }

  _render() {
    const cv = this._canvas;
    if (!cv) return;
    const w = Math.max(60, this.clientWidth || 120);
    const h = this._height;
    cv.width = w;
    cv.height = h;
    const ctx = cv.getContext('2d');
    if (!ctx) return;
    ctx.clearRect(0, 0, w, h);
    const pts = this._points;
    const color = this._resolveColor();
    const min = pts.length ? Math.min(...pts) : 0;
    const max = pts.length ? Math.max(...pts) : 0;
    const range = max - min || 1;
    const pad = 2;

    // Bars: jeden slupek na punkt, wysokosc proporcjonalna do wartosci. Linia i
    // area sa rysowane wspolna sciezka ponizej.
    if (this._variant === 'bar') {
      if (pts.length < 1) return;
      ctx.fillStyle = color;
      const gap = pts.length > 1 ? 1 : 0;
      const slotW = (w - pad * 2) / pts.length;
      const barW = Math.max(1, slotW - gap);
      pts.forEach((v, i) => {
        const bh = ((v - min) / range) * (h - pad * 2);
        const x = pad + i * slotW + (slotW - barW) / 2;
        const y = h - pad - bh;
        ctx.fillRect(x, y, barW, Math.max(1, bh));
      });
      return;
    }

    if (pts.length < 2) return;
    const stepX = (w - pad * 2) / (pts.length - 1);
    ctx.strokeStyle = color;
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    pts.forEach((v, i) => {
      const x = pad + i * stepX;
      const y = h - pad - ((v - min) / range) * (h - pad * 2);
      if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
    });
    ctx.stroke();
    if (this._fill) {
      ctx.lineTo(pad + (pts.length - 1) * stepX, h - pad);
      ctx.lineTo(pad, h - pad);
      ctx.closePath();
      ctx.globalAlpha = 0.15;
      ctx.fillStyle = color;
      ctx.fill();
      ctx.globalAlpha = 1;
    }
    if (this._showDots) {
      pts.forEach((v, i) => {
        const x = pad + i * stepX;
        const y = h - pad - ((v - min) / range) * (h - pad * 2);
        ctx.beginPath();
        ctx.arc(x, y, 2, 0, Math.PI * 2);
        ctx.fillStyle = color;
        ctx.fill();
      });
    }
  }
}

if (!customElements.get('tf-sparkline')) {
  customElements.define('tf-sparkline', TfSparkline);
}

export { TfSparkline };
