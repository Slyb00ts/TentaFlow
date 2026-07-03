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
wykrywa CUDA, ROCm/HIP, Vulkan i CPU na Linux/Windows oraz Metal i CPU na macOS.
Można wymusić osobne warianty diagnostyczne:

```bash
LLAMA_CPP_BACKENDS=cuda,vulkan,rocm,cpu ./scripts/native-libs/build-all.sh --only llama-cpp
```

CUDA build wyłącza launchery kompilatora typu `sccache`, bo `nvcc`/`fatbinary`
potrafią wtedy gubić tymczasowe pliki `*.cubin`. Domyślna równoległość CUDA to
`LLAMA_CPP_CUDA_JOBS=4`; można ją zmienić:

```bash
LLAMA_CPP_CUDA_JOBS=2 LLAMA_CPP_BACKENDS=cuda ./scripts/native-libs/build-all.sh --only llama-cpp
```

`whisper.cpp` używa analogicznego modelu, ale domyślne `auto` nie dodaje ROCm/HIP:
na AMD używamy Vulkanu, bo HIP w wybranych wersjach `whisper.cpp` i ROCm potrafi
nie przejść kompilacji na `hipblasGemmEx`. Osobne warianty diagnostyczne można
wymusić:

```bash
WHISPER_CPP_BACKENDS=cuda,vulkan,rocm,cpu ./scripts/native-libs/build-all.sh --only whisper-cpp
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

Runtime host requirements for the `gpu_cuda13` variant (not bundled):

- NVIDIA driver R580+ (CUDA 13 compatible),
- cuDNN 9 for the CUDA EP,
- TensorRT >= 10.13 for the TensorRT EP — the provider dlopens
  `libnvinfer.so.10` / `libnvonnxparser.so.10` at runtime; without them the
  detector falls back to the CUDA EP gracefully.

Prebuilt CUDA 13 binaries ship PTX, so kernels JIT-compile on SM_103 (slower
first session load). `--from-source` builds native cubins
(`ONNXRUNTIME_CUDA_ARCHS=103` by default) with `CUDA_HOME` + `TENSORRT_HOME`
pointing at a CUDA 13.x toolkit and TensorRT >= 10.13.

Ciężkie pliki `.a`, `.so`, `.dylib`, `.dll`, `.lib`, `.framework` i `.bundle`
w `native-libs/` są odblokowane w `.gitignore`, żeby maintainer mógł zbudować
je raz na właściwej maszynie i dodać do repo. Cache źródeł i katalogi buildów
pozostają poza repo.
