// =============================================================================
// Plik: js/lib/sfx.js
// Opis: Proceduralny system efektow dzwiekowych oparty o Web Audio API.
//       Wszystkie dzwieki syntetyzowane, zero plikow binarnych. Master gain
//       ograniczony; preferencje (enabled, volume) zapisywane w localStorage.
// Przyklad: import { Sfx } from '/js/lib/sfx.js'; Sfx.play('ui-click');
// =============================================================================

let ctx = null;
let masterGain = null;
let enabled = true;
let userVolume = 0.6;

try {
  const saved = localStorage.getItem('tf-sfx-enabled');
  if (saved === '0') enabled = false;
  const vol = parseFloat(localStorage.getItem('tf-sfx-volume') ?? '');
  if (!Number.isNaN(vol) && vol >= 0 && vol <= 1) userVolume = vol;
} catch (e) { /* ignored */ }

function getCtx() {
  if (!ctx) {
    const AC = window.AudioContext || window.webkitAudioContext;
    if (!AC) return null;
    try {
      ctx = new AC();
    } catch (e) {
      return null;
    }
    masterGain = ctx.createGain();
    masterGain.gain.value = userVolume;
    masterGain.connect(ctx.destination);
  }
  // Odblokowanie po user-gesture (polityka autoplay Chrome/Safari).
  if (ctx.state === 'suspended') {
    try { ctx.resume(); } catch (e) { /* ignored */ }
  }
  return ctx;
}

// Koperta gain z attack/decay (liniowy attack, wykladniczy decay do 0.001).
function envGain(c, t0, attack, decay, peak) {
  const g = c.createGain();
  g.gain.setValueAtTime(0.0001, t0);
  g.gain.linearRampToValueAtTime(peak, t0 + attack);
  g.gain.exponentialRampToValueAtTime(0.0001, t0 + attack + decay);
  return g;
}

function osc(c, type, freq) {
  const o = c.createOscillator();
  o.type = type;
  o.frequency.value = freq;
  return o;
}

function noiseBuffer(c, duration) {
  const size = Math.max(1, Math.floor(c.sampleRate * duration));
  const buf = c.createBuffer(1, size, c.sampleRate);
  const data = buf.getChannelData(0);
  for (let i = 0; i < size; i++) data[i] = Math.random() * 2 - 1;
  const src = c.createBufferSource();
  src.buffer = buf;
  return src;
}

// ---- Definicje dzwiekow ---------------------------------------------------

function playLoginSuccess(c, t0) {
  // Filtrowany szum z sweepem lowpass 200 -> 8000 Hz.
  const noise = noiseBuffer(c, 0.7);
  const noiseFilter = c.createBiquadFilter();
  noiseFilter.type = 'lowpass';
  noiseFilter.frequency.setValueAtTime(200, t0);
  noiseFilter.frequency.exponentialRampToValueAtTime(8000, t0 + 0.6);
  const noiseGain = envGain(c, t0, 0.08, 0.6, 0.18);
  noise.connect(noiseFilter).connect(noiseGain).connect(masterGain);
  noise.start(t0);
  noise.stop(t0 + 0.75);

  // Sine sweep 200 -> 1200 Hz.
  const sweep = osc(c, 'sine', 200);
  sweep.frequency.setValueAtTime(200, t0);
  sweep.frequency.exponentialRampToValueAtTime(1200, t0 + 0.8);
  const sweepGain = envGain(c, t0, 0.05, 0.85, 0.22);
  sweep.connect(sweepGain).connect(masterGain);
  sweep.start(t0);
  sweep.stop(t0 + 0.95);

  // Chime: trzy sinusoidy C5, E5, G5 z offsetami.
  const chimeFreqs = [523.25, 659.25, 783.99];
  chimeFreqs.forEach((f, i) => {
    const o = osc(c, 'sine', f);
    const g = envGain(c, t0 + i * 0.1, 0.05, 0.5, 0.12);
    o.connect(g).connect(masterGain);
    o.start(t0 + i * 0.1);
    o.stop(t0 + i * 0.1 + 0.6);
  });
}

function playLoginFail(c, t0) {
  // Dwie piły z lekkim detune, pulsowanie 3x przez ~0.4 s.
  const filter = c.createBiquadFilter();
  filter.type = 'lowpass';
  filter.frequency.value = 800;
  filter.Q.value = 6;
  filter.connect(masterGain);

  const pulseGain = c.createGain();
  pulseGain.gain.setValueAtTime(0.0001, t0);
  // Trzy pulsy (on 80ms / off 60ms / on 80ms / off 60ms / on 80ms).
  const pulses = [0.0, 0.14, 0.28];
  pulses.forEach((off) => {
    const t = t0 + off;
    pulseGain.gain.linearRampToValueAtTime(0.22, t + 0.01);
    pulseGain.gain.linearRampToValueAtTime(0.22, t + 0.07);
    pulseGain.gain.linearRampToValueAtTime(0.0001, t + 0.1);
  });
  pulseGain.connect(filter);

  const saw1 = osc(c, 'sawtooth', 150);
  const saw2 = osc(c, 'sawtooth', 155);
  saw1.connect(pulseGain);
  saw2.connect(pulseGain);
  saw1.start(t0);
  saw2.start(t0);
  saw1.stop(t0 + 0.45);
  saw2.stop(t0 + 0.45);
}

