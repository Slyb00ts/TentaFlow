# IBM Granite Embedding (vLLM `--task embed`)

Obraz dla serwowania modeli embeddingów IBM Granite. Składa się z dwóch
procesów uruchamianych równolegle przez `entrypoint.sh`:

- **vLLM** w trybie embeddingów (`vllm serve $MODEL --task embed`), nasłuchuje
  OpenAI-compatible API na `127.0.0.1:8000` i wystawia `/v1/embeddings`.
- **sidecar TentaFlow** — opakowuje to API w QUIC (iroh) dla klientów mesh.
  Proxuje `/v1` (w tym `/v1/embeddings`) zgodnie z `config.default.toml`.

## Model

Model nie jest wbudowany w obraz — jest pobierany w runtime z HuggingFace na
podstawie zmiennej środowiskowej `MODEL` (np.
`ibm-granite/granite-embedding-278m-multilingual`). Pobieranie korzysta z
`hf_transfer` (`HF_HUB_ENABLE_HF_TRANSFER=1`).

Gdy w ustawieniach TentaFlow podany jest `HF_TOKEN`, Core wstrzykuje go do
kontenera przy deployu (dostęp do repozytoriów gated).

## Ważna uwaga: wsparcie vLLM zależy od architektury modelu

Tryb `--task embed` w vLLM obsługuje embeddingi tylko dla architektur, które
dany build vLLM zna. Ten obraz pinuje **vLLM 0.28.0**:

- Warianty Granite oparte o **RoBERTa / XLM-RoBERTa** (np.
  `granite-embedding-30m-english`, `granite-embedding-278m-multilingual`) są
  obsługiwane przez vLLM `--task embed`.
- Nowsze warianty „r2" (`granite-embedding-english-r2`) bazują na
  **ModernBERT** i mogą wymagać **nowszej wersji vLLM niż 0.28.0**. Jeśli
  serwowanie wariantu r2 zwróci błąd nieobsługiwanej architektury, należy
  zaktualizować pinowaną wersję vLLM w `Dockerfile`.

To jest realna zależność od wersji vLLM, a nie gwarancja działania każdego
wariantu na pinowanej dziś wersji.
