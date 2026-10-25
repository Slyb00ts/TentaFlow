// =============================================================================
// File: lib/chat-metrics.test.js — per-conversation decode speed and TTFT maths
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { DecodeMeter, SessionMetrics, sparkMarkup, fmtRate } from './chat-metrics.js';

function clock(start = 1000) {
  let t = start;
  const now = () => t;
  now.advance = (ms) => { t += ms; };
  return now;
}

test('TTFT is the time from the request to the first delivered token', () => {
  const now = clock();
  const meter = new DecodeMeter(now);
  assert.equal(meter.ttftMs, null, 'no token yet, no TTFT');
  now.advance(184);
  meter.delta();
  assert.equal(meter.ttftMs, 184);
  now.advance(900);
  meter.delta();
  assert.equal(meter.ttftMs, 184, 'later tokens never move the TTFT');
});

test('sample() is tokens per second over the window since the previous sample', () => {
  const now = clock();
  const meter = new DecodeMeter(now);
  now.advance(200);
  meter.delta();
  for (let i = 0; i < 40; i += 1) { now.advance(25); meter.delta(); }
  const first = meter.sample();
  assert.ok(Math.abs(first - 41) < 1, `41 tokens in 1 s, got ${first}`);
  for (let i = 0; i < 20; i += 1) { now.advance(50); meter.delta(); }
  const second = meter.sample();
  assert.ok(Math.abs(second - 20) < 1, `20 tokens in 1 s, got ${second}`);
});

test('sample() waits for a token and for a window long enough to divide by', () => {
  const now = clock();
  const meter = new DecodeMeter(now);
  now.advance(5000);
  assert.equal(meter.sample(), null, 'the wait for the first token is not decode time');
  meter.delta();
  now.advance(100);
  meter.delta();
  assert.equal(meter.sample(), null, 'a 100 ms window is noise');
  now.advance(500);
  meter.delta();
  assert.ok(meter.sample() > 0, 'the window kept growing and is now usable');
});

test('overall() needs two tokens spread over a real duration', () => {
  const now = clock();
  const meter = new DecodeMeter(now);
  meter.delta();
  assert.equal(meter.overall(), null);
  now.advance(2000);
  meter.delta(10);
  assert.ok(Math.abs(meter.overall() - 5) < 0.01, '10 tokens after the first one over 2 s');
});

test('SessionMetrics aggregates only what this conversation recorded', () => {
  const s = new SessionMetrics();
  assert.equal(s.stats(), null, 'an empty conversation has no numbers');
  [44.1, 46.8, 43.2, 47.5].forEach((v) => s.push(v));
  const st = s.stats();
  assert.equal(st.n, 4);
  assert.equal(st.cur, 47.5);
  assert.equal(st.min, 43.2);
  assert.equal(st.max, 47.5);
  assert.ok(Math.abs(st.avg - 45.4) < 0.01);
});

test('SessionMetrics drops garbage samples and keeps a bounded window', () => {
  const s = new SessionMetrics();
  for (const bad of [0, -3, NaN, Infinity, undefined]) s.push(bad);
  assert.equal(s.count, 0);
  for (let i = 1; i <= 100; i += 1) s.push(i);
  assert.equal(s.count, 40);
  assert.equal(s.stats().min, 61, 'the oldest samples fell out');
});

test('seed() rebuilds the series from stored answers and skips answers without numbers', () => {
  const s = new SessionMetrics();
  s.push(99);
  s.seed([{ decodeTps: 40, ttftMs: 200 }, null, { decodeTps: 0, ttftMs: 0 }, { decodeTps: 50, ttftMs: 180 }]);
  assert.deepEqual(s.values, [40, 50]);
  assert.equal(s.ttftMs, 180);
});

test('sparkMarkup draws the average line, the series and a dot on the latest sample', () => {
  const svg = sparkMarkup([10, 20, 15], 15);
  assert.match(svg, /<line class="avg"/);
  assert.match(svg, /<polyline points="0\.0,[\d.]+ 32.0,[\d.]+ 64.0,[\d.]+"\/>/);
  assert.match(svg, /<circle cx="64.0"/);
  assert.doesNotMatch(sparkMarkup([], 0), /polyline/, 'an empty series keeps only the flat line');
});

test('fmtRate follows the reader language', () => {
  assert.equal(fmtRate(47.31, 'pl'), '47,3');
  assert.equal(fmtRate(47.31, 'en'), '47.3');
});

test('drop forgets the newest samples only', () => {
  const m = new SessionMetrics();
  [10, 20, 30, 40].forEach((v) => m.push(v));
  m.drop(2);
  assert.deepEqual(m.values, [10, 20]);
  m.drop(9);
  assert.deepEqual(m.values, []);
});
