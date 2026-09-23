#!/usr/bin/env bash
# =============================================================================
# Plik: bus-gates.sh
# Opis: Uruchamia WSZYSTKIE bramki i benchmarki TentaBus w jednym miejscu —
#       funkcjonalne (cargo test/dotnet test/node --test) i wydajnosciowe
#       (P1/P4/P5/P6/P7/P9/P10/P11/P12/P13/1M/dedup-300k), zwracajac
#       niezerowy kod wyjscia gdy dowolna ZMIERZONA bramka progowa nie
#       przejdzie. Kanoniczna lista pojedynczych komend (do odpalenia jednej
#       bramki z osobna) zyje w SUM/tentabus/PLAN-APP-PLATFORM.md §9.2 —
#       ten skrypt jest kanonicznym sposobem odpalenia ICH WSZYSTKICH naraz
#       (SUM/tentabus/OTWARTE-POZYCJE.md, "Bramki wydajnosci":
#       `P11-P12-P1M-gate-commands-run-nothing`, `no-gate-builds-any-bench`,
#       `P1-P4-P5-P10-P13-bench-in-no-gate-list`).
#
#       Bramka P11 jest ZNANA jako czerwona (ponizej progu 20 000 msg/s na
#       tej klasie sprzetu — patrz PLAN-APP-PLATFORM.md §9.2) — ten skrypt
#       raportuje to wiernie, nie probuje tego naprawiac ani ukrywac.
#
# Przyklad:
#   ./scripts/bus-gates.sh                              # wszystko, domyslny profil release
#   BUS_GATES_PROFILE=release-fast ./scripts/bus-gates.sh   # mniej pamieci przy budowie (patrz CLAUDE.md, macOS trap)
#   BUS_GATES_SKIP_FUNCTIONAL=1 ./scripts/bus-gates.sh   # tylko bramki wydajnosciowe
#   BUS_GATES_RUN_ALL_BENCHES=1 ./scripts/bus-gates.sh   # odpal tez pelne przebiegi
#                                                         # benchow, ktore domyslnie
#                                                         # sa tylko kompilowane (--no-run)
# =============================================================================
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT"

LOG_DIR="$(mktemp -d "${TMPDIR:-/tmp}/bus-gates.XXXXXX")"
trap 'rm -rf "$LOG_DIR"' EXIT

# ---- profil kompilacji ------------------------------------------------------
# Domyslnie `--release` (dosl. tekst komend w PLAN-APP-PLATFORM.md §9.2).
# `release-fast` (opt-level 3, LTO off) zuzywa mniej RAM przy linkowaniu
# (M1-WYNIKI: pelny --release/ThinLTO potrzebuje ~14,5 GiB na jednostke
# kodogeneracji) — patrz CLAUDE.md, "macOS trap" i sekcja P11 w
# PLAN-APP-PLATFORM.md dla historycznego kontekstu tego wyboru.
BUS_GATES_PROFILE="${BUS_GATES_PROFILE:-release}"
if [[ "$BUS_GATES_PROFILE" == "release" ]]; then
    CARGO_PROFILE_FLAG=(--release)
    CARGO_PROFILE_DIR="release"
else
    CARGO_PROFILE_FLAG=(--profile "$BUS_GATES_PROFILE")
    CARGO_PROFILE_DIR="$BUS_GATES_PROFILE"
fi
TARGET_DIR="$ROOT/target_shared/$CARGO_PROFILE_DIR"

BUS_GATES_SKIP_FUNCTIONAL="${BUS_GATES_SKIP_FUNCTIONAL:-0}"
BUS_GATES_RUN_ALL_BENCHES="${BUS_GATES_RUN_ALL_BENCHES:-0}"

