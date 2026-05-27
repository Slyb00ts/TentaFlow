// =============================================================================
// File: js/components/tf-face.js
// Description: Multi-instance web component rendering the Head_5 wireframe face.
//              Each <tf-face> owns its canvas pair, blendshape state, and RAF loop.
//              Mini variant (size <= 32) renders a CSS-only gradient circle.
// Example: <tf-face mode="idle" size="360"></tf-face>
// =============================================================================

import {
  NUM_VERTICES,
  BASE_POSITIONS,
  BLENDSHAPE_DELTAS,
  LEFT_MASK,
  RIGHT_MASK,
  BS_INDEX,
} from '/js/data/face-data.js';

import {
  FACEMESH_CONTOURS,
  FACEMESH_FILL,
} from '/js/data/face-edges.js';

const BS = BS_INDEX;
const PITCH_BASE_OFFSET = -0.09;
const DESKTOP_SCALE_MUL = 0.29;
const MINI_THRESHOLD = 32;
const DEFAULT_SIZE = 200;

const EDGES = (() => {
  const result = [];
  for (const [a, b] of FACEMESH_FILL) {
    if (a < NUM_VERTICES && b < NUM_VERTICES) result.push([a, b, 0]);
  }
  for (const [a, b] of FACEMESH_CONTOURS) {
    if (a < NUM_VERTICES && b < NUM_VERTICES) result.push([a, b, 1]);
  }
  return result;
})();

const BASE_NORMALS = (() => {
  const nx = new Float32Array(NUM_VERTICES);
  const ny = new Float32Array(NUM_VERTICES);
  const nz = new Float32Array(NUM_VERTICES);
  let cx = 0, cy = 0, cz = 0;
  for (let i = 0; i < NUM_VERTICES; i++) {
    const j = i * 3;
    cx += BASE_POSITIONS[j];
    cy += BASE_POSITIONS[j + 1];
    cz += BASE_POSITIONS[j + 2];
  }
  cx /= NUM_VERTICES;
  cy /= NUM_VERTICES;
  cz /= NUM_VERTICES;
  for (let i = 0; i < NUM_VERTICES; i++) {
    const j = i * 3;
    const dx = BASE_POSITIONS[j] - cx;
    const dy = BASE_POSITIONS[j + 1] - cy;
    const dz = BASE_POSITIONS[j + 2] - cz;
    const len = Math.sqrt(dx * dx + dy * dy + dz * dz);
    if (len > 1e-6) {
      nx[i] = dx / len;
      ny[i] = dy / len;
      nz[i] = dz / len;
    } else {
      nx[i] = 0; ny[i] = 0; nz[i] = 1;
    }
  }
  return { nx, ny, nz };
})();

function easeInOut(t) {
  return t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) * 0.5;
}

class TfFace extends HTMLElement {
  static get observedAttributes() {
    return ['mode', 'size'];
  }

  constructor() {
    super();
    this._raf = 0;
    this._canvas = null;
    this._ctx = null;
    this._glowCanvas = null;
    this._glowCtx = null;
    this._container = null;
    this._mini = false;
    this._resizeObs = null;
    this._mouseHandler = null;
    this._visHandler = null;

    this._workVertices = new Float32Array(NUM_VERTICES * 3);
    this._projX = new Float32Array(NUM_VERTICES);
    this._projY = new Float32Array(NUM_VERTICES);
    this._projZ = new Float32Array(NUM_VERTICES);
    this._normalZ = new Float32Array(NUM_VERTICES);

    this._state = {
      phase: 0,
      lastFrameMs: 0,
      dpr: 1,
      mimicry: {
        mouth_open: 0, smile: 0, frown: 0,
        blink_left: 0, blink_right: 0,
        eyebrow_left: 0, eyebrow_right: 0,
        cheek_puff: 0, angry: 0,
        vis_aa: 0, vis_oo: 0, vis_ee: 0, vis_mm: 0,
        vis_ff: 0, vis_ll: 0, vis_ss: 0, vis_ch: 0,
      },
      targetYaw: 0,
      targetPitch: 0,
      parallaxYaw: 0,
      parallaxPitch: 0,
      blinkState: null,
      nextBlinkAt: 0,
      actions: [],
      nextBrowSurpriseAt: 0,
      nextBrowAsymAt: 0,
      nextFrownAt: 0,
      nextYawnAt: 0,
      nextVisemeAt: 0,
      nextCheekAt: 0,
      nextSmileAt: 0,
      shakeT0: null,
      shakeDuration: 0.8,
      uiMode: 'idle',
      speechAmp: 0,
      reducedMotion: false,
    };

    this._frame = this._renderFrame.bind(this);
  }

