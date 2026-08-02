# DeepSeek V4 Flash 0731 na 2x DGX Spark — vLLM 0.26 ze źródeł

Punkt odniesienia dla późniejszych optymalizacji. Mierzone przez API OpenAI
(`scripts/bench-openai-api.py`), strumieniowo, bo TTFT jest niewidoczny w
wywołaniu blokującym.

## Konfiguracja

| | |
|---|---|
| silnik | vLLM `0.26.1.dev0+g568afb3a1` ze źródeł, torch 2.11.0+cu130 |
| bundle | `scripts/build-vllm-spark-venv.sh`, `TORCH_CUDA_ARCH_LIST=12.0` |
| model | `deepseek-ai/DeepSeek-V4-Flash-0731`, wagi **FP8 e4m3**, 167 GB |
| węzły | spark-001 + rig25, TP=2, PP=1, `mp`, NCCL po RoCE (oba bliźniaki) |
| KV | `fp8_ds_mla` (584 B/token/warstwa, 43 warstwy) |
| pamięć | `gpu_memory_utilization = 0.80` |
| spekulacja | **wyłączona** — DSpark nie startuje, patrz niżej |
| grafy CUDA | **wyłączone** (`--enforce-eager`) — z grafami silnik się zawiesza |

Pula KV przy tych ustawieniach: **18,64 GiB = 688 274 tokenów** (2,6 pełnego
kontekstu 262144).

## Wyniki

Prompt budowany z powtarzalnego tekstu polskiego, `max_tokens=200`, 2 powtórzenia.

| prompt tok | TTFT s | prefill tok/s | decode tok/s | total s |
|---|---|---|---|---|
| 22 | 0,16 | 139 | 16,7 | 7,58 |
| 3 618 | 1,88 / 0,23 | 1 923 / 15 720 | 16,8 | 7,72 / 4,27 |
| 14 265 | 5,10 / 0,47 | 2 796 / 30 080 | 16,8 | 9,57 / 9,37 |
| 56 790 | 20,43 / 11,98 | 2 780 / 4 739 | 16,7 | 29,23 / 20,89 |

Druga wartość w wierszu to powtórzenie tego samego promptu, czyli **trafienie w
cache prefiksów** — stąd 30 080 tok/s przy 14 265. Uczciwy prefill na zimno to
**1 900–4 700 tok/s**; wartości z cache nie są miarą przepustowości prefillu.

Decode jest płaski (16,7–16,8 tok/s) niezależnie od długości kontekstu, co
zgadza się z tym, że ogranicza go pasmo pamięci przy odczycie wag, a nie uwaga.

## Sufity teoretyczne

Aktywne parametry na token: **10,5 mld** (MoE 7,6 + uwaga 2,9), przy 43
warstwach, `hidden=4096`, `moe_intermediate=2048`, 6 ekspertów routowanych + 1
współdzielony. Przy FP8 to 10,5 GB odczytu na token, dzielone przez TP=2.

| | 1 Spark | 2 Sparki |
|---|---|---|
| decode @ 273 GB/s (katalog GB10) | 26,1 tok/s | **52,2 tok/s** |
| decode @ ~200 GB/s (realne ~73%) | 19,1 tok/s | **38,2 tok/s** |
| prefill @ ~250 TFLOPS FP8/Spark | — | **~20 000 tok/s** |

Zmierzone 16,7 tok/s to 44% realistycznego sufitu decode; z grafami CUDA (gdy
nie zawisną) 25,5–26,8 tok/s, czyli 70%. Prefill na zimno to **12–20%** sufitu —
tam jest największy zapas.

## Znane usterki tej platformy

### Grafy CUDA zawieszają silnik

Sygnatura złapana przez `py-spy` w trakcie zawieszenia: wątek workera stoi w
`cudaEventSynchronize` po nieblokującej kopii GPU→host do pamięci pinned, **GPU
jest w 0% wykorzystania**, a drugi rank TP jest bezczynny z pustą kolejką —
więc nic nie liczy i żadna kolektywa nie czeka. Zdarzenie po prostu nie zostaje
zasygnalizowane.

Miejsce zależy od konfiguracji: `async_copy_ready_event.synchronize()` przy
async scheduling, `transfer_event.synchronize()` w `_to_list` bez niego.
`patch_event_sync_026.py` usuwa oba te zdarzenia — i zawis przenosi się w
kolejne miejsce, co wskazuje, że problemem jest samo odtwarzanie grafu, a nie
konkretne zdarzenie.

Wykluczone dowodowo (każde osobnym pomiarem): parsery `deepseek_v4`, numer
żądania, `max_tokens`, kompilacja Tritona (limit 1800 s wyczerpany bez żadnej
kompilacji), `--async-scheduling` (po jawnym `--no-async-scheduling`), cache
prefiksów, wyczerpanie pamięci (przy 0,80 wolne nigdy nie spadło poniżej 14 GiB)
oraz `VLLM_USE_BREAKABLE_CUDAGRAPH=0`.

**Obejście:** `--enforce-eager`. Kosztuje ok. 37% wydajności dekodowania.

### DSpark nie startuje

`Check failed: num_tokens > 64 (5 vs. 64) : Decode (num_tokens <= 64) must go
through sparse_mla_sm120_decode_dsv3_2 or sparse_mla_sm120_decode_dsv4` —
weryfikacja k=5 tokenów spekulatywnych trafia w ścieżkę stronicowaną zamiast w
dedykowane wejście dekodujące SM120.

### `nvfp4_ds_mla` nie daje oszczędności

FlashInfer ma `_BPT_DSV4 = 584` zaszyte na stałe; wariantu 4-bitowego dla KV nie
ma, a wzmianki o fp4 dotyczą formatu wyjścia i są oznaczone jako nieobsługiwane.
W vLLM `nvfp4_ds_mla` nie przechodzi przez ~10 porównań `== "fp8_ds_mla"`
decydujących o układzie i po cichu spada na wiersze bf16 — stąd 2x większe KV
na token, zanim przeszliśmy na `fp8_ds_mla`.

### FlashMLA nieobecny — celowo

Upstream deklaruje dla niego wyłącznie `9.0a` i `10.0f`/`10.0a`; wariantu dla
12.x nie ma w żadnej konfiguracji, więc na GB10 jest niebudowalny. Ścieżka MLA
idzie przez `FLASHINFER_MLA_SPARSE_DSV4`.

## Odtworzenie

```bash
./scripts/build-vllm-spark-venv.sh                  # bundle na obu węzłach
./scripts/dspark-cluster.sh up --util 0.80 --graphs off --spec off
./scripts/dspark-cluster.sh wait
python3 scripts/bench-openai-api.py --contexts 64,2048,8192,32768 \
        --max-tokens 200 --repeat 2
```