# ---- natywne biblioteki dynamiczne (whisper/zvec) --------------------------
# `tentaflow/build.rs` kopiuje swiezy `.so`/`.dylib` obok BINARKI APLIKACJI
# (patrz CLAUDE.md, sekcja "Native libraries" / INVARIANT symbol isolation);
# binarki testow/benchow crate'u tentaflow-core NIE przechodza przez ten
# krok, wiec uruchomione WPROST (nie przez `cargo test`/`cargo bench`, ktore
# same odpalaja ten sam proces) musza wskazac katalog `lib-dynamic` recznie.
UNAME_S="$(uname -s)"
UNAME_M="$(uname -m)"
case "$UNAME_S" in
    Darwin) NATIVE_PLATFORM="macos-${UNAME_M}" ;;
    Linux) NATIVE_PLATFORM="linux-${UNAME_M}" ;;
    *) NATIVE_PLATFORM="" ;;
esac
NATIVE_LIB_DIR="$ROOT/native-libs/$NATIVE_PLATFORM/lib-dynamic"
DYLIB_ENV=()
if [[ -d "$NATIVE_LIB_DIR" ]]; then
    if [[ "$UNAME_S" == "Darwin" ]]; then
        DYLIB_ENV=(env "DYLD_LIBRARY_PATH=$NATIVE_LIB_DIR")
    else
        DYLIB_ENV=(env "LD_LIBRARY_PATH=$NATIVE_LIB_DIR")
    fi
fi

# ---- bookkeeping -------------------------------------------------------------
declare -a GATE_NAMES=()
declare -a GATE_STATUS=()   # PASS / FAIL / BLOCKED
FAIL=0

log() { echo "[bus-gates] $*"; }

record() {
    local name="$1" status="$2"
    GATE_NAMES+=("$name")
    GATE_STATUS+=("$status")
    case "$status" in
        PASS) echo "  PASS    $name" ;;
        FAIL) echo "  FAIL    $name"; FAIL=1 ;;
        BLOCKED) echo "  BLOCKED $name" ;;
    esac
}

# Regex of measured-number lines worth echoing to stdout for a performance
# gate, whether it passed or failed. A gate suite whose whole subject is
# throughput/latency must SHOW the numbers — "PASS" alone tells a reader
# nothing about how much headroom (or deficit) a run actually had, which is
# exactly the gap that let "18,8k msg/s vs a 20k gate" sit recorded as a
# green run for months (PLAN-APP-PLATFORM.md §9.2's own post-mortem).
PERF_NUMBER_PATTERN='msg/s|ops/s|MiB/s|p50|p99|verdict|PASS|FAIL|gate result|deleted_segments|RESULT '

# Runs a command, tees output to a per-gate log, records PASS/FAIL by exit
# code. Used for gates that already assert their own threshold internally
# (P11/P12/1M/bus_dedup_perf) — propagate the process exit code rather than
# re-implementing threshold parsing. `show_numbers` (2nd arg: 1/0) echoes
# every `PERF_NUMBER_PATTERN` line from the log even on success.
run_gate_by_exit_code() {
    local name="$1" show_numbers="$2"
    shift 2
    local logfile="$LOG_DIR/${name//[^A-Za-z0-9_.-]/_}.log"
    log "running: $name"
    log "  \$ $*"
    if "$@" >"$logfile" 2>&1; then
        record "$name" PASS
        if [[ "$show_numbers" == "1" ]]; then
            grep -E "$PERF_NUMBER_PATTERN" "$logfile" | sed 's/^/    /' || true
        fi
    else
        record "$name" FAIL
        if [[ "$show_numbers" == "1" ]]; then
            grep -E "$PERF_NUMBER_PATTERN" "$logfile" | sed 's/^/    /' || true
        fi
        echo "  --- tail of $logfile ---"
        tail -n 30 "$logfile" | sed 's/^/    /'
    fi
    echo "  log: $logfile"
}

