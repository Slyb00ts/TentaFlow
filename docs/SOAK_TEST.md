# Soak test — TentaFlow / TentaVision F1a (M3.W14)

This document describes how to run, monitor, and accept the 24-hour soak test
for the TentaVision F1a release. The same infra runs 1h smoke and 4h burn-in
profiles by changing one argument.

## Acceptance criteria

Per `notes/tentavision-f1a-implementation.md` M3.W14:

- **RSS growth < 5% / 24h** (after warm-up samples dropped)
- **FD count steady** — no monotonic growth after warm-up
- **DB pool** — never exhausted (peak `db_pool_in_use` strictly under pool cap)
- **Zero critical errors** in `logs/tentaflow.log`

`scripts/soak/analyze.py` checks the first three automatically. Log review for
criticals is manual.

## Prerequisites

1. **Release binary built**

   ```bash
   (cd tentaflow && cargo build --release)
   ```

2. **Sample video present** (regenerable via M1.W6 procedure):

   ```
   tentaflow-core/assets/test/sample_traffic.mp4
   ```

3. **System tools** — `bash`, `ps`, `curl`, `python3` (>= 3.9).

4. **Config** — `tests/e2e/config-soak.toml` (provided). Uses isolated DB at
   `/tmp/tentaflow-soak.db` and Prometheus at `:19099` to avoid collisions.

5. **Memory leak deep-dive (optional)** — requires a dhat-enabled rebuild:

   ```bash
   # NOT YET WIRED in Cargo.toml — adding `dhat` requires a code change.
   # When wired: (cd tentaflow && cargo build --release --features dhat-heap)
   ```

   Until that feature exists, use `heaptrack` externally:

   ```bash
   heaptrack ./tentaflow/target/release/tentaflow --config tests/e2e/config-soak.toml
   # then: heaptrack_gui heaptrack.tentaflow.<pid>.gz
   ```

## Quick start

```bash
# ~3 minute smoke (handy while iterating on the soak infra itself)
scripts/soak/run_soak.sh 0.05

# 1h smoke
scripts/soak/run_soak.sh 1

# 4h burn-in
scripts/soak/run_soak.sh 4

# Full 24h acceptance
scripts/soak/run_soak.sh 24
```

Output directory defaults to `/tmp/tentaflow-soak-<timestamp>/`. Override:

```bash
scripts/soak/run_soak.sh 24 /var/tmp/release-candidate-soak
```

## Seeding cameras

The dashboard speaks a custom binary WebSocket protocol
(`tentaflow-core/www/js/protocol/`). A Python client of that protocol is **not
yet implemented**, so `scripts/soak/seed_cameras.py` currently runs in
placeholder mode and writes a reminder note.

For a real 24h acceptance run, seed cameras manually before starting the
sampling loop (or right after warm-up — sampling tolerates this):

1. Open `https://127.0.0.1:18099/` in a browser.
2. Login as `admin` / `soak-test-2026` (credentials from `config-soak.toml`).
3. TentaVision -> Cameras -> Add camera, four times, with profiles:

   | # | Name       | Source (FakeFile)                         | Resolution | FPS |
   |---|------------|-------------------------------------------|------------|-----|
   | 1 | cam-low    | `assets/test/sample_traffic.mp4` (loop)   |  320x240   |  5  |
   | 2 | cam-medium | `assets/test/sample_traffic.mp4` (loop)   |  640x480   | 15  |
   | 3 | cam-hd     | `assets/test/sample_traffic.mp4` (loop)   | 1280x720   | 30  |
   | 4 | cam-fhd    | `assets/test/sample_traffic.mp4` (loop)   | 1920x1080  | 30  |

When the Python WS client lands, replace `seed_cameras.py`'s main body with
real seeding logic; `run_soak.sh` already calls it.

## What is sampled

Every `SAMPLE_INTERVAL_SEC` (default 60s), `run_soak.sh` records a CSV row:

| Column            | Source                                  |
|-------------------|-----------------------------------------|
| `ts_unix`         | `date +%s`                              |
| `rss_kb`          | `ps -o rss=`                            |
| `vsz_kb`          | `ps -o vsz=`                            |
| `cpu_pct`         | `ps -o %cpu=`                           |
| `fd_count`        | `ls /proc/<pid>/fd \| wc -l`            |
| `thread_count`    | `ps -o nlwp=`                           |
| `db_pool_in_use`  | Prometheus `sqlite_pool_in_use` (if exposed) |
| `db_pool_idle`    | Prometheus `sqlite_pool_idle` (if exposed)   |
| `uptime_sec`      | computed                                |

