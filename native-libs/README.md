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

Ciężkie pliki `.a`, `.so`, `.dylib`, `.dll`, `.lib`, `.framework` i `.bundle`
w `native-libs/` są odblokowane w `.gitignore`, żeby maintainer mógł zbudować
je raz na właściwej maszynie i dodać do repo. Cache źródeł i katalogi buildów
pozostają poza repo.
