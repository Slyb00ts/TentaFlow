# Profil kroku decode — Bielik-7B NVFP4 (B=32) i Qwen3.6-27B (C=8)

Data: 2026-07-24. GPU: RTX 4090, wolne (brak innych procesów).
Narzędzie: `nsys profile -t cuda --cuda-graph-trace=node` (BEZ tej flagi kernele
z grafu CUDA nie pojawiają się w raporcie — pierwszy przebieg wyglądał, jakby
kerneli BM32 w ogóle nie było). `ncu` nie jest zainstalowany, więc przepustowości
liczone są z czasu kernela i znanego rozmiaru wag, nie z liczników sprzętowych.
Artefakty: `~/.cache/tentaflow-profiles/bm32-decode/`.

## Wniosek nadrzędny

GPU jest zajęte **96% czasu** (Bielik) i **91%** (Qwen) w oknie pomiarowym.
Nie ma problemu z narzutem launchy, lukami między kernelami ani z hostem —
zmierzony czas to realna praca kerneli. Optymalizować trzeba same kernele,
nie orkiestrację.

## Bielik-7B NVFP4, decode-only in32/o256, C=32

`forge serve --max-active 32 --ctx 2048 --kv-pages 1536`, 1387 kroków decode
w oknie 20 s. TPOT median 12,3 ms.

| Pozycja | µs/kernel | na krok | bajty/warstwę | GB/s |
|---|--:|--:|--:|--:|
| gate+up (BM32) | 82,19 | 3,288 ms | 51,9 MB | **631** |
| down (BM32) | 39,19 | 1,568 ms | 25,9 MB | **661** |
| qkv (BM32) | 24,25 | 0,970 ms | 14,2 MB | **585** |
| o (BM32) | 14,68 | 0,587 ms | 9,4 MB | **640** |
| redukcja split-K | 1,51 | 0,181 ms | — | — |
| attention decode (split+combine) | 48,65 / 2,94 | 2,064 ms | 37,7 MB KV | **776** |
| rmsnorm residual (×2/warstwę) | 7,41 | 0,593 ms | — | — |
| głowa logitów F16 | 562,9 | 0,563 ms | 262 MB | **465** |
| sampling (topk dwuprzebiegowy) | 64,6 + 39,3 | 0,104 ms | — | — |
| silu·mul | 2,21 | 0,088 ms | — | — |
| **razem decode** | | **~10,0 ms** | | |

Reszta do 14,4 ms/krok (20 s / 1387) to prefill nowo przyjmowanych requestów —
kernele prefillowe zajmują 24,6% czasu GPU w oknie.

Obserwacje:

1. **Projekcje jadą na 585-661 GB/s, attention na 776 GB/s na tym samym GPU**
   (peak HBM ~1008 GB/s). Wąskim gardłem nie jest sama pamięć, tylko
   równoległość pamięci kerneli projekcji: kafel BM32 zużywa 39 936 B shared,
   co daje 2 bloki/SM × 128 wątków = 8 warpów z 48, czyli ~17% occupancy.
   Podniesienie projekcji do poziomu attention (776 GB/s) skróciłoby krok
   o ~1,3 ms; do 90% peak — o ~2,2 ms.
2. **Głowa logitów jest F16 i liczy 262 MB na krok przy 465 GB/s** (0,563 ms,
   4,6% czasu GPU). Paczka FP8 dla `lm_head` JEST budowana, ale używa jej
   prefill — decode zostaje na F16. To najtańszy pojedynczy zysk w tabeli.
3. Sampling po wymianie na dwuprzebiegowy top-k to już tylko 0,10 ms/krok.

## Qwen3.6-27B NVFP4 MTP (hybrydowy), decode-only in32/o256, C=8

`forge serve --max-active 8 --ctx 2048 --kv-pages 512`, 519 kroków w oknie 20 s.
Zmierzone: **40-51 tok/s agregat, TPOT median 153 ms**.

Kluczowa liczba: 519 kroków wyprodukowało ~1040 tokenów, czyli **2 tokeny na
krok** przy ośmiu aktywnych sekwencjach. `record_hybrid_batch_forward` odrzuca
każde `n != 2` (`"hybrydowy batch targetu obsługuje obecnie dokładnie B=2"`),
więc model hybrydowy nigdy nie dekoduje więcej niż dwóch sekwencji w jednym
przebiegu — osiem lane'ów to cztery kolejne kroki B=2. Skalowanie
współbieżności jest z tego powodu prawie żadne: **~40 tok/s przy C=1 → 51 tok/s
przy C=8 (1,27×)**, podczas gdy gęsty Bielik na tym samym GPU robi 2 493 tok/s
przy C=32.

Rozkład 38,5 ms kroku B=2:

| Kernel | % GPU | na krok |
|---|--:|--:|
| `gemm_nvfp4_gguf_batch` (duży) | 42,6 | 15,0 ms |
| `gemv_q8_0_dp4a` (385 launchy/krok) | 26,7 | 9,4 ms |
| `gemm_nvfp4_gguf_batch` (mały) | 16,0 | 5,6 ms |
| głowa logitów Q8_0 | 4,1 | 1,43 ms |
| rmsnorm residual | 3,9 | 1,37 ms |
| DeltaNet (conv, l2norm, gated rms, decay, beta, value-key) | ~3,5 | ~1,3 ms |
| sampling | 0,2 | 0,11 ms |

`gemv_q8_0_dp4a` uruchamia się **385 razy na krok** (6 na warstwę przy 64
warstwach) ze średnią 24,4 µs i odchyleniem 20,9 µs — to projekcje DeltaNet
liczone kernelem GEMV, czyli ścieżką jednowierszową, mimo że krok ma dwa lane'y.

Wniosek: dla Qwen 27B limitem NIE jest przepustowość pamięci ani narzut hosta,
tylko architektoniczny sufit B=2 w ścieżce hybrydowej. Dopóki on stoi, żadne
strojenie kerneli nie da skalowania współbieżności na tym modelu.
