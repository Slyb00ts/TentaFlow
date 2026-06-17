# ML Training (HF) — native bundle

Serwer treningowy LLM oparty o Hugging Face (`transformers` + `peft` + `trl`).
Realne fine-tuning: SFT, LoRA, QLoRA oraz DPO. Model bazowy i dane podajesz per
job przez REST API — nic nie jest wybierane przy deployu.

`server.py` w tym katalogu to **pojedyncze źródło** logiki serwera; obraz Docker
(`training/docker/ml-training/Dockerfile`) kopiuje dokładnie ten plik, żeby nie
duplikować implementacji.

## Wymagania

- GPU NVIDIA + sterownik CUDA 12.1 (QLoRA wymaga `bitsandbytes`).
- [`uv`](https://docs.astral.sh/uv/) — sam zainstaluje izolowany Python 3.12,
  nawet gdy host ma 3.14 bez kół `torch`.

## Uruchomienie

```bash
./run.sh                 # nasłuch na 0.0.0.0:8200
PORT=8300 ./run.sh       # inny port
```

`uv` rozwiąże zależności z `pyproject.toml`; `torch` ciągnie z indeksu CUDA 12.1
(`https://download.pytorch.org/whl/cu121`), resztę z PyPI.

## API

- `GET /health` → `{status, cuda, gpus}`.
- `POST /train` → start treningu w tle, zwraca `{job_id, status:"running"}`.
- `GET /status/{job_id}` → `{status, step, total_steps, train_loss, eval_loss, error}`.
- `GET /models/{job_id}/path` → ścieżka artefaktu po sukcesie.

### Przykład — LoRA SFT

```bash
curl -X POST http://localhost:8200/train -H 'content-type: application/json' -d '{
  "base_model": "Qwen/Qwen3.5-0.8B",
  "method": "lora",
  "objective": "sft",
  "train_data": [
    {"prompt": "Stolica Polski?", "response": "Warszawa."},
    {"messages": [{"role":"user","content":"2+2?"},{"role":"assistant","content":"4"}]},
    {"text": "Dowolny ciąg tekstu do treningu."}
  ],
  "hyperparams": {"epochs": 1, "lr": 0.0002, "lora_r": 16},
  "output_dir": "/data/adapters/run1",
  "merge_adapter": true
}'
```

### Przykład — DPO

Rekordy muszą zawierać `prompt`, `chosen`, `rejected`:

```json
{
  "base_model": "Qwen/Qwen3.5-0.8B",
  "method": "qlora",
  "objective": "dpo",
  "train_data": [
    {"prompt": "...", "chosen": "...", "rejected": "..."}
  ],
  "output_dir": "/data/adapters/dpo1"
}
```

## Metody i dane

| `method` | Co robi |
|----------|---------|
| `lora`   | LoRA na bf16 modelu bazowym |
| `qlora`  | LoRA na 4-bit (nf4 + double quant) — najmniejsze VRAM |
| `full`   | pełny fine-tune wszystkich wag |

Rekordy SFT akceptują trzy kształty: `{text}`, `{prompt,response}` albo
`{messages:[{role,content}]}` (chat template tokenizera, gdy dostępny).
Artefakty (adapter, opcjonalnie `merged/`) trafiają do `output_dir`.
