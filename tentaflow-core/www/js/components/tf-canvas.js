// =============================================================================
// Plik: components/tf-canvas.js
// Opis: Custom element <tf-canvas> renderujacy liste DrawCommand z SDK
// (SpecializedComponent::Canvas). Polecenia ustawiane sa przez property
// `commands` (Array<DrawCommand>), eventy pointerdown/move/up/cancel wysylane
// sa callbackiem `onPointer({ x, y, action, button })`. Esc emituje cancel.
// Komenda `image` rozwiazuje `ImageSource` (w tym signed_frame) przez
// `/js/utils/signed-frame.js` i cache'uje obiekty Image per URL.
// =============================================================================

import { resolveImageSource } from '/js/utils/signed-frame.js';
import { cssToken } from './shared-styles.js';

class TfCanvas extends HTMLElement {
  constructor() {
    super();
    this._commands = [];
    this._width = 600;
    this._height = 300;
    this._cursor = 'default';
    this._background = null;
    this._onPointer = null;
    this._throttleMs = 0;
    this._lastPointerTs = 0;
    this._canvas = null;
    // Cache obiektów Image — klucz: signed URL, wartość: HTMLImageElement.
    // Trzymamy je w komponencie (nie globalnie), bo cykl życia jest powiązany
    // z DOM-em; przy detachu cały komponent znika z GC.
    this._imageCache = new Map();
    // RAF throttling redraw-a po async swapach (image load / resolve).
    this._redrawScheduled = false;
    // Numer wersji commands — używamy do anulowania nieaktualnych pre-resolve'ów,
    // gdyby setter `commands` strzelił w trakcie poprzedniego pre-resolve passa.
    this._commandsVersion = 0;
    this._onKeyDown = this._onKeyDown.bind(this);
  }

  connectedCallback() {
    if (!this._canvas) {
      this._canvas = document.createElement('canvas');
      this._canvas.className = 'sdk-canvas';
      this._canvas.addEventListener('pointerdown', (e) => this._emitPointer(e, 'down'));
      this._canvas.addEventListener('pointermove', (e) => this._emitPointer(e, 'move'));
      this._canvas.addEventListener('pointerup', (e) => this._emitPointer(e, 'up'));
      this.appendChild(this._canvas);
    }
    window.addEventListener('keydown', this._onKeyDown);
    this._render();
  }

  disconnectedCallback() {
    window.removeEventListener('keydown', this._onKeyDown);
  }

  set commands(value) {
    this._commands = Array.isArray(value) ? value : [];
    this._commandsVersion += 1;
    if (this.isConnected) this._render();
  }

  set width(value) {
    const n = Number(value);
    if (Number.isFinite(n) && n > 0) this._width = n;
    if (this.isConnected) this._render();
  }

  set height(value) {
    const n = Number(value);
    if (Number.isFinite(n) && n > 0) this._height = n;
    if (this.isConnected) this._render();
  }

  set background(value) {
    this._background = value || null;
    if (this.isConnected) this._render();
  }

  set cursor(value) {
    this._cursor = value || 'default';
    if (this._canvas) this._canvas.style.cursor = this._cursor;
  }

  set onPointer(callback) {
    this._onPointer = typeof callback === 'function' ? callback : null;
  }

  set pointerThrottleMs(value) {
    const n = Number(value);
    this._throttleMs = Number.isFinite(n) && n >= 0 ? n : 0;
  }

  _emitPointer(ev, action) {
    if (!this._onPointer) return;
    if (action === 'move' && this._throttleMs > 0) {
      const now = performance.now();
      if (now - this._lastPointerTs < this._throttleMs) return;
      this._lastPointerTs = now;
    }
    const rect = this._canvas.getBoundingClientRect();
    const x = ev.clientX - rect.left;
    const y = ev.clientY - rect.top;
    this._onPointer({ x, y, action, button: ev.button });
  }

  _onKeyDown(ev) {
    if (ev.key !== 'Escape') return;
    if (!this._onPointer) return;
    // Esc anuluje aktywne rysowanie. Addon (np. m09 ZoneEditor) decyduje
    // czy reset stanu czy ignor.
    this._onPointer({ x: 0, y: 0, action: 'cancel', button: 0 });
    ev.preventDefault();
  }

  _requestRedraw() {
    if (this._redrawScheduled || !this.isConnected) return;
    this._redrawScheduled = true;
    requestAnimationFrame(() => {
      this._redrawScheduled = false;
      this._renderSync();
    });
  }

  _render() {
    // Pre-resolve commands typu `image` (signed_frame potrzebuje round-tripa).
    // Po pre-resolve odpalamy sync draw. Wszystkie inne komendy nie wymagają
    // async, więc rysują się natychmiast po pierwszej iteracji.
    this._preResolveImages().then(() => this._renderSync());
  }

