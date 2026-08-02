# DeepSeek V4 Flash 0731 — 2× DGX Spark, baseline

Punkt odniesienia zmierzony **przed** jakąkolwiek optymalizacją. Wszystkie
późniejsze porównania muszą odtwarzać konfigurację z sekcji „Środowisko" — inaczej
porównują dwie różne konfiguracje, a nie dwie wersje kodu.

Data: 2026-08-02. Mierzone przez publiczne API (`/v1/chat/completions`,
streaming), nie przez wewnętrzne liczniki silnika.

## Środowisko

| | |
|---|---|
| vLLM | `0.21.1rc1.dev339+g1967a5627bc3` |
| torch | `2.11.0+cu130` |
| obraz | `tentaflow/vllm-dspark:0.24.0` |
| sprzęt | 2× DGX Spark (GB10, sm_121a), TP=2, interconnect RoCE 100G |
| model | `deepseek-ai/DeepSeek-V4-Flash-0731` (FP4 eksperci + FP8 reszta) |

Argumenty startowe istotne dla wyniku:

```
--kv-cache-dtype nvfp4_ds_mla     --block-size 256
--speculative-config '{"method":"dspark","num_speculative_tokens":5,
                       "draft_sample_method":"probabilistic"}'
--max-num-seqs 6                  --max-cudagraph-capture-size 36
--gpu-memory-utilization 0.75     --max-model-len 264192
```

Trzy z nich są load-bearing, nie strojeniem:

- **`num_speculative_tokens = 5`** — musi równać się `dspark_block_size` z
  `config.json` checkpointu. vLLM odrzuca inne wartości wprost
  (`7 must be divisible by n_predict=5`). Karta modelu na HuggingFace podaje 7 i
  jest w tym punkcie błędna.
- **`max-cudagraph-capture-size = 36`** = `max_num_seqs × (k+1)`. Musi być
  wielokrotnością `k+1`, inaczej po zaokrągleniu nie zostaje żaden prawidłowy
  rozmiar i silnik nie wstaje.
- **`gpu_memory_utilization = 0.75`** — przy 0.90 pula KV wychodzi **ujemna**
  (`Available KV cache memory: -25.07 GiB`) i silnik odmawia startu. Na pamięci
  unified capture CUDA-graph konkuruje z wagami o tę samą pulę.

Przy tych ustawieniach pula KV to `+5.61 GiB` / **378 096 tokenów**.

## Wyniki

Prompt: polski tekst techniczny, polecenie streszczenia. `max_tokens = 200`,
`temperature 1.0`, `top_p 0.95` (wartości z karty modelu).

| prompt tok | TTFT (prefill) | decode tok/s | out tok | total s |
|---:|---:|---:|---:|---:|
| 22 | 1,31 s | 19,3 | 127 | 7,90 |
| 3 618 | 3,06 s | 24,9 | 93 | 6,80 |
| 14 265 | 5,96 s | 24,9 | 139 | 11,54 |
| 56 790 | 24,41 s | 23,5 | 165 | 31,43 |

## Interpretacja

**Koszt długiego kontekstu siedzi w całości w prefillu.** Przy 2 600-krotnym
wzroście promptu (22 → 56 790 tokenów) TTFT rośnie **19×**, a decode zostaje
płaski (19–25 tok/s, bez trendu). Optymalizacja prefillu ma tu nieporównanie
większy potencjał niż optymalizacja dekodowania.

**Decode przy najkrótszym promptcie jest wolniejszy niż przy dłuższych**
(19,3 vs 24,9). To sygnatura spekulacji: przy krótkim kontekście drafter ma mniej
materiału, akceptacja spada i część z `k=5` draftowanych tokenów się marnuje.

## Rozbieżność wobec publikowanych liczb

Przepis referencyjny dla tej samej konfiguracji sprzętowej podaje **42 tok/s** na
prozie i 76 na kodzie. Mamy 23–25. Hipotezy do sprawdzenia w fazie optymalizacji,
w kolejności podejrzeń:

1. **Język polski** — więcej tokenów na słowo i niższa akceptacja draftu niż na
   angielskim. Ten sam przepis raportuje akceptację 0,338 dla prozy wobec 0,825
   dla kodu, więc wrażliwość na typ tekstu jest tu duża.
2. **`max_num_seqs 6`** zamiast 12 — zejść trzeba było, żeby zmieścić capture
   przy `k=5`. Przepis notuje, że zejście do 2 kosztuje ~1/3 przepustowości.
3. **`util 0.75`** zamiast zalecanych 0,78 — suwak w kreatorze ma krok 0,05.

Pierwsza hipoteza jest sprawdzalna najtaniej: ten sam benchmark na promptach
angielskich i na kodzie.

## Powtórzenie

```bash
scripts/bench-openai-api.py \
  --url http://<head-ip>:<port>/v1/chat/completions \
  --container tentaflow-vllm-dspark-<port> \
  --contexts 64,2048,8192,32768 --max-tokens 200 --repeat 3
```

Skrypt drukuje wersję vLLM i argumenty startowe razem z wynikami, więc każdy
przebieg sam się dokumentuje. `--repeat` uśrednia, żeby odróżnić realną zmianę
od szumu.
