# Obsługa Gemma 4 w FORGE — stan i plan

Model odniesienia: `google/gemma-4-12B-it-qat-q4_0-gguf`
(`gemma-4-12b-it-qat-q4_0.gguf`, 6,98 GB, 667 tensorów, 48 warstw).
Wszystkie liczby poniżej są ODCZYTANE z tego pliku, nie z dokumentacji.

## Architektura, którą trzeba obsłużyć

| parametr | wartość |
|---|---|
| `block_count` | 48 |
| `embedding_length` | 3840 |
| `feed_forward_length` | 15360 |
| `attention.head_count` | 16 |
| `attention.head_count_kv` | **[8, 8, 8, 8, 8, 1]** (wzorzec powtarzalny) |
| `attention.key_length` / `_swa` | **512** / **256** |
| `attention.sliding_window` | 1024 |
| `attention.sliding_window_pattern` | **[T, T, T, T, T, F]** (powtarzalny) |
| `rope.freq_base` / `_swa` | 1e6 / 1e4 |
| `rope.dimension_count` / `_swa` | 512 / 256 |
| `final_logit_softcapping` | 30,0 |
| `context_length` | 262144 |
| słownik | 262144, embedding Q6_K, wagi Q4_0 |
| `tokenizer.ggml.model` | `gemma4` |

Rozkład warstw wynikający z wzorca: warstwy o indeksie `% 6 != 5` są LOKALNE
(okno 1024, head_dim 256, 8 głowic KV), a `% 6 == 5` GLOBALNE (head_dim 512,
1 głowica KV). Potwierdzone na tensorach: `blk.0.attn_q` ma 4096 = 16 x 256,
a `blk.5.attn_q` 8192 = 16 x 512.

**Warstwy globalne nie mają projekcji V — wtedy V = K.** Potwierdzone w
implementacji wzorcowej (`llama.cpp` `ff067f76`, `src/models/gemma4.cpp`):

```cpp
// note: use_alternative_attention (v_proj is optional, if it's not present, use k_proj)
layer.wv = create_tensor(tn(LLM_TENSOR_ATTN_V, "weight", i), {...}, TENSOR_NOT_REQUIRED);
...
ggml_tensor * Vcur = model.layers[il].wv ? build_lora_mm(...) : Kcur;
```

Dodatkowo każda warstwa ma dwie normy „sandwich" (`post_attention_norm`,
`post_ffw_norm`) i skalar `layer_output_scale`.

## Referencja i baseline

`llama.cpp` z master (`ff067f76`) zbudowany pod ROCm/gfx1030 w
`~/llama.cpp-master/build-rocm-master` — OSOBNY worktree i katalog builda, żeby
nie ruszać buildu `~/llama.cpp/build-rocm`, z którego pochodzą dotychczasowe
liczby odniesienia dla qwena i Mistrala.

Zmierzone na tym modelu (6900 XT, `-ngl 99`, pp1024/tg128):

| silnik | prefill | decode |
|---|--:|--:|
| llama.cpp `ff067f76` | **1 384,7 tok/s** | **27,4 tok/s** |
| FORGE | nie uruchamia (patrz plan) | — |

Wyjście referencyjne (`--temp 0`, prompt „Wymień trzy największe miasta w
Polsce.”) jest sensowne i po polsku, z widocznym tokiem rozumowania modelu.
UWAGA: decode 27,4 zmierzone przy zegarze pamięci zatrzaśniętym na 456 MHz
(patrz `STATUS.md`), więc porównywać wolno tylko z naszym pomiarem w tym samym
stanie.

## Graf forward — ustalenia z implementacji wzorcowej

Rzeczy, których NIE DA SIĘ odgadnąć z samych metadanych, a każda zmienia wynik:

1. **`f_attention_scale = 1.0`** — Gemma 4 NIE skaluje przez `1/sqrt(head_dim)`.
2. **Embedding wejściowy mnożony przez `sqrt(n_embd)`** (tylko dla wejścia
   tokenowego, nie dla surowych embeddingów obrazu).
3. **V dostaje CZYSTĄ normalizację RMS bez wagi** (`ggml_rms_norm`), podczas gdy
   Q i K mają swoje wagi `attn_q_norm` / `attn_k_norm`.
4. Kolejność w bloku uwagi: projekcja -> reshape -> RMSNorm -> rope. K jest
   ropowane, V nie.
5. **Warstwy globalne używają tensora `rope_freqs`** jako `freq_factors`
   (proporcjonalne rope); warstwy z oknem nie.
6. FFN to **GeGLU z `ggml_gelu`**, czyli przybliżenie tanh — nie dokładny erf.
   Nasz `gelu_mul_f16` jest zgodny.
7. **`layer_output_scale` mnoży wyjście CAŁEJ warstwy**, po FFN i rezydualu.
8. Softcapping logitów: `tanh(x / cap) * cap` po `output_norm`, przed samplingiem.
9. Cache KV jest **przeplatany** (`build_attn_inp_kv_iswa`) — osobny dla warstw z
   oknem i osobny dla globalnych.

## Co jest już zrobione

- **Kafel `gemm_q4_0_dot4`** (int8 na `v_dot4_i32_i8`, warianty 64x64/128x64/128x128
  i out_f32). Sprawdzony wobec referencji hosta na trzech kształtach, błąd 4,8e-4
  czyli samo zaokrąglenie f16. Przydaje się każdemu modelowi Q4_0, nie tylko Gemmie.
- **`gelu_mul_f16`** — GeGLU z przybliżeniem tanh (wariant z referencji Gemmy,
  nie dokładny erf).
- **`gemma4.ron`** — mapa ról tensorów, z `AttnV` jako opcjonalną.
- **Nowe role**: `PostAttnNorm`, `PostFfwNorm`, `LayerOutputScale`.
- **`AltAttnParams`** w `Hyperparams` — naprzemienna geometria uwagi (wzorzec
  okien, `head_dim_swa`, `n_kv_heads_swa`, `rope_theta_swa`), plus
  `ffn_activation` i `final_logit_softcap`. Wzorce z GGUF są krótkie i
  powtarzalne, więc parser rozwija je modulo długość — bez tego geometria od
  siódmej warstwy byłaby zła.

## Co zostaje, w kolejności

1. ~~Referencja dla warstw bez V.~~ **ZROBIONE** — V = K, patrz wyżej.
2. **Kernele uwagi head_dim 512.** Obecne specjalizacje to hd64/hd128/hd256, a
   silnik odrzuca inne wprost (`head_dim {} has no attention specialization`).
   Potrzebne decode i prefill, prawdopodobnie w wariancie split (hd512 z f16 KV
   to 2 KB na token na głowicę).
3. **Okno przesuwne** — maskowanie w kernelach uwagi plus polityka w cache KV
   (warstwy lokalne trzymają najwyżej 1024 tokeny).
4. **Cache KV o geometrii per warstwa.** To najgłębsza zmiana: dziś układ zakłada
   jednolitą liczbę bajtów na token na warstwę, a tu jest 8 x 256 wobec 1 x 512.
   Istniejąca zwarta mapa `global_layer -> kv_layer` (z prac nad Qwen3.6) jest
   punktem wyjścia.
5. **Graf forward**: dwie normy sandwich, `layer_output_scale`, GeGLU, dwie
   konfiguracje rope per typ warstwy.
6. **Softcapping logitów** (`softcap * tanh(x / softcap)`) przed samplingiem.
7. **Tokenizer `gemma4`** — nowy typ modelu tokenizera w GGUF.