  async _preResolveImages() {
    const version = this._commandsVersion;
    const imageCommands = this._commands.filter((c) => c && c.kind === 'image');
    if (imageCommands.length === 0) return;

    await Promise.all(
      imageCommands.map(async (cmd) => {
        try {
          cmd._resolved = await resolveImageSource(cmd.source);
        } catch (err) {
          console.warn('[tf-canvas] image resolve failed:', err?.code, err?.message);
          cmd._resolved = { kind: 'placeholder' };
        }
      }),
    );

    // Jeśli commands zmieniły się w trakcie pre-resolve — porzucamy wynik;
    // świeższy setter wystartuje nowy pass.
    if (version !== this._commandsVersion) return;

    // Pre-load HTMLImageElement dla resolved URL-i. Po `onload` wymuszamy redraw,
    // żeby pierwsza pełna klatka pojawiła się bez czekania na kolejny refresh.
    for (const cmd of imageCommands) {
      const r = cmd._resolved;
      if (!r || r.kind !== 'url') continue;
      if (this._imageCache.has(r.url)) continue;
      const img = new Image();
      img.decoding = 'async';
      img.onload = () => this._requestRedraw();
      img.onerror = () => {
        // Niepowodzenie ładowania traktujemy jak placeholder — usuwamy z cache,
        // żeby kolejny render mógł spróbować ponownie (np. po wygaśnięciu URL-a).
        this._imageCache.delete(r.url);
        this._requestRedraw();
      };
      img.src = r.url;
      this._imageCache.set(r.url, img);
    }
  }

  _renderSync() {
    if (!this._canvas) return;
    const cv = this._canvas;
    cv.width = this._width;
    cv.height = this._height;
    cv.style.cursor = this._cursor;
    const ctx = cv.getContext('2d');
    if (!ctx) return;
    ctx.clearRect(0, 0, cv.width, cv.height);
    if (this._background) {
      ctx.fillStyle = colorVar(this._background);
      ctx.fillRect(0, 0, cv.width, cv.height);
    }
    for (const cmd of this._commands) {
      drawCommand(ctx, cmd, this._imageCache);
    }
  }
}

// Color role → CSS variable from controls.css (--tf-* design tokens).
function colorVar(role) {
  if (!role) return '#888';
  const map = {
    primary: '--tf-accent-1',
    primary_hover: '--tf-accent-2',
    accent: '--tf-accent-2',
    accent_hover: '--tf-accent-1',
    success: '--tf-success',
    warning: '--tf-warning',
    danger: '--tf-danger',
    info: '--tf-info',
    text: '--tf-text',
    text_muted: '--tf-text-2',
    text_subtle: '--tf-text-3',
    text_inverse: '--tf-bg',
    bg: '--tf-bg',
    bg_elevated: '--tf-bg-card',
    bg_surface: '--tf-bg-2',
    bg_input: '--tf-bg-input',
    border: '--tf-border',
    border_hover: '--tf-border-hover',
  };
  const varName = map[role];
  if (!varName) return '#888';
  return cssToken(varName, '#888');
}

function drawCommand(ctx, cmd, imageCache) {
  if (!cmd || typeof cmd.kind !== 'string') return;
  switch (cmd.kind) {
    case 'line':
      ctx.strokeStyle = colorVar(cmd.color);
      ctx.lineWidth = Number(cmd.width) || 1;
      ctx.beginPath();
      ctx.moveTo(cmd.from.x, cmd.from.y);
      ctx.lineTo(cmd.to.x, cmd.to.y);
      ctx.stroke();
      break;
    case 'polygon': {
      const pts = Array.isArray(cmd.points) ? cmd.points : [];
      if (pts.length < 2) return;
      ctx.beginPath();
      ctx.moveTo(pts[0].x, pts[0].y);
      for (let i = 1; i < pts.length; i++) ctx.lineTo(pts[i].x, pts[i].y);
      if (cmd.closed) ctx.closePath();
      if (cmd.fill) { ctx.fillStyle = colorVar(cmd.fill); ctx.fill(); }
      if (cmd.stroke) {
        ctx.strokeStyle = colorVar(cmd.stroke.color);
        ctx.lineWidth = Number(cmd.stroke.width) || 1;
        if (Array.isArray(cmd.stroke.dash)) ctx.setLineDash(cmd.stroke.dash);
        ctx.stroke();
        ctx.setLineDash([]);
      }
      break;
    }
    case 'rect': {
      const r = Number(cmd.corner_radius) || 0;
      const path = new Path2D();
      if (r > 0 && typeof path.roundRect === 'function') {
        path.roundRect(cmd.x, cmd.y, cmd.width, cmd.height, r);
      } else {
        path.rect(cmd.x, cmd.y, cmd.width, cmd.height);
      }
      if (cmd.fill) { ctx.fillStyle = colorVar(cmd.fill); ctx.fill(path); }
      if (cmd.stroke) {
        ctx.strokeStyle = colorVar(cmd.stroke.color);
        ctx.lineWidth = Number(cmd.stroke.width) || 1;
        if (Array.isArray(cmd.stroke.dash)) ctx.setLineDash(cmd.stroke.dash);
        ctx.stroke(path);
        ctx.setLineDash([]);
      }
      break;
    }
    case 'circle':
      ctx.beginPath();
      ctx.arc(cmd.center.x, cmd.center.y, cmd.radius, 0, Math.PI * 2);
      if (cmd.fill) { ctx.fillStyle = colorVar(cmd.fill); ctx.fill(); }
      if (cmd.stroke) {
        ctx.strokeStyle = colorVar(cmd.stroke.color);
        ctx.lineWidth = Number(cmd.stroke.width) || 1;
        ctx.stroke();
      }
      break;
    case 'text':
      ctx.fillStyle = colorVar(cmd.color);
      ctx.font = `${Number(cmd.size_px) || 14}px system-ui, sans-serif`;
      ctx.textAlign = mapTextAlign(cmd.align);
      ctx.fillText(String(cmd.text ?? ''), cmd.pos.x, cmd.pos.y);
      break;
    case 'image':
      drawImageCommand(ctx, cmd, imageCache);
      break;
    default:
      break;
  }
}