# Same as above but a nonzero exit is not itself a failure (a missing tool):
# records BLOCKED instead of FAIL.
run_optional_gate() {
    local name="$1"
    shift
    if ! command -v "$1" >/dev/null 2>&1; then
        record "$name" BLOCKED
        echo "  (missing: $1)"
        return
    fi
    run_gate_by_exit_code "$name" 0 "$@"
}

# Runs a command whose own exit code is always 0 (bench harnesses like
# bus_replication.rs's P6/P7/P9 that only ever `eprintln!` a verdict —
# see that file's own module doc) and instead greps the captured log for a
# clear PASS/FAIL verdict line per the task's explicit fallback.
run_gate_by_output_verdict() {
    local name="$1" pattern="$2"
    shift 2
    local logfile="$LOG_DIR/${name//[^A-Za-z0-9_.-]/_}.log"
    log "running: $name"
    log "  \$ $*"
    "$@" >"$logfile" 2>&1
    local exit_code=$?
    if [[ $exit_code -ne 0 ]]; then
        record "$name" FAIL
        echo "  process exited $exit_code — see log"
    elif grep -Eq "$pattern" "$logfile"; then
        local verdicts
        verdicts=$(grep -E "$pattern" "$logfile")
        if echo "$verdicts" | grep -q "FAIL"; then
            record "$name" FAIL
        else
            record "$name" PASS
        fi
        # Print the full measured picture, not only the verdict lines — the
        # raw msg/s / MiB/s / p99 numbers are what a report needs (see
        # `PERF_NUMBER_PATTERN`'s own comment).
        grep -E "$PERF_NUMBER_PATTERN" "$logfile" | sed 's/^/    /' || true
    else
        record "$name" BLOCKED
        echo "  no verdict line matched in log (gate_blocked / trio never reached quorum-ready ISR?)"
    fi
    echo "  log: $logfile"
}

# Finds the freshest built test/bench binary matching a name prefix under
# $TARGET_DIR/deps — never hardcode the rustc-hash suffix (it changes on
# every dependency-graph-affecting rebuild). `ls -t` (portable across BSD
# and GNU `ls`, unlike `find -printf`/`stat -f` which differ per platform)
# lists matches newest-first; the first regular, executable, non-`.d` one
# is the binary `cargo bench --no-run` just produced.
newest_binary() {
    local prefix="$1"
    local candidate
    while IFS= read -r candidate; do
        [[ "$candidate" == *.d ]] && continue
        [[ -f "$candidate" && -x "$candidate" ]] || continue
        echo "$candidate"
        return
    done < <(ls -t "$TARGET_DIR"/deps/"${prefix}"-* 2>/dev/null)
}

echo "=========================================================================="
echo " TentaBus gate suite — profile=$BUS_GATES_PROFILE  target_dir=$TARGET_DIR"
echo "=========================================================================="

