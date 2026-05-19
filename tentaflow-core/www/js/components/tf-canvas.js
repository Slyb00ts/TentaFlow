// =============================================================================
// Plik: components/tf-canvas.js
// Opis: Custom element <tf-canvas> renderujacy liste DrawCommand z SDK
// (SpecializedComponent::Canvas). Polecenia ustawiane sa przez property
// `commands` (Array<DrawCommand>), eventy pointerdown/move/up wysylane sa
// callbackiem `onPointer({ x, y, action, button })`.
// =============================================================================

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
    this._render();
  }

  set commands(value) {
    this._commands = Array.isArray(value) ? value : [];
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

  _render() {
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
      drawCommand(ctx, cmd);
    }
  }
}

// Mapowanie semantycznych rol koloru SDK na konkretne CSS variables ustawione
// przez sdk-theme.css. Zwracamy resolved hex/rgb przez getComputedStyle.
function colorVar(role) {
  if (!role) return '#888';
  const map = {
    primary: '--sdk-color-primary',
    primary_hover: '--sdk-color-primary-hover',
    accent: '--sdk-color-accent',
    accent_hover: '--sdk-color-accent-hover',
    success: '--sdk-color-success',
    warning: '--sdk-color-warning',
    danger: '--sdk-color-danger',
    info: '--sdk-color-info',
    text: '--sdk-color-text',
    text_muted: '--sdk-color-text-muted',
    text_subtle: '--sdk-color-text-subtle',
    text_inverse: '--sdk-color-text-inverse',
    bg: '--sdk-color-bg',
    bg_elevated: '--sdk-color-bg-elevated',
    bg_surface: '--sdk-color-bg-surface',
    bg_input: '--sdk-color-bg-input',
    border: '--sdk-color-border',
    border_hover: '--sdk-color-border-hover',
  };
  const varName = map[role];
  if (!varName) return '#888';
  const v = getComputedStyle(document.documentElement).getPropertyValue(varName).trim();
  return v || '#888';
}

function drawCommand(ctx, cmd) {
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
      // Zrodla obrazow obslugujemy przez addon UI render w warstwie wyzszej
      // — Canvas nie ma kontekstu addona do rozwiazania signed_frame. Pomijamy
      // ten command w MVP; addon ma uzyc <tf-video-stream> dla strumieni.
      break;
    default:
      break;
  }
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