function drawImageCommand(ctx, cmd, imageCache) {
  const x = Number(cmd.x) || 0;
  const y = Number(cmd.y) || 0;
  const w = Number(cmd.width) || 0;
  const h = Number(cmd.height) || 0;
  if (w <= 0 || h <= 0) return;

  const opacity = Number.isFinite(Number(cmd.opacity)) ? Number(cmd.opacity) : 1.0;
  const prevAlpha = ctx.globalAlpha;
  ctx.globalAlpha = Math.max(0, Math.min(1, opacity));

  const resolved = cmd._resolved;
  if (resolved && resolved.kind === 'url') {
    const img = imageCache.get(resolved.url);
    if (img && img.complete && img.naturalWidth > 0) {
      ctx.drawImage(img, x, y, w, h);
      ctx.globalAlpha = prevAlpha;
      return;
    }
    // Obraz jeszcze nie załadowany — rysujemy placeholder; redraw odpali się
    // z `onload` w pre-resolve passie.
    drawImagePlaceholder(ctx, x, y, w, h);
    ctx.globalAlpha = prevAlpha;
    return;
  }

  // resolved.kind === 'placeholder' albo brak (np. pre-resolve nie zdążył).
  drawImagePlaceholder(ctx, x, y, w, h);
  ctx.globalAlpha = prevAlpha;
}

function drawImagePlaceholder(ctx, x, y, w, h) {
  ctx.fillStyle = colorVar('bg_surface');
  ctx.fillRect(x, y, w, h);
  ctx.strokeStyle = colorVar('border');
  ctx.lineWidth = 1;
  ctx.setLineDash([4, 4]);
  ctx.strokeRect(x + 0.5, y + 0.5, w - 1, h - 1);
  ctx.setLineDash([]);

  // Glyph "image" — prosty piktogram (góra + okrąg + trójkąty), uniwersalny
  // jak ikona w SVG sprite. Nie ładujemy tu SVG-ka, bo wymagałby async
  // i drugi kanał renderu; rysujemy ścieżkę natywnie w kontekście 2d.
  const cx = x + w / 2;
  const cy = y + h / 2;
  const size = Math.min(w, h) * 0.35;
  if (size < 6) return;
  ctx.fillStyle = colorVar('text_subtle');
  ctx.globalAlpha *= 0.6;
  ctx.beginPath();
  ctx.arc(cx - size * 0.3, cy - size * 0.2, size * 0.18, 0, Math.PI * 2);
  ctx.fill();
  ctx.beginPath();
  ctx.moveTo(cx - size * 0.6, cy + size * 0.5);
  ctx.lineTo(cx - size * 0.1, cy - size * 0.05);
  ctx.lineTo(cx + size * 0.2, cy + size * 0.25);
  ctx.lineTo(cx + size * 0.55, cy - size * 0.15);
  ctx.lineTo(cx + size * 0.6, cy + size * 0.5);
  ctx.closePath();
  ctx.fill();
}

function mapTextAlign(a) {
  switch (a) {
    case 'center': return 'center';
    case 'end': return 'end';
    case 'justify': return 'start';
    default: return 'start';
  }
}

if (!customElements.get('tf-canvas')) {
  customElements.define('tf-canvas', TfCanvas);
}

export { TfCanvas };
