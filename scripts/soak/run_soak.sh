#!/usr/bin/env bash
# ============================================================================
# File: scripts/soak/run_soak.sh — orchestrate tentaflow soak test + metric sampling
# ============================================================================
#
# Usage:
#   scripts/soak/run_soak.sh [DURATION_HOURS] [OUTPUT_DIR]
#
# Examples:
#   scripts/soak/run_soak.sh 1                    # 1h smoke
#   scripts/soak/run_soak.sh 4                    # 4h burn-in
#   scripts/soak/run_soak.sh 24                   # full acceptance run
#   scripts/soak/run_soak.sh 0.05                 # ~3 min smoke
#
# Output layout:
#   $OUTPUT_DIR/logs/tentaflow.log       — full stdout/stderr
#   $OUTPUT_DIR/metrics/snapshot.csv     — sampled metrics (1-min cadence)
#   $OUTPUT_DIR/metrics/prom-raw.txt     — last raw Prometheus scrape
#   $OUTPUT_DIR/summary.txt              — analyze.py result (post-run)
#
# Acceptance targets (per tentavision-f1a §17.9 / M3.W14):
#   - RSS growth < 5% / 24h
#   - FD count steady (no monotonic growth after warm-up)
#   - DB pool: no exhaustion
#
# Author: tentaflow-soak

set -euo pipefail

DURATION_HOURS="${1:-24}"
OUTPUT_DIR="${2:-/tmp/tentaflow-soak-$(date +%Y%m%d-%H%M%S)}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BINARY="${REPO_ROOT}/tentaflow/target/release/tentaflow"
CONFIG="${REPO_ROOT}/tests/e2e/config-soak.toml"
PROM_URL="${PROM_URL:-http://127.0.0.1:19099/metrics}"
SAMPLE_INTERVAL_SEC="${SAMPLE_INTERVAL_SEC:-60}"

if [[ ! -x "${BINARY}" ]]; then
    echo "ERROR: ${BINARY} not built or not executable" >&2
    echo "  Run: (cd tentaflow && cargo build --release)" >&2
    exit 1
fi
if [[ ! -f "${CONFIG}" ]]; then
    echo "ERROR: config missing: ${CONFIG}" >&2
    exit 1
fi

mkdir -p "${OUTPUT_DIR}/logs" "${OUTPUT_DIR}/metrics"
echo "Soak run: duration=${DURATION_HOURS}h, output=${OUTPUT_DIR}"
echo "Binary  : ${BINARY}"
echo "Config  : ${CONFIG}"
echo

# --- Start tentaflow ---
"${BINARY}" --config "${CONFIG}" \
    > "${OUTPUT_DIR}/logs/tentaflow.log" 2>&1 &
TF_PID=$!
echo "tentaflow PID=${TF_PID}"

cleanup() {
    if kill -0 "${TF_PID}" 2>/dev/null; then
        echo "Stopping tentaflow (PID=${TF_PID})..."
        kill -TERM "${TF_PID}" 2>/dev/null || true
        for _ in $(seq 1 10); do
            kill -0 "${TF_PID}" 2>/dev/null || break
            sleep 1
        done
        kill -KILL "${TF_PID}" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

# Warm-up: wait until process is alive and (optionally) Prometheus responds.
sleep 5
if ! kill -0 "${TF_PID}" 2>/dev/null; then
    echo "ERROR: tentaflow died during warm-up. Tail of log:" >&2
    tail -n 50 "${OUTPUT_DIR}/logs/tentaflow.log" >&2 || true
    exit 1
fi

# --- Seed cameras (optional; see seed_cameras.py for status) ---
if [[ -x "${REPO_ROOT}/scripts/soak/seed_cameras.py" ]]; then
    echo "Seeding cameras..."
    python3 "${REPO_ROOT}/scripts/soak/seed_cameras.py" \
        --config "${CONFIG}" \
        --output "${OUTPUT_DIR}/logs/seed.log" \
        || echo "WARNING: seed_cameras.py returned non-zero (continuing without seeded cameras)"
fi

# --- Sampling loop ---
START_TS=$(date +%s)
END_TS=$(awk -v s="${START_TS}" -v h="${DURATION_HOURS}" 'BEGIN { printf "%d", s + h * 3600 }')
CSV="${OUTPUT_DIR}/metrics/snapshot.csv"
echo "ts_unix,rss_kb,vsz_kb,cpu_pct,fd_count,thread_count,db_pool_in_use,db_pool_idle,uptime_sec" > "${CSV}"

echo "Sampling every ${SAMPLE_INTERVAL_SEC}s until $(date -d "@${END_TS}" 2>/dev/null || date -r "${END_TS}")"

sample_one() {
    local now rss vsz cpu fd threads pool_in_use pool_idle uptime
    now=$(date +%s)
    if ! kill -0 "${TF_PID}" 2>/dev/null; then
        echo "ERROR: tentaflow died at $(date)" >&2
        return 1
    fi
    # ps fields: rss(KiB), vsz(KiB), %cpu, nlwp (thread count)
    read -r rss vsz cpu threads < <(ps -p "${TF_PID}" -o rss=,vsz=,%cpu=,nlwp= 2>/dev/null | awk '{print $1,$2,$3,$4}')
    fd=$(ls -1 "/proc/${TF_PID}/fd" 2>/dev/null | wc -l | tr -d ' ')
    uptime=$(( now - START_TS ))
    pool_in_use=""
    pool_idle=""
    if command -v curl >/dev/null 2>&1; then
        if curl -sf --max-time 5 "${PROM_URL}" -o "${OUTPUT_DIR}/metrics/prom-raw.txt"; then
            pool_in_use=$(grep -E '^(sqlite_pool_in_use|tentaflow_db_pool_in_use)([{ ]|$)' \
                "${OUTPUT_DIR}/metrics/prom-raw.txt" | awk '{print $NF}' | head -n1)
            pool_idle=$(grep -E '^(sqlite_pool_idle|tentaflow_db_pool_idle)([{ ]|$)' \
                "${OUTPUT_DIR}/metrics/prom-raw.txt" | awk '{print $NF}' | head -n1)
        fi
    fi
    printf "%s,%s,%s,%s,%s,%s,%s,%s,%s\n" \
        "${now}" "${rss:-}" "${vsz:-}" "${cpu:-}" "${fd:-}" "${threads:-}" \
        "${pool_in_use:-}" "${pool_idle:-}" "${uptime}" >> "${CSV}"
}

while [[ "$(date +%s)" -lt "${END_TS}" ]]; do
    if ! sample_one; then
        echo "Sampling aborted (process gone)." >&2
        break
    fi
    # Sleep but allow early exit if process dies.
    for _ in $(seq 1 "${SAMPLE_INTERVAL_SEC}"); do
        kill -0 "${TF_PID}" 2>/dev/null || break 2
        sleep 1
    done
done

echo
echo "Soak loop finished. Stopping tentaflow gracefully..."
cleanup
trap - EXIT

# --- Post-run analysis ---
if [[ -x "${REPO_ROOT}/scripts/soak/analyze.py" ]]; then
    python3 "${REPO_ROOT}/scripts/soak/analyze.py" "${OUTPUT_DIR}" \
        | tee "${OUTPUT_DIR}/summary.txt"
else
    echo "analyze.py missing — raw CSV at ${CSV}"
fi

echo
echo "Done. Artifacts: ${OUTPUT_DIR}"
