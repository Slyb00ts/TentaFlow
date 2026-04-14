# python-bundles/

Rozwiazanie deployu Pythonowych silnikow AI **bez Dockera**. Dla kazdego
silnika ktory wymaga Pythona + PyTorcha (vLLM, SGLang, XTTS, VoxCPM,
Parakeet, Qwen-ASR, ComfyUI) definiujemy tutaj jego "bundle" — pinowane
wheels + entrypoint + metadata — a runtime tentaflow-core `deploy::python_venv`
bootstrapuje go na maszynie uzytkownika.

## Jak to dziala

1. **Python relokowalny**: przy pierwszym deployu tentaflow pobiera
   [python-build-standalone](https://github.com/astral-sh/python-build-standalone)
   dla platformy usera (~80 MB per OS/arch). Zapis do
   `~/.cache/tentaflow/python/<version>/`.
2. **uv jako instalator**: tentaflow wywoluje `uv pip install -r
   requirements.lock` wewnatrz swiezo utworzonego venv (10-100x szybciej
   niz pip). Venv w `~/.cache/tentaflow/envs/<engine>/`.
3. **Entrypoint**: kazdy silnik ma `entrypoint.py` lub komende w `bundle.toml`
   (np. `python -m vllm.entrypoints.openai.api_server ...`). Tentaflow
   uruchamia to jako subprocess, nasluchuje na `127.0.0.1:<wewnetrzny_port>`.
4. **Sidecar QUIC** na boku → ten sam flow co przy deployu Docker.
5. **Cache**: kolejne deploye reusuje venv (~30 s start zamiast 10-15 min).

## Dlaczego nie Docker / nie PyInstaller / nie conda-pack

- **Docker**: user moze go nie miec, my chcemy zero-setup.
- **PyInstaller/PyOxidizer**: kompresuja Pythona + wheels do 1 pliku, ale
  torch + CUDA = 3-8 GB per binarka i cross-platform buildy sa koszmarem.
- **conda-pack**: tarball venv (~4 GB) nie jest w pelni relokowalny
  (shebang paths, compiled C extensions z absolutnym RPATH).
- **python-build-standalone + uv**: venv w cache usera, 10-min start raz,
  potem natychmiastowo, aktualizacje przez `uv pip install -U`.

## Struktura bundla

```
python-bundles/<engine>/
├── bundle.toml          # meta: nazwa, python_version, entrypoint, ports, env
├── requirements.lock    # pinowane wheels (vllm==0.6.3, torch==2.5.1+cu124, ...)
├── entrypoint.py        # opcjonalny launcher (FastAPI wrapper jesli potrzebny)
└── README.md            # instrukcja co ten bundle robi
```

`bundle.toml` przyklad:

```toml
[bundle]
engine = "vllm"
python_version = "3.11"
description = "vLLM OpenAI-compatible LLM server"

[launch]
command = "python"
args = ["-m", "vllm.entrypoints.openai.api_server", "--host", "127.0.0.1"]
# Env: MODEL, VLLM_PORT itp. — przekazywane z wizarda jak przy Dockerze
internal_port = 8000

[requires]
cuda = ">=12.4"
gpu_memory_gb = 8
disk_gb = 20
```

## Silniki w tym katalogu

| Silnik | Status | Uzasadnienie |
|--------|--------|--------------|
| `vllm/` | plan | OpenAI API, CUDA-heavy |
| `sglang/` | plan | OpenAI API + structured outputs |
| `xtts/` | plan | voice cloning, coqui-TTS |
| `voxcpm/` | plan | nowy TTS |
| `parakeet/` | plan | NVIDIA NeMo |
| `qwen-asr/` | plan | transformers-based STT |
| `comfyui/` | plan | image generation |

## Ograniczenia

- **macOS**: vLLM/SGLang nie maja wsparcia oficjalnego (brak CUDA). Na
  Apple Silicon jedyna opcja to MLX (in-process). Bundle bedzie zwracal
  blad "not supported on this platform".
- **Windows**: vLLM ma exp. support, bedzie dzialac przez WSL albo native
  Linux. SGLang podobnie.
- **Linux bez NVIDIA**: silniki CUDA-only beda niedostepne.
- Detekcja CUDA/Metal i mapowanie na `supported_engines` juz dziala w
  `tentaflow-core/src/system_check/` — GUI wizard pokazuje userowi od razu
  co moze a czego nie.

## Dalsze kroki

1. Dla kazdego silnika wygenerowac `requirements.lock` (np. przez `uv pip
   compile pyproject.toml`) z dokladnymi wersjami.
2. Testy e2e: `deploy::python_venv::bootstrap(bundle)` na trzech platformach.
3. GUI: radio "Backend: Docker / Native (Python)" w wizardzie dla
   silnikow ktore maja oba warianty.
