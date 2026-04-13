# tentaflow-containers

Katalog z definicjami kontenerow Docker integrowanych z TentaFlow.

## Architektura

Kazdy kontener sklada sie z:

- **Silnik AI** (vLLM, whisper.cpp, Sherpa, XTTS itp.) — uruchamiany w kontenerze,
  nasluchuje na `localhost` (wewnatrz kontenera) na wlasnym API HTTP.
- **Sidecar QUIC** (`tentaflow-sidecar`) — generyczna binarka Rust, nasluchuje
  QUIC na porcie 5000, tlumaczy QUIC/rkyv ↔ lokalne HTTP silnika. Rola wybierana
  przez `/data/config.toml`. Dzieki temu kazdy kontener uzywa tej samej binarki
  a roznia sie tylko silnikiem i konfiguracja.
- **Dockerfile** multistage: builder + runtime.
- **entrypoint.sh** — startuje silnik w tle + sidecar na pierwszym planie.
- **config.default.toml** — rola + aliasy modelow + upstream URL.

## Lista kontenerow

| Kontener | Silnik | Rola sidecara |
|----------|--------|---------------|
| `sidecar/` | — | *crate Rust, zrodlo binarki* |
| `teams-bot/` | Chromium + audio | `teams_bot` (istnieje, migracja w planie) |
| `llm-llamacpp/` | llama.cpp server | `reverse_proxy` (OpenAI) |
| `llm-vllm/` | vLLM z git | `reverse_proxy` (OpenAI) |
| `llm-sglang/` | SGLang | `reverse_proxy` (OpenAI) |
| `llm-ollama/` | Ollama | `reverse_proxy` (OpenAI) |
| `stt-whisper/` | whisper.cpp / faster-whisper | `reverse_proxy` (OpenAI audio) |
| `stt-parakeet/` | NVIDIA NeMo Parakeet-TDT-0.6B-v3 | `reverse_proxy` (custom) |
| `stt-qwen-asr/` | Qwen3-ASR-1.7B przez vLLM | `reverse_proxy` (OpenAI audio) |
| `tts-sherpa/` | Sherpa ONNX | `reverse_proxy` (Sherpa) |
| `tts-xtts/` | XTTS v2 (coqui) | `reverse_proxy` (custom) |
| `tts-voxcpm/` | VoxCPM2 | `reverse_proxy` (custom) |
| `embeddings/` | ONNX Runtime | `onnx_in_process` albo `reverse_proxy` |
| `reranker/` | ONNX Runtime (BGE) | `onnx_in_process` albo `reverse_proxy` |
| `comfyui/` | ComfyUI | `reverse_proxy` (ComfyUI API) |

## Deploy

Sidecar + caly kontekst buildu kazdego kontenera bedzie **embedowany w binarce
`tentaflow`** przez `include_bytes!` w `tentaflow/build.rs`. Uzytkownik uruchamia
tylko `tentaflow`, GUI wola:

```
POST /api/deploy/<container_name>  →  extract embedded context → docker build → docker run
```

Nic nie trzeba klonowac ani budowac recznie.
