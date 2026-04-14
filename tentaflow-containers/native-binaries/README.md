# native-binaries/

Skrypty buildujace natywne binarki silnikow AI — alternatywa do Dockerow
dla maszyn ktore nie maja dockera. Binarki sa cross-compilowane per
platforma, pakowane do `.tar.gz` i embedowane w `tentaflow` przez
`tentaflow-core/build.rs` (analogicznie jak `tentaflow-containers/`).

## Cel

Uzytkownik uruchamia jeden plik `tentaflow` → w wizardzie deploy wybiera
"Backend: Native" → tentaflow rozpakowuje binarke do
`~/.cache/tentaflow/engines/<engine>/` i uruchamia jako subprocess.
Sidecar QUIC obok niej → rejestracja w mesh jak przy Dockerze.

## Zakres (co MA sens jako binarka natywna)

| Silnik | Binarka | Wymagania | Kategoria |
|--------|---------|-----------|-----------|
| `llama-server` | llama.cpp | CUDA / Metal / Vulkan / CPU | LLM |
| `whisper-server` | whisper.cpp | j.w. | STT |
| `sherpa-tts` | sherpa-onnx | ONNX Runtime + CUDA (opt.) | TTS |
| `text-embeddings-router` | HF TEI (Rust+Candle) | CUDA / Metal / CPU | Embeddings + Reranker |
| `sd-server` | stable-diffusion.cpp | CUDA / Vulkan / CPU | Image gen |

## Poza zakresem (tylko Docker lub venv)

- `vllm`, `sglang` — Python + PyTorch + specyficzne CUDA deps
- `xtts`, `voxcpm`, `qwen-asr`, `parakeet` — Python + transformers
- `comfyui` — Python + ekosystem node'ow

Tych silnikow NIE da sie sensownie dostarczyc jako pojedyncza binarka
cross-platform. Pozostaja w ścieżce Docker (`tentaflow-containers/`).

## Struktura katalogu

```
native-binaries/
├── README.md              # ten plik
├── build-all.sh           # buduje wszystkie silniki dla wszystkich platform
├── llama-cpp/
│   └── build.sh           # klonuje llama.cpp z git, cmake z odpowiednim backendem
├── whisper-cpp/
│   └── build.sh
├── sherpa-onnx/
│   └── build.sh
├── text-embeddings/
│   └── build.sh
├── stable-diffusion-cpp/
│   └── build.sh
└── output/                # artefakty — .tar.gz per (engine × platforma × backend)
    └── (generowane)
```

Nazewnictwo archiwow:
`<engine>-<platform>-<backend>.tar.gz`
przykladowo: `llama-server-linux-x86_64-cuda.tar.gz`,
`whisper-server-macos-aarch64-metal.tar.gz`.

## Platformy docelowe

| Platforma | Target triple | Backend GPU domyslny |
|-----------|---------------|----------------------|
| Linux x86_64 | `x86_64-unknown-linux-gnu` | CUDA (NVIDIA) / Vulkan (fallback) |
| Linux aarch64 | `aarch64-unknown-linux-gnu` | CUDA (Jetson, DGX Spark) / Vulkan |
| macOS aarch64 | `aarch64-apple-darwin` | Metal |
| Windows x86_64 | `x86_64-pc-windows-msvc` | CUDA / Vulkan |

Kazda platforma dostaje osobny zip — tentaflow build.rs wybiera przy
release ktory zestaw embedowac (docelowo builduje sie tentaflow per
platforma, embedujac tylko jej binarki).

## Detekcja runtime

Tentaflow sprawdza co maszyna potrafi (`tentaflow-core/src/system_check/`):
- CUDA version (`nvidia-smi` + `/usr/local/cuda/version.json`)
- Metal (macOS — zawsze gdy Apple Silicon / recent Intel)
- Vulkan (`vulkaninfo` lub przez wgpu adapter enumeration)
- CPU features (AVX2 / AVX512 / NEON)

W wizardzie deploy widac od razu, ktory backend jest dostepny.
Brak CUDA → llama-server dostanie wariant CPU albo Vulkan.

## Status

SZKIELET — skrypty build.sh jeszcze nie napisane. Docelowo uruchamia je
CI (GitHub Actions) na 4 matrix runners (ubuntu x86, ubuntu-arm, macOS,
windows), a wynikowe zip'y trafiaja do release artifacts.
