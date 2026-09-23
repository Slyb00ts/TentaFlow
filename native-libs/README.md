# Native Libraries

Ten katalog jest miejscem na gotowe artefakty natywnych bibliotek per platforma.
Nic poza tym plikiem nie trafia do repozytorium — każdy buduje je lokalnie.
Źródła i pobrane archiwa leżą poza repo w `TENTAFLOW_NATIVE_CACHE`
(domyślnie `~/.cache/tentaflow-native-libs`, na Windows
`%LOCALAPPDATA%\tentaflow-native-libs`).

## Wersje

Wszystkie wersje bibliotek i ich sumy SHA-256 są w jednym pliku:
[`scripts/versions.env`](../scripts/versions.env) (llama.cpp, whisper.cpp,
sherpa-onnx, zvec, ONNX Runtime, runtime'y NVIDIA, PDFium, WASI SDK, GStreamer,
CUDA, Vulkan SDK). Czytają go skrypty bash (`scripts/lib/versions.sh`),
PowerShell (`scripts/lib/windows.ps1`) i MSBuild addonów C#. Podbicie biblioteki
to zmiana jednej linii i sum obok niej. Zmienna środowiskowa o tej samej nazwie
nadpisuje plik na jedno uruchomienie; nadpisana wersja wymaga też nadpisanej
sumy — skrypty nie pobierają niczego bez weryfikacji.

## Budowanie

Linux/macOS:

```bash
./scripts/native-libs/build-all.sh                    # pełna edycja
./scripts/native-libs/build-all.sh --edition slim     # tylko zvec + pdfium
```

Windows x86_64 (po `scripts\setup.ps1`, z dowolnego PowerShella):

```powershell
.\scripts\native-libs\build-all.ps1 -Backend cuda      # wariant CUDA
.\scripts\native-libs\build-all.ps1 -Backend vulkan    # wariant Vulkan
.\scripts\native-libs\build-all.ps1 -Edition slim      # tylko zvec + pdfium
```

`build-all.ps1` ładuje środowisko MSVC x64 (vswhere), Ninja, CUDA/Vulkan SDK i
Pythona, a potem uruchamia te same skrypty bash przez Git Bash — lista kroków i
wersje są identyczne na każdej platformie. Wynik:

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
wszystkimi wykrytymi backendami GPU (`LLAMA_CPP_BACKENDS=auto`: CUDA, gdy jest
widoczne GPU NVIDIA i toolkit; Vulkan, gdy jest `glslc` z Vulkan SDK; zawsze
CPU; na macOS Metal). Kart AMD i Intel nie budujemy przez HIP/ROCm — jadą na
Vulkanie. Jawne backendy budują osobne warianty, które cargo wybiera przez
`LLAMA_CPP_NATIVE_VARIANT` / `WHISPER_CPP_NATIVE_VARIANT` (na Windows robi to
`scripts\build.ps1 -Edition full -Backend <backend>`):

```bash
LLAMA_CPP_BACKENDS=cuda,vulkan ./scripts/native-libs/build-all.sh --only llama-cpp
```

CUDA build wyłącza launchery kompilatora typu `sccache`, bo `nvcc`/`fatbinary`
potrafią wtedy gubić tymczasowe pliki `*.cubin`. Domyślna równoległość CUDA to
`LLAMA_CPP_CUDA_JOBS=4`; można ją zmienić:

```bash
LLAMA_CPP_CUDA_JOBS=2 LLAMA_CPP_BACKENDS=cuda ./scripts/native-libs/build-all.sh --only llama-cpp
```

`whisper.cpp` używa analogicznego modelu (`WHISPER_CPP_BACKENDS`). Jego wynik to
jedna izolowana biblioteka (`libwhisper_tf.so` / `.dylib` / `whisper_tf.dll`)
z prywatnym ggml, eksportująca wyłącznie `whisper_*` — na Windows przez plik
`.def` generowany z `whisper.lib`.

zvec na Windows pochodzi z oficjalnego prebuilt SDK tej samej wersji
(`zvec_c_api.dll` ze statycznym CRT, eksportuje tylko `zvec_*`); Linux/macOS
budują go ze źródeł.

## ONNX Runtime GPU (CUDA / TensorRT)

`build-onnxruntime.sh` provisions the runtime loaded by the `ort` crate
(load-dynamic) plus the `providers_{shared,cuda,tensorrt}` execution providers.
On `linux-x86_64`, and on Windows when an NVIDIA GPU is present (or
`build-all.ps1 -Backend cuda`), it downloads the official GPU release; the CUDA
line is auto-detected from the driver (`ONNXRUNTIME_CUDA=auto|12|13`). CUDA
13-capable drivers get the `gpu_cuda13` artifact, which is required for SM_103
(B300, Blackwell Ultra).

```bash
./scripts/native-libs/build-all.sh --only onnxruntime           # prebuilt (default)
./scripts/native-libs/build-onnxruntime.sh linux-x86_64 --from-source  # native SM_103 cubins
```

For the `gpu_cuda13` variant the script also vendors the NVIDIA runtime stack
from the official NVIDIA wheels (pypi.nvidia.com, SHA-256 pinned in
`scripts/versions.env`): TensorRT 10 (nvinfer, nvinfer_plugin, nvonnxparser and
the builder resource for the target SM + ptx), the full cuDNN 9 split-lib set,
and the CUDA toolkit runtime (cudart 13, cublas/cublasLt 13, cufft 12,
curand 10). Everything lands flat in `lib-dynamic/`, so `tentaflow/build.rs`
copies it next to the binary — **no system TensorRT, cuDNN or CUDA toolkit is
needed on the target host; an NVIDIA R580+ driver is the only requirement**.
Expect ~3 GB in `lib-dynamic/` (and again next to the binary).

- `TENTAFLOW_SKIP_TRT_VENDOR=1` skips ALL vendoring on hosts that have a
  system TensorRT/cuDNN/CUDA toolkit.
- `TENTAFLOW_SKIP_CUDA_VENDOR=1` skips only the CUDA toolkit libs.
- `TENSORRT_SMS` picks builder-resource buckets (`auto` detects the local
  GPU; use `TENSORRT_SMS=sm100` when provisioning for a B300 from another
  machine; `all` vendors every SM bucket).

Prebuilt CUDA 13 binaries ship PTX, so kernels JIT-compile on SM_103 (slower
first session load). `--from-source` builds native cubins
(`ONNXRUNTIME_CUDA_ARCHS=103` by default) with `CUDA_HOME` + `TENSORRT_HOME`
pointing at a CUDA 13.x toolkit and TensorRT >= 10.13.
