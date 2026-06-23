# Realne zużycie VRAM — modele NVIDIA / NeMo Retriever

Pomiary wykonane na **hazai** (RTX 3090, 24 GB), deploy przez dashboard (Docker,
pin na pojedynczą kartę), odczyt `nvidia-smi` po załadowaniu modelu oraz wagi z
logów silnika.

## Jak czytać liczby

- **Wagi** — faktyczny rozmiar wag w VRAM (z logu vLLM `Model loading took …`).
  To twarde minimum, żeby model się załadował.
- **`nvidia-smi`** — pełna alokacja procesu po starcie.
  - Silniki **vLLM dla czatu/VLM** rezerwują KV-cache do `gpu_memory_utilization`
    × VRAM karty, więc na pustej karcie 24 GB potrafią zająć ~19–22 GB
    **niezależnie od rozmiaru wag**. Liczba `nvidia-smi` to NIE zapotrzebowanie
    modelu — to KV-cache skalujący się z kontekstem i równoległością. Planuj wg
    kolumny *Wagi* + zapas na KV-cache pod swój kontekst.
  - Silniki **embeddings/rerank** (vLLM pooling) NIE alokują KV-cache → `smi`
    ≈ wagi + narzut.
  - Modele **custom (torch/transformers)** nie dopychają KV-cache → `smi` ≈ realne.

## Zmierzone (RTX 3090)

| Serwis | Model | Silnik | Wagi | `nvidia-smi` | Uwaga |
|--------|-------|--------|------|--------------|-------|
| nemotron-embed | llama-nemotron-embed-1b-v2 | vLLM (embed) | **2.32 GiB** | ~3.0 GB | bez KV-cache |
| nemotron-embed-vl | llama-nemotron-embed-vl-1b-v2 | vLLM (embed) | **3.13 GiB** | ~4.3 GB | multimodalny |
| nemotron-rerank | llama-nemotron-rerank-1b-v2 | vLLM (score) | **2.32 GiB** | ~3.6 GB | |
| nemotron-rerank-vl | llama-nemotron-rerank-vl-1b-v2 | vLLM (score) | **3.13 GiB** | ~4.3 GB | multimodalny |
| nemotron-ocr | nemotron-ocr-v1 | torch (custom) | — | **~1.0 GB** | detector+recognizer+relational |
| nemotron-page-elements | nemotron-page-elements-v3 | YOLOX-L (torch) | — | **~0.8 GB** | |
| nemotron-table-structure | nemotron-table-structure-v1 | YOLOX-L (torch) | — | ~0.8 GB | ten sam backbone co page-elements |
| nemotron-graphic-elements | nemotron-graphic-elements-v1 | YOLOX-L (torch) | — | ~0.8 GB | ten sam backbone |
| nemotron-parse | NVIDIA-Nemotron-Parse-v1.2 | transformers VLM | — | **~2.1 GB** | C-RADIO ViT-H + mBART 885M |
| granite-vision | granite-vision-3.3-2b | vLLM | **5.54 GiB** | do ~22 GB | `smi` = KV-cache do util |
| vllm (chat) | Qwen3.5-0.8B | vLLM | **1.72 GiB** | do ~19 GB | `smi` = KV-cache do util |

## Szacunki (nie mieszczą się lub nie deployowane na 24 GB)

Z liczby parametrów × bajty/dtype + narzut. FP8 ≈ 1 B/param, BF16 ≈ 2 B/param.

| Model | Param | dtype | Szac. wagi | 24 GB? |
|-------|-------|-------|------------|--------|
| nemotron-nano-12b-v2-vl | 12 B | FP8 | ~12 GB | tak (1× 24 GB) |
| nemotron-3-nano-omni-30b-a3b | 30 B (MoE) | FP8 | ~30 GB | nie (≥2× 24 GB / 48 GB) |
| qwen3-vl-30b-a3b-instruct | 30 B (MoE) | BF16 | ~60 GB | nie |
| nemotron-3-super-120b-a12b | 120 B | FP8 | ~120 GB | nie (B200/B300 / multi-GPU) |
| qwen3-vl-4b-instruct | 4 B | BF16 | ~8–9 GB | tak |
| paddle-ocr (PaddleOCR-VL) | ~0.9 B | BF16 | ~2–3 GB | tak |

## Dostęp zewnętrzny (OpenAI API z tokenem)

Każdy z powyższych jest wystawiany przez tentaflow-core po HTTP z kluczem API
(`Authorization: Bearer`) i uprawnieniem per-model (klucz `general` + scope na
model). Trasy:

- `POST /v1/chat/completions` — LLM i VLM (chat + obraz): vllm, granite-vision,
  qwen3-vl, **nemotron-parse** (kontrakt NVIDIA nemoretriever-parse).
- `POST /v1/embeddings` — nemotron-embed / embed-vl.
- `POST /v1/rerank` — reranking w stylu Cohere/Jina (vLLM).
- `POST /v1/ranking` — **kontrakt NVIDIA NeMo Retriever** (`query`/`passages` →
  `rankings`/`logit`); Core tłumaczy na `/v1/rerank` backendu.
- `POST /v1/infer` — **kontrakt NVIDIA NIM** dla OCR i detektorów: nemotron-ocr,
  paddle-ocr (`text_detections`), nemotron-page-elements / table-structure /
  graphic-elements (`bounding_boxes` po klasach).
