# =============================================================================
# Plik: Dockerfile (vllm-spark-ray)
# Opis: CIENKA warstwa nad JUZ ZBUDOWANYM obrazem from-source `vllm-spark`
#       (vLLM 0.27 + torch sm_121a). Dodaje TYLKO rdma-core/libibverbs (RoCE dla
#       NCCL) i Ray 2.55.1 — wymagane do multi-node tensor-parallel (head+worker,
#       --distributed-executor-backend ray). Buduje sie w ~1-2 min (potem cache),
#       zamiast 20-40 min rebuildu vLLM ze zrodel. Uzywany WYLACZNIE przez deploy
#       rozproszony (D3); single-node vllm-spark dalej idzie na czystej bazie.
#
# Baza jest przekazywana przez build-arg `BASE_IMAGE` (Core wybiera realny tag
# bazy obecny na nodzie); jej brak = czytelny blad w preflight (NIE rebuild).
# =============================================================================

ARG BASE_IMAGE=tentaflow/vllm-spark:0.27.1
FROM ${BASE_IMAGE}

# rdma-core/libibverbs: userspace IB verbs — bez nich NCCL nie otworzy
# libibverbs.so i multi-node TP spadnie z RoCE/ConnectX na wolny TCP.
RUN apt-get update && apt-get install -y --no-install-recommends \
      libibverbs1 librdmacm1 ibverbs-providers rdma-core \
    && rm -rf /var/lib/apt/lists/*

# Ray w TYM SAMYM venv co vLLM (`/opt/vllm-venv`). 2.55.1 ma kola aarch64 i jest
# kompatybilny z vllm 0.27 / torch 2.13.
RUN /opt/vllm-venv/bin/pip install --no-cache-dir ray==2.55.1

# ENTRYPOINT / ENV / HEALTHCHECK dziedziczone z bazy (entrypoint.sh obsluguje
# ENGINE_LAUNCH_CMD, ktorego uzywa deploy rozproszony do `ray start && vllm serve`).
