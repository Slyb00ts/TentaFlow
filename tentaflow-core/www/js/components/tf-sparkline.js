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
  set color(value) { this._color = value || null; if (this.isConnected) this._render(); }
  set fill(value) { this._fill = Boolean(value); if (this.isConnected) this._render(); }
  set showDots(value) { this._showDots = Boolean(value); if (this.isConnected) this._render(); }
  set height(value) { const n = Number(value); if (Number.isFinite(n) && n > 0) this._height = n; if (this.isConnected) this._render(); }

  _resolveColor() {
    const role = this._color || 'primary';
    const map = { primary: '--sdk-color-primary', success: '--sdk-color-success', warning: '--sdk-color-warning', danger: '--sdk-color-danger', info: '--sdk-color-info', accent: '--sdk-color-accent' };
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
    ctx.clearRect(0, 0, w, h);
    const pts = this._points;
    if (pts.length < 2) return;
    const min = Math.min(...pts);
    const max = Math.max(...pts);
    const range = max - min || 1;
    const pad = 2;
    const stepX = (w - pad * 2) / (pts.length - 1);
    const color = this._resolveColor();
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
