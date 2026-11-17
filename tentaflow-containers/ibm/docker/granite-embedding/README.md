# IBM Granite Embedding (vLLM `--runner pooling`)

Obraz dla serwowania modeli embeddingów IBM Granite. Składa się z dwóch
procesów uruchamianych równolegle przez `entrypoint.sh`:

- **vLLM** w trybie embeddingów (`vllm serve $MODEL --runner pooling`), nasłuchuje
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

Tryb `--runner pooling` w vLLM obsługuje embeddingi tylko dla architektur, które
dany build vLLM zna. Ten obraz pinuje **vLLM 0.34.0**, którego rejestr modeli
embeddingowych zawiera architektury wszystkich trzech presetów:

- `RobertaModel` — `granite-embedding-30m-english`,
- `XLMRobertaModel` — `granite-embedding-278m-multilingual`,
- `ModernBertModel` — `granite-embedding-english-r2`.

Nowy wariant Granite o innej architekturze wymaga sprawdzenia, czy pinowana
wersja vLLM ją rejestruje; jeśli nie, należy zaktualizować wersję vLLM
w `Dockerfile`.
