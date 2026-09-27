# EKS-A11 — ramię ANE w silniku: cache programów i tryb T256 (Apple M1, 16 GB)

Dwa pytania po EKS-A10: (1) czy silnik naprawdę nie zapisuje skompilowanych
programów ANE do cache (hipoteza z EKS-A10 §3: „cache zawiera tylko `model.milhash`,
E5 bundle rekompilowany przy każdym ładowaniu, ~3 GB pamięci anonimowej”), (2) czy
ograniczenie zestawu funkcji do T256 (4× mniejsze bufory we/wy programu ANE) zmienia
wynik prefillu, który z T1024 był WOLNIEJSZY niż GPU+CPU (40,9 wobec 129,7 tok/s).

Maszyna: Apple M1 (4P+4E), 16 GB, macOS 26.6.2 (25G83). Modele:
`.runtime/ane/bielik-minitron-7b-s060-int8/` (80 mlmodelc multifunction T256/T512/T1024,
int8 per-channel, share 0,6). Silnik: `cpu_share_prefill` (release, cecha `ane`),
prompt 512 i 1024, `FORGE_BENCH_REPS=1` (2 przebiegi, liczony drugi). Stan maszyny
przed pomiarem: wolne 5,8 GB + speculative 2,1 GB, wired 2,4 GB, bez ostrzeżeń
termicznych przed ani po żadnym przebiegu (`pmset -g therm`).

## Krótko

| pytanie | odpowiedź |
|---|---|
| **cache programów ANE w silniku** | **Działa, nic nie naprawiano.** Skompilowany program z wagami (`model.hwx`, 26,3 MiB `down` / 52,7 MiB `gate_up`) leży w systemowym cache `aned`: `/Library/Caches/com.apple.aned/25G83/ModelAssetsCache/<nazwa procesu>/<hash>/<hash>/model.hwx` (tylko root), a kernel mapuje go przy każdym ładowaniu (`AppleH11ANEInterface aneVnodeSecureLookup: Success … size: 27574272`). `~/Library/Caches/<proces>/com.apple.e5rt.e5bundlecache/` zawiera **w obu procesach** to samo: stub `H13G.e5` (2,4 KB) + `model.anehash` + `model.milhash`. 80 wpisów po 26/52 MiB (`weights/weights.bin`) w cache harnessu pochodzi z **jednego** przebiegu `MLModel.compileModel` z katalogu tymczasowego (21:18) — E5RT skopiował wagi, bo źródło miało zniknąć; pozostałe 400 bundle'i harnessu nie mają wag, tak samo jak silnik. Ładowanie w silniku: 80 modeli w 1,0–1,5 s (12–19 ms/model) = ciepły cache, nie rekompilacja (200–350 ms/model). Komunikat „must re-compile the E5 bundle” nie wystąpił (stdout, stderr, `log stream`). |
| **T256 wobec GPU+CPU** | **Zysk.** 1024 tokeny: GPU+CPU 130,0 → GPU+CPU+ANE **179,8 tok/s (+38%)**; 512: 121,9 → **199,2 tok/s (+63%)**. Wariant domyślny (wszystkie T, T1024 dla 1024 tokenów): **36,3 tok/s** — thrashing pamięci (wired 8,5 GB, 218 tys. drobnych i 106 twardych błędów stron na prefill, swapins +20 GB w trakcie przebiegu). |
| **gdzie idzie czas przy T256** | ANE nie jest wąskim gardłem: 320 predict × 6,4 ms = 2,0 s z 5,7 s prefillu 1024, czekanie w `join` 104 ms na cały prefill. Predict trwa 6,1–6,4 ms zamiast 4,4 ms z harnessu (współdzielona magistrala z GPU/CPU), ale i tak ANE stoi bezczynnie 64% czasu — udział ANE (share 0,6 FFN) można podnieść. |

## 1. Cache programów ANE — co naprawdę jest gdzie

Metoda: `log stream --predicate 'process == "aned" OR subsystem CONTAINS "coreml" OR
… OR eventMessage CONTAINS "E5"' --style compact` do pliku podczas
`the_ane_share_does_not_reach_decode` (ładuje 80 modeli T1024) i podczas
`eks_a10_multi … run n=80`; przeszukane stdout/stderr obu procesów; `codesign -dvvv`
obu binarek; zawartość obu katalogów cache.

