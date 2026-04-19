// =============================================================================
// Plik: js/modules/faceBackground.js
// Opis: Pełnoekranowe tło wireframe twarzy (port Head_5 "piotr.bin" z projektu
//       tentaflow-buddy) rysowane na canvas 2D. Pipeline: blendshape'y →
//       rotacja yaw/pitch → projekcja perspektywiczna → stroke krawędzi.
//       Idle-animacje: mrugnięcia, mikrouśmiechy, oddech, ruchy brwi, ziewanie,
//       mikro-visemes, cheek puff, marszczenie brwi, plus parallax z myszy.
//       Back-face culling wg aproksymowanych normali (kierunek od centroidu).
// Przykład: FaceBackground.show(); ... FaceBackground.hide();
// =============================================================================

import {
  NUM_VERTICES,
  BASE_POSITIONS,
  BLENDSHAPE_DELTAS,
  LEFT_MASK,
  RIGHT_MASK,
  BS_INDEX,
} from '/js/generated/face-data.js';

import {
  FACEMESH_CONTOURS,
  FACEMESH_FILL,
} from '/js/generated/face-edges.js';

const CONTAINER_ID = 'face-bg-root';
const CANVAS_ID = 'face-bg-canvas';

// Domyślne nachylenie głowy w dół (broda niżej). Wartość ujemna, bo po
// korekcie parallax negatywny pitch = spojrzenie w dół. Sumuje się z
// oscylacją pitch_base oraz parallaxem z myszy. ~-5.2° w stopniach.
const PITCH_BASE_OFFSET = -0.09;

// Mnożniki skali bazowej — mobile dostaje większą twarz, bo mały ekran
// wymaga większego wypełnienia viewportu.
const DESKTOP_SCALE_MUL = 0.29;
const MOBILE_SCALE_MUL = 0.44;

// Parametry mapowania DeviceOrientationEvent → parallax. Gamma (left-right)
// clampowana do ±GYRO_GAMMA_RANGE; beta (front-back) względem betaBaseline
// clampowana do ±GYRO_BETA_RANGE. Wzmocnienia (gain) odpowiadają zasięgowi
// podobnemu do parallaxu myszy (±0.25 yaw, ±0.18 pitch).
const GYRO_GAMMA_RANGE = 45;
const GYRO_BETA_RANGE = 30;
const GYRO_YAW_GAIN = 0.3;
const GYRO_PITCH_GAIN = 0.22;

// Mapa kluczy mimicry → indeks w BLENDSHAPE_DELTAS. Wartość -1 = brak.
// BS_INDEX pochodzi z face-data.js i jest źródłem prawdy — odpowiada
// `Head5BsIdx` w Rust (src/board/face/head5_piotr.rs). `usize::MAX` w Rust
// zamienia się na -1 w JS.
const BS = BS_INDEX;

// Łączny zestaw krawędzi: najpierw fill (ciemniejszy), potem kontury
// (jaśniejsze). Każda krawędź: [aIdx, bIdx, isContour]. Zestaw odpowiada
// wariantowi Dense w rendererze Rust (Tab5): 131 konturów + ~970 krawędzi
// tessellation po filtrach holes i stretch.
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

// Parametry glow przez offscreen canvas + ctx.filter='blur()'. Pipeline:
// 1) offscreen: pełny biały stroke wszystkich bucketów,
// 2) main: dwie warstwy blur z compositem 'lighter' (szeroki + wąski glow),
// 3) main: ostry biały rdzeń bez filtra. Parametry wyniesione, żeby tuning
// intensywności nie wymagał edycji pętli renderującej.
const GLOW_OUTER_BLUR_PX = 12;
const GLOW_OUTER_ALPHA = 0.55;
const GLOW_INNER_BLUR_PX = 4;
const GLOW_INNER_ALPHA = 0.85;

// Aproksymowane normale per-vertex: kierunek od centroidu do wierzchołka.
// Dla quasi-wypukłej bryły twarzy to dobra aproksymacja; pozwala tanio
// wykryć ściany tylne po rotacji (kulling wg znaku normalZ).
function buildBaseNormals() {
  const nx = new Float32Array(NUM_VERTICES);
  const ny = new Float32Array(NUM_VERTICES);
  const nz = new Float32Array(NUM_VERTICES);

  let cx = 0;
  let cy = 0;
  let cz = 0;
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
      // Zabezpieczenie przed NaN: wierzchołek w centroidzie → patrzy "w przód".
      nx[i] = 0;
      ny[i] = 0;
      nz[i] = 1;
    }
  }
  return { nx, ny, nz };
}

