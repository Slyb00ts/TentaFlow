# =============================================================================
# ds4 (antirez/ds4) — DeepSeek V4 Flash/Pro, CUDA build (Linux). Buduje
# ds4-server ze źródeł w obrazie CUDA devel i serwuje OpenAI-compatible API.
# transport=direct-http: Core gada HTTP wprost do host-mapped portu.
#
# UWAGA build-arch: `cuda-generic` ustawia CUDA_ARCH=native, co przy `docker
# build` BEZ widocznego GPU może nie wykryć architektury. Na hoście bez GPU w
# czasie builda podaj DS4_CUDA_ARCH (np. sm_100 dla Blackwell/B300) — wtedy
# leci `make cuda CUDA_ARCH=$DS4_CUDA_ARCH`. DGX Spark/GB10 buduj jako native
# (deploy.native, make cuda-spark), nie przez ten obraz.
# =============================================================================
ARG CUDA_IMAGE=nvidia/cuda:13.0.3-cudnn-devel-ubuntu24.04
FROM ${CUDA_IMAGE}

ARG DS4_REF=main
ARG DS4_MAKE_TARGET=cuda-generic
ARG DS4_CUDA_ARCH=

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential git curl ca-certificates python3 python3-pip \
    && rm -rf /var/lib/apt/lists/*

# Oficjalny HF downloader dla wariantów PRO (download_model.sh używa `hf`).
RUN pip3 install --no-cache-dir --break-system-packages -U "huggingface_hub" "hf_xet" || \
    pip3 install --no-cache-dir -U "huggingface_hub" "hf_xet"

WORKDIR /opt
RUN git clone https://github.com/antirez/ds4.git ds4 \
    && cd ds4 && git checkout "${DS4_REF}" || true

WORKDIR /opt/ds4
RUN if [ -n "${DS4_CUDA_ARCH}" ]; then \
        make cuda CUDA_ARCH="${DS4_CUDA_ARCH}"; \
    else \
        make "${DS4_MAKE_TARGET}"; \
    fi \
    && test -x ./ds4-server

# Persystentny katalog na wagi GGUF (montowany jako wolumen przez Core).
ENV DS4_GGUF_DIR=/models
RUN mkdir -p /models

COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

EXPOSE 8000
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
