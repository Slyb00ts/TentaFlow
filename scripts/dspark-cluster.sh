#!/bin/bash
# ===== File: scripts/dspark-cluster.sh — manual 2-node vLLM launcher for tuning =====
# Runs the same cluster TentaFlow's distributed deploy would, but directly from
# the native bundle, so a flag change costs seconds instead of a wizard walk plus
# the P0-P6 phase chain. Benchmarking and tuning ONLY -- production goes through
# the app, which owns port allocation, the service registry and teardown.
#
# Native, not docker: the bundle IS the artifact (see build-vllm-spark-venv.sh).
# Running it directly means what we measure and tune is exactly what the native
# deploy ships, with no image layer in between to drift.
#
# The NCCL/RoCE environment mirrors `distributed.rs::nccl_env` exactly: both RoCE
# twins in NCCL_IB_HCA (that is what gives the full ~200G), the per-node GID
# index (a property of the node's GID table, not a constant), and VLLM_HOST_IP
# pinned to the RDMA address so vLLM does not bind the 1G port.
#
# Uzycie:
#   dspark-cluster.sh up                      # start z domyslnym profilem
#   dspark-cluster.sh up --util 0.85 --seqs 8 # nadpisanie pojedynczych flag
#   dspark-cluster.sh up --extra "--foo 1"    # cokolwiek dodatkowego
#   dspark-cluster.sh logs | status | down
set -uo pipefail

BUNDLE="${BUNDLE:-/opt/TentaFlow/.runtime/bundles/vllm-spark}"
VENV="$BUNDLE/venv"
MODEL="${MODEL:-deepseek-ai/DeepSeek-V4-Flash-0731}"
MODELS_DIR="${MODELS_DIR:-/opt/TentaFlow/.runtime/models}"
CACHE_DIR="${CACHE_DIR:-/opt/TentaFlow/.runtime/cache/vllm}"

# Wezel 0 = head (trzyma endpoint + TCPStore), wezel 1 = worker (headless).
HEAD_IP="${HEAD_IP:-10.10.10.24}"
WORKER_SSH="${WORKER_SSH:-critix@rig25}"
WORKER_IP="${WORKER_IP:-10.10.10.25}"
IB_HCA="${IB_HCA:-roceP2p1s0f0,rocep1s0f0}"
SOCKET_IF="${SOCKET_IF:-enP2p1s0f0np0}"
GID="${GID:-3}"

PORT="${PORT:-8100}"
DIST_PORT="${DIST_PORT:-8101}"

UTIL="${UTIL:-0.80}"
# fp8_ds_mla is upstream's first-class packed DSV4 layout (448B NoPE + 128B RoPE
# + 8B scale = 584B/token) and the one the SM120 FlashInfer sparse kernel
# validates against. nvfp4_ds_mla is a third-party dtype that upstream does not
# thread through its ~20 layout decisions, so it silently degrades to bf16 rows.
KVDTYPE="${KVDTYPE:-fp8_ds_mla}"
# "dspark" albo "off" — do izolowania bledow dispatchu kerneli.
SPEC="${SPEC:-off}"
# "on"/"off" — parsery reasoning/tool-call dzialaja per token w strumieniu.
PARSERS="${PARSERS:-on}"
# "on"/"off" — async scheduling rozdziela execute_model i sample_tokens na osobne
# RPC; zawieszenia widzimy wlasnie w sample_tokens.
ASYNC="${ASYNC:-on}"
# "on"/"off" — cache prefiksow; czesciowe trafienie + prefill w kawalkach to
# najlepszy kandydat na zrodlo pelzania przy promptach > max-num-batched-tokens.
PREFIX="${PREFIX:-on}"
# "on"/"off" — grafy CUDA. Zawis siedzi w cudaEventSynchronize po async D2H
# copy; grafy sa ostatnia warstwa, ktora moze na to wplywac.
GRAPHS="${GRAPHS:-off}"
BREAKABLE="${BREAKABLE:-1}"
SEQS="${SEQS:-6}"
KVEC=5                       # = dspark_block_size checkpointu, NIE do strojenia
CAPTURE=""                   # domyslnie seqs*(k+1)
MAXLEN="${MAXLEN:-262144}"
BATCHED="${BATCHED:-8192}"
EXTRA=""