const BASE_NORMALS = buildBaseNormals();

// Stan runtime modułu.
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
    mouth_open: 0,
    smile: 0,
    frown: 0,
    blink_left: 0,
    blink_right: 0,
    eyebrow_left: 0,
    eyebrow_right: 0,
    cheek_puff: 0,
    angry: 0,
    vis_aa: 0,
    vis_oo: 0,
    vis_ee: 0,
    vis_mm: 0,
    vis_ff: 0,
    vis_ll: 0,
    vis_ss: 0,
    vis_ch: 0,
  },
  targetYaw: 0,
  targetPitch: 0,
  parallaxYaw: 0,
  parallaxPitch: 0,
  blinkState: null,
  nextBlinkAt: 0,
  // Generyczny scheduler akcji idle. Każda akcja: { bsKey, side, peakValue,
  // t0, attack, hold, release }. Wartości sumują się na mimicry przed
  // zastosowaniem blendshape'ów.
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
  scaleMul: DESKTOP_SCALE_MUL,
};

/**
 * Oblicza `workVertices = BASE + Σ weight_i * DELTA_i`. Pomija blendshape'y
 * z wagą poniżej progu. Dla blink/eyebrow używa masek left/right do
 * niezależnego sterowania każdą stroną.
 */
function applyBlendshapes(m) {
  const dst = state.workVertices;
  dst.set(BASE_POSITIONS);

  const WEIGHT_THRESHOLD = 1e-4;
  const apply = (bsIdx, weight, maskLeft, maskRight) => {
    if (bsIdx < 0 || Math.abs(weight) <= WEIGHT_THRESHOLD) return;
    const deltas = BLENDSHAPE_DELTAS[bsIdx];
    if (maskLeft) {
      for (let i = 0; i < NUM_VERTICES; i++) {
        const w = weight * maskLeft[i];
        if (w === 0) continue;
        const j = i * 3;
        dst[j] += deltas[j] * w;
        dst[j + 1] += deltas[j + 1] * w;
        dst[j + 2] += deltas[j + 2] * w;
      }
    } else if (maskRight) {
      for (let i = 0; i < NUM_VERTICES; i++) {
        const w = weight * maskRight[i];
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

/**
 * Projekcja 3D → 2D: rotacja yaw (oś Y) → rotacja pitch (oś X) →
 * perspektywa 1/(1.8 - z'). W tej samej pętli rotuje również normale
 * (bez translacji, bez perspektywy), żeby uniknąć drugiego przejścia po
 * wierzchołkach. Wypełnia `projX/Y/Z` oraz `normalZ`.
 */
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

/**
 * Zapewnia istnienie offscreen canvas (OffscreenCanvas jeśli wspierany,
 * fallback do zwykłego <canvas>) o wymiarach odpowiadających fizycznym
 * pikselom głównego canvas. Jeśli rozmiar się zmienił — realokuje bufor.
 */
function ensureGlowCanvas(w, h) {
  const existing = state.glowCanvas;
  if (existing && existing.width === w && existing.height === h) {
    return;
  }
  if (!existing) {
    if (typeof OffscreenCanvas !== 'undefined') {
      state.glowCanvas = new OffscreenCanvas(w, h);
    } else {
      state.glowCanvas = document.createElement('canvas');
      state.glowCanvas.width = w;
      state.glowCanvas.height = h;
    }
    state.glowCtx = state.glowCanvas.getContext('2d', { alpha: true });
  } else {
    existing.width = w;
    existing.height = h;
  }
}

/**
 * Rysuje krawędzie z batchowaniem po alpha i grubości. Nie odrzuca krawędzi
 * back-facing — zamiast tego tłumi je miękko krzywą smoothstep, żeby tył
 * i boki głowy delikatnie prześwitywały zamiast znikać skokowo. Alpha rośnie
 * z gamma 1.8 od tyłu do przodu, lineWidth też skaluje się z głębokością.
 */
function drawEdges(ctx, dpr) {
  const px = state.projX;
  const py = state.projY;
  const pz = state.projZ;
  const nz = state.normalZ;

  // Klucz bucketu koduje: isContour (1 bit) | lineWidth bucket (4 bity) |
  // alpha bucket (5 bitów). Kubełki 0..19 dla alpha, 0..9 dla grubości.
  const buckets = new Map();

  for (let i = 0; i < EDGES.length; i++) {
    const e = EDGES[i];
    const a = e[0];
    const b = e[1];
    const isContour = e[2];

    const visibility = (nz[a] + nz[b]) * 0.5;
    // Miękkie tłumienie po normali: visibility ∈ [-1, 1] mapowane na
    // t ∈ [0, 1] przesunięte tak, że sam tył (-1) → t=0, sylwetka (0) → t≈0.23,
    // przód (+1) → t=1. Smoothstep (Hermite 3t² - 2t³) łagodzi krzywą.
    // Najciemniejszy visFade = 0.08 (tylne krawędzie jak duch), najjaśniejszy
    // = 1.0 (przód), bez twardego cięcia.
    let t = (visibility + 0.3) / 1.3;
    if (t < 0) t = 0;
    else if (t > 1) t = 1;
    const smooth = t * t * (3 - 2 * t);
    const visFade = 0.08 + smooth * 0.92;

    const avgZ = (pz[a] + pz[b]) * 0.5;
    // Zakres Z po projekcji ≈ [-1, 1]. Shiftujemy tak, żeby dolny region
    // (z ≈ -0.8) dawał niemal 0, a przód (z ≈ +0.5) ≈ 1.
    let depthT = (avgZ + 0.5) * 1.3;
    if (depthT < 0) depthT = 0;
    else if (depthT > 1) depthT = 1;

    // Krzywa głębi z baseline 0.30 i liniową rampą do 0.95. Wyraźny kontrast
    // przód-tył: tył głowy × najciemniejszy visFade ≈ 0.024 (prawie znika),
    // przód × visFade=1 ≈ 0.95 (pełna jasność).
    let alpha = (depthT * 0.65 + 0.3) * visFade;
    if (!isContour) alpha *= 0.5;
    if (alpha < 0.01) continue;

    const alphaBucket = Math.round(alpha * 19);
    const widthBucket = Math.round(depthT * 9);
    const key = (isContour << 9) | (widthBucket << 5) | alphaBucket;

    let arr = buckets.get(key);
    if (!arr) {
      arr = [];
      buckets.set(key, arr);
    }
    arr.push(i);
  }

  // `butt` zamiast `round`, bo każda krawędź to osobny segment moveTo/lineTo —
  // `round` dawał białe kropki na końcówkach w miejscach łączenia trójkątów.

  const mainCanvas = state.canvas;
  ensureGlowCanvas(mainCanvas.width, mainCanvas.height);
  const glowCanvas = state.glowCanvas;
  const glowCtx = state.glowCtx;

  // Pass 1 (offscreen): batche stroke dla warstwy glow. Kolor i alpha identyczne
  // jak core, żeby dwie warstwy blur miały właściwą intensywność wynikową.
  glowCtx.clearRect(0, 0, glowCanvas.width, glowCanvas.height);
  glowCtx.lineCap = 'butt';
  glowCtx.globalCompositeOperation = 'source-over';
  for (const [key, arr] of buckets) {
    const alphaBucket = key & 0x1f;
    const widthBucket = (key >> 5) & 0xf;
    const alpha = alphaBucket / 19;
    const depthT = widthBucket / 9;
    glowCtx.lineWidth = dpr * (0.9 + depthT * 0.4);
    glowCtx.strokeStyle = `rgba(255, 255, 255,${alpha.toFixed(3)})`;
    glowCtx.beginPath();
    for (let i = 0; i < arr.length; i++) {
      const e = EDGES[arr[i]];
      const a = e[0];
      const b = e[1];
      glowCtx.moveTo(px[a], py[a]);
      glowCtx.lineTo(px[b], py[b]);
    }
    glowCtx.stroke();
  }

  // Pass 2 (main): dwie warstwy blur z compositem 'lighter'. Szeroki outer glow
  // daje aureolę, wąski inner glow gęstnieje bliżej linii. 'lighter' sumuje
  // intensywności — dwie warstwy dają płynny gradient poświaty.
  ctx.save();
  ctx.globalCompositeOperation = 'lighter';

  if (typeof ctx.filter !== 'undefined') {
    ctx.filter = `blur(${GLOW_OUTER_BLUR_PX * dpr}px)`;
    ctx.globalAlpha = GLOW_OUTER_ALPHA;
    ctx.drawImage(glowCanvas, 0, 0, glowCanvas.width, glowCanvas.height);

    ctx.filter = `blur(${GLOW_INNER_BLUR_PX * dpr}px)`;
    ctx.globalAlpha = GLOW_INNER_ALPHA;
    ctx.drawImage(glowCanvas, 0, 0, glowCanvas.width, glowCanvas.height);

    ctx.filter = 'none';
  } else {
    // Fallback dla środowisk bez ctx.filter (bardzo starych silników):
    // nakładamy ostrą kopię offscreen z obniżoną alfą jako przybliżenie glow.
    ctx.globalAlpha = GLOW_OUTER_ALPHA;
    ctx.drawImage(glowCanvas, 0, 0, glowCanvas.width, glowCanvas.height);
  }

  ctx.globalAlpha = 1.0;
  ctx.restore();

  // Pass 3 (main): ostry biały rdzeń bez filtra, nad glow. Krawędzie są ostro
  // białe w środku, a wokół nich miękka poświata z dwóch warstw blur.
  ctx.lineCap = 'butt';
  ctx.globalCompositeOperation = 'source-over';
  for (const [key, arr] of buckets) {
    const alphaBucket = key & 0x1f;
    const widthBucket = (key >> 5) & 0xf;
    const alpha = alphaBucket / 19;
    const depthT = widthBucket / 9;
    ctx.lineWidth = dpr * (0.9 + depthT * 0.4);
    ctx.strokeStyle = `rgba(255, 255, 255,${alpha.toFixed(3)})`;
    ctx.beginPath();
    for (let i = 0; i < arr.length; i++) {
      const e = EDGES[arr[i]];
      const a = e[0];
      const b = e[1];
      ctx.moveTo(px[a], py[a]);
      ctx.lineTo(px[b], py[b]);
    }
    ctx.stroke();
  }
}

// ---- Scheduler akcji idle -------------------------------------------------

// Łagodne ease-in-out dla faz attack i release.
function easeInOut(t) {
  return t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) * 0.5;
}

/**
 * Dodaje nową akcję do schedulera. `bsKey` wskazuje klucz w `mimicry`
 * (np. 'smile', 'mouth_open'). `side` dla brwi: 'left' | 'right' | 'both';
 * pominięte dla pojedynczych blendshape'ów. Wartości sumują się.
 */
function scheduleAction(now, opts) {
  state.actions.push({
    bsKey: opts.bsKey,
    side: opts.side || null,
    peakValue: opts.peakValue,
    t0: now,
    attack: opts.attack,
    hold: opts.hold,
    release: opts.release,
  });
}

/**
 * Wylicza aktualną wartość każdej aktywnej akcji i dodaje ją do mimicry;
 * usuwa akcje zakończone. `attack` i `release` używają ease-in-out.
 */
function evalActions(now, m) {
  const actions = state.actions;
  for (let i = actions.length - 1; i >= 0; i--) {
    const a = actions[i];
    const local = now - a.t0;
    const total = a.attack + a.hold + a.release;
    if (local >= total) {
      actions.splice(i, 1);
      continue;
    }
    let v = 0;
    if (local < a.attack) {
      v = easeInOut(local / a.attack) * a.peakValue;
    } else if (local < a.attack + a.hold) {
      v = a.peakValue;
    } else {
      const releaseLocal = (local - a.attack - a.hold) / a.release;
      v = easeInOut(1 - releaseLocal) * a.peakValue;
    }
    if (a.bsKey === 'eyebrow') {
      if (a.side === 'left' || a.side === 'both') m.eyebrow_left += v;
      if (a.side === 'right' || a.side === 'both') m.eyebrow_right += v;
    } else {
      m[a.bsKey] += v;
    }
  }
}

/**
 * Aktualizuje idle-animacje: pulsuje oddech + zarządza mrugnięciami + ticker
 * dla każdej z 7 rodzin akcji (brew zaskoczenia, asymetryczny brew flex,
 * marszczenie brwi, ziewanie, mikro-viseme, cheek puff, pół-uśmieszki).
 * Wartości `mimicry` są zerowane i składane od nowa w każdej klatce.
 */
function tickIdle() {
  const m = state.mimicry;
  const t = state.phase;

  m.mouth_open = 0;
  m.smile = 0;
  m.frown = 0;
  m.eyebrow_left = 0;
  m.eyebrow_right = 0;
  m.cheek_puff = 0;
  m.angry = 0;
  m.vis_aa = 0;
  m.vis_oo = 0;
  m.vis_ee = 0;
  m.vis_mm = 0;
  m.vis_ff = 0;
  m.vis_ll = 0;
  m.vis_ss = 0;
  m.vis_ch = 0;

  // Oddech jako pasywny offset na ustach.
  m.mouth_open = 0.05 + Math.sin(t * 0.8) * 0.02;

  // Mrugnięcia — harmonogram: co 3.5-5.5 s, krzywa wewnątrz blinkState.
  if (state.blinkState === null && t >= state.nextBlinkAt) {
    state.blinkState = { phase: 'in', t0: t, duration: 0.08 };
  }
  if (state.blinkState) {
    const bs = state.blinkState;
    const local = t - bs.t0;
    let value = 0;
    if (bs.phase === 'in') {
      value = Math.min(local / bs.duration, 1);
      if (local >= bs.duration) {
        bs.phase = 'hold';
        bs.t0 = t;
        bs.duration = 0.05;
      }
    } else if (bs.phase === 'hold') {
      value = 1;
      if (local >= bs.duration) {
        bs.phase = 'out';
        bs.t0 = t;
        bs.duration = 0.12;
      }
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

  // Mikro-uśmieszki / grymasy — losowa polaryzacja (obie strony zerowe lub ujemne).
  if (t >= state.nextSmileAt) {
    const polarity = Math.random() < 0.7 ? 1 : -1;
    const peak = polarity > 0 ? 0.15 + Math.random() * 0.15 : -(0.1 + Math.random() * 0.15);
    scheduleAction(t, {
      bsKey: 'smile',
      peakValue: peak,
      attack: 0.3,
      hold: 0.6,
      release: 0.3,
    });
    state.nextSmileAt = t + 11.0 + Math.random() * 6.0;
  }

  // Brew zaskoczenia — oba brwi równo w górę, często z mikro-mouthOpen.
  if (t >= state.nextBrowSurpriseAt) {
    scheduleAction(t, {
      bsKey: 'eyebrow',
      side: 'both',
      peakValue: 0.6,
      attack: 0.2,
      hold: 0.4,
      release: 0.9,
    });
    if (Math.random() < 0.7) {
      scheduleAction(t, {
        bsKey: 'mouth_open',
        peakValue: 0.15,
        attack: 0.2,
        hold: 0.3,
        release: 0.5,
      });
    }
    state.nextBrowSurpriseAt = t + 14.0 + Math.random() * 8.0;
  }

  // Asymetryczny brew flex — jedna brew w górę.
  if (t >= state.nextBrowAsymAt) {
    const side = Math.random() < 0.5 ? 'left' : 'right';
    scheduleAction(t, {
      bsKey: 'eyebrow',
      side,
      peakValue: 0.45,
      attack: 0.25,
      hold: 0.4,
      release: 0.35,
    });
    state.nextBrowAsymAt = t + 9.0 + Math.random() * 7.0;
  }

  // Marszczenie brwi (angry) — długie, łagodne.
  if (t >= state.nextFrownAt) {
    scheduleAction(t, {
      bsKey: 'angry',
      peakValue: 0.4,
      attack: 0.3,
      hold: 0.6,
      release: 0.3,
    });
    state.nextFrownAt = t + 18.0 + Math.random() * 12.0;
  }

  // Ziewnięcie / westchnięcie — szeroko otwarte usta + lekki brew lift.
  if (t >= state.nextYawnAt) {
    scheduleAction(t, {
      bsKey: 'mouth_open',
      peakValue: 0.4,
      attack: 0.5,
      hold: 0.3,
      release: 0.7,
    });
    scheduleAction(t, {
      bsKey: 'eyebrow',
      side: 'both',
      peakValue: 0.2,
      attack: 0.5,
      hold: 0.3,
      release: 0.7,
    });
    state.nextYawnAt = t + 25.0 + Math.random() * 15.0;
  }

  // Mikro-viseme — losowy dźwięk przez ~0.35 s (jakby mruczała pod nosem).
  if (t >= state.nextVisemeAt) {
    const choices = ['vis_aa', 'vis_oo', 'vis_ee', 'vis_mm'];
    const key = choices[Math.floor(Math.random() * choices.length)];
    const peak = 0.3 + Math.random() * 0.2;
    scheduleAction(t, {
      bsKey: key,
      peakValue: peak,
      attack: 0.08,
      hold: 0.19,
      release: 0.08,
    });
    state.nextVisemeAt = t + 5.0 + Math.random() * 4.0;
  }

  // Delikatny cheek puff.
  if (t >= state.nextCheekAt) {
    scheduleAction(t, {
      bsKey: 'cheek_puff',
      peakValue: 0.3,
      attack: 0.2,
      hold: 0.3,
      release: 0.2,
    });
    state.nextCheekAt = t + 20.0 + Math.random() * 15.0;
  }

  evalActions(t, m);
}

/**
 * Rysuje pełną klatkę: update idle + parallax lerp → pipeline vertex →
 * projekcja → stroke krawędzi.
 */
function renderFrame(nowMs) {
  if (document.hidden) {
    state.rafId = 0;
    return;
  }
  const dt = state.lastFrameMs > 0 ? (nowMs - state.lastFrameMs) / 1000 : 1 / 60;
  state.lastFrameMs = nowMs;
  state.phase += dt;

  tickIdle();

  // Parallax lerp — niskoprzepustowe wygładzenie (~300 ms).
  const alpha = 0.06;
  state.parallaxYaw += (state.targetYaw - state.parallaxYaw) * alpha;
  state.parallaxPitch += (state.targetPitch - state.parallaxPitch) * alpha;

  applyBlendshapes(state.mimicry);

  const ctx = state.ctx;
  const canvas = state.canvas;
  const w = canvas.width;
  const h = canvas.height;
  ctx.clearRect(0, 0, w, h);

  const cx = w * 0.5;
  // Środek projekcji przesunięty o 6% wysokości viewportu w dół — twarz
  // siada wizualnie niżej niż geometryczne centrum ekranu.
  const cy = h * 0.56;
  const baseScale = Math.min(w, h) * state.scaleMul;

  const yawBase = Math.sin(state.phase * 0.15) * 0.15;
  const pitchBase = PITCH_BASE_OFFSET + Math.sin(state.phase * 0.1) * 0.08;
  const yaw = yawBase + state.parallaxYaw;
  const pitch = pitchBase + state.parallaxPitch;

  project(cx, cy, baseScale, yaw, pitch);
  drawEdges(ctx, state.dpr);

  if (!state.reducedMotion && !document.hidden) {
    state.rafId = requestAnimationFrame(renderFrame);
  } else {
    state.rafId = 0;
  }
}

/**
 * Renderuje jedną statyczną klatkę z neutralną mimiką (tryb reduced-motion).
 */
function renderStaticFrame() {
  const neutral = state.mimicry;
  neutral.mouth_open = 0;
  neutral.smile = 0;
  neutral.frown = 0;
  neutral.blink_left = 0;
  neutral.blink_right = 0;
  neutral.eyebrow_left = 0;
  neutral.eyebrow_right = 0;
  neutral.cheek_puff = 0;
  neutral.angry = 0;
  neutral.vis_aa = 0;
  neutral.vis_oo = 0;
  neutral.vis_ee = 0;
  neutral.vis_mm = 0;
  neutral.vis_ff = 0;
  neutral.vis_ll = 0;
  neutral.vis_ss = 0;
  neutral.vis_ch = 0;
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

/**
 * Synchronizuje wymiary canvas z viewportem (DPR × innerWidth/Height).
 */
function syncCanvasSize() {
  const dpr = window.devicePixelRatio || 1;
  state.dpr = dpr;
  const w = window.innerWidth;
  const h = window.innerHeight;
  state.canvas.width = Math.max(1, Math.floor(w * dpr));
  state.canvas.height = Math.max(1, Math.floor(h * dpr));
  state.canvas.style.width = `${w}px`;
  state.canvas.style.height = `${h}px`;
  // Offscreen glow musi podążać za rozmiarem main canvas w fizycznych pikselach.
  ensureGlowCanvas(state.canvas.width, state.canvas.height);
}

function startLoop() {
  if (state.rafId !== 0) return;
  if (state.reducedMotion) return;
  state.lastFrameMs = 0;
  state.rafId = requestAnimationFrame(renderFrame);
}

function stopLoop() {
  if (state.rafId !== 0) {
    cancelAnimationFrame(state.rafId);
    state.rafId = 0;
  }
}

// Parallax: kursor w prawo → twarz patrzy w prawo, kursor w dół → twarz
// schyla się (patrzy w dół). Znak pitch odwrócony względem `my`, bo z-osą
// do kamery dodatni pitch obracał czubek nosa do góry (iluzja spojrzenia w górę).
function handleMouseMove(e) {
  const mx = (e.clientX / window.innerWidth - 0.5) * 2;
  const my = (e.clientY / window.innerHeight - 0.5) * 2;
  state.targetYaw = mx * 0.25;
  state.targetPitch = -my * 0.18;
}

// Wykrycie urządzenia touch-first. `pointer: coarse` pokrywa telefony/tablety,
// fallback na szerokość okna obsługuje desktopowe przeglądarki w wąskim oknie
// (traktujemy je jak mobile wizualnie — większa twarz lepiej wygląda).
function isMobileViewport() {
  if (typeof window === 'undefined') return false;
  const mql = window.matchMedia ? window.matchMedia('(pointer: coarse)') : null;
  if (mql && mql.matches) return true;
  return window.innerWidth < 768;
}

// Obsługa żyroskopu: gamma (odchylenie lewo-prawo) → yaw, beta (nachylenie
// przód-tył) → pitch. betaBaseline łapany przy pierwszym evencie, bo pozycja
// spoczynkowa telefonu bywa różna (leży płasko = 0°, trzymany pionowo = 90°).
function handleDeviceOrientation(e) {
  const gammaRaw = e.gamma;
  const betaRaw = e.beta;
  if (gammaRaw == null || betaRaw == null) return;

  if (state.betaBaseline === null) {
    state.betaBaseline = betaRaw;
  }

  let gamma = gammaRaw;
  if (gamma < -GYRO_GAMMA_RANGE) gamma = -GYRO_GAMMA_RANGE;
  else if (gamma > GYRO_GAMMA_RANGE) gamma = GYRO_GAMMA_RANGE;

  let betaDelta = betaRaw - state.betaBaseline;
  if (betaDelta < -GYRO_BETA_RANGE) betaDelta = -GYRO_BETA_RANGE;
  else if (betaDelta > GYRO_BETA_RANGE) betaDelta = GYRO_BETA_RANGE;

  state.targetYaw = (gamma / GYRO_GAMMA_RANGE) * GYRO_YAW_GAIN;
  state.targetPitch = -(betaDelta / GYRO_BETA_RANGE) * GYRO_PITCH_GAIN;
}

// Dopina listener orientation po uzyskaniu permission (iOS) albo bezpośrednio
// (Android / starsze API). Jeśli environment nie ma DeviceOrientationEvent,
// pozostajemy bez parallaxu — tylko idle animacje.
function attachDeviceOrientationListener() {
  if (typeof window === 'undefined') return;
  if (typeof DeviceOrientationEvent === 'undefined') return;
  if (state.orientationHandler) return;
  state.orientationHandler = handleDeviceOrientation;
  window.addEventListener('deviceorientation', state.orientationHandler);
}

// One-shot setup po pierwszym user gesture. iOS 13+ wymaga permission gated
// na tap/click; Android i inne od razu otrzymują listener. Flaga
// orientationSetupAttempted chroni przed wielokrotnym wywołaniem.
function setupOrientationAfterGesture() {
  if (state.orientationSetupAttempted) return;
  state.orientationSetupAttempted = true;

  if (typeof DeviceOrientationEvent === 'undefined') return;

  const requestPermission = DeviceOrientationEvent.requestPermission;
  if (typeof requestPermission === 'function') {
    requestPermission.call(DeviceOrientationEvent).then((result) => {
      if (result === 'granted') {
        attachDeviceOrientationListener();
      }
    }).catch(() => {
      // Permission denied lub błąd — zostajemy bez parallaxu.
    });
  } else {
    attachDeviceOrientationListener();
  }
}

// Rejestruje one-shot listener na pierwszy touchstart/click, który wywołuje
// setupOrientationAfterGesture i sam się usuwa.
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
  if (document.hidden) {
    stopLoop();
  } else {
    startLoop();
  }
}

function handleResize() {
  syncCanvasSize();
  // Rotacja telefonu albo zmiana rozmiaru okna może zmienić klasyfikację
  // mobile/desktop — skala bazowa musi reagować.
  state.scaleMul = isMobileViewport() ? MOBILE_SCALE_MUL : DESKTOP_SCALE_MUL;
  if (state.reducedMotion) {
    renderStaticFrame();
  }
}

export const FaceBackground = {
  show() {
    if (document.getElementById(CONTAINER_ID)) return;

    const container = document.createElement('div');
    container.id = CONTAINER_ID;
    container.className = 'face-bg';

    const canvas = document.createElement('canvas');
    canvas.id = CANVAS_ID;
    canvas.setAttribute('aria-hidden', 'true');
    container.appendChild(canvas);

    document.body.appendChild(container);
    document.body.classList.add('has-face-bg');

    state.canvas = canvas;
    state.ctx = canvas.getContext('2d', { alpha: true });
    state.reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

    // Reset harmonogramu idle-animacji na start sesji.
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
    state.orientationSetupAttempted = false;
    state.betaBaseline = null;
    state.scaleMul = isMobileViewport() ? MOBILE_SCALE_MUL : DESKTOP_SCALE_MUL;

    syncCanvasSize();

    // Pierwsza klatka, zanim RAF wystartuje — żeby tło nie było puste.
    renderStaticFrame();

    requestAnimationFrame(() => {
      container.classList.add('is-visible');
    });

    state.resizeHandler = handleResize;
    window.addEventListener('resize', state.resizeHandler);

    if (!state.reducedMotion) {
      if (isMobileViewport()) {
        // Mobile: parallax z żyroskopu (wymaga user gesture na iOS dla permission).
        setupDeviceOrientation();
      } else {
        state.mouseHandler = handleMouseMove;
        window.addEventListener('mousemove', state.mouseHandler);
      }
      state.visibilityHandler = handleVisibilityChange;
      document.addEventListener('visibilitychange', state.visibilityHandler);
      startLoop();
    }
  },

  hide() {
    const container = document.getElementById(CONTAINER_ID);
    if (!container) return;

    stopLoop();
    if (state.mouseHandler) {
      window.removeEventListener('mousemove', state.mouseHandler);
      state.mouseHandler = null;
    }
    if (state.orientationHandler) {
      window.removeEventListener('deviceorientation', state.orientationHandler);
      state.orientationHandler = null;
    }
    if (state.orientationSetupHandler) {
      window.removeEventListener('touchstart', state.orientationSetupHandler);
      window.removeEventListener('click', state.orientationSetupHandler);
      state.orientationSetupHandler = null;
    }
    state.orientationSetupAttempted = false;
    state.betaBaseline = null;
    if (state.visibilityHandler) {
      document.removeEventListener('visibilitychange', state.visibilityHandler);
      state.visibilityHandler = null;
    }
    if (state.resizeHandler) {
      window.removeEventListener('resize', state.resizeHandler);
      state.resizeHandler = null;
    }

    container.classList.remove('is-visible');
    document.body.classList.remove('has-face-bg');
    setTimeout(() => {
      container.remove();
      state.canvas = null;
      state.ctx = null;
      state.glowCanvas = null;
      state.glowCtx = null;
    }, 650);
  },
};

export default FaceBackground;
