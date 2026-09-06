// ============ File: face-speech.test.js - Speech feature ranges and sample-rate handling. ============
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { FaceSpeechAnalyser } from './face-speech.js';

function signal(rms, frequency, sampleRate) {
  return new FaceSpeechAnalyser({
    context: { sampleRate },
    frequencyBinCount: 512,
    getFloatTimeDomainData(buffer) { buffer.fill(rms); },
    getFloatFrequencyData(buffer) {
      buffer.fill(-Infinity);
      if (rms > 0) buffer[Math.round(frequency * 1024 / sampleRate)] = -10;
    },
  });
}

test('silence produces finite zero speech features', () => {
  assert.deepEqual(signal(0, 0, 48000).read(), { rms: 0, level: 0, round: 0, wide: 0, fricative: 0 });
});

test('speech level is monotonic and compresses loud input without clipping', () => {
  const levels = [0, 0.01, 0.1, 0.5, 1].map(rms => signal(rms, 500, 48000).read().level);
  for (let i = 1; i < levels.length; i++) assert(levels[i] > levels[i - 1]);
  assert(levels.at(-1) < 0.9);
});

test('spectral cues retain their meaning for chat and Teams sample rates', () => {
  for (const rate of [16000, 24000, 44100, 48000]) {
    const low = signal(0.1, 500, rate).read();
    const mid = signal(0.1, 1600, rate).read();
    const high = signal(0.1, 4000, rate).read();
    assert(low.round > mid.round && low.round <= 0.5);
    assert(mid.wide > low.wide);
    assert(high.fricative > mid.fricative);
  }
});