cmd="${1:-help}"; shift || true
while [ $# -gt 0 ]; do
  case "$1" in
    --util)    UTIL="$2"; shift 2;;
    --kv-dtype) KVDTYPE="$2"; shift 2;;
    --spec)    SPEC="$2"; shift 2;;
    --parsers) PARSERS="$2"; shift 2;;
    --async)   ASYNC="$2"; shift 2;;
    --prefix)  PREFIX="$2"; shift 2;;
    --graphs)  GRAPHS="$2"; shift 2;;
    --breakable) BREAKABLE="$2"; shift 2;;
    --seqs)    SEQS="$2"; shift 2;;
    --maxlen)  MAXLEN="$2"; shift 2;;
    --batched) BATCHED="$2"; shift 2;;
    --capture) CAPTURE="$2"; shift 2;;
    --extra)   EXTRA="$2"; shift 2;;
    *) echo "nieznana opcja: $1"; exit 2;;
  esac
done
# Capture MUSI byc wielokrotnoscia k+1, inaczej po zaokragleniu nie zostaje zaden
# prawidlowy rozmiar i silnik nie wstaje.
[ -n "$CAPTURE" ] || CAPTURE=$(( SEQS * (KVEC + 1) ))

# Generates the launch script for one rank. It goes through a FILE, never through
# `bash -c "<string>"`: the speculative-config JSON carries double quotes, and
# threading them through ssh + eval strips them silently -- vLLM then sees
# `{method:dspark,...}` and dies with an empty log.
write_launcher() {  # $1 = rank, $2 = this node's RDMA ip
  local rank="$1" ip="$2" headless="" SPEC_ARG=""
  [ "$SPEC" = "off" ] || SPEC_ARG="--speculative-config '{\"method\":\"$SPEC\",\"num_speculative_tokens\":$KVEC,\"draft_sample_method\":\"probabilistic\"}'"
  local PARSER_ARG="" ASYNC_ARG="" PREFIX_ARG="--no-enable-prefix-caching"
  local GRAPH_ARG="--max-cudagraph-capture-size $CAPTURE"
  [ "$GRAPHS" = "off" ] && GRAPH_ARG="--enforce-eager"
  [ "$PREFIX" = "off" ] || PREFIX_ARG="--enable-prefix-caching"
  # SchedulerConfig.async_scheduling defaults to None, which means ENABLED --
  # "If set to False, disable async scheduling". Omitting the flag therefore
  # disables nothing, so an earlier A/B that just dropped it tested nothing.
  if [ "$ASYNC" = "off" ]; then ASYNC_ARG="--no-async-scheduling"; else ASYNC_ARG="--async-scheduling"; fi
  [ "$PARSERS" = "off" ] || PARSER_ARG="--reasoning-parser deepseek_v4 --tool-call-parser deepseek_v4 --enable-auto-tool-choice"
  [ "$rank" = "0" ] || headless="--headless"
  cat > "$CACHE_DIR/serve-rank$rank.sh" <<LAUNCH
#!/bin/bash
set -x
export NCCL_IB_HCA=$IB_HCA NCCL_SOCKET_IFNAME=$SOCKET_IF GLOO_SOCKET_IFNAME=$SOCKET_IF
export NCCL_IB_DISABLE=0 NCCL_IB_GID_INDEX=$GID VLLM_HOST_IP=$ip
export HF_HUB_CACHE=$MODELS_DIR HF_HUB_OFFLINE=1
export VLLM_ALLOW_LONG_MAX_MODEL_LEN=1 VLLM_SKIP_INIT_MEMORY_CHECK=1
export PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True
export VLLM_FLASHINFER_AUTOTUNE_CACHE_DIR=$CACHE_DIR/fi-at
export TRITON_PTXAS_PATH=/usr/local/cuda/bin/ptxas
# vLLM auto-enables breakable cudagraphs on this platform and says so at
# startup. They are the newer capture mode and the prime suspect for the
# hang in cudaEventSynchronize after an async D2H copy; 0 opts out.
export VLLM_USE_BREAKABLE_CUDAGRAPH=$BREAKABLE
# vLLM JIT-compiles Triton kernels per SHAPE during inference (its own monitor
# says so: "_build_c128a_topk_metadata_kernel ... causes a latency spike"). On
# this aarch64/sm_121 pair a first-time compile can exceed the 300s default of
# VLLM_EXECUTE_MODEL_TIMEOUT_SECONDS, which applies only when TP > 1 -- exactly
# our case. The RPC then times out and takes EngineCore down, which looked like
# a random hang: a repeated prompt reuses a cached shape and is fast, while a
# new prompt length compiles and dies.
export VLLM_EXECUTE_MODEL_TIMEOUT_SECONDS=\${VLLM_EXECUTE_MODEL_TIMEOUT_SECONDS:-1800}
# Persist the compiled kernels so the cost is paid once, not per restart.
export TRITON_CACHE_DIR=$CACHE_DIR/triton
# nvcc MUST be on PATH. vLLM's has_flashinfer() reports FlashInfer as
# unavailable when there are no pre-built cubins AND nvcc is missing, because it
# then cannot JIT its kernels. On SM120 the DeepSeek V4 MLA path is
# FLASHINFER_MLA_SPARSE_DSV4, so without this the engine refuses to start with
# "requires FlashInfer's sparse MLA decode API" even though the API is present.
# We deliberately carry no flashinfer-cubin package: no build matching
# flashinfer-python 0.6.14 exists on PyPI, and JIT covers it.
# The venv's own bin must be on PATH too. Calling \$VENV/bin/vllm directly does
# NOT activate it, so FlashInfer's JIT could not find ninja -- which lives
# there -- and died with "[Errno 2] No such file or directory".
# NOTE: this heredoc is unquoted, so backticks and \$(...) here would be
# EXECUTED while generating the script. Keep prose free of both.
export PATH=$VENV/bin:/usr/local/cuda/bin:\$PATH
# Do NOT redirect FLASHINFER_CACHE_DIR here: \`python -m flashinfer
# download-cubin\` ignores it and writes to the default under \$HOME, so pointing
# the server elsewhere hides the prebuilt cubins and sends it back to JIT --
# which is what made long prompts return 500 with "RPC call to sample_tokens
# timed out" while short ones worked.
exec $VENV/bin/vllm serve $MODEL --host 0.0.0.0 --port $PORT \\
  --tensor-parallel-size 2 --pipeline-parallel-size 1 \\
  --distributed-executor-backend mp \\
  --nnodes 2 --node-rank $rank --master-addr $HEAD_IP --master-port $DIST_PORT $headless \\
  --trust-remote-code \\
  --kv-cache-dtype $KVDTYPE --block-size 256 \\
  --max-model-len $MAXLEN --max-num-seqs $SEQS \\
  --max-num-batched-tokens $BATCHED $GRAPH_ARG \\
  --gpu-memory-utilization $UTIL \\
  $PREFIX_ARG --enable-chunked-prefill $ASYNC_ARG \\
  $SPEC_ARG \\
  --tokenizer-mode deepseek_v4 $PARSER_ARG \\
  --override-generation-config '{"temperature":1.0,"top_p":0.95}' \\
  $EXTRA
LAUNCH
  chmod +x "$CACHE_DIR/serve-rank$rank.sh"
}