# =============================================================================
# 1. Functional / correctness suites (PLAN-APP-PLATFORM.md §9.2, non-
#    performance block). No profile flag — matches the documented commands
#    verbatim.
# =============================================================================
if [[ "$BUS_GATES_SKIP_FUNCTIONAL" != "1" ]]; then
    echo ""
    echo "-- Functional / correctness --------------------------------------------"

    run_gate_by_exit_code "functional: bus::" 0 \
        cargo test -p tentaflow-core --features test-support -- bus::
    run_gate_by_exit_code "functional: dispatch::bus" 0 \
        cargo test -p tentaflow-core --features test-support -- dispatch::bus
    run_gate_by_exit_code "functional: dispatch::app_gate" 0 \
        cargo test -p tentaflow-core --features test-support -- dispatch::app_gate
    run_gate_by_exit_code "functional: sync::" 0 \
        cargo test -p tentaflow-core --features test-support -- sync::
    run_gate_by_exit_code "functional: db::migrations" 0 \
        cargo test -p tentaflow-core --features test-support -- db::migrations
    run_gate_by_exit_code "functional: addon::" 0 \
        cargo test -p tentaflow-core --features test-support -- addon::
    run_gate_by_exit_code "functional: services::bus_authorizer" 0 \
        cargo test -p tentaflow-core --features test-support -- services::bus_authorizer
    run_gate_by_exit_code "functional: services::metrics_export" 0 \
        cargo test -p tentaflow-core --features test-support -- services::metrics_export
    run_gate_by_exit_code "functional: native_app_lifecycle" 0 \
        cargo test -p tentaflow-core --features test-support --test native_app_lifecycle
    run_gate_by_exit_code "functional: native_app_multi_instance" 0 \
        cargo test -p tentaflow-core --features test-support --test native_app_multi_instance
    run_gate_by_exit_code "functional: tentabus_two_instances" 0 \
        cargo test -p tentaflow-core --test tentabus_two_instances
    run_gate_by_exit_code "functional: bus_addon_integration_e2e" 0 \
        cargo test -p tentaflow-core --test bus_addon_integration_e2e
    run_gate_by_exit_code "functional: bus_replication_three_node" 0 \
        cargo test -p tentaflow-core --test bus_replication_three_node
    run_gate_by_exit_code "functional: bus_demo_seed" 0 \
        cargo test -p tentaflow-core --test bus_demo_seed
    run_gate_by_exit_code "functional: tentaflow-protocol" 0 \
        cargo test -p tentaflow-protocol
    run_gate_by_exit_code "functional: tentaflow-sdk-spec" 0 \
        cargo test -p tentaflow-sdk-spec
    run_gate_by_exit_code "functional: tentaflow-protocol-wasm" 0 \
        cargo test -p tentaflow-protocol-wasm

    run_optional_gate "functional: dotnet test tentaflow-sdk-dotnet" \
        dotnet test "$ROOT/tentaflow-sdk-dotnet"

    if command -v node >/dev/null 2>&1 && [[ -f "$ROOT/tentaflow-core/www/js/modules/tentabus.request-builders.test.js" ]]; then
        # `npm test`'s own script (tentaflow-core/www/package.json) runs
        # `node --test` with paths relative to that directory — `cd` there
        # first rather than passing absolute paths, matching that exactly.
        run_gate_by_exit_code "functional: tentabus.request-builders.test.js" 0 \
            bash -c 'cd "$1" && node --test --import ./js/_test-register.js --test-reporter=spec js/modules/tentabus.request-builders.test.js' \
            _ "$ROOT/tentaflow-core/www"
    else
        record "functional: tentabus.request-builders.test.js" BLOCKED
        echo "  (missing: node, or the test file itself)"
    fi
else
    log "BUS_GATES_SKIP_FUNCTIONAL=1 — skipping functional/correctness suites"
fi

# =============================================================================
# 2. The three ignored performance gates (OTWARTE-POZYCJE.md
#    `P11-P12-P1M-gate-commands-run-nothing`). `bus_flow_chain_p11_gate`
#    needs `test-support` (its `required-features` in Cargo.toml);
#    `bus_addon_p12_gate` and `bus_full_path_1m` do not.
# =============================================================================
echo ""
echo "-- Performance: P11 / P12 / 1M gates ------------------------------------"

run_gate_by_exit_code "perf: P11 bus_flow_chain_p11_gate (>=20000 msg/s)" 1 \
    cargo test -p tentaflow-core "${CARGO_PROFILE_FLAG[@]}" --features test-support \
        --test bus_flow_chain_p11_gate -- --ignored --nocapture

run_gate_by_exit_code "perf: P12 bus_addon_p12_gate (>=50000 msg/s)" 1 \
    cargo test -p tentaflow-core "${CARGO_PROFILE_FLAG[@]}" \
        --test bus_addon_p12_gate -- --ignored --nocapture

run_gate_by_exit_code "perf: bus_full_path_1m" 1 \
    cargo test -p tentaflow-core "${CARGO_PROFILE_FLAG[@]}" \
        --test bus_full_path_1m -- --ignored --nocapture

