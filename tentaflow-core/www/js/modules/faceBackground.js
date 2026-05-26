// =============================================================================
// File: js/modules/faceBackground.js
// Description: Backward-compatible wrapper over <tf-face> web component.
//              show()/hide()/transitionOut() handle fullscreen login overlay.
//              embed() creates a <tf-face> inside the given host.
// Example: FaceBackground.show(); ... FaceBackground.hide();
//          const handle = FaceBackground.embed(node); handle.setMode('idle');
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

// Ensure tf-face is registered before use
import '/js/components/tf-face.js';

const CONTAINER_ID = 'face-bg-root';
const CANVAS_ID = 'face-bg-canvas';
const GLOW_CANVAS_ID = 'face-glow-canvas';
const PITCH_BASE_OFFSET = -0.09;
const DESKTOP_SCALE_MUL = 0.29;
const MOBILE_SCALE_MUL = 0.44;

const GYRO_GAMMA_RANGE = 45;
const GYRO_BETA_RANGE = 30;
const GYRO_YAW_GAIN = 0.3;
const GYRO_PITCH_GAIN = 0.22;

const BS = BS_INDEX;

function buildEdgeList() {
  const result = [];
  for (const [a, b] of FACEMESH_FILL) {
    if (a < NUM_VERTICES && b < NUM_VERTICES) result.push([a, b, 0]);
  }
  for (const [a, b] of FACEMESH_CONTOURS) {
    if (a < NUM_VERTICES && b < NUM_VERTICES) result.push([a, b, 1]);
  }
  return result;
}

const EDGES = buildEdgeList();

function findEyeIndicesFromBlink(side) {
  const mask = side === 'viewer-left' ? LEFT_MASK : RIGHT_MASK;
  const blinkIdx = BS_INDEX.blink;
  if (blinkIdx == null || blinkIdx < 0) return [];
  const deltas = BLENDSHAPE_DELTAS[blinkIdx];
  if (!deltas) return [];
  const MASK_THRESHOLD = 0.5;
  const candidates = [];
  for (let i = 0; i < NUM_VERTICES; i++) {
    if (mask[i] < MASK_THRESHOLD) continue;
    const j = i * 3;
    const dx = deltas[j];
    const dy = deltas[j + 1];
    const dz = deltas[j + 2];
    const mag = Math.sqrt(dx * dx + dy * dy + dz * dz);
    if (mag > 0) candidates.push({ i, mag });
  }
  candidates.sort((a, b) => b.mag - a.mag);
  const TOP_K = Math.min(12, candidates.length);
  return candidates.slice(0, TOP_K).map((c) => c.i);
}

const VIEWER_LEFT_EYE_INDICES = findEyeIndicesFromBlink('viewer-left');

function buildBaseNormals() {
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
    if (len > 1e-6) { nx[i] = dx / len; ny[i] = dy / len; nz[i] = dz / len; }
    else { nx[i] = 0; ny[i] = 0; nz[i] = 1; }
  }
  return { nx, ny, nz };
}

const BASE_NORMALS = buildBaseNormals();

// -- fullscreen login state (singleton — only one login overlay at a time) -----

const state = {
  canvas: null,
  ctx: null,
  glowCanvas: null,
  glowCtx: null,
  rafId: 0,
  startTime: 0,
  phase: 0,
  lastFrameMs: 0,
  dpr: 1,
  workVertices: new Float32Array(NUM_VERTICES * 3),
  projX: new Float32Array(NUM_VERTICES),
  projY: new Float32Array(NUM_VERTICES),
  projZ: new Float32Array(NUM_VERTICES),
  normalZ: new Float32Array(NUM_VERTICES),
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
  reducedMotion: false,
  mouseHandler: null,
  visibilityHandler: null,
  resizeHandler: null,
  orientationHandler: null,
  orientationSetupHandler: null,
  orientationSetupAttempted: false,
  betaBaseline: null,
  gammaBaseline: null,
  scaleMul: DESKTOP_SCALE_MUL,
  transitioning: false,
  transitionRafId: 0,
  zoomCx: null,
  zoomCy: null,
  yawOverride: null,
  pitchOverride: null,
  shakeT0: null,
  shakeDuration: 0.8,
  mountMode: 'none',
  uiMode: 'idle',
  speechAmp: 0,
  listenAmp: 0,
};

// Active embed element reference for cleanup on show()
let activeEmbedEl = null;