  connectedCallback() {
    this._build();
  }

  disconnectedCallback() {
    this._teardown();
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal) return;
    if (name === 'size') {
      const wasMini = this._mini;
      const isMini = this._parsedSize() <= MINI_THRESHOLD;
      if (wasMini !== isMini) {
        this._teardown();
        this._build();
      } else if (!this._mini) {
        this._syncCanvasSize();
      }
      return;
    }
    if (name === 'mode') {
      this._state.uiMode = newVal || 'idle';
      if (this._container) this._container.dataset.mode = this._state.uiMode;
      if (this._state.uiMode === 'speak') this._state.actions.length = 0;
    }
  }

  setSpeechAmplitude(rms) {
    const v = Number(rms);
    if (!Number.isFinite(v)) return;
    this._state.speechAmp = Math.max(0, Math.min(1, v));
  }

  shakeHead() {
    const s = this._state;
    if (!this._canvas) return;
    if (s.reducedMotion) {
      s.shakeT0 = s.phase;
      s.shakeDuration = 0.4;
      return;
    }
    s.shakeT0 = s.phase;
    s.shakeDuration = 0.8;
    this._scheduleAction(s.phase, {
      bsKey: 'angry', peakValue: 0.4, attack: 0.1, hold: 0.5, release: 0.2,
    });
  }

  _parsedSize() {
    const raw = this.getAttribute('size');
    if (raw === null || raw === '') return DEFAULT_SIZE;
    const n = parseInt(raw, 10);
    return Number.isFinite(n) && n > 0 ? n : DEFAULT_SIZE;
  }

  _build() {
    const size = this._parsedSize();
    this._mini = size <= MINI_THRESHOLD;
    this._state.uiMode = this.getAttribute('mode') || 'idle';

    if (this._mini) {
      this._buildMini();
    } else {
      this._buildFull();
    }
  }

  _buildMini() {
    const dot = document.createElement('div');
    dot.className = 'tf-face-mini';
    dot.dataset.mode = this._state.uiMode;
    const s = this._parsedSize();
    dot.style.width = `${s}px`;
    dot.style.height = `${s}px`;
    this.appendChild(dot);
    this._container = dot;
  }

  _buildFull() {
    const wrap = document.createElement('div');
    wrap.className = 'tf-face-glow';
    wrap.dataset.mode = this._state.uiMode;
    const s = this._parsedSize();
    wrap.style.width = `${s}px`;
    wrap.style.height = `${s}px`;
    wrap.style.background = 'radial-gradient(circle at 50% 45%, #0e1234 0%, #050818 70%)';

    const glow = document.createElement('canvas');
    glow.className = 'tf-face-glow-layer';
    glow.setAttribute('aria-hidden', 'true');
    wrap.appendChild(glow);

    const main = document.createElement('canvas');
    main.className = 'tf-face-main-layer';
    main.setAttribute('aria-hidden', 'true');
    wrap.appendChild(main);

    this.appendChild(wrap);
    this._container = wrap;
    this._canvas = main;
    this._glowCanvas = glow;
    this._ctx = main.getContext('2d', { alpha: true });
    this._glowCtx = glow.getContext('2d', { alpha: true });

    this._state.reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    this._resetIdleSchedule();
    this._syncCanvasSize();
    this._renderStaticFrame();

    this._mouseHandler = (e) => this._handleMouseMove(e);
    window.addEventListener('mousemove', this._mouseHandler);
    this._visHandler = () => {
      if (document.hidden) this._stopLoop();
      else this._startLoop();
    };
    document.addEventListener('visibilitychange', this._visHandler);
    if (typeof ResizeObserver !== 'undefined') {
      this._resizeObs = new ResizeObserver(() => {
        this._syncCanvasSize();
        if (this._state.reducedMotion) this._renderStaticFrame();
      });
      this._resizeObs.observe(wrap);
    }

    if (!this._state.reducedMotion) {
      this._startLoop();
    }
  }

  _teardown() {
    this._stopLoop();
    if (this._mouseHandler) {
      window.removeEventListener('mousemove', this._mouseHandler);
      this._mouseHandler = null;
    }
    if (this._visHandler) {
      document.removeEventListener('visibilitychange', this._visHandler);
      this._visHandler = null;
    }
    if (this._resizeObs) {
      this._resizeObs.disconnect();
      this._resizeObs = null;
    }
    while (this.firstChild) this.removeChild(this.firstChild);
    this._canvas = null;
    this._ctx = null;
    this._glowCanvas = null;
    this._glowCtx = null;
    this._container = null;
  }

  _resetIdleSchedule() {
    const s = this._state;
    s.phase = 0;
    s.lastFrameMs = 0;
    s.blinkState = null;
    s.actions.length = 0;
    s.nextBlinkAt = 1.5 + Math.random() * 2.0;
    s.nextSmileAt = 4.0 + Math.random() * 4.0;
    s.nextBrowSurpriseAt = 6.0 + Math.random() * 6.0;
    s.nextBrowAsymAt = 3.0 + Math.random() * 5.0;
    s.nextFrownAt = 10.0 + Math.random() * 8.0;
    s.nextYawnAt = 15.0 + Math.random() * 10.0;
    s.nextVisemeAt = 2.0 + Math.random() * 4.0;
    s.nextCheekAt = 12.0 + Math.random() * 10.0;
    s.targetYaw = 0;
    s.targetPitch = 0;
    s.parallaxYaw = 0;
    s.parallaxPitch = 0;
    s.shakeT0 = null;
    s.shakeDuration = 0.8;
    s.speechAmp = 0;
  }

  _syncCanvasSize() {
    if (!this._canvas || !this._container) return;
    const dpr = window.devicePixelRatio || 1;
    this._state.dpr = dpr;
    const w = Math.max(1, this._container.clientWidth);
    const h = Math.max(1, this._container.clientHeight);

    this._canvas.width = Math.max(1, Math.floor(w * dpr));
    this._canvas.height = Math.max(1, Math.floor(h * dpr));
    this._canvas.style.width = `${w}px`;
    this._canvas.style.height = `${h}px`;
    const gw = Math.max(1, Math.floor(w));
    const gh = Math.max(1, Math.floor(h));
    if (this._glowCanvas.width !== gw || this._glowCanvas.height !== gh) {
      this._glowCanvas.width = gw;
      this._glowCanvas.height = gh;
      this._glowCtx = this._glowCanvas.getContext('2d', { alpha: true });
    }
  }

  _startLoop() {
    if (this._raf !== 0) return;
    if (this._state.reducedMotion) return;
    this._state.lastFrameMs = 0;
    this._raf = requestAnimationFrame(this._frame);
  }

  _stopLoop() {
    if (this._raf !== 0) {
      cancelAnimationFrame(this._raf);
      this._raf = 0;
    }
  }

  _handleMouseMove(e) {
    if (!this._container) return;
    const rect = this._container.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;
    const inside = e.clientX >= rect.left && e.clientX <= rect.right
                && e.clientY >= rect.top && e.clientY <= rect.bottom;
    if (!inside) {
      this._state.targetYaw = 0;
      this._state.targetPitch = 0;
      return;
    }
    const mx = ((e.clientX - rect.left) / rect.width - 0.5) * 2;
    const my = ((e.clientY - rect.top) / rect.height - 0.5) * 2;
    this._state.targetYaw = mx * 0.25;
    this._state.targetPitch = -my * 0.18;
  }

  _applyBlendshapes(m) {
    const dst = this._workVertices;
    dst.set(BASE_POSITIONS);
    const THRESHOLD = 1e-4;

    const apply = (bsIdx, weight, maskL, maskR) => {
      if (bsIdx < 0 || Math.abs(weight) <= THRESHOLD) return;
      const deltas = BLENDSHAPE_DELTAS[bsIdx];
      const mask = maskL || maskR || null;
      if (mask) {
        for (let i = 0; i < NUM_VERTICES; i++) {
          const w = weight * mask[i];
          if (w === 0) continue;
          const j = i * 3;
          dst[j] += deltas[j] * w;
          dst[j + 1] += deltas[j + 1] * w;
          dst[j + 2] += deltas[j + 2] * w;
        }
      } else {
        for (let i = 0; i < NUM_VERTICES; i++) {
          const j = i * 3;
          dst[j] += deltas[j] * weight;
          dst[j + 1] += deltas[j + 1] * weight;
          dst[j + 2] += deltas[j + 2] * weight;
        }
      }
    };

    apply(BS.mouth_open, m.mouth_open, null, null);
    apply(BS.smile, m.smile, null, null);
    apply(BS.frown, m.frown, null, null);
    apply(BS.blink, m.blink_left, LEFT_MASK, null);
    apply(BS.blink, m.blink_right, null, RIGHT_MASK);
    apply(BS.brow_up, m.eyebrow_left, LEFT_MASK, null);
    apply(BS.brow_up, m.eyebrow_right, null, RIGHT_MASK);
    apply(BS.cheek_puff, m.cheek_puff, null, null);
    apply(BS.angry, m.angry, null, null);
    apply(BS.vis_aa, m.vis_aa, null, null);
    apply(BS.vis_oo, m.vis_oo, null, null);
    apply(BS.vis_ee, m.vis_ee, null, null);
    apply(BS.vis_mm, m.vis_mm, null, null);
    apply(BS.vis_ff, m.vis_ff, null, null);
    apply(BS.vis_ll, m.vis_ll, null, null);
    apply(BS.vis_ss, m.vis_ss, null, null);
    apply(BS.vis_ch, m.vis_ch, null, null);
  }

  _project(cx, cy, scale, yaw, pitch) {
    const sinY = Math.sin(yaw);
    const cosY = Math.cos(yaw);
    const sinP = Math.sin(pitch);
    const cosP = Math.cos(pitch);
    const scalePersp = scale * 1.8;
    const src = this._workVertices;
    const px = this._projX;
    const py = this._projY;
    const pz = this._projZ;
    const nz = this._normalZ;
    const bnx = BASE_NORMALS.nx;
    const bny = BASE_NORMALS.ny;
    const bnz = BASE_NORMALS.nz;

    for (let i = 0; i < NUM_VERTICES; i++) {
      const j = i * 3;
      const x = src[j];
      const y = src[j + 1];
      const z = src[j + 2];
      const x1 = x * cosY + z * sinY;
      const z1 = -x * sinY + z * cosY;
      const y1 = y * cosP - z1 * sinP;
      const z2 = y * sinP + z1 * cosP;
      const depth = 1.8 - z2;
      const invDepth = depth > 0.1 ? 1.0 / depth : 1.0 / 0.1;
      px[i] = cx + x1 * invDepth * scalePersp;
      py[i] = cy + y1 * invDepth * scalePersp;
      pz[i] = z2;

      const nx0 = bnx[i];
      const ny0 = bny[i];
      const nz0 = bnz[i];
      const nz1r = -nx0 * sinY + nz0 * cosY;
      nz[i] = ny0 * sinP + nz1r * cosP;
    }
  }

  _computeTintColor() {
    const s = this._state;

    // Shake override — white → red
    if (s.shakeT0 !== null) {
      const t = s.phase - s.shakeT0;
      const D = s.shakeDuration;
      if (t >= 0 && t < D) {
        let strength;
        if (t < D * 0.2) strength = t / (D * 0.2);
        else if (t < D * 0.7) strength = 1;
        else strength = 1 - (t - D * 0.7) / (D * 0.3);
        strength = strength * strength * (3 - 2 * strength);
        return {
          r: 255,
          g: Math.round(255 + (60 - 255) * strength),
          b: Math.round(255 + (60 - 255) * strength),
        };
      }
    }

    // Mode-based tint color
    const mode = this.getAttribute('mode') || 'idle';
    switch (mode) {
      case 'listen': return { r: 34, g: 197, b: 94 };
      case 'think':  return { r: 245, g: 158, b: 11 };
      case 'speak':  return { r: 167, g: 139, b: 250 };
      default:       return { r: 255, g: 255, b: 255 };
    }
  }

  _drawEdges(ctx, dpr) {
    const px = this._projX;
    const py = this._projY;
    const pz = this._projZ;
    const nz = this._normalZ;
    const tint = this._computeTintColor();
    const buckets = new Map();

    for (let i = 0; i < EDGES.length; i++) {
      const e = EDGES[i];
      const a = e[0];
      const b = e[1];
      const isContour = e[2];
      const visibility = (nz[a] + nz[b]) * 0.5;

      let t = (visibility + 0.3) / 1.3;
      if (t < 0) t = 0;
      else if (t > 1) t = 1;
      const smooth = t * t * (3 - 2 * t);
      const visFade = 0.08 + smooth * 0.92;

      const avgZ = (pz[a] + pz[b]) * 0.5;
      let depthT = (avgZ + 0.5) * 1.3;
      if (depthT < 0) depthT = 0;
      else if (depthT > 1) depthT = 1;

      let alpha = (depthT * 0.55 + 0.45) * visFade;
      if (!isContour) alpha *= 0.5;
      if (alpha < 0.01) continue;

      const alphaBucket = Math.round(alpha * 19);
      const widthBucket = Math.round(depthT * 9);
      const key = (isContour << 9) | (widthBucket << 5) | alphaBucket;

      let arr = buckets.get(key);
      if (!arr) { arr = []; buckets.set(key, arr); }
      arr.push(i);
    }

    const mainW = this._canvas.width;
    const mainH = this._canvas.height;
    const glowW = Math.max(1, Math.floor(mainW / dpr));
    const glowH = Math.max(1, Math.floor(mainH / dpr));
    if (this._glowCanvas.width !== glowW || this._glowCanvas.height !== glowH) {
      this._glowCanvas.width = glowW;
      this._glowCanvas.height = glowH;
      this._glowCtx = this._glowCanvas.getContext('2d', { alpha: true });
    }
    const gc = this._glowCtx;
    gc.setTransform(1 / dpr, 0, 0, 1 / dpr, 0, 0);
    gc.clearRect(0, 0, mainW, mainH);
    gc.lineCap = 'butt';
    gc.globalCompositeOperation = 'source-over';
    for (const [key, arr] of buckets) {
      const aB = key & 0x1f;
      const wB = (key >> 5) & 0xf;
      const al = aB / 19;
      const dT = wB / 9;
      gc.lineWidth = dpr * (1.4 + dT * 0.5);
      gc.strokeStyle = `rgba(${tint.r},${tint.g},${tint.b},${al.toFixed(3)})`;
      gc.beginPath();
      for (let i = 0; i < arr.length; i++) {
        const e = EDGES[arr[i]];
        gc.moveTo(px[e[0]], py[e[0]]);
        gc.lineTo(px[e[1]], py[e[1]]);
      }
      gc.stroke();
    }
    gc.setTransform(1, 0, 0, 1, 0, 0);

    ctx.lineCap = 'butt';
    ctx.globalCompositeOperation = 'source-over';
    for (const [key, arr] of buckets) {
      const aB = key & 0x1f;
      const wB = (key >> 5) & 0xf;
      const al = aB / 19;
      const dT = wB / 9;
      ctx.lineWidth = dpr * (1.4 + dT * 0.5);
      ctx.strokeStyle = `rgba(${tint.r},${tint.g},${tint.b},${al.toFixed(3)})`;
      ctx.beginPath();
      for (let i = 0; i < arr.length; i++) {
        const e = EDGES[arr[i]];
        ctx.moveTo(px[e[0]], py[e[0]]);
        ctx.lineTo(px[e[1]], py[e[1]]);
      }
      ctx.stroke();
    }
  }

  _scheduleAction(now, opts) {
    this._state.actions.push({
      bsKey: opts.bsKey,
      side: opts.side || null,
      peakValue: opts.peakValue,
      t0: now,
      attack: opts.attack,
      hold: opts.hold,
      release: opts.release,
    });
  }

  _evalActions(now, m) {
    const actions = this._state.actions;
    for (let i = actions.length - 1; i >= 0; i--) {
      const a = actions[i];
      const local = now - a.t0;
      const total = a.attack + a.hold + a.release;
      if (local >= total) { actions.splice(i, 1); continue; }
      let v = 0;
      if (local < a.attack) {
        v = easeInOut(local / a.attack) * a.peakValue;
      } else if (local < a.attack + a.hold) {
        v = a.peakValue;
      } else {
        v = easeInOut(1 - (local - a.attack - a.hold) / a.release) * a.peakValue;
      }
      if (a.bsKey === 'eyebrow') {
        if (a.side === 'left' || a.side === 'both') m.eyebrow_left += v;
        if (a.side === 'right' || a.side === 'both') m.eyebrow_right += v;
      } else {
        m[a.bsKey] += v;
      }
    }
  }

  _applySpeakMode(m) {
    const s = this._state;
    s.actions.length = 0;
    m.smile = 0;
    m.frown = 0;
    m.cheek_puff = 0;
    m.mouth_open = m.mouth_open + (s.speechAmp - m.mouth_open) * 0.4;
    const wobble = Math.abs(Math.sin(s.phase * 8));
    const wobble2 = Math.abs(Math.cos(s.phase * 8));
    m.vis_aa += 0.3 * s.speechAmp * wobble;
    m.vis_oo += 0.3 * s.speechAmp * wobble2;
  }

  _tickIdle() {
    const s = this._state;
    const m = s.mimicry;
    const t = s.phase;

    m.mouth_open = 0; m.smile = 0; m.frown = 0;
    m.eyebrow_left = 0; m.eyebrow_right = 0;
    m.cheek_puff = 0; m.angry = 0;
    m.vis_aa = 0; m.vis_oo = 0; m.vis_ee = 0; m.vis_mm = 0;
    m.vis_ff = 0; m.vis_ll = 0; m.vis_ss = 0; m.vis_ch = 0;

    m.mouth_open = 0.05 + Math.sin(t * 0.8) * 0.02;
    if (s.blinkState === null && t >= s.nextBlinkAt) {
      s.blinkState = { phase: 'in', t0: t, duration: 0.08 };
    }
    if (s.blinkState) {
      const bs = s.blinkState;
      const local = t - bs.t0;
      let value = 0;
      if (bs.phase === 'in') {
        value = Math.min(local / bs.duration, 1);
        if (local >= bs.duration) { bs.phase = 'hold'; bs.t0 = t; bs.duration = 0.05; }
      } else if (bs.phase === 'hold') {
        value = 1;
        if (local >= bs.duration) { bs.phase = 'out'; bs.t0 = t; bs.duration = 0.12; }
      } else if (bs.phase === 'out') {
        value = Math.max(1 - local / bs.duration, 0);
        if (local >= bs.duration) {
          s.blinkState = null;
          s.nextBlinkAt = t + 3.5 + Math.random() * 2.0;
        }
      }
      m.blink_left = value;
      m.blink_right = value;
    } else {
      m.blink_left = 0;
      m.blink_right = 0;
    }

    const uiMode = s.uiMode;
    const suppressLively = uiMode === 'speak' || uiMode === 'think';
    const dampenLively = uiMode === 'listen';

    if (!suppressLively && t >= s.nextSmileAt) {
      const polarity = Math.random() < 0.7 ? 1 : -1;
      let peak = polarity > 0 ? 0.15 + Math.random() * 0.15 : -(0.1 + Math.random() * 0.15);
      if (dampenLively) peak *= 0.5;
      this._scheduleAction(t, { bsKey: 'smile', peakValue: peak, attack: 0.3, hold: 0.6, release: 0.3 });
      s.nextSmileAt = t + 11.0 + Math.random() * 6.0;
    }

    if (!suppressLively && t >= s.nextBrowSurpriseAt) {
      this._scheduleAction(t, { bsKey: 'eyebrow', side: 'both', peakValue: 0.6, attack: 0.2, hold: 0.4, release: 0.9 });
      if (Math.random() < 0.7) {
        this._scheduleAction(t, { bsKey: 'mouth_open', peakValue: 0.15, attack: 0.2, hold: 0.3, release: 0.5 });
      }
      s.nextBrowSurpriseAt = t + 14.0 + Math.random() * 8.0;
    }

    if (!suppressLively && t >= s.nextBrowAsymAt) {
      const side = Math.random() < 0.5 ? 'left' : 'right';
      this._scheduleAction(t, { bsKey: 'eyebrow', side, peakValue: 0.45, attack: 0.25, hold: 0.4, release: 0.35 });
      s.nextBrowAsymAt = t + 9.0 + Math.random() * 7.0;
    }

    if (!suppressLively && t >= s.nextFrownAt) {
      this._scheduleAction(t, { bsKey: 'angry', peakValue: 0.4, attack: 0.3, hold: 0.6, release: 0.3 });
      s.nextFrownAt = t + 18.0 + Math.random() * 12.0;
    }

    if (!suppressLively && !dampenLively && t >= s.nextYawnAt) {
      this._scheduleAction(t, { bsKey: 'mouth_open', peakValue: 0.4, attack: 0.5, hold: 0.3, release: 0.7 });
      this._scheduleAction(t, { bsKey: 'eyebrow', side: 'both', peakValue: 0.2, attack: 0.5, hold: 0.3, release: 0.7 });
      s.nextYawnAt = t + 25.0 + Math.random() * 15.0;
    }

    if (uiMode !== 'speak' && t >= s.nextVisemeAt) {
      const choices = ['vis_aa', 'vis_oo', 'vis_ee', 'vis_mm'];
      const key = choices[Math.floor(Math.random() * choices.length)];
      this._scheduleAction(t, { bsKey: key, peakValue: 0.3 + Math.random() * 0.2, attack: 0.08, hold: 0.19, release: 0.08 });
      s.nextVisemeAt = t + 5.0 + Math.random() * 4.0;
    }

    if (!suppressLively && !dampenLively && t >= s.nextCheekAt) {
      this._scheduleAction(t, { bsKey: 'cheek_puff', peakValue: 0.3, attack: 0.2, hold: 0.3, release: 0.2 });
      s.nextCheekAt = t + 20.0 + Math.random() * 15.0;
    }

    this._evalActions(t, m);

    if (uiMode === 'listen') {
      m.eyebrow_left += 0.10;
      m.eyebrow_right += 0.10;
    } else if (uiMode === 'think') {
      m.angry += 0.15;
      m.eyebrow_left -= 0.05;
      m.eyebrow_right -= 0.05;
      m.mouth_open = 0;
    } else if (uiMode === 'speak') {
      this._applySpeakMode(m);
    }
  }

  _renderStaticFrame() {
    if (!this._canvas) return;
    const s = this._state;
    const m = s.mimicry;
    for (const k of Object.keys(m)) m[k] = 0;
    s.phase = 0;
    this._applyBlendshapes(m);
    const w = this._canvas.width;
    const h = this._canvas.height;
    this._ctx.clearRect(0, 0, w, h);
    const cx = w * 0.5;
    const cy = h * 0.56;
    const baseScale = Math.min(w, h) * DESKTOP_SCALE_MUL;
    this._project(cx, cy, baseScale, 0, PITCH_BASE_OFFSET);
    this._drawEdges(this._ctx, s.dpr);
  }

  _renderFrame(nowMs) {
    if (document.hidden || !this._canvas) { this._raf = 0; return; }
    const s = this._state;
    const dt = s.lastFrameMs > 0 ? (nowMs - s.lastFrameMs) / 1000 : 1 / 60;
    s.lastFrameMs = nowMs;
    s.phase += dt;

    this._tickIdle();

    const alpha = 0.06;
    s.parallaxYaw += (s.targetYaw - s.parallaxYaw) * alpha;
    s.parallaxPitch += (s.targetPitch - s.parallaxPitch) * alpha;

    this._applyBlendshapes(s.mimicry);

    const ctx = this._ctx;
    const w = this._canvas.width;
    const h = this._canvas.height;
    ctx.clearRect(0, 0, w, h);

    const cx = w * 0.5;
    const cy = h * 0.56;
    const baseScale = Math.min(w, h) * DESKTOP_SCALE_MUL;

    let yaw, pitch;
    if (s.shakeT0 !== null && (s.phase - s.shakeT0) >= s.shakeDuration) {
      s.shakeT0 = null;
    }
    if (s.shakeT0 !== null) {
      const tShake = s.phase - s.shakeT0;
      const damp = 1 - tShake / s.shakeDuration;
      yaw = damp * Math.sin((tShake / s.shakeDuration) * Math.PI * 6) * 0.35;
      pitch = PITCH_BASE_OFFSET;
    } else {
      const yawBase = Math.sin(s.phase * 0.15) * 0.15;
      const pitchBase = PITCH_BASE_OFFSET + Math.sin(s.phase * 0.1) * 0.08;
      yaw = yawBase + s.parallaxYaw;
      pitch = pitchBase + s.parallaxPitch;
    }

    this._project(cx, cy, baseScale, yaw, pitch);
    this._drawEdges(ctx, s.dpr);

    if (!s.reducedMotion && !document.hidden) {
      this._raf = requestAnimationFrame(this._frame);
    } else {
      this._raf = 0;
    }
  }
}

customElements.define('tf-face', TfFace);

export { TfFace };