function playClick(c, t0) {
  const o = osc(c, 'sine', 600);
  const g = envGain(c, t0, 0.005, 0.05, 0.25);
  o.connect(g).connect(masterGain);
  o.start(t0);
  o.stop(t0 + 0.08);
}

function playMenuOpen(c, t0) {
  const o = osc(c, 'sine', 400);
  o.frequency.setValueAtTime(400, t0);
  o.frequency.exponentialRampToValueAtTime(800, t0 + 0.15);
  const g = envGain(c, t0, 0.02, 0.18, 0.15);
  o.connect(g).connect(masterGain);
  o.start(t0);
  o.stop(t0 + 0.22);
}

function playMenuClose(c, t0) {
  const o = osc(c, 'sine', 800);
  o.frequency.setValueAtTime(800, t0);
  o.frequency.exponentialRampToValueAtTime(400, t0 + 0.13);
  const g = envGain(c, t0, 0.02, 0.14, 0.1);
  o.connect(g).connect(masterGain);
  o.start(t0);
  o.stop(t0 + 0.18);
}

function playWindowOpen(c, t0) {
  const noise = noiseBuffer(c, 0.35);
  const bp = c.createBiquadFilter();
  bp.type = 'bandpass';
  bp.Q.value = 1.2;
  bp.frequency.setValueAtTime(1500, t0);
  bp.frequency.exponentialRampToValueAtTime(3000, t0 + 0.3);
  const noiseGain = envGain(c, t0, 0.03, 0.3, 0.18);
  noise.connect(bp).connect(noiseGain).connect(masterGain);
  noise.start(t0);
  noise.stop(t0 + 0.4);

  const chime = osc(c, 'sine', 800);
  const chimeGain = envGain(c, t0, 0.03, 0.35, 0.1);
  chime.connect(chimeGain).connect(masterGain);
  chime.start(t0);
  chime.stop(t0 + 0.42);
}

function playWindowClose(c, t0) {
  const noise = noiseBuffer(c, 0.3);
  const bp = c.createBiquadFilter();
  bp.type = 'bandpass';
  bp.Q.value = 1.2;
  bp.frequency.setValueAtTime(3000, t0);
  bp.frequency.exponentialRampToValueAtTime(1000, t0 + 0.25);
  const g = envGain(c, t0, 0.02, 0.27, 0.14);
  noise.connect(bp).connect(g).connect(masterGain);
  noise.start(t0);
  noise.stop(t0 + 0.33);
}

function playToggle(c, t0) {
  const o = osc(c, 'triangle', 1000);
  const g = envGain(c, t0, 0.003, 0.035, 0.18);
  o.connect(g).connect(masterGain);
  o.start(t0);
  o.stop(t0 + 0.06);
}

function playHover(c, t0) {
  const o = osc(c, 'sine', 1200);
  const g = envGain(c, t0, 0.01, 0.07, 0.03);
  o.connect(g).connect(masterGain);
  o.start(t0);
  o.stop(t0 + 0.1);
}

// ---- Publiczne API --------------------------------------------------------

export const Sfx = {
  play(name) {
    if (!enabled) return;
    const c = getCtx();
    if (!c) return;
    const t0 = c.currentTime + 0.002;
    switch (name) {
      case 'login-success': playLoginSuccess(c, t0); break;
      case 'login-fail': playLoginFail(c, t0); break;
      case 'ui-click': playClick(c, t0); break;
      case 'menu-open': playMenuOpen(c, t0); break;
      case 'menu-close': playMenuClose(c, t0); break;
      case 'window-open': playWindowOpen(c, t0); break;
      case 'window-close': playWindowClose(c, t0); break;
      case 'toggle': playToggle(c, t0); break;
      case 'hover': playHover(c, t0); break;
      default: break;
    }
  },
  setEnabled(v) {
    enabled = !!v;
    try { localStorage.setItem('tf-sfx-enabled', enabled ? '1' : '0'); } catch (e) { /* ignored */ }
  },
  setVolume(v) {
    userVolume = Math.max(0, Math.min(1, v));
    if (masterGain) masterGain.gain.value = userVolume;
    try { localStorage.setItem('tf-sfx-volume', String(userVolume)); } catch (e) { /* ignored */ }
  },
  isEnabled() { return enabled; },
  getVolume() { return userVolume; },
};

export default Sfx;