function setFaceLayerOpacity(opacity) {
  const value = String(opacity);
  if (state.glowCanvas) state.glowCanvas.style.opacity = value;
  if (state.canvas) state.canvas.style.opacity = value;
}

function resetFaceLayerStyles() {
  if (state.glowCanvas) { state.glowCanvas.style.transition = ''; state.glowCanvas.style.opacity = ''; }
  if (state.canvas) { state.canvas.style.transition = ''; state.canvas.style.opacity = ''; }
}

function easeInOut(t) {
  return t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) * 0.5;
}

function applyBlendshapes(m) {
  const dst = state.workVertices;
  dst.set(BASE_POSITIONS);
  const WEIGHT_THRESHOLD = 1e-4;
  const apply = (bsIdx, weight, maskLeft, maskRight) => {
    if (bsIdx < 0 || Math.abs(weight) <= WEIGHT_THRESHOLD) return;
    const deltas = BLENDSHAPE_DELTAS[bsIdx];
    const mask = maskLeft || maskRight || null;
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

function project(cx, cy, scale, yaw, pitch) {
  const sinY = Math.sin(yaw);
  const cosY = Math.cos(yaw);
  const sinP = Math.sin(pitch);
  const cosP = Math.cos(pitch);
  const scalePersp = scale * 1.8;
  const src = state.workVertices;
  const px = state.projX;
  const py = state.projY;
  const pz = state.projZ;
  const nz = state.normalZ;
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

function ensureGlowCanvas(w, h) {
  const glow = state.glowCanvas;
  if (!glow) return;
  if (glow.width !== w || glow.height !== h) {
    glow.width = w;
    glow.height = h;
    state.glowCtx = glow.getContext('2d', { alpha: true });
  }
}

function computeTintColor() {
  if (state.shakeT0 === null || state.shakeT0 === undefined) {
    return { r: 255, g: 255, b: 255 };
  }
  const t = state.phase - state.shakeT0;
  const D = state.shakeDuration;
  if (t < 0 || t >= D) return { r: 255, g: 255, b: 255 };
  let strength;
  if (t < D * 0.2) strength = t / (D * 0.2);
  else if (t < D * 0.7) strength = 1;
  else strength = 1 - (t - D * 0.7) / (D * 0.3);
  strength = strength * strength * (3 - 2 * strength);
  return {
    r: Math.round(255),
    g: Math.round(255 + (60 - 255) * strength),
    b: Math.round(255 + (60 - 255) * strength),
  };
}

function drawEdges(ctx, dpr) {
  const px = state.projX;
  const py = state.projY;
  const pz = state.projZ;
  const nz = state.normalZ;
  const tint = computeTintColor();
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

  const mainCanvas = state.canvas;
  const glowW = Math.max(1, Math.floor(mainCanvas.width / dpr));
  const glowH = Math.max(1, Math.floor(mainCanvas.height / dpr));
  ensureGlowCanvas(glowW, glowH);
  const glowCtx = state.glowCtx;

  if (state.glowCanvas && glowCtx) {
    glowCtx.setTransform(1 / dpr, 0, 0, 1 / dpr, 0, 0);
    glowCtx.clearRect(0, 0, mainCanvas.width, mainCanvas.height);
    glowCtx.lineCap = 'butt';
    glowCtx.globalCompositeOperation = 'source-over';
    for (const [key, arr] of buckets) {
      const aB = key & 0x1f;
      const wB = (key >> 5) & 0xf;
      const al = aB / 19;
      const dT = wB / 9;
      glowCtx.lineWidth = dpr * (1.4 + dT * 0.5);
      glowCtx.strokeStyle = `rgba(${tint.r},${tint.g},${tint.b},${al.toFixed(3)})`;
      glowCtx.beginPath();
      for (let i = 0; i < arr.length; i++) {
        const e = EDGES[arr[i]];
        glowCtx.moveTo(px[e[0]], py[e[0]]);
        glowCtx.lineTo(px[e[1]], py[e[1]]);
      }
      glowCtx.stroke();
    }
    glowCtx.setTransform(1, 0, 0, 1, 0, 0);
  }

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

function scheduleAction(now, opts) {
  state.actions.push({
    bsKey: opts.bsKey, side: opts.side || null, peakValue: opts.peakValue,
    t0: now, attack: opts.attack, hold: opts.hold, release: opts.release,
  });
}

function evalActions(now, m) {
  const actions = state.actions;
  for (let i = actions.length - 1; i >= 0; i--) {
    const a = actions[i];
    const local = now - a.t0;
    const total = a.attack + a.hold + a.release;
    if (local >= total) { actions.splice(i, 1); continue; }
    let v = 0;
    if (local < a.attack) v = easeInOut(local / a.attack) * a.peakValue;
    else if (local < a.attack + a.hold) v = a.peakValue;
    else v = easeInOut(1 - (local - a.attack - a.hold) / a.release) * a.peakValue;
    if (a.bsKey === 'eyebrow') {
      if (a.side === 'left' || a.side === 'both') m.eyebrow_left += v;
      if (a.side === 'right' || a.side === 'both') m.eyebrow_right += v;
    } else {
      m[a.bsKey] += v;
    }
  }
}

function applySpeakMode(m) {
  state.actions.length = 0;
  m.smile = 0; m.frown = 0; m.cheek_puff = 0;
  m.mouth_open = m.mouth_open + (state.speechAmp - m.mouth_open) * 0.4;
  const wobble = Math.abs(Math.sin(state.phase * 8));
  const wobble2 = Math.abs(Math.cos(state.phase * 8));
  m.vis_aa += 0.3 * state.speechAmp * wobble;
  m.vis_oo += 0.3 * state.speechAmp * wobble2;
}

function tickIdle() {
  const m = state.mimicry;
  const t = state.phase;

  m.mouth_open = 0; m.smile = 0; m.frown = 0;
  m.eyebrow_left = 0; m.eyebrow_right = 0;
  m.cheek_puff = 0; m.angry = 0;
  m.vis_aa = 0; m.vis_oo = 0; m.vis_ee = 0; m.vis_mm = 0;
  m.vis_ff = 0; m.vis_ll = 0; m.vis_ss = 0; m.vis_ch = 0;

  m.mouth_open = 0.05 + Math.sin(t * 0.8) * 0.02;

  if (state.blinkState === null && t >= state.nextBlinkAt) {
    state.blinkState = { phase: 'in', t0: t, duration: 0.08 };
  }
  if (state.blinkState) {
    const bs = state.blinkState;
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
        state.blinkState = null;
        state.nextBlinkAt = t + 3.5 + Math.random() * 2.0;
      }
    }
    m.blink_left = value;
    m.blink_right = value;
  } else {
    m.blink_left = 0;
    m.blink_right = 0;
  }

  if (state.transitioning) { evalActions(t, m); return; }

  const uiMode = state.uiMode;
  const suppressLively = uiMode === 'speak' || uiMode === 'think';
  const dampenLively = uiMode === 'listen';

  if (!suppressLively && t >= state.nextSmileAt) {
    const polarity = Math.random() < 0.7 ? 1 : -1;
    let peak = polarity > 0 ? 0.15 + Math.random() * 0.15 : -(0.1 + Math.random() * 0.15);
    if (dampenLively) peak *= 0.5;
    scheduleAction(t, { bsKey: 'smile', peakValue: peak, attack: 0.3, hold: 0.6, release: 0.3 });
    state.nextSmileAt = t + 11.0 + Math.random() * 6.0;
  }

  if (!suppressLively && t >= state.nextBrowSurpriseAt) {
    scheduleAction(t, { bsKey: 'eyebrow', side: 'both', peakValue: 0.6, attack: 0.2, hold: 0.4, release: 0.9 });
    if (Math.random() < 0.7) {
      scheduleAction(t, { bsKey: 'mouth_open', peakValue: 0.15, attack: 0.2, hold: 0.3, release: 0.5 });
    }
    state.nextBrowSurpriseAt = t + 14.0 + Math.random() * 8.0;
  }

  if (!suppressLively && t >= state.nextBrowAsymAt) {
    const side = Math.random() < 0.5 ? 'left' : 'right';
    scheduleAction(t, { bsKey: 'eyebrow', side, peakValue: 0.45, attack: 0.25, hold: 0.4, release: 0.35 });
    state.nextBrowAsymAt = t + 9.0 + Math.random() * 7.0;
  }

  if (!suppressLively && t >= state.nextFrownAt) {
    scheduleAction(t, { bsKey: 'angry', peakValue: 0.4, attack: 0.3, hold: 0.6, release: 0.3 });
    state.nextFrownAt = t + 18.0 + Math.random() * 12.0;
  }

  if (!suppressLively && !dampenLively && t >= state.nextYawnAt) {
    scheduleAction(t, { bsKey: 'mouth_open', peakValue: 0.4, attack: 0.5, hold: 0.3, release: 0.7 });
    scheduleAction(t, { bsKey: 'eyebrow', side: 'both', peakValue: 0.2, attack: 0.5, hold: 0.3, release: 0.7 });
    state.nextYawnAt = t + 25.0 + Math.random() * 15.0;
  }

  if (uiMode !== 'speak' && t >= state.nextVisemeAt) {
    const choices = ['vis_aa', 'vis_oo', 'vis_ee', 'vis_mm'];
    const key = choices[Math.floor(Math.random() * choices.length)];
    scheduleAction(t, { bsKey: key, peakValue: 0.3 + Math.random() * 0.2, attack: 0.08, hold: 0.19, release: 0.08 });
    state.nextVisemeAt = t + 5.0 + Math.random() * 4.0;
  }

  if (!suppressLively && !dampenLively && t >= state.nextCheekAt) {
    scheduleAction(t, { bsKey: 'cheek_puff', peakValue: 0.3, attack: 0.2, hold: 0.3, release: 0.2 });
    state.nextCheekAt = t + 20.0 + Math.random() * 15.0;
  }

  evalActions(t, m);

  if (uiMode === 'listen') {
    m.eyebrow_left += 0.10;
    m.eyebrow_right += 0.10;
  } else if (uiMode === 'think') {
    m.angry += 0.15;
    m.eyebrow_left -= 0.05;
    m.eyebrow_right -= 0.05;
    m.mouth_open = 0;
  } else if (uiMode === 'speak') {
    applySpeakMode(m);
  }
}

function renderFrame(nowMs) {
  if (document.hidden) { state.rafId = 0; return; }
  const dt = state.lastFrameMs > 0 ? (nowMs - state.lastFrameMs) / 1000 : 1 / 60;
  state.lastFrameMs = nowMs;
  state.phase += dt;

  tickIdle();

  const alpha = 0.06;
  state.parallaxYaw += (state.targetYaw - state.parallaxYaw) * alpha;
  state.parallaxPitch += (state.targetPitch - state.parallaxPitch) * alpha;

  applyBlendshapes(state.mimicry);

  const ctx = state.ctx;
  const canvas = state.canvas;
  const w = canvas.width;
  const h = canvas.height;
  ctx.clearRect(0, 0, w, h);

  const cx = state.zoomCx !== null ? state.zoomCx : w * 0.5;
  const cy = state.zoomCy !== null ? state.zoomCy : h * 0.56;
  const baseScale = Math.min(w, h) * state.scaleMul;

  let yaw, pitch;
  if (state.shakeT0 !== null && (state.phase - state.shakeT0) >= state.shakeDuration) {
    state.shakeT0 = null;
  }
  if (state.yawOverride !== null) {
    yaw = state.yawOverride;
    pitch = state.pitchOverride;
  } else if (state.shakeT0 !== null) {
    const tShake = state.phase - state.shakeT0;
    const damp = 1 - tShake / state.shakeDuration;
    yaw = damp * Math.sin((tShake / state.shakeDuration) * Math.PI * 6) * 0.35;
    pitch = PITCH_BASE_OFFSET;
  } else {
    const yawBase = Math.sin(state.phase * 0.15) * 0.15;
    const pitchBase = PITCH_BASE_OFFSET + Math.sin(state.phase * 0.1) * 0.08;
    yaw = yawBase + state.parallaxYaw;
    pitch = pitchBase + state.parallaxPitch;
  }

  project(cx, cy, baseScale, yaw, pitch);
  drawEdges(ctx, state.dpr);

  if (!state.reducedMotion && !document.hidden) {
    state.rafId = requestAnimationFrame(renderFrame);
  } else {
    state.rafId = 0;
  }
}

function renderStaticFrame() {
  const neutral = state.mimicry;
  for (const k of Object.keys(neutral)) neutral[k] = 0;
  state.phase = 0;
  applyBlendshapes(neutral);
  const ctx = state.ctx;
  const canvas = state.canvas;
  const w = canvas.width;
  const h = canvas.height;
  ctx.clearRect(0, 0, w, h);
  const cx = w * 0.5;
  const cy = h * 0.56;
  const baseScale = Math.min(w, h) * state.scaleMul;
  project(cx, cy, baseScale, 0, PITCH_BASE_OFFSET);
  drawEdges(ctx, state.dpr);
}

function syncCanvasSize() {
  const dpr = window.devicePixelRatio || 1;
  state.dpr = dpr;
  const w = window.innerWidth;
  const h = window.innerHeight;
  state.canvas.width = Math.max(1, Math.floor(w * dpr));
  state.canvas.height = Math.max(1, Math.floor(h * dpr));
  state.canvas.style.width = `${w}px`;
  state.canvas.style.height = `${h}px`;
  ensureGlowCanvas(Math.max(1, Math.floor(w)), Math.max(1, Math.floor(h)));
}

function startLoop() {
  if (state.rafId !== 0) return;
  if (state.reducedMotion) return;
  state.lastFrameMs = 0;
  state.rafId = requestAnimationFrame(renderFrame);
}

function stopLoop() {
  if (state.rafId !== 0) { cancelAnimationFrame(state.rafId); state.rafId = 0; }
}

function isMobileViewport() {
  if (typeof window === 'undefined') return false;
  const mql = window.matchMedia ? window.matchMedia('(pointer: coarse)') : null;
  if (mql && mql.matches) return true;
  return window.innerWidth < 768;
}

function handleMouseMove(e) {
  const mx = (e.clientX / window.innerWidth - 0.5) * 2;
  const my = (e.clientY / window.innerHeight - 0.5) * 2;
  state.targetYaw = mx * 0.25;
  state.targetPitch = -my * 0.18;
}

function handleDeviceOrientation(e) {
  const gammaRaw = e.gamma;
  const betaRaw = e.beta;
  if (gammaRaw == null || betaRaw == null) return;
  if (state.betaBaseline === null) state.betaBaseline = betaRaw;
  if (state.gammaBaseline === null) state.gammaBaseline = gammaRaw;
  const gDelta = gammaRaw - state.gammaBaseline;
  const bDelta = betaRaw - state.betaBaseline;
  const angle = (typeof screen !== 'undefined' && screen.orientation)
    ? screen.orientation.angle : (window.orientation || 0);
  let yawRaw, pitchRaw;
  if (angle === 90) { yawRaw = -bDelta; pitchRaw = gDelta; }
  else if (angle === -90 || angle === 270) { yawRaw = bDelta; pitchRaw = -gDelta; }
  else if (angle === 180) { yawRaw = -gDelta; pitchRaw = -bDelta; }
  else { yawRaw = gDelta; pitchRaw = bDelta; }
  const clampG = (v) => Math.max(-GYRO_GAMMA_RANGE, Math.min(GYRO_GAMMA_RANGE, v));
  const clampB = (v) => Math.max(-GYRO_BETA_RANGE, Math.min(GYRO_BETA_RANGE, v));
  yawRaw = clampG(yawRaw);
  pitchRaw = clampB(pitchRaw);
  state.targetYaw = -(yawRaw / GYRO_GAMMA_RANGE) * GYRO_YAW_GAIN;
  state.targetPitch = (pitchRaw / GYRO_BETA_RANGE) * GYRO_PITCH_GAIN;
}

function attachDeviceOrientationListener() {
  if (typeof window === 'undefined') return;
  if (typeof DeviceOrientationEvent === 'undefined') return;
  if (state.orientationHandler) return;
  state.orientationHandler = handleDeviceOrientation;
  window.addEventListener('deviceorientation', state.orientationHandler);
}

function setupOrientationAfterGesture() {
  if (state.orientationSetupAttempted) return;
  state.orientationSetupAttempted = true;
  if (typeof DeviceOrientationEvent === 'undefined') return;
  const requestPermission = DeviceOrientationEvent.requestPermission;
  if (typeof requestPermission === 'function') {
    requestPermission.call(DeviceOrientationEvent).then((result) => {
      if (result === 'granted') attachDeviceOrientationListener();
    }).catch(() => {});
  } else {
    attachDeviceOrientationListener();
  }
}

function setupDeviceOrientation() {
  if (typeof window === 'undefined') return;
  if (typeof DeviceOrientationEvent === 'undefined') return;
  const handler = () => {
    window.removeEventListener('touchstart', handler);
    window.removeEventListener('click', handler);
    state.orientationSetupHandler = null;
    setupOrientationAfterGesture();
  };
  state.orientationSetupHandler = handler;
  window.addEventListener('touchstart', handler, { passive: true });
  window.addEventListener('click', handler);
}

function handleVisibilityChange() {
  if (document.hidden) stopLoop();
  else startLoop();
}

function handleResize() {
  if (state.mountMode !== 'fullscreen') return;
  syncCanvasSize();
  state.scaleMul = isMobileViewport() ? MOBILE_SCALE_MUL : DESKTOP_SCALE_MUL;
  if (state.reducedMotion) renderStaticFrame();
}

function resetIdleSchedule() {
  state.phase = 0;
  state.lastFrameMs = 0;
  state.blinkState = null;
  state.actions.length = 0;
  state.nextBlinkAt = 1.5 + Math.random() * 2.0;
  state.nextSmileAt = 4.0 + Math.random() * 4.0;
  state.nextBrowSurpriseAt = 6.0 + Math.random() * 6.0;
  state.nextBrowAsymAt = 3.0 + Math.random() * 5.0;
  state.nextFrownAt = 10.0 + Math.random() * 8.0;
  state.nextYawnAt = 15.0 + Math.random() * 10.0;
  state.nextVisemeAt = 2.0 + Math.random() * 4.0;
  state.nextCheekAt = 12.0 + Math.random() * 10.0;
  state.targetYaw = 0;
  state.targetPitch = 0;
  state.parallaxYaw = 0;
  state.parallaxPitch = 0;
  state.shakeT0 = null;
  state.shakeDuration = 0.8;
}

function computeEyeOffset(indices, yaw, pitch) {
  const sinY = Math.sin(yaw);
  const cosY = Math.cos(yaw);
  const sinP = Math.sin(pitch);
  const cosP = Math.cos(pitch);
  const src = state.workVertices;
  let sumX = 0, sumY = 0, count = 0;
  for (let k = 0; k < indices.length; k++) {
    const i = indices[k];
    if (i >= NUM_VERTICES) continue;
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
    sumX += x1 * invDepth;
    sumY += y1 * invDepth;
    count++;
  }
  if (count === 0) return { dx: 0, dy: 0 };
  return { dx: sumX / count, dy: sumY / count };
}

// -- public API ---------------------------------------------------------------

export const FaceBackground = {
  show() {
    if (document.getElementById(CONTAINER_ID)) return;

    // Destroy active embed if any
    if (activeEmbedEl) {
      activeEmbedEl.remove();
      activeEmbedEl = null;
    }

    const container = document.createElement('div');
    container.id = CONTAINER_ID;
    container.className = 'face-bg';

    const glowCanvas = document.createElement('canvas');
    glowCanvas.id = GLOW_CANVAS_ID;
    glowCanvas.setAttribute('aria-hidden', 'true');
    container.appendChild(glowCanvas);

    const canvas = document.createElement('canvas');
    canvas.id = CANVAS_ID;
    canvas.setAttribute('aria-hidden', 'true');
    container.appendChild(canvas);

    document.body.appendChild(container);
    document.body.classList.add('has-face-bg');

    state.canvas = canvas;
    state.ctx = canvas.getContext('2d', { alpha: true });
    state.glowCanvas = glowCanvas;
    state.glowCtx = glowCanvas.getContext('2d', { alpha: true });
    state.reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    resetFaceLayerStyles();

    resetIdleSchedule();
    state.orientationSetupAttempted = false;
    state.betaBaseline = null;
    state.gammaBaseline = null;
    state.zoomCx = null;
    state.zoomCy = null;
    state.yawOverride = null;
    state.pitchOverride = null;
    state.mountMode = 'fullscreen';
    state.uiMode = 'idle';
    state.speechAmp = 0;
    state.listenAmp = 0;
    state.scaleMul = isMobileViewport() ? MOBILE_SCALE_MUL : DESKTOP_SCALE_MUL;

    syncCanvasSize();
    renderStaticFrame();

    requestAnimationFrame(() => { container.classList.add('is-visible'); });

    state.resizeHandler = handleResize;
    window.addEventListener('resize', state.resizeHandler);

    if (!state.reducedMotion) {
      if (typeof window !== 'undefined' && typeof DeviceOrientationEvent !== 'undefined') {
        setupDeviceOrientation();
      }
      state.mouseHandler = handleMouseMove;
      window.addEventListener('mousemove', state.mouseHandler);
      state.visibilityHandler = handleVisibilityChange;
      document.addEventListener('visibilitychange', state.visibilityHandler);
      startLoop();
    }
  },

  transitionOut(opts) {
    const onMidpoint = (opts && opts.onMidpoint) || (() => {});
    const onComplete = (opts && opts.onComplete) || (() => {});

    if (!state.canvas) { onMidpoint(); onComplete(); return; }

    if (state.reducedMotion) {
      onMidpoint();
      if (state.glowCanvas) state.glowCanvas.style.transition = 'opacity 0.2s linear';
      if (state.canvas) state.canvas.style.transition = 'opacity 0.2s linear';
      setFaceLayerOpacity('0');
      setTimeout(() => { FaceBackground.hide(); onComplete(); }, 200);
      return;
    }

    state.transitioning = true;
    state.actions.length = 0;
    state.targetYaw = 0;
    state.targetPitch = 0;
    state.parallaxYaw = 0;
    state.parallaxPitch = 0;
    state.yawOverride = 0;
    state.pitchOverride = PITCH_BASE_OFFSET;

    const eyeIndices = VIEWER_LEFT_EYE_INDICES.length > 0 ? VIEWER_LEFT_EYE_INDICES : [0];
    const scaleStart = state.scaleMul;
    const isMobile = isMobileViewport();
    const scaleEnd = isMobile ? 18 : 13;
    const DURATION = 1600;
    const FADE_START_T = 0.7;
    const UI_OFFSET_X = 15;
    const UI_OFFSET_Y = 25;
    const startTime = performance.now();

    const easeInOutCubic = (t) =>
      t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;

    try { onMidpoint(); } catch (e) { console.error('[faceBg] onMidpoint error:', e); }
    let uiRoot = null;

    const tick = (nowMs) => {
      const elapsed = nowMs - startTime;
      const t = Math.min(elapsed / DURATION, 1);
      const et = easeInOutCubic(t);

      state.scaleMul = scaleStart + (scaleEnd - scaleStart) * et;

      const canvas = state.canvas;
      if (!canvas) {
        state.transitionRafId = 0;
        state.transitioning = false;
        onComplete();
        return;
      }
      const wPx = canvas.width;
      const hPx = canvas.height;
      const baseScale = Math.min(wPx, hPx) * state.scaleMul;
      const scalePersp = baseScale * 1.8;
      const { dx, dy } = computeEyeOffset(eyeIndices, 0, PITCH_BASE_OFFSET);
      state.zoomCx = wPx * 0.5 - dx * scalePersp;
      state.zoomCy = hPx * 0.5 - dy * scalePersp;

      const uiScale = state.scaleMul / scaleEnd;

      if (!uiRoot) uiRoot = document.getElementById('app-root');
      if (uiRoot) {
        if (!uiRoot.classList.contains('is-emerging')) uiRoot.classList.add('is-emerging');
        uiRoot.style.setProperty('--tf-ui-scale', uiScale.toFixed(4));
        const uiOpacity = Math.min(1, uiScale * 5);
        uiRoot.style.setProperty('--tf-ui-opacity', uiOpacity.toFixed(3));
        const offFactor = Math.max(0, 1 - uiScale);
        uiRoot.style.setProperty('--tf-ui-offset-x', `${(UI_OFFSET_X * offFactor).toFixed(1)}px`);
        uiRoot.style.setProperty('--tf-ui-offset-y', `${(UI_OFFSET_Y * offFactor).toFixed(1)}px`);
      }

      if (t > FADE_START_T && (state.canvas || state.glowCanvas)) {
        const ft = (t - FADE_START_T) / (1 - FADE_START_T);
        setFaceLayerOpacity((1 - ft).toFixed(3));
      }

      if (t < 1) {
        state.transitionRafId = requestAnimationFrame(tick);
      } else {
        state.transitionRafId = 0;
        state.transitioning = false;
        state.zoomCx = null;
        state.zoomCy = null;
        state.yawOverride = null;
        state.pitchOverride = null;
        state.scaleMul = scaleStart;
        if (uiRoot) {
          uiRoot.classList.remove('is-emerging');
          uiRoot.style.removeProperty('--tf-ui-scale');
          uiRoot.style.removeProperty('--tf-ui-opacity');
          uiRoot.style.removeProperty('--tf-ui-offset-x');
          uiRoot.style.removeProperty('--tf-ui-offset-y');
        }
        FaceBackground.hide();
        try { onComplete(); } catch (e) { console.error('[faceBg] onComplete error:', e); }
      }
    };

    state.transitionRafId = requestAnimationFrame(tick);
  },

  shakeHead() {
    if (!state.canvas) return;
    if (state.reducedMotion) {
      state.shakeT0 = state.phase;
      state.shakeDuration = 0.4;
      return;
    }
    state.shakeT0 = state.phase;
    state.shakeDuration = 0.8;
    scheduleAction(state.phase, {
      bsKey: 'angry', peakValue: 0.4, attack: 0.1, hold: 0.5, release: 0.2,
    });
  },

  hide() {
    const container = document.getElementById(CONTAINER_ID);
    if (!container) return;

    stopLoop();
    if (state.mouseHandler) { window.removeEventListener('mousemove', state.mouseHandler); state.mouseHandler = null; }
    if (state.orientationHandler) { window.removeEventListener('deviceorientation', state.orientationHandler); state.orientationHandler = null; }
    if (state.orientationSetupHandler) {
      window.removeEventListener('touchstart', state.orientationSetupHandler);
      window.removeEventListener('click', state.orientationSetupHandler);
      state.orientationSetupHandler = null;
    }
    state.orientationSetupAttempted = false;
    state.betaBaseline = null;
    state.gammaBaseline = null;
    if (state.visibilityHandler) { document.removeEventListener('visibilitychange', state.visibilityHandler); state.visibilityHandler = null; }
    if (state.resizeHandler) { window.removeEventListener('resize', state.resizeHandler); state.resizeHandler = null; }

    container.classList.remove('is-visible');
    document.body.classList.remove('has-face-bg');
    setTimeout(() => {
      container.remove();
      state.canvas = null;
      state.ctx = null;
      state.glowCanvas = null;
      state.glowCtx = null;
      state.mountMode = 'none';
    }, 650);
  },

  /**
   * Embed mode — creates a <tf-face> element inside the given container.
   * Returns a handle compatible with the old embed() API.
   */
  embed(container) {
    if (!container || !(container instanceof HTMLElement)) {
      throw new Error('FaceBackground.embed: container must be HTMLElement');
    }
    // Destroy previous embed if different container
    if (activeEmbedEl && activeEmbedEl.parentNode) {
      if (activeEmbedEl.parentNode === container) {
        return activeEmbedEl._handle;
      }
      activeEmbedEl.remove();
      activeEmbedEl = null;
    }
    if (state.mountMode === 'fullscreen') {
      FaceBackground.hide();
    }

    const face = document.createElement('tf-face');
    face.setAttribute('mode', 'idle');
    const w = container.clientWidth || 360;
    const h = container.clientHeight || 360;
    face.setAttribute('size', String(Math.min(w, h)));
    face.style.width = '100%';
    face.style.height = '100%';
    face.style.display = 'block';

    container.classList.add('face-embed-host');
    container.appendChild(face);
    activeEmbedEl = face;

    // ResizeObserver keeps the size attribute in sync with host
    let resizeObs = null;
    if (typeof ResizeObserver !== 'undefined') {
      resizeObs = new ResizeObserver(() => {
        const cw = container.clientWidth || 360;
        const ch = container.clientHeight || 360;
        face.setAttribute('size', String(Math.min(cw, ch)));
      });
      resizeObs.observe(container);
    }

    const handle = {
      setMode(mode) {
        if (mode !== 'idle' && mode !== 'listen' && mode !== 'think' && mode !== 'speak') {
          console.warn('[faceBg] setMode: unknown mode', mode);
          return;
        }
        face.setAttribute('mode', mode);
        container.style.setProperty('--ui-mode', mode);
        container.dataset.uiMode = mode;
      },
      setSpeechAmplitude(rms) {
        face.setSpeechAmplitude(rms);
      },
      setListenAmplitude(rms) {
        const v = Number(rms);
        if (!Number.isFinite(v)) return;
        const clamped = Math.max(0, Math.min(1, v));
        container.style.setProperty('--listen-amp', clamped.toFixed(3));
      },
      destroy() {
        if (resizeObs) { resizeObs.disconnect(); resizeObs = null; }
        if (face.parentNode === container) container.removeChild(face);
        container.classList.remove('face-embed-host');
        container.style.removeProperty('--ui-mode');
        container.style.removeProperty('--listen-amp');
        delete container.dataset.uiMode;
        if (activeEmbedEl === face) activeEmbedEl = null;
      },
    };

    face._handle = handle;
    return handle;
  },
};

export default FaceBackground;