# A pidfile, never `pkill -f vllm`: a pattern broad enough to catch the server is
# also broad enough to match this script and the ssh command running it, which
# is how an earlier session killed its own shell.
start_node() {  # $1 = rank, $2 = ip, $3 = "" local | ssh target
  local rank="$1" ip="$2" via="${3:-}"
  local sh="$CACHE_DIR/serve-rank$rank.sh"
  local log="$CACHE_DIR/serve-rank$rank.log"
  local pidf="$CACHE_DIR/serve-rank$rank.pid"
  write_launcher "$rank" "$ip"
  # setsid so the recorded pid IS the process-group leader: a plain background
  # job inherits the shell's process group, and `kill -- -$pid` would then miss
  # the children (or hit the wrong group entirely).
  if [ -z "$via" ]; then
    setsid "$sh" > "$log" 2>&1 &
    echo $! > "$pidf"
  else
    scp -q "$sh" "$via:$sh"
    ssh "$via" "setsid $sh > $log 2>&1 & echo \$! > $pidf"
  fi
}

stop_node() {  # $1 = rank, $2 = "" local | ssh target
  local pidf="$CACHE_DIR/serve-rank$1.pid" via="${2:-}"
  # Kill the process GROUP: `vllm serve` forks EngineCore and worker children,
  # and killing only the parent leaves them holding the GPU and the port.
  local killer="p=\$(cat $pidf 2>/dev/null); [ -n \"\$p\" ] && kill -TERM -\$p 2>/dev/null; sleep 3; [ -n \"\$p\" ] && kill -KILL -\$p 2>/dev/null; rm -f $pidf; true"
  if [ -z "$via" ]; then bash -c "$killer"; else ssh "$via" "$killer"; fi
}

