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

**Warstwy globalne nie mają projekcji V.** Brak `blk.{5,11,17,23,29,35,41,47}.attn_v`
przy obecnym `attn_k` o szerokości 512. Nie wiadomo jeszcze, czy V = K, czy to
inny wariant uwagi — TEGO NIE WOLNO ZGADYWAĆ, bo błąd da poprawnie wyglądający,
ale merytorycznie zły model. Potrzebna referencja (llama.cpp master albo
`transformers`); lokalny build llama.cpp jest sprzed premiery i tego modelu nie
ładuje.

Dodatkowo każda warstwa ma dwie normy „sandwich" (`post_attention_norm`,
`post_ffw_norm`) i skalar `layer_output_scale`.

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

1. **Referencja dla warstw bez V.** Zbudować llama.cpp z master (albo przeczytać
   `modeling_gemma4` z `transformers`) i ustalić, czym jest V w warstwach
   globalnych. Bez tego kroku reszta jest bezcelowa.
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
