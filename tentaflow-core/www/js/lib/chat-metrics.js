// =============================================================================
// File: lib/chat-metrics.js — decode speed and time-to-first-token of ONE chat
// conversation, measured from the deltas that conversation's own stream
// delivers. Nothing here reads node-wide or model-wide counters: other users'
// traffic must never move these numbers.
// =============================================================================

// A window shorter than this is too noisy to divide by (two deltas landing in
// the same frame would read as thousands of tok/s).
const MIN_WINDOW_MS = 400;
// The sparkline and the min/max/avg describe the recent past of a conversation,
// not its whole life; an old fast answer must not pin the average forever.
const MAX_SAMPLES = 40;

/**
 * Meter of a single generation turn. `delta()` is called per delivered chunk of
 * text, `sample()` once a second while the turn runs; the clock is injected so
 * the maths is testable without waiting.
 */
export class DecodeMeter {
  constructor(now = () => performance.now()) {
    this.now = now;
    this.startedAt = now();
    this.firstAt = null;
    this.lastAt = null;
    this.tokens = 0;
    this.windowAt = null;
    this.windowTokens = 0;
    this.sampled = false;
  }

  /** Time from the request to the first delivered token, null before it arrives. */
  get ttftMs() {
    return this.firstAt === null ? null : Math.max(0, Math.round(this.firstAt - this.startedAt));
  }

  /** Counts `tokens` freshly delivered; the first call fixes the TTFT. */
  delta(tokens = 1) {
    const t = this.now();
    if (this.firstAt === null) {
      this.firstAt = t;
      this.windowAt = t;
    }
    this.lastAt = t;
    this.tokens += tokens;
  }

  /**
   * Tokens per second over the window since the previous sample. Null while no
   * token has arrived or the window is too short to mean anything; in that case
   * the window keeps growing instead of being reset.
   */
  sample() {
    if (this.firstAt === null) return null;
    const t = this.now();
    const span = t - this.windowAt;
    if (span < MIN_WINDOW_MS) return null;
    const rate = ((this.tokens - this.windowTokens) * 1000) / span;
    this.windowAt = t;
    this.windowTokens = this.tokens;
    this.sampled = true;
    return rate;
  }

  /**
   * Whole-turn speed, first token to last token. Null when fewer than two
   * deltas arrived: one delta has no duration to divide by.
   */
  overall() {
    if (this.firstAt === null || this.lastAt === null || this.tokens < 2) return null;
    const span = this.lastAt - this.firstAt;
    return span < MIN_WINDOW_MS ? null : ((this.tokens - 1) * 1000) / span;
  }
}

/** Samples of one conversation plus the latest TTFT. */
export class SessionMetrics {
  constructor() {
    this.values = [];
    this.ttftMs = null;
  }

  push(rate) {
    if (!Number.isFinite(rate) || rate <= 0) return;
    this.values.push(rate);
    if (this.values.length > MAX_SAMPLES) this.values.shift();
  }

  /** Forgets the newest `n` samples — the ones a cut-off answer contributed. */
  drop(n) {
    if (n > 0) this.values.splice(Math.max(0, this.values.length - n), n);
  }

  setTtft(ms) {
    if (Number.isFinite(ms) && ms > 0) this.ttftMs = Math.round(ms);
  }

  /** Rebuilds the series from answers already stored on the conversation. */
  seed(perfList) {
    this.values = [];
    this.ttftMs = null;
    for (const perf of perfList) {
      if (!perf) continue;
      this.push(Number(perf.decodeTps));
      this.setTtft(Number(perf.ttftMs));
    }
  }

  get count() { return this.values.length; }

  /** null while there is nothing to show — the bar renders a dash then. */
  stats() {
    const v = this.values;
    if (v.length === 0) return null;
    const sum = v.reduce((a, b) => a + b, 0);
    return {
      cur: v[v.length - 1],
      avg: sum / v.length,
      min: Math.min(...v),
      max: Math.max(...v),
      n: v.length,
    };
  }
}

const SPARK_W = 64;
const SPARK_H = 22;

/**
 * Inner markup of the 72x32 sparkline: the dashed session average and the
 * series with a dot on the latest sample. Empty series draw only a flat
 * average line so the cell keeps its shape.
 */
export function sparkMarkup(values, avg) {
  if (values.length === 0) {
    const mid = SPARK_H / 2;
    return `<line class="avg" x1="0" x2="${SPARK_W}" y1="${mid}" y2="${mid}"/>`;
  }
  const lo = Math.min(...values, avg);
  const hi = Math.max(...values, avg);
  const span = Math.max(hi - lo, 8);
  const y = (v) => (SPARK_H - 3 - ((v - lo) / span) * (SPARK_H - 6)).toFixed(1);
  const step = values.length > 1 ? SPARK_W / (values.length - 1) : 0;
  const pts = values.map((v, i) => `${(i * step).toFixed(1)},${y(v)}`).join(' ');
  const last = values.length - 1;
  return `<line class="avg" x1="0" x2="${SPARK_W}" y1="${y(avg)}" y2="${y(avg)}"/>`
    + `<polyline points="${pts}"/>`
    + `<circle cx="${(last * step).toFixed(1)}" cy="${y(values[last])}" r="2.4"/>`;
}

/** One decimal in the reader's language: 47,3 in Polish, 47.3 in English. */
export function fmtRate(value, lang) {
  return new Intl.NumberFormat(lang || undefined, { minimumFractionDigits: 1, maximumFractionDigits: 1 }).format(value);
}