# =============================================================================
# 3. Replication gates P6/P7/P9 (bus_replication.rs). Compiled via
#    `cargo bench --no-run`, then the compiled binary is invoked DIRECTLY
#    (PLAN-APP-PLATFORM.md §9.2's own note: no LC_RPATH on this bench
#    binary, and wrapping the direct invocation in `sh -c` lets macOS SIP
#    strip DYLD_* — so this script never does that). None of P6/P7/P9
#    `assert!` internally (they only `eprintln!` a verdict — see
#    `gate_p6`/`gate_p7`/`gate_p9`'s own doc comments), so this uses the
#    output-verdict fallback.
# =============================================================================
echo ""
echo "-- Performance: P6 / P7 / P9 replication gates --------------------------"

log "compiling bus_replication ($BUS_GATES_PROFILE)..."
if cargo bench -p tentaflow-core --no-run --bench bus_replication "${CARGO_PROFILE_FLAG[@]}" \
    >"$LOG_DIR/bus_replication_compile.log" 2>&1; then
    BUS_REPL_BIN="$(newest_binary bus_replication)"
    if [[ -z "$BUS_REPL_BIN" ]]; then
        record "perf: P6/P7/P9 bus_replication" FAIL
        echo "  compiled but no bus_replication-<hash> binary found under $TARGET_DIR/deps"
    else
        run_gate_by_output_verdict "perf: P6/P7/P9 bus_replication" \
            '^P[679][a-z ]* verdict.*(PASS|FAIL)' \
            "${DYLIB_ENV[@]}" "$BUS_REPL_BIN"
    fi
else
    record "perf: P6/P7/P9 bus_replication" FAIL
    echo "  compile failed — see $LOG_DIR/bus_replication_compile.log"
    tail -n 30 "$LOG_DIR/bus_replication_compile.log" | sed 's/^/    /'
fi

# =============================================================================
# 4. dedup-300k gate (bus_dedup_perf.rs, TentaBus 1C
#    `dedup-300k-gate-unbenchmarked`) — replaces the abandoned fjall path in
#    tentaflow-bus/benches/meta_perf.rs. Asserts internally
#    (gate_dedup_300k's own `assert!`), so its exit code is authoritative.
# =============================================================================
echo ""
echo "-- Performance: dedup-300k gate (MmapDedupStore) ------------------------"

log "compiling bus_dedup_perf ($BUS_GATES_PROFILE)..."
if cargo bench -p tentaflow-core --no-run --bench bus_dedup_perf "${CARGO_PROFILE_FLAG[@]}" \
    >"$LOG_DIR/bus_dedup_perf_compile.log" 2>&1; then
    DEDUP_BIN="$(newest_binary bus_dedup_perf)"
    if [[ -z "$DEDUP_BIN" ]]; then
        record "perf: dedup-300k bus_dedup_perf" FAIL
        echo "  compiled but no bus_dedup_perf-<hash> binary found under $TARGET_DIR/deps"
    else
        run_gate_by_exit_code "perf: dedup-300k bus_dedup_perf" 1 \
            "${DYLIB_ENV[@]}" "$DEDUP_BIN"
    fi
else
    record "perf: dedup-300k bus_dedup_perf" FAIL
    echo "  compile failed — see $LOG_DIR/bus_dedup_perf_compile.log"
    tail -n 30 "$LOG_DIR/bus_dedup_perf_compile.log" | sed 's/^/    /'
fi

