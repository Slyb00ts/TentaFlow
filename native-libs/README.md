# Native Libraries

Ten katalog jest miejscem na gotowe artefakty natywnych bibliotek per platforma.
Źródła pobierane przez skrypty nie trafiają do repozytorium: domyślny cache to
`/tmp/tentaflow-native-libs`, a można go zmienić przez `TENTAFLOW_NATIVE_CACHE`.

## Budowanie

Linux/macOS:

```bash
./scripts/native-libs/build-all.sh
```

Windows:

```powershell
.\scripts\native-libs\build-all.ps1
```

Skrypty wykrywają platformę automatycznie i zapisują wynik w:

```text
native-libs/<platform>/
├── include/
├── lib-static/
├── lib-dynamic/
└── manifest.toml
```

## Zasada linkowania

- `lib-static/` zawiera biblioteki preferowane do statycznego linkowania.
- `lib-dynamic/` zawiera biblioteki, których nie da się sensownie zlinkować
  statycznie albo które wymagają runtime loadera systemu.
- `tentaflow/build.rs` kopiuje zawartość `lib-dynamic/` obok budowanej binarki,
  żeby lokalny build miał dynamiczne zależności w jednym miejscu.

`llama.cpp` domyślnie buduje wariant `multi`, czyli jeden zestaw bibliotek z
wszystkimi wykrytymi backendami GPU. Domyślny ref to `master`, więc
`--update` pobiera aktualny upstream; można go przypiąć przez `LLAMA_CPP_REF`.
Domyślnie `LLAMA_CPP_BACKENDS=auto`
wykrywa CUDA, Vulkan i CPU na Linux/Windows oraz Metal i CPU na macOS.
Kart AMD i Intel nie budujemy przez HIP/ROCm — jadą na Vulkanie.
Można wymusić osobne warianty diagnostyczne:

```bash
LLAMA_CPP_BACKENDS=cuda,vulkan,cpu ./scripts/native-libs/build-all.sh --only llama-cpp
```

CUDA build wyłącza launchery kompilatora typu `sccache`, bo `nvcc`/`fatbinary`
potrafią wtedy gubić tymczasowe pliki `*.cubin`. Domyślna równoległość CUDA to
`LLAMA_CPP_CUDA_JOBS=4`; można ją zmienić:

```bash
LLAMA_CPP_CUDA_JOBS=2 LLAMA_CPP_BACKENDS=cuda ./scripts/native-libs/build-all.sh --only llama-cpp
```

`whisper.cpp` używa analogicznego modelu. Osobne warianty diagnostyczne można
wymusić:

```bash
WHISPER_CPP_BACKENDS=cuda,vulkan,cpu ./scripts/native-libs/build-all.sh --only whisper-cpp
```

## ONNX Runtime GPU (CUDA / TensorRT, NVIDIA B300)

`build-onnxruntime.sh` provisions the runtime dlopened by the `ort` crate
(load-dynamic): `lib-dynamic/libonnxruntime.so.<ver>` plus the
`libonnxruntime_providers_{shared,cuda,tensorrt}.so` execution providers. On
`linux-x86_64` it downloads the official GPU release and verifies a pinned
SHA-256. The CUDA line is auto-detected from the driver
(`ONNXRUNTIME_CUDA=auto|12|13`): CUDA 13-capable drivers get the `gpu_cuda13`
artifact, which is required for SM_103 (B300, Blackwell Ultra).

```bash
./scripts/native-libs/build-all.sh --only onnxruntime           # prebuilt (default)
./scripts/native-libs/build-onnxruntime.sh linux-x86_64 --from-source  # native SM_103 cubins
```

For the `gpu_cuda13` variant the script also vendors the full NVIDIA runtime
stack from the official NVIDIA wheels (pypi.nvidia.com / pypi.org, SHA-256
pinned):

- TensorRT: `libnvinfer.so.10` + `libnvinfer_plugin.so.10` +
  `libnvonnxparser.so.10` + the builder resource for the target SM
  (per-SM split since TRT 10.15; sm100 + ptx ≈ 1.24 GB),
- the full cuDNN 9 split-lib set (≈ 0.97 GB),
- the CUDA toolkit runtime libs the EPs DT_NEED and a driver-only host lacks:
  `libcudart.so.13`, `libcublas.so.13` + `libcublasLt.so.13`,
  `libcufft.so.12`, `libcurand.so.10` (≈ 0.96 GB, from the
  `nvidia-cuda-runtime` / `nvidia-cublas` / `nvidia-cufft` / `nvidia-curand`
  wheels — the CUDA 13 line dropped the `-cuXX` package suffix).

Everything lands flat in `lib-dynamic/`, so `tentaflow/build.rs` copies it
next to the binary and the loader resolves it via `$ORIGIN` — **no system
TensorRT, cuDNN or CUDA toolkit install is needed on the target host; an
NVIDIA R580+ driver (CUDA 13 compatible) is the only host requirement**.
Expect ~3.2 GB in `lib-dynamic/` (and again next to the binary).

- `TENTAFLOW_SKIP_TRT_VENDOR=1` skips ALL vendoring on hosts that have a
  system TensorRT/cuDNN/CUDA toolkit.
- `TENTAFLOW_SKIP_CUDA_VENDOR=1` skips only the CUDA toolkit libs (host has
  the toolkit installed but no TensorRT/cuDNN).
- `TENSORRT_SMS` picks builder-resource buckets (`auto` detects the local
  GPU; use `TENSORRT_SMS=sm100` when provisioning for a B300 from another
  machine; `all` vendors every SM bucket, ~1.9 GB extra).
- `TENSORRT_VENDOR_REF` / `CUDNN_VENDOR_REF` / `CUDART_VENDOR_REF` /
  `CUBLAS_VENDOR_REF` / `CUFFT_VENDOR_REF` / `CURAND_VENDOR_REF` override the
  pinned wheel versions (for TRT/cuDNN provide `*_VENDOR_SHA256` to keep
  checksum verification; other overrides warn loudly and skip verification).

Prebuilt CUDA 13 binaries ship PTX, so kernels JIT-compile on SM_103 (slower
first session load). `--from-source` builds native cubins
(`ONNXRUNTIME_CUDA_ARCHS=103` by default) with `CUDA_HOME` + `TENSORRT_HOME`
pointing at a CUDA 13.x toolkit and TensorRT >= 10.13.

Ciężkie pliki `.a`, `.so`, `.dylib`, `.dll`, `.lib`, `.framework` i `.bundle`
w `native-libs/` są odblokowane w `.gitignore`, żeby maintainer mógł zbudować
je raz na właściwej maszynie i dodać do repo. Cache źródeł i katalogi buildów
pozostają poza repo.