node_status() {  # $1 = rank, $2 = "" local | ssh target
  local pidf="$CACHE_DIR/serve-rank$1.pid" via="${2:-}"
  local q="p=\$(cat $pidf 2>/dev/null); if [ -n \"\$p\" ] && kill -0 \$p 2>/dev/null; then echo \"pid \$p dziala\"; else echo 'nie dziala'; fi"
  if [ -z "$via" ]; then bash -c "$q"; else ssh "$via" "$q"; fi
}

mkdir -p "$CACHE_DIR"

case "$cmd" in
  up)
    [ -x "$VENV/bin/vllm" ] || { echo "brak bundla: $VENV — uruchom scripts/build-vllm-spark-venv.sh"; exit 1; }
    echo "profil: util=$UTIL seqs=$SEQS capture=$CAPTURE maxlen=$MAXLEN batched=$BATCHED k=$KVEC kv=$KVDTYPE spec=$SPEC parsery=$PARSERS async=$ASYNC prefix=$PREFIX grafy=$GRAPHS breakable=$BREAKABLE"
    "$0" down >/dev/null 2>&1
    # Worker pierwszy: head binduje TCPStore master i od razu szuka rankow.
    start_node 1 "$WORKER_IP" "$WORKER_SSH" && echo "worker: start"
    sleep 3
    start_node 0 "$HEAD_IP" "" && echo "head:   start"
    echo "log: $0 logs   |   gotowosc: curl -s http://$HEAD_IP:$PORT/v1/models"
    ;;
  logs)   tail -f "$CACHE_DIR/serve-rank0.log";;
  wait)
    # Poll the API, never the log: a readiness string from a PREVIOUS run is
    # still in the file until the next start truncates it, and grepping for it
    # reports ready while the engine is loading weights -- which sends a whole
    # benchmark into connection-refused.
    deadline=$(( $(date +%s) + ${WAIT_SECS:-3600} ))
    while [ "$(date +%s)" -lt "$deadline" ]; do
      [ "$(curl -s -o /dev/null -w '%{http_code}' -m 5 "http://$HEAD_IP:$PORT/v1/models")" = "200" ] && {
        echo "gotowy"; exit 0; }
      p=$(cat "$CACHE_DIR/serve-rank0.pid" 2>/dev/null)
      [ -n "$p" ] && kill -0 "$p" 2>/dev/null || { echo "proces head padl"; exit 1; }
      sleep 15
    done
    echo "przekroczono czas oczekiwania"; exit 1;;
  status)
    printf "head   : %s\n" "$(node_status 0 '')"
    printf "worker : %s\n" "$(node_status 1 "$WORKER_SSH")"
    printf "api    : %s\n" "$(curl -s -o /dev/null -w '%{http_code}' -m 5 "http://$HEAD_IP:$PORT/v1/models")"
    ;;
  down)
    stop_node 0 ""
    stop_node 1 "$WORKER_SSH"
    echo "zatrzymane"
    ;;
  *) sed -n '2,20p' "$0";;
esac