# =============================================================================
# 5. Every OTHER bench target that no gate previously touched
#    (OTWARTE-POZYCJE.md `no-gate-builds-any-bench`,
#    `P1-P4-P5-P10-P13-bench-in-no-gate-list`): bus_path (P1/P4/P5/P10/P13)
#    plus tentaflow-bus's log_perf/device_ceiling/read_perf/meta_perf.
#    Compiled by default (`--no-run`) to keep this script's default runtime
#    bounded on a shared host; set BUS_GATES_RUN_ALL_BENCHES=1 to actually
#    execute them (bus_path's P13 can run several minutes — see its own
#    module doc and `TENTABUS_P13_TARGET_GIB` for the full 10 GiB scale).
# =============================================================================
echo ""
echo "-- Bench compile/run: bus_path (P1/P4/P5/P10/P13) -----------------------"
if [[ "$BUS_GATES_RUN_ALL_BENCHES" == "1" ]]; then
    run_gate_by_exit_code "bench: bus_path (full run)" 1 \
        cargo bench -p tentaflow-core --bench bus_path "${CARGO_PROFILE_FLAG[@]}" -- --noplot
else
    run_gate_by_exit_code "bench: bus_path (compile-only)" 0 \
        cargo bench -p tentaflow-core --no-run --bench bus_path "${CARGO_PROFILE_FLAG[@]}"
fi

echo ""
echo "-- Bench compile/run: tentaflow-bus (log_perf/device_ceiling/read_perf/meta_perf) --"
for bench in log_perf device_ceiling read_perf meta_perf; do
    if [[ "$BUS_GATES_RUN_ALL_BENCHES" == "1" ]]; then
        run_gate_by_exit_code "bench: tentaflow-bus/$bench (full run)" 1 \
            cargo bench -p tentaflow-bus --bench "$bench" "${CARGO_PROFILE_FLAG[@]}" -- --noplot
    else
        run_gate_by_exit_code "bench: tentaflow-bus/$bench (compile-only)" 0 \
            cargo bench -p tentaflow-bus --no-run --bench "$bench" "${CARGO_PROFILE_FLAG[@]}"
    fi
done

# =============================================================================
# 6. Cross-process p99 publish->consume example (OTWARTE-POZYCJE.md
#    `bus-e2e-bench-example-missing`). Compiled by default; run fully under
#    BUS_GATES_RUN_ALL_BENCHES=1 (spawns two real child processes,
#    publishes/consumes a few thousand records — bounded, but not
#    instantaneous, hence gated the same way as the benches above).
# =============================================================================
echo ""
echo "-- Example: bus_e2e_bench (cross-process P4) -----------------------------"
if [[ -f "$ROOT/tentaflow-core/examples/bus_e2e_bench.rs" ]]; then
    if [[ "$BUS_GATES_RUN_ALL_BENCHES" == "1" ]]; then
        run_gate_by_exit_code "example: bus_e2e_bench (full run)" 1 \
            cargo run -p tentaflow-core "${CARGO_PROFILE_FLAG[@]}" --example bus_e2e_bench
    else
        run_gate_by_exit_code "example: bus_e2e_bench (compile-only)" 0 \
            cargo build -p tentaflow-core "${CARGO_PROFILE_FLAG[@]}" --example bus_e2e_bench
    fi
else
    record "example: bus_e2e_bench" BLOCKED
    echo "  (tentaflow-core/examples/bus_e2e_bench.rs does not exist yet)"
fi

# =============================================================================
# Summary
# =============================================================================
echo ""
echo "=========================================================================="
echo " Summary"
echo "=========================================================================="
printf "  %-8s  %s\n" "STATUS" "GATE"
for i in "${!GATE_NAMES[@]}"; do
    printf "  %-8s  %s\n" "${GATE_STATUS[$i]}" "${GATE_NAMES[$i]}"
done
echo ""
echo "  logs kept at: $LOG_DIR (removed when this script exits — copy anything"
echo "  you want to keep before it does)"
trap - EXIT

if [[ $FAIL -ne 0 ]]; then
    echo ""
    echo "[bus-gates] AT LEAST ONE GATE FAILED — see FAIL rows above."
    rm -rf "$LOG_DIR"
    exit 1
fi
echo ""
echo "[bus-gates] all measured gates passed (BLOCKED gates need their tool installed to actually run)."
rm -rf "$LOG_DIR"
exit 0