The Prometheus columns may stay empty if those specific metric names are not
exposed — the script is metric-name tolerant (also accepts
`tentaflow_db_pool_*`). Add the missing exporter if pool tracking is required
for acceptance sign-off.

Override sampling cadence:

```bash
SAMPLE_INTERVAL_SEC=30 scripts/soak/run_soak.sh 24
```

## Output layout

```
/tmp/tentaflow-soak-<ts>/
├── logs/
│   ├── tentaflow.log         # full stdout/stderr from the binary
│   └── seed.log              # output of seed_cameras.py
├── metrics/
│   ├── snapshot.csv          # one row per sample
│   └── prom-raw.txt          # last Prometheus scrape (raw text)
└── summary.txt               # analyze.py output (PASS / FAIL + numbers)
```

## Result interpretation

After the run, `run_soak.sh` invokes `analyze.py` automatically. To re-run:

```bash
scripts/soak/analyze.py /tmp/tentaflow-soak-<ts>
```

Exit codes: `0` = all acceptance checks pass, `1` = at least one failed,
`2` = bad input.

The summary reports first/last/peak RSS, RSS growth %, CPU mean and p99, FD
delta and range, and DB pool peak/mean. A FAIL line lists which acceptance
checks tripped.

## Memory leak deep-dive

If `analyze.py` reports RSS growth at or near the 5% limit, run a second pass
with `heaptrack`:

```bash
heaptrack ./tentaflow/target/release/tentaflow --config tests/e2e/config-soak.toml
# stop after ~2h, then:
heaptrack_print heaptrack.tentaflow.*.gz | less   # CLI
heaptrack_gui   heaptrack.tentaflow.*.gz          # interactive
```

Alternatively, add the `dhat` crate as an optional dependency, wire a
`#[cfg(feature = "dhat-heap")]` global allocator in the binary entry point,
and rebuild with `--features dhat-heap`. This is a code change, not part of
the soak infra.

## RTSP camera testing (F1b P1.B)

For RTSP integration tests (`tests/camera_rtsp_integration.rs`, marked
`#[ignore]` so they do not run in the default suite):

- Install a local RTSP test server, e.g. `mediamtx` (Go) or `gst-rtsp-server`
  (C). The default URL the tests expect is `rtsp://127.0.0.1:8554/test`.
- System GStreamer plugins required on the host running the tests:
  - Debian/Ubuntu: `gstreamer1.0-plugins-good` (rtspsrc, rtph264depay),
    `gstreamer1.0-plugins-bad` (h264parse), `gstreamer1.0-libav` (avdec_h264),
    `gstreamer1.0-rtsp`.
  - macOS: `brew install gstreamer`.
- Sanity check the toolchain before running the suite:
  `gst-launch-1.0 rtspsrc location=rtsp://127.0.0.1:8554/test ! fakesink`.

Logs and `CameraHealth.status_message` redact RTSP credentials (`user:pass@`
is replaced with `***:***@`) before emission, so it is safe to attach raw
logs to bug reports.

## ONVIF camera testing (F1b P1.D)

ONVIF discovery uses UDP multicast 239.255.255.250:3702 (WS-Discovery).
Requirements:
- Network: outbound multicast permitted (firewall/router)
- LAN ONVIF camera (Hikvision, Axis, Dahua, etc.) responding to Probe
- Or ONVIF simulator: https://github.com/Vorlent/onvif-discovery-server

Integration test paths:
- `cargo test --features camera,dashboard-api --test camera_onvif_integration -- --include-ignored`
  (requires live ONVIF device on LAN)

ONVIF cameras are discovery-only in F1b. To add as camera, operator
extracts the RTSP stream URI from discovery output (manufacturer scope)
and adds via `vendor='rtsp'`. ONVIF profile retrieval (GetStreamUri SOAP)
deferred to F1c.

`camera_test_connection_v1` for ONVIF forces path `/onvif/device_service`
to prevent arbitrary HTTP target probing.

## Troubleshooting

- **`tentaflow died during warm-up`** — see `logs/tentaflow.log`. Usually a
  port conflict (something already on 18099 / 19099) or a missing migration.
- **All `db_pool_*` cells empty** — Prometheus metric names differ. Inspect
  `metrics/prom-raw.txt` and update the grep patterns in `run_soak.sh`
  accordingly (or expose the metrics in the binary).
- **FD count climbs steadily** — that is the signal this test is designed to
  catch. Inspect `lsof -p <pid>` mid-run; correlate growth with camera/stream
  lifecycle.
