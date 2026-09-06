// =============================================================================
// File: js/components/tf-face.js
// Description: Multi-instance web component rendering the Head_5 wireframe face.
//              Each <tf-face> owns its canvas pair, blendshape state, and RAF loop.
//              Mini variant (size <= 32) renders a CSS-only gradient circle.
//              `fullscreen` attribute fills the host and adapts scale to the
//              viewport; `track="pointer gyro"` opts in to head tracking.
// Example: <tf-face mode="idle" size="360"></tf-face>
//          <tf-face fullscreen track="pointer gyro"></tf-face>
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
const MOBILE_SCALE_MUL = 0.44;
const MINI_THRESHOLD = 32;
const DEFAULT_SIZE = 200;

const GYRO_GAMMA_RANGE = 45;
const GYRO_BETA_RANGE = 30;
const GYRO_YAW_GAIN = 0.3;
const GYRO_PITCH_GAIN = 0.22;

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

// A shorter camera distance increases perspective between the nose and cheeks.
const VIEW_CAMERA_DISTANCE = 4.0;
// Apply the same horizontal scale to the mesh and blendshapes so animated
// features stay aligned with the narrowed face.
const FACE_WIDTH_SCALE = 0.849;
const NOSE_WIDTH_SCALE = 0.88;
const NOSE_CENTER_X = -0.055;

// Capture-space bounds isolate the nose; smooth edges avoid creases where it
// joins the cheeks. Fixed weights keep the same transform during mimicry.
const NOSE_SCALES = (() => {
  const smooth = (low, high, value) => {
    const t = Math.max(0, Math.min(1, (value - low) / (high - low)));
    return t * t * (3 - 2 * t);
  };
  return Float32Array.from({ length: NUM_VERTICES }, (_, i) => {
    const j = i * 3;
    const x = BASE_POSITIONS[j] - NOSE_CENTER_X;
    const y = BASE_POSITIONS[j + 1];
    const z = BASE_POSITIONS[j + 2];
    const weight = (1 - smooth(0.28, 0.48, Math.abs(x)))
      * smooth(-0.42, -0.18, y)
      * (1 - smooth(0.20, 0.34, y))
      * smooth(0.42, 0.58, z);
    return 1 - (1 - NOSE_WIDTH_SCALE) * weight;
  });
})();

// Base mesh with the uniform width squeeze applied. Every renderer path uses
// this; raw BASE_POSITIONS only feed the capture-space rigid fit.
const RENDER_POSITIONS = (() => {
  const out = new Float32Array(BASE_POSITIONS.length);
  for (let i = 0; i < NUM_VERTICES; i++) {
    const j = i * 3;
    out[j] = (NOSE_CENTER_X + (BASE_POSITIONS[j] - NOSE_CENTER_X)
      * NOSE_SCALES[i]) * FACE_WIDTH_SCALE;
    out[j + 1] = BASE_POSITIONS[j + 1];
    out[j + 2] = BASE_POSITIONS[j + 2];
  }
  return out;
})();

