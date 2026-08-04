# Bielik w GGUF — co ten format naprawdę zawiera i czego brakuje, żeby go uruchomić

Pobrany `speakleash/Bielik-Minitron-7B-v3.0-Instruct-GGUF`, wariant **Q4_K_M**,
4,50 GB. To DOKŁADNIE ten sam model, który mamy w MLX 4-bit (4,207 GB), więc
porównanie izoluje format, a nie model.

Plik: `.runtime/models/bielik-minitron-7b-v3-gguf/`.

## Co czyta nasz parser

Bez zmian w kodzie, `Gguf::open` + `ModelDescriptor::detect`:

```
tensory: 363     architektura: llama, warstw: 40
  Q4K   241 tensorów   3,566 GB
  Q6K    41 tensorów   0,934 GB
  f32    81 tensorów   0,001 GB
```

Architektura i liczba warstw zgadzają się z naszym checkpointem MLX, a role
tensorów mapuje istniejąca tabela w `arch.rs`. Czyli wejście do formatu jest
gotowe — brakuje wyjścia na kernele.

## To nie jest jednolity 4-bit

Litera „M" w `Q4_K_M` oznacza mieszankę. Q6_K trafiło na:

- `attn_v` w 20 z 40 warstw,
- `ffn_down` w 20 z 40 warstw,
- `output.weight` (głowa).

## Skutek dla dekodowania: ten format jest z założenia wolniejszy

Dekodowanie czyta całą macierz na token, więc bajty przekładają się wprost na
sufit:

| | rozmiar | sufit dekodowania przy 102,4 GB/s |
|---|---:|---:|
| MLX 4-bit affine, grupa 64 | 4,207 GB | **24,34 tok/s** |
| GGUF Q4_K_M | 4,500 GB | **22,76 tok/s** |

**+7% bajtów to −6,5% sufitu.** Nie da się tego odrobić kernelem; to własność
formatu, nie implementacji.

## Q4_K JEST formą afiniczną — kernele już to potrafią

Nasz własny dekoder wzorcowy (`dequant.rs::dq_q4_k`) liczy

```
y = (d·sc) · q − (min·m)
```

na podblokach po 32 elementy. To jest dokładnie `q · skala + przesunięcie`,
czyli forma MLX affine — z `skala = d·sc` i `przesunięcie = −min·m`, przy
grupie 32 zamiast 64.

To ta sama tożsamość, którą wykorzystaliśmy w NA1 (MLX affine ≡ GGML Q4_1),
tylko w drugą stronę. **241 tensorów Q4_K da się więc podać istniejącym
kernelom 4-bitowym po przepakowaniu przy ładowaniu, bez pisania kernela.**
Skale warto trzymać w f16, nie bf16 — `ScaleDtype::F16` jest już obsługiwane, a
bf16 ma tylko 8 bitów mantysy na iloczyn `d·sc`.

Koszt: afiniczna grupa 32 to 5 bitów na wagę wobec 4,5 w Q4_K, więc przepakowane
241 tensorów urosłoby z 3,566 do około 3,96 GB.

Q6_K (0,934 GB) nie ma takiej drogi — to 6 bitów i nasze kernele go nie liczą.
Albo dochodzi kernel, albo te 41 tensorów idzie do f16 (0,934 → 1,87 GB, co
psuje dekodowanie jeszcze bardziej).

## PUŁAPKA: GGUF ma przestawione wiersze Q i K

Porównanie tych samych tensorów w obu formatach po rozpakowaniu:

| tensor | względna L2 |
|---|---:|
| `attn_q` | **1,3998** |
| `attn_k` | **1,3970** |
| `attn_v` | 0,1108 |
| `attn_output` | 0,1213 |
| `ffn_gate` | 0,1170 |
| `ffn_up` | 0,1168 |

Wszystko poza Q i K mieści się w 11–12%, czyli w rozrzucie dwóch niezależnych
kwantyzacji 4-bitowych tego samego oryginału. Q i K różnią się o 140% — to nie
jest kwantyzacja, to inna macierz.

Przyczyna: konwerter llama.cpp PERMUTUJE wiersze Q i K, bo llama.cpp liczy RoPE
na przeplatanych parach, a HF obraca połówki wektora. Odwrócenie permutacji:

```
HF[h·hd + a·(hd/2) + b] = GGUF[h·hd + b·2 + a]
```

Po jej zastosowaniu `attn_q` spada z 1,3998 na **0,1142**, a `attn_k` z 1,3970
na **0,1132** — czyli dokładnie tam, gdzie reszta. Diagnoza jest więc
potwierdzona, a nie założona.

To jest najgroźniejszy element całej listy, bo **nie objawia się awarią**. Nasz
kernel RoPE to `ROPE_HALF_SPLIT`, czyli wariant HF. Podanie mu wag GGUF wprost
dałoby model, który generuje poprawnie wyglądający, całkowicie błędny tekst.

## Czego brakuje, żeby to uruchomić

1. Ścieżka ładowania GGUF w `MlxDense` — dziś czyta wyłącznie
   `model.safetensors` i konfigurację z `config.json`; tu jedno i drugie siedzi
   w metadanych GGUF.
2. Przepakowanie Q4_K → affine grupa 32 przy ładowaniu (algebraicznie dokładne).
3. Odwrócenie permutacji Q/K.
4. Decyzja o 41 tensorach Q6_K: własny kernel albo rozpakowanie do f16.
5. Tokenizer — w GGUF jest osadzony, a my czytamy `tokenizer.json`.

Punkty 1–3 są mechaniczne. Punkt 4 jest realną pracą, a punkt 5 decyduje o tym,
czy da się w ogóle porównać wyjście tekstowe z obecną ścieżką.
