# Zrównoleglenie na wiele kart — plan oparty na pomiarze

Stanowisko: dwie Radeon AI PRO R9700 (`gfx1201`, 32 GiB każda), model
ThinkingCap-Qwen3.6-27B (`qwen35`): **gęsty** (bez MoE), ale **hybrydowy** —
48 warstw Gated-DeltaNet + 16 warstw uwagi + 1 blok MTP.

## Dlaczego to się opłaca — liczby, nie intuicja

Dekodowanie jest ograniczone CZYTANIEM WAG: 17 GB na token przy 551 GB/s to
~31 ms, i faktycznie mierzymy 34 ms. Podział wag na dwie karty daje 8,5 GB na
kartę, czyli ~15,5 ms.

Koszt synchronizacji zmierzony na tym sprzęcie (`tests/cluster_peer.rs`):

| | wartość |
|---|--:|
| peer access między kartami | **jest** |
| wymiana ukrytego stanu tokena (10 KiB) | **5,58 us** |
| 65 warstw x 2 wymiany na token | **0,73 ms** |

Czyli 0,73 ms narzutu wobec ~15,5 ms oszczędności: **komunikacja zjada 4,5%
zysku**. Oczekiwane przyspieszenie dekodowania to ~2,1x. Prefill jest
ograniczony obliczeniami, więc tam podział też daje blisko 2x.

## Stan obecny — co już jest, a czego nie ma

`--tp-cards` dzieli FFN kolumnowo i jest jedyną formą zrównoleglenia. Odmawia
temu modelowi:

```
Error: unsupported: model hybrydowy ma własną pętlę warstw, bez tego FFN
```

Rozpoznanie kodu pokazuje, że blokady są PŁYTSZE, niż sugeruje opis flagi:

- **Klaster jest gotowy.** `cluster.rs` ma peer access, `exchange_on` na
  wskazanym strumieniu, `wait_for` po zdarzeniach i kalibrację mocy kart.
- **Sharding jest format-agnostyczny.** `BlockFormat::of` obsługuje Q8_0, Q4_K i
  Q6_K; brakuje wyłącznie NVFP4 GGUF (36 B / 64 wartości) — jedno ramię `match`.
- **Dispatch GEMV ma już strukturę `match` po formacie**, tylko z jednym
  wypełnionym ramieniem (Q8_0). Kernele per karta dla NVFP4 i K-kwantów ISTNIEJĄ
  — to te same, których używa ścieżka jednokartowa.
- **Brakuje trzech rzeczy:** ramion formatów w dispatchu, wpięcia w pętlę warstw
  modelu hybrydowego, oraz ścieżki macierzowej (dziś tylko GEMV, czyli decode).

## Etapy

**A. TP dla FFN w modelu hybrydowym, dekodowanie.** Największy i najbardziej
samodzielny zysk. `gate`/`up` dzielone po wierszach, `down` po kolumnach,
all-reduce po `down`. Wymaga: NVFP4 w `BlockFormat`, ramion formatów w
`gemv_*_column_split`/`row_split`, zdjęcia bramki `is_hybrid` i wpięcia w
`hybrid_forward_staged`. Oczekiwane: decode 29,8 -> ~55 tok/s bez MTP.

**B. TP dla projekcji uwagi i DeltaNet.** `q`/`k`/`v` kolumnowo, `o` wierszowo.
DeltaNet dzieli się po GŁOWICACH — stan rekurencyjny jest per głowica i niezależny,
więc karta trzyma stan swoich głowic bez wymiany w skanie. To domyka warstwę.

**C. TP w prefillu.** Ta sama geometria, ale kernele macierzowe zamiast GEMV.
Prefill jest compute-bound, więc zysk jest bliski liniowemu.

**D. Pipeline parallel.** Podział po warstwach zamiast po wagach. Nie skraca
opóźnienia pojedynczego żądania tak jak TP, ale skaluje się na więcej kart i
zdejmuje limit VRAM dla większych modeli. Sensowne jako WARSTWA NAD TP przy 4+
kartach.

**E. Expert parallel dla MoE.** Eksperci są z natury niezależni, więc to
najprostsza forma podziału — ale ten checkpoint nie ma MoE, więc bez modelu
testowego byłby to kod niesprawdzony na realnych danych.

## Kolejność i uzasadnienie

A -> C -> B -> D -> E. A daje najwięcej na jednostkę pracy i jest testowalne
natychmiast (decode to najprostsza ścieżka do porównania bit w bit z jedną
kartą). C jest drugie, bo prefill już wygrywamy z llama.cpp i podwojenie go
przesuwa przewagę z 1,3x na ~2,5x. B jest większe, bo dotyka stanu DeltaNet.
D i E nie mają dziś czym być zweryfikowane na tym stanowisku.

**Bramka jakości dla każdego etapu: suma SHA 128 tokenów musi zostać
`0bf2b86b…`.** Podział nie zmienia matematyki, tylko rozkłada ją na karty —
jeśli suma się zmieni, znaczy że kolejność redukcji all-reduce jest inna niż
sekwencyjna i trzeba to naprawić, a nie zaakceptować.