// Shading normals derived from the render geometry so edge visibility fades
// match what is actually drawn.
const BASE_NORMALS = (() => {
  const nx = new Float32Array(NUM_VERTICES);
  const ny = new Float32Array(NUM_VERTICES);
  const nz = new Float32Array(NUM_VERTICES);
  let cx = 0, cy = 0, cz = 0;
  for (let i = 0; i < NUM_VERTICES; i++) {
    const j = i * 3;
    cx += RENDER_POSITIONS[j];
    cy += RENDER_POSITIONS[j + 1];
    cz += RENDER_POSITIONS[j + 2];
  }
  cx /= NUM_VERTICES;
  cy /= NUM_VERTICES;
  cz /= NUM_VERTICES;
  for (let i = 0; i < NUM_VERTICES; i++) {
    const j = i * 3;
    const dx = RENDER_POSITIONS[j] - cx;
    const dy = RENDER_POSITIONS[j + 1] - cy;
    const dz = RENDER_POSITIONS[j + 2] - cz;
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

// Removes the rigid head motion (rotation + translation) baked into one raw
// blendshape delta. The capture frames behind face-data.js had different head
// poses, so every raw delta carries a whole-head transform — animating any
// weight (blink, speech visemes) rocks the entire skull. A trimmed Kabsch fit
// (orthogonal Procrustes with IRLS halving: each round keeps the half of the
// vertex set with the smallest rigid-fit residual, which converges on the
// skull) finds that rigid transform; subtracting it leaves only the local
// deformation. Pure function, runs once per blendshape at import. Returns a
// new Float32Array (same layout as the input delta) or null when the
// iteration goes non-finite.
export function removeRigidMotion(basePositions, delta, numVertices) {
  const finite = (m) => m.every(Number.isFinite);
  // c = a * b for row-major 3x3 matrices
  const matMul = (a, b) => {
    const c = new Array(9);
    for (let r = 0; r < 3; r++) {
      for (let k = 0; k < 3; k++) {
        c[r * 3 + k] =
          a[r * 3] * b[k] + a[r * 3 + 1] * b[3 + k] + a[r * 3 + 2] * b[6 + k];
      }
    }
    return c;
  };
  // c = a^T * b
  const matTMul = (a, b) => {
    const c = new Array(9);
    for (let r = 0; r < 3; r++) {
      for (let k = 0; k < 3; k++) {
        c[r * 3 + k] = a[r] * b[k] + a[3 + r] * b[3 + k] + a[6 + r] * b[6 + k];
      }
    }
    return c;
  };
  const det3 = (m) =>
    m[0] * (m[4] * m[8] - m[5] * m[7]) -
    m[1] * (m[3] * m[8] - m[5] * m[6]) +
    m[2] * (m[3] * m[7] - m[4] * m[6]);

  // Best rigid (R, t) mapping P -> Q = P + delta over the given index set.
  const fitRigid = (indices) => {
    const n = indices.length;
    let pcx = 0, pcy = 0, pcz = 0, qcx = 0, qcy = 0, qcz = 0;
    for (const i of indices) {
      const j = i * 3;
      pcx += basePositions[j];
      pcy += basePositions[j + 1];
      pcz += basePositions[j + 2];
      qcx += delta[j];
      qcy += delta[j + 1];
      qcz += delta[j + 2];
    }
    pcx /= n; pcy /= n; pcz /= n;
    qcx = qcx / n + pcx; qcy = qcy / n + pcy; qcz = qcz / n + pcz;

    // Cross-covariance M = sum (q - qc)(p - pc)^T; the Kabsch rotation is
    // the orthogonal polar factor of M.
    const M = [0, 0, 0, 0, 0, 0, 0, 0, 0];
    for (const i of indices) {
      const j = i * 3;
      const px = basePositions[j] - pcx;
      const py = basePositions[j + 1] - pcy;
      const pz = basePositions[j + 2] - pcz;
      const qx = basePositions[j] + delta[j] - qcx;
      const qy = basePositions[j + 1] + delta[j + 1] - qcy;
      const qz = basePositions[j + 2] + delta[j + 2] - qcz;
      M[0] += qx * px; M[1] += qx * py; M[2] += qx * pz;
      M[3] += qy * px; M[4] += qy * py; M[5] += qy * pz;
      M[6] += qz * px; M[7] += qz * py; M[8] += qz * pz;
    }
    let frob = 0;
    for (const v of M) frob += v * v;
    frob = Math.sqrt(frob);
    let R;
    if (!(frob > 1e-12)) {
      // Degenerate covariance (e.g. near-zero delta): rigid part is pure
      // translation between centroids.
      R = [1, 0, 0, 0, 1, 0, 0, 0, 1];
    } else {
      // Newton-Schulz polar iteration on the Frobenius-normalized M; its
      // singular values are then <= 1, inside the convergence region.
      let X = M.map((v) => v / frob);
      for (let it = 0; it < 30; it++) {
        const T = matTMul(X, X);
        const E = [
          3 - T[0], -T[1], -T[2],
          -T[3], 3 - T[4], -T[5],
          -T[6], -T[7], 3 - T[8],
        ];
        X = matMul(X, E).map((v) => v * 0.5);
      }
      if (!finite(X)) return null;
      R = X;
      if (det3(R) <= 0) {
        // Polar factor is a reflection; flip the smallest singular direction:
        // R <- R (I - 2 v v^T), v = eigenvector of M^T M with the smallest
        // eigenvalue, found by power iteration on (trace(A) I - A).
        const A = matTMul(M, M);
        const s = A[0] + A[4] + A[8];
        const B = [
          s - A[0], -A[1], -A[2],
          -A[3], s - A[4], -A[5],
          -A[6], -A[7], s - A[8],
        ];
        let v = [1, 0.5, 0.25];
        for (let it = 0; it < 60; it++) {
          const w = [
            B[0] * v[0] + B[1] * v[1] + B[2] * v[2],
            B[3] * v[0] + B[4] * v[1] + B[5] * v[2],
            B[6] * v[0] + B[7] * v[1] + B[8] * v[2],
          ];
          const len = Math.sqrt(w[0] * w[0] + w[1] * w[1] + w[2] * w[2]);
          if (!(len > 1e-12)) break;
          v = [w[0] / len, w[1] / len, w[2] / len];
        }
        const F = [
          1 - 2 * v[0] * v[0], -2 * v[0] * v[1], -2 * v[0] * v[2],
          -2 * v[1] * v[0], 1 - 2 * v[1] * v[1], -2 * v[1] * v[2],
          -2 * v[2] * v[0], -2 * v[2] * v[1], 1 - 2 * v[2] * v[2],
        ];
        R = matMul(R, F);
      }
    }
    const t = [
      qcx - (R[0] * pcx + R[1] * pcy + R[2] * pcz),
      qcy - (R[3] * pcx + R[4] * pcy + R[5] * pcz),
      qcz - (R[6] * pcx + R[7] * pcy + R[8] * pcz),
    ];
    if (!finite(R) || !finite(t)) return null;
    return { R, t };
  };

  const residual = (R, t, i) => {
    const j = i * 3;
    const px = basePositions[j];
    const py = basePositions[j + 1];
    const pz = basePositions[j + 2];
    const rx = px + delta[j] - (R[0] * px + R[1] * py + R[2] * pz + t[0]);
    const ry = py + delta[j + 1] - (R[3] * px + R[4] * py + R[5] * pz + t[1]);
    const rz = pz + delta[j + 2] - (R[6] * px + R[7] * py + R[8] * pz + t[2]);
    return Math.sqrt(rx * rx + ry * ry + rz * rz);
  };

  let indices = Array.from({ length: numVertices }, (_, i) => i);
  let fit = null;
  for (let round = 0; round < 3; round++) {
    fit = fitRigid(indices);
    if (!fit) return null;
    if (round < 2) {
      const scored = indices.map((i) => ({ i, r: residual(fit.R, fit.t, i) }));
      scored.sort((a, b) => a.r - b.r);
      indices = scored
        .slice(0, Math.max(3, Math.ceil(scored.length / 2)))
        .map((s) => s.i);
    }
  }

  const { R, t } = fit;
  const out = new Float32Array(numVertices * 3);
  for (let i = 0; i < numVertices; i++) {
    const j = i * 3;
    const px = basePositions[j];
    const py = basePositions[j + 1];
    const pz = basePositions[j + 2];
    out[j] = px + delta[j] - (R[0] * px + R[1] * py + R[2] * pz + t[0]);
    out[j + 1] = py + delta[j + 1] - (R[3] * px + R[4] * py + R[5] * pz + t[1]);
    out[j + 2] = pz + delta[j + 2] - (R[6] * px + R[7] * py + R[8] * pz + t[2]);
  }
  return out;
}

// Rigid-motion-free blendshape deltas; the renderer must only ever use these
// (never raw BLENDSHAPE_DELTAS) so animating a weight deforms the face
// locally instead of rocking the whole head. The rigid fit runs in capture
// space (against raw BASE_POSITIONS). Each vertex's horizontal delta uses the
// same fixed scale as its base position to keep animated features aligned.
const CLEAN_DELTAS = (() => {
  const scaleDelta = (delta) => {
    const out = Float32Array.from(delta);
    for (let j = 0; j < out.length; j += 3) {
      out[j] *= FACE_WIDTH_SCALE * NOSE_SCALES[j / 3];
    }
    return out;
  };
  return BLENDSHAPE_DELTAS.map((delta, s) => {
    const cleaned = removeRigidMotion(BASE_POSITIONS, delta, NUM_VERTICES);
    if (cleaned) return scaleDelta(cleaned);
    console.warn(`tf-face: rigid-motion removal failed for blendshape ${s}, keeping raw delta`);
    return scaleDelta(delta);
  });
})();

// Viewer-left eye vertices, derived from the blink blendshape: the vertices
// the blink moves the most inside LEFT_MASK. Used as the zoom anchor in
// transitionOut(). Uses CLEAN_DELTAS so the magnitude ranking reflects the
// actual eyelid deformation, not the rigid head motion baked into raw data.
const VIEWER_LEFT_EYE_INDICES = (() => {
  const blinkIdx = BS_INDEX.blink;
  if (blinkIdx == null || blinkIdx < 0) return [];
  const deltas = CLEAN_DELTAS[blinkIdx];
  if (!deltas) return [];
  const MASK_THRESHOLD = 0.5;
  const candidates = [];
  for (let i = 0; i < NUM_VERTICES; i++) {
    if (LEFT_MASK[i] < MASK_THRESHOLD) continue;
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
})();

function easeInOut(t) {
  return t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) * 0.5;
}

function isMobileViewport() {
  if (typeof window === 'undefined') return false;
  const mql = window.matchMedia ? window.matchMedia('(pointer: coarse)') : null;
  if (mql && mql.matches) return true;
  return window.innerWidth < 768;
}

class TfFace extends HTMLElement {
  static get observedAttributes() {
    return ['mode', 'size', 'fullscreen', 'track'];
  }

  constructor() {
    super();
    this._raf = 0;
    this._transitionRaf = 0;
    this._canvas = null;
    this._ctx = null;
    this._glowCanvas = null;
    this._glowCtx = null;
    this._container = null;
    this._mini = false;
    this._fullscreen = false;
    this._scaleMul = DESKTOP_SCALE_MUL;
    this._resizeObs = null;
    this._mouseHandler = null;
    this._visHandler = null;
    this._orientationHandler = null;
    this._orientationSetupHandler = null;
    this._orientationSetupAttempted = false;
    this._betaBaseline = null;
    this._gammaBaseline = null;

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
      nextBrowAsymAt: 0,
      nextSmileAt: 0,
      shakeT0: null,
      shakeDuration: 0.8,
      uiMode: 'idle',
      speechAmp: 0,
      speechUpdatedAt: 0,
      speechEnvelope: 0,
      speechRound: 0,
      speechWide: 0,
      speechFricative: 0,
      speechEmphasis: 0,
      nextEmphasisAt: 0,
      modeWeights: { idle: 1, listen: 0, think: 0, speak: 0 },
      reducedMotion: false,
      transitioning: false,
      zoomCx: null,
      zoomCy: null,
      yawOverride: null,
      pitchOverride: null,
    };

    this._mimicryTarget = { ...this._state.mimicry };
    this._mimicryVelocity = { ...this._state.mimicry };

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
    if (name === 'fullscreen' || name === 'track') {
      if (this._container) {
        this._teardown();
        this._build();
      }
      return;
    }
    if (name === 'size') {
      if (this._fullscreen) return;
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
    }
  }

  setSpeechAmplitude(rms, { round = 0, wide = 0, fricative = 0 } = {}) {
    const v = Number(rms);
    if (!Number.isFinite(v)) return;
    this._state.speechAmp = Math.max(0, Math.min(1, v));
    this._state.speechUpdatedAt = performance.now();
    this._state.speechRound = Number.isFinite(round) ? Math.max(0, Math.min(1, round)) : 0;
    this._state.speechWide = Number.isFinite(wide) ? Math.max(0, Math.min(1, wide)) : 0;
    this._state.speechFricative = Number.isFinite(fricative) ? Math.max(0, Math.min(1, fricative)) : 0;
  }

  // Meeting video owns its clock and opaque capture canvas. Keep the same
  // renderer without attaching a second animation loop to the Teams document.
  renderToCanvas(canvas, nowMs) {
    if (!this._canvas) {
      this._canvas = document.createElement('canvas');
      this._ctx = this._canvas.getContext('2d', { alpha: true });
      this._glowCanvas = document.createElement('canvas');
      this._glowCtx = this._glowCanvas.getContext('2d', { alpha: true });
      this._state.dpr = 1;
      this._resetIdleSchedule();
    }
    if (this._canvas.width !== canvas.width || this._canvas.height !== canvas.height) {
      this._canvas.width = canvas.width;
      this._canvas.height = canvas.height;
    }
    this._renderFrame(nowMs, false);
    const ctx = canvas.getContext('2d');
    ctx.save();
    ctx.globalCompositeOperation = 'lighter';
    ctx.globalAlpha = 0.85;
    ctx.filter = 'blur(8px)';
    ctx.drawImage(this._glowCanvas, 0, 0);
    ctx.globalCompositeOperation = 'source-over';
    ctx.globalAlpha = 1;
    ctx.filter = 'none';
    ctx.drawImage(this._canvas, 0, 0);
    ctx.restore();
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

  // Cinematic exit: zoom into the viewer-left eye, then fade the face out.
  // opts.onMidpoint fires synchronously at start (caller mounts the UI there),
  // opts.onProgress(uiScale) fires each animation frame with the 0..1 UI scale,
  // opts.onComplete fires after the zoom + fade finished.
  transitionOut(opts) {
    const onMidpoint = (opts && opts.onMidpoint) || (() => {});
    const onProgress = (opts && opts.onProgress) || (() => {});
    const onComplete = (opts && opts.onComplete) || (() => {});
    const s = this._state;

    if (!this._canvas) { onMidpoint(); onComplete(); return; }

    if (s.reducedMotion) {
      onMidpoint();
      this._glowCanvas.style.transition = 'opacity 0.2s linear';
      this._canvas.style.transition = 'opacity 0.2s linear';
      this._setLayerOpacity('0');
      setTimeout(() => { onComplete(); }, 200);
      return;
    }

    s.transitioning = true;
    s.actions.length = 0;
    s.targetYaw = 0;
    s.targetPitch = 0;
    s.parallaxYaw = 0;
    s.parallaxPitch = 0;
    s.yawOverride = 0;
    s.pitchOverride = PITCH_BASE_OFFSET;

    const eyeIndices = VIEWER_LEFT_EYE_INDICES.length > 0 ? VIEWER_LEFT_EYE_INDICES : [0];
    const scaleStart = this._scaleMul;
    const scaleEnd = isMobileViewport() ? 18 : 13;
    const DURATION = 1600;
    const FADE_START_T = 0.7;
    const startTime = performance.now();

    const easeInOutCubic = (t) =>
      t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;

    try { onMidpoint(); } catch (e) { console.error('[tf-face] onMidpoint error:', e); }

    const tick = (nowMs) => {
      const elapsed = nowMs - startTime;
      const t = Math.min(elapsed / DURATION, 1);
      const et = easeInOutCubic(t);

      this._scaleMul = scaleStart + (scaleEnd - scaleStart) * et;

      const canvas = this._canvas;
      if (!canvas) {
        this._transitionRaf = 0;
        s.transitioning = false;
        onComplete();
        return;
      }
      const wPx = canvas.width;
      const hPx = canvas.height;
      const baseScale = Math.min(wPx, hPx) * this._scaleMul;
      const scalePersp = baseScale * VIEW_CAMERA_DISTANCE;
      const { dx, dy } = this._computeEyeOffset(eyeIndices, 0, PITCH_BASE_OFFSET);
      s.zoomCx = wPx * 0.5 - dx * scalePersp;
      s.zoomCy = hPx * 0.5 - dy * scalePersp;

      try { onProgress(this._scaleMul / scaleEnd); } catch (e) { console.error('[tf-face] onProgress error:', e); }

      if (t > FADE_START_T) {
        const ft = (t - FADE_START_T) / (1 - FADE_START_T);
        this._setLayerOpacity((1 - ft).toFixed(3));
      }

      if (t < 1) {
        this._transitionRaf = requestAnimationFrame(tick);
      } else {
        this._transitionRaf = 0;
        s.transitioning = false;
        s.zoomCx = null;
        s.zoomCy = null;
        s.yawOverride = null;
        s.pitchOverride = null;
        this._scaleMul = scaleStart;
        try { onComplete(); } catch (e) { console.error('[tf-face] onComplete error:', e); }
      }
    };

    this._transitionRaf = requestAnimationFrame(tick);
  }

  _setLayerOpacity(opacity) {
    const value = String(opacity);
    if (this._glowCanvas) this._glowCanvas.style.opacity = value;
    if (this._canvas) this._canvas.style.opacity = value;
  }

  _computeEyeOffset(indices, yaw, pitch) {
    const sinY = Math.sin(yaw);
    const cosY = Math.cos(yaw);
    const sinP = Math.sin(pitch);
    const cosP = Math.cos(pitch);
    const src = this._workVertices;
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
      const depth = VIEW_CAMERA_DISTANCE - z2;
      const invDepth = depth > 0.1 ? 1.0 / depth : 1.0 / 0.1;
      sumX += x1 * invDepth;
      sumY += y1 * invDepth;
      count++;
    }
    if (count === 0) return { dx: 0, dy: 0 };
    return { dx: sumX / count, dy: sumY / count };
  }

  _parsedSize() {
    const raw = this.getAttribute('size');
    if (raw === null || raw === '') return DEFAULT_SIZE;
    const n = parseInt(raw, 10);
    return Number.isFinite(n) && n > 0 ? n : DEFAULT_SIZE;
  }

  _build() {
    this._fullscreen = this.hasAttribute('fullscreen');
    const size = this._parsedSize();
    this._mini = !this._fullscreen && size <= MINI_THRESHOLD;
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
    wrap.dataset.mode = this._state.uiMode;
    if (this._fullscreen) {
      wrap.className = 'tf-face-fullscreen';
      wrap.style.width = '100%';
      wrap.style.height = '100%';
    } else {
      wrap.className = 'tf-face-glow';
      const s = this._parsedSize();
      wrap.style.width = `${s}px`;
      wrap.style.height = `${s}px`;
      wrap.style.background = 'radial-gradient(circle at 50% 45%, #0e1234 0%, #050818 70%)';
    }

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

    const track = (this.getAttribute('track') || '').split(/\s+/);
    if (!this._state.reducedMotion) {
      if (track.includes('pointer')) {
        this._mouseHandler = (e) => this._handleMouseMove(e);
        window.addEventListener('mousemove', this._mouseHandler);
      }
      if (track.includes('gyro')) {
        this._setupDeviceOrientation();
      }
    }
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
    if (this._transitionRaf !== 0) {
      cancelAnimationFrame(this._transitionRaf);
      this._transitionRaf = 0;
    }
    if (this._mouseHandler) {
      window.removeEventListener('mousemove', this._mouseHandler);
      this._mouseHandler = null;
    }
    if (this._orientationHandler) {
      window.removeEventListener('deviceorientation', this._orientationHandler);
      this._orientationHandler = null;
    }
    if (this._orientationSetupHandler) {
      window.removeEventListener('touchstart', this._orientationSetupHandler);
      window.removeEventListener('click', this._orientationSetupHandler);
      this._orientationSetupHandler = null;
    }
    this._orientationSetupAttempted = false;
    this._betaBaseline = null;
    this._gammaBaseline = null;
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
    s.nextBrowAsymAt = 3.0 + Math.random() * 5.0;
    s.targetYaw = 0;
    s.targetPitch = 0;
    s.parallaxYaw = 0;
    s.parallaxPitch = 0;
    s.shakeT0 = null;
    s.shakeDuration = 0.8;
    s.speechAmp = 0;
    s.speechEnvelope = 0;
    s.speechEmphasis = 0;
    s.nextEmphasisAt = 0;
    for (const key of Object.keys(s.modeWeights)) s.modeWeights[key] = key === s.uiMode ? 1 : 0;
    for (const key of Object.keys(s.mimicry)) {
      s.mimicry[key] = 0;
      this._mimicryTarget[key] = 0;
      this._mimicryVelocity[key] = 0;
    }
    s.transitioning = false;
    s.zoomCx = null;
    s.zoomCy = null;
    s.yawOverride = null;
    s.pitchOverride = null;
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
    if (this._fullscreen && !this._state.transitioning) {
      this._scaleMul = isMobileViewport() ? MOBILE_SCALE_MUL : DESKTOP_SCALE_MUL;
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

  _handleDeviceOrientation(e) {
    const gammaRaw = e.gamma;
    const betaRaw = e.beta;
    if (gammaRaw == null || betaRaw == null) return;
    if (this._betaBaseline === null) this._betaBaseline = betaRaw;
    if (this._gammaBaseline === null) this._gammaBaseline = gammaRaw;
    const gDelta = gammaRaw - this._gammaBaseline;
    const bDelta = betaRaw - this._betaBaseline;
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
    this._state.targetYaw = -(yawRaw / GYRO_GAMMA_RANGE) * GYRO_YAW_GAIN;
    this._state.targetPitch = (pitchRaw / GYRO_BETA_RANGE) * GYRO_PITCH_GAIN;
  }

  _attachDeviceOrientationListener() {
    if (typeof window === 'undefined') return;
    if (typeof DeviceOrientationEvent === 'undefined') return;
    if (this._orientationHandler) return;
    this._orientationHandler = (e) => this._handleDeviceOrientation(e);
    window.addEventListener('deviceorientation', this._orientationHandler);
  }

  _setupOrientationAfterGesture() {
    if (this._orientationSetupAttempted) return;
    this._orientationSetupAttempted = true;
    if (typeof DeviceOrientationEvent === 'undefined') return;
    const requestPermission = DeviceOrientationEvent.requestPermission;
    if (typeof requestPermission === 'function') {
      requestPermission.call(DeviceOrientationEvent).then((result) => {
        if (result === 'granted') this._attachDeviceOrientationListener();
      }).catch(() => {});
    } else {
      this._attachDeviceOrientationListener();
    }
  }

  // iOS exposes gyro data only after requestPermission() from a user gesture,
  // so the listener is armed lazily on the first touch/click.
  _setupDeviceOrientation() {
    if (typeof window === 'undefined') return;
    if (typeof DeviceOrientationEvent === 'undefined') return;
    const handler = () => {
      window.removeEventListener('touchstart', handler);
      window.removeEventListener('click', handler);
      this._orientationSetupHandler = null;
      this._setupOrientationAfterGesture();
    };
    this._orientationSetupHandler = handler;
    window.addEventListener('touchstart', handler, { passive: true });
    window.addEventListener('click', handler);
  }

  _applyBlendshapes(m) {
    const dst = this._workVertices;
    dst.set(RENDER_POSITIONS);
    const THRESHOLD = 1e-4;

    const apply = (bsIdx, weight, maskL, maskR) => {
      if (bsIdx < 0 || Math.abs(weight) <= THRESHOLD) return;
      const deltas = CLEAN_DELTAS[bsIdx];
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
    const scalePersp = scale * VIEW_CAMERA_DISTANCE;
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
      const depth = VIEW_CAMERA_DISTANCE - z2;
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

    const w = s.modeWeights;
    return {
      r: Math.round(255 * w.idle + 34 * w.listen + 245 * w.think + 167 * w.speak),
      g: Math.round(255 * w.idle + 197 * w.listen + 158 * w.think + 139 * w.speak),
      b: Math.round(255 * w.idle + 94 * w.listen + 11 * w.think + 250 * w.speak),
    };
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
      // Counter the depth highlight locally so the nose matches the cheeks;
      // this also attenuates its glow, which shares the same draw buckets.
      const noseWeight = (2 - NOSE_SCALES[a] - NOSE_SCALES[b])
        / (2 * (1 - NOSE_WIDTH_SCALE));
      alpha *= 1 - noseWeight * 0.52;
      // Preserve the curved underside geometry while reducing the accumulated
      // brightness of its tightly packed interior edges.
      if (!isContour) {
        const baseY = (BASE_POSITIONS[a * 3 + 1] + BASE_POSITIONS[b * 3 + 1]) * 0.5;
        const underside = Math.max(0, Math.min(1, (baseY - 0.06) / 0.11));
        alpha *= 1 - noseWeight * underside * underside * (3 - 2 * underside) * 0.72;
      }
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
    // Scale stroke width with rendered face size (CSS px): faces >= 560px keep
    // full thickness, smaller embedded faces thin down with a 0.5 floor.
    const sizeK = Math.min(1, Math.max(0.5, (Math.min(mainW, mainH) / dpr) / 560));
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
      gc.lineWidth = dpr * (1.4 + dT * 0.5) * sizeK;
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
      ctx.lineWidth = dpr * (1.4 + dT * 0.5) * sizeK;
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
      if (local < 0) continue;
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

  _applySpeakMode(m, dt, nowMs) {
    const s = this._state;
    const fresh = nowMs - s.speechUpdatedAt < 180;
    const level = fresh && s.uiMode === 'speak' ? s.speechAmp : 0;
    const tau = level > s.speechEnvelope ? 0.025 : 0.065;
    s.speechEnvelope += (level - s.speechEnvelope) * (1 - Math.exp(-dt / tau));
    const energy = s.speechEnvelope * s.modeWeights.speak;
    // Audio features select a continuous blend, never a clock-driven vowel loop.
    // These are acoustic cues, not phoneme recognition.
    const round = s.speechRound;
    const wide = s.speechWide * (1 - round);
    const fricative = s.speechFricative;
    m.mouth_open += energy * 0.28 * (1 - fricative * 0.65);
    m.vis_aa = energy * 0.42 * (1 - round) * (1 - wide) * (1 - fricative);
    m.vis_oo = energy * round * 0.5 * (1 - fricative);
    m.vis_ee = energy * wide * 0.45 * (1 - fricative);
    m.vis_ff = energy * fricative * 0.3;
    m.smile *= 1 - energy * 0.7;

    s.speechEmphasis += (energy - s.speechEmphasis) * (1 - Math.exp(-dt / 0.3));
    if (energy - s.speechEmphasis > 0.16 && s.phase >= s.nextEmphasisAt) {
      this._scheduleAction(s.phase, {
        bsKey: 'eyebrow', side: 'both', peakValue: 0.06 + energy * 0.06,
        attack: 0.16, hold: 0.08, release: 0.55,
      });
      s.nextEmphasisAt = s.phase + 1.8 + Math.random() * 2;
    }
  }

  _tickIdle(dt, nowMs) {
    const s = this._state;
    const m = this._mimicryTarget;
    const t = s.phase;
    for (const key of Object.keys(m)) m[key] = 0;
    for (const key of Object.keys(s.modeWeights)) {
      const target = key === s.uiMode ? 1 : 0;
      s.modeWeights[key] += (target - s.modeWeights[key]) * (1 - Math.exp(-dt / 0.22));
    }

    if (s.blinkState === null && t >= s.nextBlinkAt) {
      s.blinkState = { t0: t, close: 0.055 + Math.random() * 0.025,
        hold: 0.015 + Math.random() * 0.02, open: 0.12 + Math.random() * 0.05 };
    }
    if (s.blinkState) {
      const blink = s.blinkState;
      const local = t - blink.t0;
      const duration = blink.close + blink.hold + blink.open;
      const value = (time) => {
        if (time < 0 || time >= duration) return 0;
        if (time < blink.close) return easeInOut(time / blink.close);
        if (time < blink.close + blink.hold) return 1;
        return 1 - easeInOut((time - blink.close - blink.hold) / blink.open);
      };
      m.blink_left = value(local);
      m.blink_right = value(local - 0.008);
      if (local >= duration + 0.008) {
        s.blinkState = null;
        s.nextBlinkAt = t + 2.2 + Math.random() * 4.2;
      }
    }

    if (!s.transitioning) {
      if (t >= s.nextSmileAt) {
        this._scheduleAction(t, { bsKey: 'smile', peakValue: 0.04 + Math.random() * 0.05,
          attack: 1.1, hold: 1.2, release: 1.6 });
        s.nextSmileAt = t + 10 + Math.random() * 12;
      }
      if (t >= s.nextBrowAsymAt) {
        this._scheduleAction(t, { bsKey: 'eyebrow', side: Math.random() < 0.6 ? 'both' : 'left',
          peakValue: 0.04 + Math.random() * 0.05, attack: 0.7, hold: 0.5, release: 1.1 });
        s.nextBrowAsymAt = t + 7 + Math.random() * 10;
      }
      m.smile = 0.015 + s.modeWeights.listen * 0.025;
      m.eyebrow_left = s.modeWeights.listen * 0.055 + s.modeWeights.think * 0.025;
      m.eyebrow_right = s.modeWeights.listen * 0.045;
      this._applySpeakMode(m, dt, nowMs);
    }
    this._evalActions(t, m);

    // Exact critically damped step: retain velocity across target and mode
    // changes, with independent response times for eyelids, lips and brows.
    for (const key of Object.keys(m)) {
      const target = Math.max(0, Math.min(1, m[key]));
      const omega = key.startsWith('blink') ? 140
        : key.startsWith('vis_') || key === 'mouth_open' ? 65 : 12;
      const offset = s.mimicry[key] - target;
      const velocity = this._mimicryVelocity[key];
      const c = velocity + omega * offset;
      const decay = Math.exp(-omega * dt);
      s.mimicry[key] = target + (offset + c * dt) * decay;
      this._mimicryVelocity[key] = (velocity - omega * c * dt) * decay;
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
    const baseScale = Math.min(w, h) * this._scaleMul;
    this._project(cx, cy, baseScale, 0, PITCH_BASE_OFFSET);
    this._drawEdges(this._ctx, s.dpr);
  }

  _renderFrame(nowMs, automatic = true) {
    if ((automatic && document.hidden) || !this._canvas) { this._raf = 0; return; }
    const s = this._state;
    const dt = s.lastFrameMs > 0 ? Math.max(0, Math.min(0.05, (nowMs - s.lastFrameMs) / 1000)) : 1 / 60;
    s.lastFrameMs = nowMs;
    s.phase += dt;

    this._tickIdle(dt, nowMs);

    const alpha = 1 - Math.exp(-dt / 0.28);
    s.parallaxYaw += (s.targetYaw - s.parallaxYaw) * alpha;
    s.parallaxPitch += (s.targetPitch - s.parallaxPitch) * alpha;

    this._applyBlendshapes(s.mimicry);

    const ctx = this._ctx;
    const w = this._canvas.width;
    const h = this._canvas.height;
    ctx.clearRect(0, 0, w, h);

    const cx = s.zoomCx !== null ? s.zoomCx : w * 0.5;
    const cy = s.zoomCy !== null ? s.zoomCy : h * 0.56;
    const baseScale = Math.min(w, h) * this._scaleMul;

    let yaw, pitch;
    if (s.shakeT0 !== null && (s.phase - s.shakeT0) >= s.shakeDuration) {
      s.shakeT0 = null;
    }
    if (s.yawOverride !== null) {
      yaw = s.yawOverride;
      pitch = s.pitchOverride;
    } else if (s.shakeT0 !== null) {
      const tShake = s.phase - s.shakeT0;
      const damp = 1 - tShake / s.shakeDuration;
      yaw = damp * Math.sin((tShake / s.shakeDuration) * Math.PI * 6) * 0.35;
      pitch = PITCH_BASE_OFFSET;
    } else {
      const yawBase = Math.sin(s.phase * 0.43) * 0.018 + Math.sin(s.phase * 0.17) * 0.012;
      const pitchBase = PITCH_BASE_OFFSET + Math.sin(s.phase * 1.25) * 0.004
        + Math.sin(s.phase * 0.31) * 0.009 + s.speechEmphasis * 0.022;
      yaw = yawBase + s.parallaxYaw;
      pitch = pitchBase + s.parallaxPitch;
    }

    this._project(cx, cy, baseScale, yaw, pitch);
    this._drawEdges(ctx, s.dpr);

    if (automatic && !s.reducedMotion && !document.hidden) {
      this._raf = requestAnimationFrame(this._frame);
    } else {
      this._raf = 0;
    }
  }
}

customElements.define('tf-face', TfFace);

export { TfFace };
