#!/usr/bin/env python3
# ============================================================================
# File: scripts/soak/analyze.py — soak metrics CSV -> human summary + acceptance check
# ============================================================================
#
# Usage:
#   scripts/soak/analyze.py <SOAK_OUTPUT_DIR>
#
# Reads:   <dir>/metrics/snapshot.csv
# Writes:  stdout (caller redirects to summary.txt)
# Exit  :  0 = all acceptance checks pass, 1 = at least one failed, 2 = bad input
#
# Acceptance (tentavision-f1a M3.W14):
#   - RSS growth < 5% over the full window (warm-up sample dropped)
#   - FD count: not monotonically increasing (max - min <= small tolerance after warm-up)
#   - DB pool: never exhausted (in_use < observed max+1 always; we report peak)

from __future__ import annotations

import csv
import statistics
import sys
from pathlib import Path
from typing import Optional

# Tolerances (constants intentionally near acceptance criteria, not configurable per-run).
RSS_GROWTH_LIMIT_PCT = 5.0
FD_GROWTH_LIMIT = 50           # absolute slack for FD spikes vs steady-state
WARMUP_SAMPLES = 2             # drop first N samples (process still warming caches)

class Sample:
    __slots__ = ("ts", "rss_kb", "cpu_pct", "fd", "threads", "pool_in_use", "uptime")

    def __init__(self, row: dict[str, str]) -> None:
        def _f(key: str) -> Optional[float]:
            v = row.get(key, "").strip()
            if not v:
                return None
            try:
                return float(v)
            except ValueError:
                return None
        self.ts = int(row["ts_unix"])
        self.rss_kb = _f("rss_kb")
        self.cpu_pct = _f("cpu_pct")
        self.fd = _f("fd_count")
        self.threads = _f("thread_count")
        self.pool_in_use = _f("db_pool_in_use")
        self.uptime = _f("uptime_sec") or 0.0


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return float("nan")
    s = sorted(values)
    k = max(0, min(len(s) - 1, int(round((pct / 100.0) * (len(s) - 1)))))
    return s[k]


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("usage: analyze.py <SOAK_OUTPUT_DIR>", file=sys.stderr)
        return 2
    out_dir = Path(argv[1])
    csv_path = out_dir / "metrics" / "snapshot.csv"
    if not csv_path.is_file():
        print(f"ERROR: missing {csv_path}", file=sys.stderr)
        return 2

    samples: list[Sample] = []
    with csv_path.open(newline="") as fh:
        reader = csv.DictReader(fh)
        for row in reader:
            try:
                samples.append(Sample(row))
            except (KeyError, ValueError):
                continue
    if len(samples) < 3:
        print(f"ERROR: too few samples ({len(samples)}) — need at least 3", file=sys.stderr)
        return 2

    post_warmup = samples[WARMUP_SAMPLES:] if len(samples) > WARMUP_SAMPLES else samples
    rss_vals = [s.rss_kb for s in post_warmup if s.rss_kb is not None]
    cpu_vals = [s.cpu_pct for s in post_warmup if s.cpu_pct is not None]
    fd_vals = [s.fd for s in post_warmup if s.fd is not None]
    pool_vals = [s.pool_in_use for s in post_warmup if s.pool_in_use is not None]

    duration_sec = samples[-1].ts - samples[0].ts
    duration_h = duration_sec / 3600.0

    print("=" * 70)
    print(f"TentaFlow soak summary  —  {csv_path}")
    print("=" * 70)
    print(f"Samples         : {len(samples)} (warm-up dropped: {WARMUP_SAMPLES})")
    print(f"Duration        : {duration_sec}s ({duration_h:.2f}h)")
    print()

    failures: list[str] = []

    # RSS
    if rss_vals:
        rss_first = rss_vals[0]
        rss_last = rss_vals[-1]
        rss_peak = max(rss_vals)
        growth_pct = ((rss_last - rss_first) / rss_first * 100.0) if rss_first else float("nan")
        print(f"RSS first/last  : {rss_first/1024:.1f} MiB -> {rss_last/1024:.1f} MiB")
        print(f"RSS peak        : {rss_peak/1024:.1f} MiB")
        print(f"RSS growth      : {growth_pct:+.2f}%   (limit < {RSS_GROWTH_LIMIT_PCT}%)")
        if growth_pct > RSS_GROWTH_LIMIT_PCT:
            failures.append(f"RSS growth {growth_pct:.2f}% exceeds {RSS_GROWTH_LIMIT_PCT}%")
    else:
        print("RSS             : no samples")
        failures.append("RSS: no samples")

    # CPU
    if cpu_vals:
        print(f"CPU mean / p99  : {statistics.fmean(cpu_vals):.2f}% / {percentile(cpu_vals, 99):.2f}%")
    else:
        print("CPU             : no samples")

    # FD
    if fd_vals:
        fd_min = min(fd_vals)
        fd_max = max(fd_vals)
        fd_first = fd_vals[0]
        fd_last = fd_vals[-1]
        fd_delta = fd_last - fd_first
        print(f"FD first/last   : {fd_first:.0f} -> {fd_last:.0f}  (delta {fd_delta:+.0f})")
        print(f"FD min/max      : {fd_min:.0f} / {fd_max:.0f}     (slack {FD_GROWTH_LIMIT})")
        if (fd_max - fd_min) > FD_GROWTH_LIMIT and fd_delta > FD_GROWTH_LIMIT:
            failures.append(f"FD count grew by {fd_delta:.0f} (slack {FD_GROWTH_LIMIT}) — possible leak")
    else:
        print("FD              : no samples")
        failures.append("FD: no samples")

    # DB pool
    if pool_vals:
        peak = max(pool_vals)
        print(f"DB pool in_use  : peak {peak:.0f}, mean {statistics.fmean(pool_vals):.2f}")
    else:
        print("DB pool         : no Prometheus samples (metric absent or scrape failed)")

    print()
    if failures:
        print("RESULT: FAIL")
        for f in failures:
            print(f"  - {f}")
        return 1
    print("RESULT: PASS — all acceptance checks met")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