| co | silnik (`cpu_share_prefill-0ba35e45b311095d`) | harness (`eks_a10_multi`) |
|---|---|---|
| podpis | adhoc, linker-signed, `Identifier=cpu_share_prefill-0ba35e45b311095d` | adhoc, linker-signed, `Identifier=eks_a10_multi` |
| `~/Library/Caches/<proces>/com.apple.e5rt.e5bundlecache/25G83/` | 480 wpisów: 240 × `model.milhash` (64 B, wskazuje inny wpis w tym samym cache) + 240 × bundle (`H13G.e5` 2,4 KB, `T{256,512,1024}_ane/model.anehash` 129 B); **0 × `weights.bin`** | 640 wpisów, 480 bundle, **80 × `weights/weights.bin`** — wszystkie z 21:18 (przebieg `compileModel`); 400 bundle bez wag |
| `log stream` przy ładowaniu 80 modeli | 56 (z 80, reszta „messages dropped") × `aneVnodeSecureLookup: Success … /Library/Caches/com.apple.aned/25G83/ModelAssetsCache/cpu_share_prefill-0ba35e45b311095d/…/model.hwx`, rozmiary 27 574 272 i 55 312 384 B | 29 × to samo, `ModelAssetsCache/eks_a10_multi/…` |
| komunikaty E5RT / „re-compile” | 0 | 0 |
| ładowanie 80 modeli | 997–1506 ms (T1024), 1270–1353 ms (T256) | 1,0–1,1 s (EKS-A10) |

Wniosek: e5bundlecache nie jest miejscem, gdzie żyje program ANE — jest nim
`ModelAssetsCache` demona `aned`, kluczowany **nazwą procesu**. To ma jedną
konsekwencję dla binarek `cargo test`: każda zmiana hasha w nazwie binarki to
jednorazowa rekompilacja 80 modeli (~20 s) i kolejne ~3 GB w katalogu roota
(`/Library/Caches/com.apple.aned`, bez `sudo` nie da się sprawdzić ani sprzątać).
Dla binarki produkcyjnej o stałej nazwie problemu nie ma. Kandydaci (a)–(e) z zadania
odpadają: podpis identyczny, ścieżka bez symlinków, `functionName` i konfiguracja
takie same, a klucz po nazwie procesu jest cechą `aned`, nie błędem silnika.

## 2. Zmiany w silniku (`crates/forge-kernels/src/ane_matmul.rs`)

- `FORGE_ANE_SHAPES` (np. `256` albo `256,1024`; domyślnie wszystkie z manifestu)
  zawęża zestaw funkcji. Kształt, który nie dzieli slotu aktywacji (`PREFILL_CHUNK`
  = 1024), jest odmową przy ładowaniu.
- Gdy dla `tokens` nie ma funkcji T' ≥ tokens, grupa liczy ceil(tokens / T_s)
  predictów największym dostępnym T_s < tokens, kolejno na kawałkach wejścia
  (krok T_s·cols·2 B) i wyjścia (krok T_s·width·2 B); `ane_rows()` zwraca ogon także
  wtedy. `AneDone.predicts` i `AneStats.sub_predicts` zliczają sub-predicty.
- Polityka ładowania: przy starcie komplet dla **najmniejszego** dozwolonego T,
  reszta leniwie w budżecie `MAX_RESIDENT` = 120 (bez zmian).
- `AneMatmul::load` przyjmuje `max_tokens` (wiersze slotu; wołający podaje
  `PREFILL_CHUNK`); bufor wyjściowy ma `max(max T, max_tokens)` wierszy.
- Test `what_the_three_units_are_worth_in_prefill` wypisuje `vm_stat` (free /
  speculative / inactive / wired) przed i po załadowaniu ANE i po każdym ramieniu
  z ANE, oraz błędy stron procesu (`getrusage`: `ru_minflt` / `ru_majflt`) per
  przebieg dla wszystkich czterech ramion.

## 3. Pomiar: tok/s czterech ramion

`FORGE_BENCH_REPS=1 FORGE_BENCH_PROMPTS=512,1024`, oba warianty jeden po drugim
(najpierw T256), 20 s przerwy.

| prompt | wariant | GPU | GPU+CPU | GPU+CPU+ANE | GPU+ANE | zysk ANE wobec GPU+CPU |
|---:|---|--:|--:|--:|--:|--:|
| 512 | `FORGE_ANE_SHAPES=256` | 101,1 | 121,9 | **199,2** | 145,9 | **+63%** |
| 1024 | `FORGE_ANE_SHAPES=256` | 99,9 | 130,0 | **179,8** | 156,4 | **+38%** |
| 512 | domyślny (T512 dla 512) | 101,1 | 123,4 | 115,1 | 100,0 | −7% |
| 1024 | domyślny (T1024 dla 1024) | 99,9 | 129,8 | **36,3** | 42,2 | −72% |

Bramka logitów `the_ane_share_keeps_the_logits` z `FORGE_ANE_SHAPES=256` (512 tokenów
= 2 predict T256 na grupę): PASS, argmax 842/842, RMS 0,151% rozpiętości, max 0,62%.

## 4. Liczniki ANE i błędy stron (na jeden prefill)

| prompt | wariant | ramię | zleceń / predict | predict razem | na predict | czekanie w `join` | prefill | błędy stron drobne / twarde | doładowań / wypchnięć |
|---:|---|---|---|--:|--:|--:|--:|---|---|
| 512 | T256 | GPU+CPU+ANE | 80 / 160 | 975 ms | 6,1 ms | 62 ms | 2570 ms | 32 170 / 0 | 0 / 0 |
| 512 | T256 | GPU+ANE | 80 / 160 | 1542 ms | 9,6 ms | 64 ms | 3509 ms | 7 / 0 | 0 / 0 |
| 1024 | T256 | GPU+CPU+ANE | 80 / 320 | 2040 ms | 6,4 ms | 104 ms | 5696 ms | 56 811 / 0 | 0 / 0 |
| 1024 | T256 | GPU+ANE | 80 / 320 | 2754 ms | 8,6 ms | 137 ms | 6548 ms | 16 / 0 | 0 / 0 |
| 512 | domyślny | GPU+CPU+ANE | 80 / 80 (T512) | 1341 ms | 16,8 ms | 20 ms | 4450 ms | 117 808 / **86** | 80 (1866 ms) / 40 |
| 512 | domyślny | GPU+ANE | 80 / 80 | 1760 ms | 22,0 ms | 0 ms | 5118 ms | 74 853 / 0 | 0 / 0 |
| 1024 | domyślny | GPU+CPU+ANE | 80 / 80 (T1024) | 9524 ms | **119 ms** | 995 ms | 28 225 ms | 217 633 / **106** | 80 (1932 ms) / 80 |
| 1024 | domyślny | GPU+ANE | 80 / 80 | 13 859 ms | **173 ms** | 2311 ms | 24 252 ms | 155 788 / 3 | 0 / 0 |

Odniesienie bez ANE: GPU 512 = 5063 ms (73 drobnych błędów stron), GPU+CPU 512 =
4201 ms (41 361 — wątki CPU dotykają slotów), GPU 1024 = 10 250 ms (6), GPU+CPU
1024 = 7879 ms (2475–42 251). Twardych błędów stron nie ma nigdzie poza wariantem
domyślnym.

Uwaga do wariantu domyślnego: przy trzech kształtach jest 240 funkcji na budżet
120, więc każda zmiana długości promptu to 80 doładowań i 40–80 wypchnięć (1,9 s)
— to nowa polityka „najmniejszy T na starcie” w połączeniu z pełnym zestawem.
Z `FORGE_ANE_SHAPES=256` wszystkie 80 funkcji siedzi w budżecie i doładowań nie ma.

## 5. Pamięć (vm_stat, MiB)

| moment | wariant | free | speculative | inactive | wired |
|---|---|--:|--:|--:|--:|
| przed uruchomieniem testu | T256 | 5758 | 2136 | 993 | 2413 |
| po wczytaniu checkpointu, przed ANE | T256 | 62 | 279 | 5730 | 2391 |
| po załadowaniu 80 × T256 | T256 | 91 | 111 | 1688 | **6384** (+3993) |
| w trakcie prefillu 512 / 1024 z ANE | T256 | 56–59 | 1–6 | 3015–3618 | 7385–8541 |
| po zakończeniu procesu | T256 | 3847 | 86 | 3062 | 4605 |
| po wczytaniu checkpointu, przed ANE | domyślny | 60 | 353 | 3827 | 2415 |
| po załadowaniu 80 × T256 (start) | domyślny | 56 | 140 | 1723 | 6363 |
| w trakcie prefillu 512 (po doładowaniu T512) | domyślny | 56–74 | 14–16 | 1420–1494 | **8193–8464** |
| w trakcie prefillu 1024 (po wypchnięciu 80, T1024) | domyślny | 56–60 | 49–153 | 1853–2005 | 5088–5434 |
| po zakończeniu procesu | domyślny | 6221 | 48 | 1893 | 4909 |

Liczniki jądra w oknie całego wariantu: T256 swapins +12,8 tys. stron (200 MB),
swapouts +12,9 tys.; domyślny swapins **+1,35 mln stron (20,5 GB)**, swapouts
**+1,61 mln (24,6 GB)**. To jest ten sam mechanizm co w EKS-A10 §1 (balast 4 GiB,
N=80): wired ponad ~8 GB przy 16 GB → każdy predict płaci błędami stron w jądrze.

## 6. Wniosek

1. Cache programów ANE w silniku jest w porządku; hipoteza z EKS-A10 §3 była
   pomyłką w odczycie e5bundlecache (stub, nie program). Nie ma czego naprawiać.
2. **Przy T256 ANE daje zysk**: +38% przy 1024 tokenach i +63% przy 512 wobec
   GPU+CPU, bez twardych błędów stron; bramka logitów przechodzi. Rekomendacja:
   `FORGE_ANE_SHAPES=256` jako ustawienie dla tej maszyny (16 GB); wariant
   domyślny z T1024 pozostaje pułapką pamięciową.
3. Czas przy T256 zjada GPU+CPU, nie ANE: ANE liczy 2,0 s z 5,7 s prefillu 1024
   (36% zajętości), czekanie w `join` to 104 ms. Predict jest 40% wolniejszy niż
   w izolacji (6,4 wobec 4,4 ms) — współdzielona magistrala, nie strony (błędy
   stron drobne 57 tys., ale to slot aktywacji dotykany przez CPU, tyle samo co
   GPU+CPU bez ANE). Następny krok to większy udział ANE (share > 0,6 albo
   q/k/v/o), bo ma 64% wolnego czasu; drugi — sprawdzić, czy 4 sub-predicty można
   wysyłać asynchronicznie, żeby ukryć koszt wywołania CoreML (ok. 1–2 ms z 6,4).
