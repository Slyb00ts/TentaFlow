# FORGE wobec llama.cpp na Radeonie AI PRO R9700 — Qwen3.6-27B (2026-07-30)

Stanowisko: jedna R9700 (`gfx1201`, RDNA4, 32 GiB), Ryzen 9 7950X. W maszynie są
dwie takie karty, ale **wszystkie pomiary poniżej są jednokartowe**
(`HIP_VISIBLE_DEVICES=0`) — druga karta służyła do kompilacji katalogu kerneli i
mikrobenchmarków, żeby nie zaburzać pomiaru.

Modele (oba to ten sam checkpoint `ThinkingCap-Qwen3.6-27B-MTP`, `qwen35`,
65 bloków, `nextn_predict_layers = 1`):

- `qwen36-27b-Q4_K_M.gguf` — 15,65 GiB, `token_embd` i większość projekcji Q4_K,
  `ffn_down` oraz `attn_qkv` w Q6_K,
- `qwen36-27b-NVFP4.gguf` — 16,95 GiB wg llama.cpp (plik 18,2 GB).

llama.cpp: build `3018a11e (109)`, `-DGGML_HIP=ON -DAMDGPU_TARGETS=gfx1201`,
`-ngl 99 -fa 1`. FORGE: `forge bench --prefix-cache off`.

## 1. Wynik — p1024 / tg128

`llama-bench -p 1024 -n 128` wobec `forge bench --prompt-tokens 1024 --tokens 128`.

| model | miara | FORGE przed | **FORGE teraz** | llama.cpp | |
|---|---|--:|--:|--:|---|
| Q4_K_M | prefill p1024 | 842,9 | **1481,2** | 1027,9 | **FORGE 1,44x** |
| Q4_K_M | decode tg128 | 27,7 | **27,7** | 27,4 | FORGE 1,01x |
| Q4_K_M | decode tg128 + MTP | *nie startowało* | **55,8** | brak w llama-bench | 2,01x nad własnym decode |
| NVFP4 | prefill p1024 | 1013,4 | **1311,2** | 929,0 | **FORGE 1,41x** |
| NVFP4 | decode tg128 | 29,1 | **29,1** | 28,0 | FORGE 1,04x |
| NVFP4 | decode tg128 + MTP | 66,0 | **66,0** | brak w llama-bench | 2,27x nad własnym decode |

`llama-bench` nie ma trybu spekulatywnego, więc MTP po obu stronach mierzy się
osobno, na realnym prompcie (§2).

Suma SHA 128 wygenerowanych tokenów jest **ta sama dla obu kwantyzacji, z
MTP i bez** (`0bf2b86b…`) — czyli ani optymalizacje prefillu, ani spekulacja nie
zmieniły ani jednego tokena.

## 2. Realny prompt — jedyny uczciwy pomiar MTP

`llama-bench` nie ma trybu spekulatywnego, więc MTP mierzy się osobno. Protokół:
ten sam prompt po polsku (pytanie o HBM wobec GDDR), 200 tokenów, `temp 0`,
szablon czatu po obu stronach, `-st` po stronie llama.cpp (bez tego `llama-cli`
nie kończy tury). llama.cpp: `--spec-type draft-mtp --spec-draft-n-max 3`.

| model | tryb | FORGE | llama.cpp | |
|---|---|--:|--:|---|
| Q4_K_M | bez spekulacji | **31,9** | 27,7 | **FORGE 1,15x** |
| Q4_K_M | MTP K=3 | **52,1** | 43,4 | **FORGE 1,20x** |
| NVFP4 | bez spekulacji | **33,6** | 28,1 | **FORGE 1,20x** |
| NVFP4 | MTP K=3 | **58,0** | 54,1 | **FORGE 1,07x** |

Liczby FORGE to `tok/s overall`, czyli RAZEM z prefillem 32-tokenowego promptu;
liczby llama.cpp to samo `Generation`. Przewaga jest więc liczona na niekorzyść
FORGE — przy prompcie 32 tokenów prefill to poniżej 1% czasu przebiegu.

**Wszystkie cztery komórki wygrywają — ale to jest pomiar na prompcie
32-tokenowym; na długim kontekście MTP przegrywa, patrz §2b.** Poprzednia wersja tego dokumentu
notowała MTP na NVFP4 jako jedyną przegraną (40,6 wobec 46,4) i wskazywała
akceptację draftu jako przyczynę. Pomiar tego nie potwierdza:

| model | akceptacja | tokeny na forward |
|---|--:|--:|
| Q4_K_M | 1,96/krok | 2,96x |
| NVFP4 | 1,87/krok | 2,87x |

Akceptacja jest praktycznie taka sama dla obu kwantyzacji i daleko od notowanych
wcześniej 1,35/krok — czyli hipoteza „słaby proposer na NVFP4" nie ma podstaw.
Stara liczba nie jest odtwarzalna z obecnego drzewa i była zbierana przy innym
obramowaniu promptu (56 tokenów zamiast 32), więc nie przypisuję poprawy
konkretnej zmianie; przypisuję ją protokołowi pomiaru.

## 2b. Długi kontekst — tu MTP nam się sypie

Protokół §2 ma prompt 32-tokenowy. Ta sama para silników na **realnym prompcie
4672 tokenów** (28 notatek technicznych po polsku, plik, nie ziarno tokenizera) i
odpowiedzi 512 tokenów wygląda inaczej. `llama-cli -st -c 8192 -f`, `forge run`.

Prefill rośnie łagodnie po obu stronach i FORGE wygrywa na całej długości —
`llama-bench -p` wobec `forge bench --prompt-tokens`, oba przemierzone dziś:

| model | miara | FORGE | llama.cpp | |
|---|---|--:|--:|---|
| Q4_K_M | pp1024 | **1477,9** | 1043,4 | 1,42x |
| Q4_K_M | pp4096 | **1369,6** | 965,6 | 1,42x |
| Q4_K_M | pp8192 | **1293,8** | 901,2 | 1,44x |
| NVFP4 | pp1024 | **1287,8** | 920,0 | 1,40x |
| NVFP4 | pp4096 | **1292,9** | 860,4 | 1,50x |
| NVFP4 | pp8192 | **1228,2** | 814,7 | 1,51x |

Decode bez spekulacji też trzyma się na długim kontekście (liczby llama.cpp
wyliczone z `-pg pp,tg`: to przepustowość ŁĄCZNA, więc decode wychodzi z
odjęcia prefillu):

| model | kontekst | FORGE | llama.cpp | |
|---|---|--:|--:|---|
| Q4_K_M | 4096 | **26,6** | 26,7 | remis |
| Q4_K_M | 8192 | 25,9 | **26,2** | llama.cpp 1,01x |
| NVFP4 | 4096 | **28,3** | 27,5 | 1,03x |
| NVFP4 | 8192 | **27,4** | 27,1 | 1,01x |

**MTP na długim kontekście miał defekt strukturalny — znaleziony profilem i
naprawiony.** Stan po naprawie (prompt 4672 + odpowiedź 512):

| model | tryb | FORGE decode | llama.cpp decode | FORGE łącznie | llama.cpp łącznie |
|---|---|--:|--:|--:|--:|
| Q4_K_M | bez spekulacji | **26,6** | 26,9 | **22,63 s** | 24,22 s |
| Q4_K_M | MTP K=3 | 27,3 -> **38,5** | 43,5 | 22,21 -> **16,76 s** | 16,77 s |
| NVFP4 | bez spekulacji | **28,0** | 27,4 | **21,92 s** | 24,36 s |
| NVFP4 | MTP K=3 | 34,0 -> **54,2** | 59,9 | 18,71 -> **13,08 s** | 14,03 s |

NVFP4 z MTP wygrywa teraz cały przebieg (13,52 s wobec 14,03 s), bo nasz prefill
jest o połowę szybszy; w samym decode zostaje 0,86x i to jest praca do zrobienia.

### Defekt: uwaga weryfikatora liczyła się kaflem PREFILLOWYM

`rocprofv3 --kernel-trace`, ten sam przebieg z MTP i bez, kontekst 4672:

| kernel | bez MTP | z MTP | delta |
|---|--:|--:|--:|
| `attn_prefill_wmma` | 245,9 ms | **804,0 ms** | +558,1 |
| `gemv_nvfp4_gguf_q8_1_b4` | 0 | 526,1 ms | +526,1 |

Kafel prefillowy zrównolegla po TOKENACH. Przy weryfikacji T=4 daje to grid
`ceil(4/64) x 24 głowice` = **24 grupy robocze na 64 CU**, z których każda
szeregowo przechodzi cały kontekst — 2,28 ms na warstwę, razy 16 warstw uwagi,
czyli 36,5 ms na krok weryfikacji.

Kernel z podziałem kontekstu (`attn_verify_split8_f16_hd256_t3/t4`) istniał,
artefakty dla gfx1201 BYŁY w katalogu, a launcher i tak go nie brał, bo bramka
sprawdzała `caps.vendor != Nvidia`. Kernel nie ma ani jednego intrinsicu
producenta — `warp.sum`, bariery, LDS — więc realnym wymogiem jest fala 32 i
pojemność LDS, i tak brzmi teraz warunek. Po zmianie: **804,0 -> 49,0 ms**
(16,4x), suma SHA bez zmian dla obu kwantyzacji, z MTP i bez.

To trzeci raz w tym pliku, gdy „NVIDIA-only" okazuje się bramką postawioną za
szeroko, a nie własnością kernela — po dense prefillu i po `head_dim`.

### Co zostaje: 0,87x w samym decode i gdzie dokładnie siedzi

Druga pozycja z profilu, `gemv_nvfp4_gguf_q8_1_b4` (526 ms), NIE jest defektem:
to poprawny kernel WSADOWY B4, a 27,7 ms na krok wobec ~31 ms teoretycznego
pełnego odczytu wag oznacza, że siedzi na roofline pamięci.

Ślad rozdzielony na cel i draft (segmentacja po wsadowym `lm_head` celu):

| część kroku weryfikacji | ms/krok | uwaga |
|---|--:|---|
| cel, forward T=4 | 45,8 | z tego 27,7 to NVFP4 B4 na roofline |
| draft, 3 kroki MTP | 7,6 | z tego **6,4 to `lm_head` draftu** |
| **razem GPU zajęte** | **53,4** | |
| bezczynność GPU | 8,4 | 13% zegara |

Dwa wnioski liczbowe:

1. **Forward celu T=4 jest już optymalny.** Kosztuje 45,8 ms wobec 35,7 ms
   forwardu dekodowania T=1 i daje do czterech tokenów zamiast jednego. Część
   NVFP4 czyta model RAZ, z przepustowością 551 GB/s.
2. **Cała nadwyżka to draft i narzut uruchomień.** Przy 53,4 ms pracy GPU i
   3,13 tokena na forward wychodzi 17,1 ms na token, czyli **58,6 tok/s** —
   praktycznie tyle, co llama.cpp (59,9). Mierzymy 51,9, więc **całą różnicę
   zjada 13% bezczynności GPU.**

Bezczynność ma trzy zidentyfikowane źródła (1,8 ms/krok) i jedno rozproszone:

- powrót do hosta po każdym tokenie draftu (3 x 273 us) — pętla draftu czyta
  argmax na hoście, więc jest z natury szeregowa;
- `synchronize` po grafie celu (278 us) — konieczny, host czyta decyzję;
- `copyBuffer` po migawce stanu DeltaNet (145 us);
- **reszta, ~6,6 ms/krok, to przerwy poniżej 5 us rozłożone na ~1400 uruchomień
  kerneli.** Forward celu jest przechwycony w graf (`hybrid_verify_graphs`), więc
  jego ~700 uruchomień jest tanie; **pętla draftu graf, którego nie ma**.

Dalszy przebieg tej pracy — co zadziałało, a co nie:

**Głowa `lm_head` draftu: 15,36 s -> 13,08 s, i jest teraz domyślna.** Diagnoza z
profilu potwierdziła się co do joty. `gemv_nvfp4_gguf_out_f32` liczył JEDEN
workgroup na wiersz słownika (248320 grup roboczych) i czytał wagi BAJT PO
BAJCIE — 111 GB/s. Obok, w tym samym pliku, stał już szybki wariant `_wave`:
fala na wiersz, redukcja wewnątrz fali bez LDS, odczyty ośmioelementowe. Różnił
się WYŁĄCZNIE typem zapisu (f16). Sparametryzowanie go przez `OUT: DType` i
usunięcie starego odwróciło wynik, więc `FORGE_MTP_DRAFT_HEAD` ma teraz domyślne
`auto`: repack robi się, gdy head źródłowy jest Q8_0 i mieści się w puli wag, a
inaczej zostaje Q8_0 (jawne `nvfp4` nadal jest błędem, jeśli się nie da). Target
weryfikuje na oryginalnym headzie, więc wyjście jest bez zmian.

To NIE dotyczy checkpointu Q4_K_M: jego `output.weight` jest Q6_K, nie Q8_0, więc
`auto` zostawia go w spokoju. Ta sama różnica tłumaczy lukę w akceptacji —
**1,73/krok na Q4_K wobec 2,15 na NVFP4** przy identycznym wyjściu obu
kwantyzacji. Głowa 6-bitowa daje gorsze propozycje draftu, a nie gorszy model.

**Trzy hipotezy o bezczynności GPU — WSZYSTKIE OBALONE pomiarem:**

1. *Drenaże urządzenia.* Zwężenie `device.synchronize()` do `stream.synchronize()`
   w propose i po grafie celu: 13,50 -> 13,36 s, czyli 1%. Zmiana została, bo jest
   ściśle węższa i poprawna, ale to nie było źródło.
2. *Powroty do hosta w pętli draftu.* `mtp_propose_pending` jest JUŻ w całości na
   GPU: gather po indeksie z bufora, argmax na GPU, jeden odczyt K identyfikatorów
   na końcu.
3. *Narzut wysyłki w niezgrafowanej pętli draftu.* Segmentacja śladu pokazała coś
   odwrotnego niż zakładałem:

   | faza kroku | uruchomienia | zajęte | bezczynne |
   |---|--:|--:|--:|
   | draft (3 kroki MTP) | 65 | 7,65 ms | **0,77 ms** |
   | cel (forward T=4) | 1793 | 45,46 ms | **~9 ms** |

   Draft jest oszczędny. Cała bezczynność siedzi w forwardzie CELU — a on JEST w
   grafie. `FORGE_HYBRID_VERIFY_GRAPH=0` daje 13,57 wobec 13,38 s, czyli graf
   odzyskuje 1,4%. Zatem te ~9 ms to nie narzut wysyłki, tylko przestoje na
   łańcuchu 1793 kerneli, z których większość jest drobna: 516 kwantyzacji
   aktywacji (0,79 ms realnej pracy), 171 `copyBuffer`, 136 rmsnorm.

Stąd jedyna pozostała droga jest strukturalna: **zmniejszyć liczbę kerneli w
forwardzie celu.** Dwa konkretne miejsca: (a) `prequant_q8_1` kwantyzuje
aktywację OSOBNO dla każdej projekcji, choć `q`/`k`/`v` oraz `gate`/`up` dzielą to
samo wejście — potrzebny jawny uchwyt „przygotowanej" aktywacji, wzorem
istniejącego `Q8ActPrepared`, bez nowego kernela Mojo; (b) migawka stanu DeltaNet
to 96 osobnych kopii D2D na krok, bo stan każdej warstwy jest osobną alokacją.

**K nie jest tu dźwignią** — sprawdzone po naprawie, długi kontekst:

| model | K=2 | K=3 |
|---|--:|--:|
| NVFP4 | 2,50 tok/forward, 14,98 s | **3,13 tok/forward, 13,50 s** |
| Q4_K_M | 2,42 tok/forward, 26,06 s | **2,73 tok/forward, 16,72 s** |

## 2c. Realne zadanie generacyjne — kod Snake 3D

Prompt 331 tokenów (specyfikacja gry Snake 3D w Pythonie: pygame + PyOpenGL,
kamera orbitalna, kolizje, HUD, zapis wyniku), 1536 wygenerowanych tokenów kodu.
`llama-cli -st -c 8192 -f` wobec `forge run`. Prefill FORGE liczony z przebiegu
`-n 1` po odjęciu jednego kroku dekodowania; decode z pary `-n 1` i `-n 1536`.

| model | tryb | FORGE prefill | llama.cpp | | FORGE decode | llama.cpp | |
|---|---|--:|--:|--:|--:|--:|--:|
| NVFP4 | bez MTP | **931** | 695 | **1,34x** | **29,8** | 27,8 | **1,07x** |
| NVFP4 | MTP K=3 | **847** | 685 | **1,24x** | 51,2 | **58,5** | 0,88x |
| Q4_K_M | bez MTP | **964** | 766 | **1,26x** | **27,9** | 27,1 | **1,03x** |
| Q4_K_M | MTP K=3 | **876** | 714 | **1,23x** | 44,9 | **45,9** | 0,98x |

Prefill wygrywa w KAŻDEJ komórce, decode bez spekulacji w obu modelach; decode z
MTP to remis na Q4_K i przegrana na NVFP4.

**Uwaga o kolumnie prefillu: te liczby są WYPROWADZONE i zaniżone dla trybu MTP.**
Prefill FORGE liczyłem jako `331 / (T_1 - 1/decode)`, ale przy MTP pierwszy krok
to pełna weryfikacja (~53 ms), a nie `1/51,5 s = 19 ms`. Odjąłem o ~34 ms za
mało, więc spadek prefillu przy MTP jest w tej tabeli sztucznie zawyżony. Pomiar
FAZAMI z `forge bench` (§2d) pokazuje prawdziwy obraz.

**Akceptacja zależy od PROMPTU, nie od kwantyzacji.** Na tym zadaniu NVFP4 daje
1,83/krok a Q4_K 2,11 — dokładnie ODWROTNIE niż na streszczeniu (2,15 wobec
1,73). Wcześniejsza teza z tego dokumentu, że 6-bitowa głowa `output.weight`
Q4_K daje gorsze propozycje, NIE MA POPARCIA i zostaje wycofana.

### Bezczynność GPU skaluje się z liczbą uruchomień kerneli

Zmierzone na obu ścieżkach, kontekst 4672:

| ścieżka | uruchomienia | zajęte | bezczynne | us/uruchomienie |
|---|--:|--:|--:|--:|
| decode bez MTP (na token) | 1600 | 31,5 ms | 6,13 ms (16%) | 3,8 |
| krok weryfikacji T=4 | 1793 | 45,5 ms | ~9 ms | 5,0 |

To jest stała naszego potoku: **każde uruchomienie kernela kosztuje ~4-5 us
przestoju**, niezależnie od tego, ile pracy wykonuje. Rozkład na token pokazał, że
około 1060 uruchomień wykonuje 1,8 ms pracy i kosztuje ~4 ms przestoju.

Największa pozycja: **430 kopii `copyBuffer` na token** przy 0,48 ms realnej
pracy. Profil wskazał je jako serie po ~8 na warstwę DeltaNet, a źródłem była
replikacja GQA w `hybrid_delta_mixer`:

```rust
for r in 0..rep {                 // rep = n_v / n_k = 4
    self.device.copy(&hb.q16, 0, &hb.q32, r * key_bytes, key_bytes, stream)?;
    self.device.copy(&hb.k16, 0, &hb.k32, r * key_bytes, key_bytes, stream)?;
}
```

Osiem kopii na warstwę razy 48 warstw = 384 uruchomienia na token, żeby
przenieść 8 KiB. Zastąpił je jeden kernel `deltanet_repeat_qk_f16`. Dodatkowo
cięcie wyjścia splotu na wycinki q/k robiły dwie kolejne kopie na warstwę tylko
po to, żeby konsument dostał bufor od zera — `l2norm_heads_f16_at` czyta je przez
przesunięcie bajtowe, bez nowego kernela Mojo (bufory `q16src`/`k16src` usunięte).

Wynik, ta sama sekwencja, kontekst 4672:

| | uruchomienia/token | `copyBuffer`/token | zegar/token | bezczynne |
|---|--:|--:|--:|--:|
| przed | 1600 | 430,2 | 37,66 ms | 6,13 ms |
| **po** | **1269** | **52,2** | **36,92 ms** | **5,04 ms** |

Minus 331 uruchomień i minus 1,09 ms przestoju na token; decode bez spekulacji
29,1 -> 29,8 (NVFP4) i 27,4 -> 27,9 (Q4_K). Suma SHA bez zmian — kernel robi
dokładnie tę samą kopię.

**Ścieżka MTP na tym nie zyskała** i to jest spójne: weryfikacja T=4 nie używa
miksera jednotokenowego, tylko `deltanet_prepare_f16`, który ma podział QKV oraz
L2/repeat JUŻ SCALONE w jednym kernelu, i dzieli kwantyzację aktywacji przez
`prepare_q8_1`. Dlatego miała 171 kopii na krok, nie 430 — czyli ta klasa błędu
była tam naprawiona od początku.

## 2d. Ile naprawdę kosztuje MTP przy prompcie

`forge bench` raportuje fazy osobno, więc nie trzeba nic wyprowadzać. p1024:

| model | prefill bez MTP | prefill z MTP | | catch-up MTP | TTFT bez -> z MTP |
|---|--:|--:|--:|--:|--:|
| NVFP4 | 1323,1 | 1310,7 | -0,9% | 34,4 -> **3,5 ms** | 781 -> 823 -> **805 ms** |
| Q4_K_M | 1461,1 | 1449,6 | -0,8% | 34,3 -> **3,5 ms** | 707 -> 747 -> **720 ms** |

**Sam prefill spada o 0,9%**, czyli tyle samo co u llama.cpp (-1,4%). Cały koszt
MTP przy prompcie siedział w OSOBNEJ fazie catch-upu, która dokłada się do TTFT,
a nie zmniejsza przepustowości prefillu.

### Catch-up czytał macierz wag na każdy token

Faza skalowała się liniowo — 34,3 ms dla 1024 i 137,6 ms dla 4096, czyli stałe
33,5 us na token promptu. Dla porównania prefill CELU to 11,7 us na token na
warstwę, a catch-up przechodzi przez JEDNĄ warstwę MTP. Trzykrotność kosztu
warstwy była sygnałem, że coś jest nie tak.

`mtp_project_joined_q8_f16` ma `token = block_idx.y`, czyli siatkę
`(hidden/8, n_tokens)`: **jeden blok na parę (grupa wierszy, token)**, więc każdy
token czyta całą macierz `eh_proj` (5120 x 10240 Q8_0 = 55,7 MB) od nowa. To ten
sam antywzorzec co w głowie draftu — kernel jednotokenowy użyty do wsadu.

Projekcja `eh_proj` to zwykły GEMM `[t, 2h] x [h, 2h]ᵀ`, więc ścieżka wsadowa
przechodzi teraz przez `self.gemm`, który czyta wagi RAZ:

| prompt | catch-up przed | po | |
|---|--:|--:|--:|
| 1024 | 34,3 ms | **3,47 ms** | **9,8x** |
| 4096 | 137,6 ms | **12,3 ms** | **11,1x** |

TTFT z MTP spada o 18-27 ms, czyli różnica wobec przebiegu bez spekulacji zeszła
z +5,3% do **+0,5%** (NVFP4) i z +5,7% do **+0,3%** (Q4_K).

Kolejność redukcji różni się od wariantu sekwencyjnego, ale catch-up karmi
PROPOSER, a target weryfikuje każdy token — suma SHA 128 tokenów jest niezmieniona
(`0bf2b86b…`) dla obu kwantyzacji, z MTP i bez.

## 2e. Systematyczne polowanie na wzorzec „kernel jednotokenowy we wsadzie"

Ten sam błąd wyszedł w tej sesji trzy razy, więc zamiast czekać na czwarty
przeszukałem kod trzema kryteriami. Wynik: **na poziomie launcherów jest czysto,
zostaje rozproszony podatek od uruchomień.**

1. **Siatka `(wiersze/X, n_tokens)`** — wagi czytane raz na token. Jedno
   trafienie w całym `launchers.rs`: `mtp_project_joined_q8_f16` (naprawione,
   §2d). Pozostałe siatki z `n_tokens` są albo kafelkowane
   (`n_tokens.div_ceil(bm)`), albo dotyczą uwagi i KV, gdzie praca na token jest
   z natury per token i nie ma macierzy wag.
2. **Jeden workgroup na wiersz bez kafelkowania** — `rmsnorm_*` i `layernorm_*`
   (poprawne: jeden blok na token, każdy token normalizowany niezależnie),
   `gemv_f16`/`gemv_f16_bias` (z definicji batch 1), `pack_gguf_fp8` (jednorazowy
   pack). Jedyny błędny przypadek, `gemv_nvfp4_gguf_out_f32`, naprawiono
   wcześniej.
3. **Pętle hosta wysyłające wiele uruchomień** — pętle po warstwach są
   nieuniknione (każda ma inne wagi). Nie-warstwowe: replikacja GQA (naprawiona),
   `perplexity` z jednym uruchomieniem na token (ścieżka pomiarowa, nie serwująca)
   oraz `mtp_catchup_verified_prefix_pending` z pętlą po zaakceptowanych tokenach
   (do 4 iteracji, marginalne).

Ranking pomiarowy tego, co zostało — po LICZBIE uruchomień, nie po czasie pracy
(faza decode, 64 tokeny z MTP, kontekst 4672):

| kernel | uruchomienia | praca | us/szt |
|---|--:|--:|--:|
| `quantize_act_q8_1` | 9289 | 14,2 ms | 1,5 |
| `gemv_nvfp4_gguf_q8_1_b4` | 5472 | 492,8 ms | 90,1 |
| `gemm_q8_0_small_batch` | 3456 | 123,6 ms | 35,8 |
| `copyBuffer` | 3078 | 13,0 ms | 4,2 |
| `rmsnorm_residual_f16` | 2450 | 12,8 ms | 5,2 |

Dwie pozycje robią realną pracę (`b4` i `small_batch` siedzą na roofline
pamięci). Trzy pozostałe to 14 800 uruchomień wykonujących 40 ms pracy — przy
~4,5 us przestoju na uruchomienie kosztują około 67 ms, czyli **więcej niż
wykonują**. Ale w przeciwieństwie do 384 kopii GQA nie da się ich usunąć jednym
kernelem:

- `quantize_act_q8_1` (516 na krok) to jedna kwantyzacja na GEMM; rodzeństwo
  projekcji dzieli wejście, więc jawny uchwyt „przygotowanej" aktywacji wzorem
  `Q8ActPrepared` zdjąłby jej część — szacunkowo 1-2%;
- `copyBuffer` (171 na krok) to w większości migawka stanu DeltaNet: 48 warstw
  razy dwa bufory. Scalenie wymaga jednej ciągłej alokacji stanu zamiast alokacji
  per warstwa, czyli przebudowy puli stanów;
- `rmsnorm_residual` jest już jednym uruchomieniem na warstwę.

**Wniosek: skoncentrowane instancje wzorca zostały wyczyszczone.** To, co zostało,
to podatek rozłożony po ~1800 uruchomieniach na krok i zdejmuje się go fuzją
(norm + kwantyzacja + GEMM w jednym kernelu), a nie kolejnym pojedynczym fixem.

## 2f. Dwie karty — co z nich mamy dzisiaj

W maszynie są dwie R9700 (`rocminfo` widzi dwa agenty `gfx1201`). Wszystkie
pomiary w tym dokumencie są JEDNOKARTOWE, i dla tego modelu nie da się inaczej.

**Tensor parallel nie obejmuje tego modelu.** Jedyny mechanizm wielokartowy w
FORGE to `--tp-cards`, czyli podział FFN. `tp_ffn_capable` odrzuca modele
hybrydowe, co sprawdzone na sprzęcie:

```
$ forge run qwen36-27b-NVFP4.gguf --tp-cards 1
Error: unsupported: model hybrydowy ma własną pętlę warstw, bez tego FFN
```

Ograniczenia `--tp-cards` są zresztą szersze niż samo „bez hybryd": obejmuje
DEKODOWANIE modeli gęstych z wagami Q8_0, a prefill i tak zostaje na jednej
karcie. Dla `qwen35` nie zadziała nic z tego — model jest hybrydowy (48 warstw
DeltaNet + 16 uwagi), a jego wagi to NVFP4 albo Q4_K/Q6_K.

**Data parallel działa i skaluje się liniowo.** Dwie niezależne instancje, po
jednej na kartę, ten sam prompt 4672 + 512 tokenów z MTP:

| | czas | wobec jednej karty solo (12,95 s) |
|---|--:|--:|
| karta 0 | 12,94 s | 1,00x |
| karta 1 | 12,77 s | 0,99x |

Zero interferencji — każda instancja trzyma pełną prędkość jednokartową, więc
przepustowość zbiorcza jest **2x**. Model waży 17 GB, karta ma 32 GB, więc
komplet mieści się dwa razy.

Podsumowując: dziś druga karta podwaja PRZEPUSTOWOŚĆ (dwa równoległe żądania),
ale nie skraca opóźnienia POJEDYNCZEGO żądania ani o milisekundę. Żeby to
zmienić, trzeba by rozszerzyć podział na pętlę warstw modelu hybrydowego i objąć
nim prefill oraz formaty inne niż Q8_0 — czyli praktycznie napisać ścieżkę TP od
nowa pod ten model.

## 3. Optymalizacja prefillu — co dało ile

Kolejność wyznaczył PROFIL (`rocprofv3 --kernel-trace`), nie intuicja. Rozkład
czasu prefillu Q4_K_M (p1024) w stanie wyjściowym:

| kernel | udział |
|---|--:|
| `gemm_q4_k_wmma` | 52,5% |
| **`gemm_q6_k_dot4`** | **25,4%** |
| `deltanet_prepare` | 11,8% |
| `deltanet_value_key` | 5,7% |
| `attn_prefill` | 3,4% |

### 3.1 Q6_K nie miał kernela macierzowego

Q2_K, Q3_K, Q4_K i Q5_K miały wariant WMMA; **Q6_K jako jedyny K-kwant liczył się
na `v_dot4_i32_i8`**. W tym checkpoincie w Q6_K są `ffn_down` i `attn_qkv`, czyli
jedna czwarta prefillu. Jedna projekcja `down` kosztowała 4,66 ms wobec 1,43 ms
projekcji Q4_K o tej samej liczbie mnożeń.

`src/gemm_q6_k_wmma.mojo` czyta surowe superbloki 210 B tak samo, jak rodzina
Q5_K czyta swoje 176 B. Kluczowa własność układu: szesnaście kolejnych kolumn ma
to samo `half`, tę samą grupę i to samo `l // 16`, więc **dzieli jedną skalę** i
leży w jednym ciągłym odczycie 16 bajtów `ql` oraz 16 bajtów `qh`. Kafel K=16
pokrywa się z granicą skalowania i skalę wolno wmnożyć w wagi przed mnożeniem
macierzowym, zamiast rozbijać akumulację.

Skutek uboczny, który jest tu ważniejszy niż sama prędkość: ścieżka `dot4`
kwantyzowała aktywacje do int8, WMMA liczy w f16. **Suma SHA 128 wygenerowanych
tokenów zmieniła się z `d6e51316…` na `0bf2b86b…` — czyli na dokładnie tę samą,
którą daje ten model w NVFP4.** Q4_K przestał się rozjeżdżać z referencją.

### 3.2 Szesnaście linii czytało fragment z jednego banku LDS

Wszystkie kafle tej rodziny (Q4_K, Q6_K, NVFP4 GGUF) trzymają rozpakowane wagi w
LDS z krokiem wiersza `CHUNK * 2 = 128 B`. Fragment `b` czyta linia `lane % 16`,
czyli szesnaście linii pod adresami odległymi o 128 B — a to **dokładna
wielokrotność 32 banków LDS**, więc wszystkie trafiają w ten sam bank.

Rozsunięcie wiersza o 16 wartości (`LDS_PAD`) kosztuje 4 KiB LDS na kafel i nie
zmienia matematyki. Zmierzone na R9700, T=1024, TFLOPS:

| kernel | kształt | bez rozsunięcia | z rozsunięciem |
|---|---|--:|--:|
| Q4_K | 6144x5120 | 65 | **73** |
| Q4_K | 5120x6144 | 49 | **65** |
| Q6_K | 5120x17408 (T=512) | 52 | **60** |
| NVFP4 | 6144x5120 | 51 | **68** |

### 3.3 Kafel BN=64 czytał aktywacje dwa razy za dużo

Ruch aktywacji to `(n_rows / BN) * T * K * 2 B`. Przy BN=64 macierz aktywacji
`ffn_down` (35,6 MB) jest czytana 80 razy — mieści się w 64 MB Infinity Cache, ale
i tak dominuje. BN=128 połowi ten ruch i **wygrywa na każdym zmierzonym
kształcie**, w każdym z trzech formatów (TFLOPS, T=1024):

| format | kształt | BM256/BN64 | BM256/BN128 | BM512/BN128 |
|---|---|--:|--:|--:|
| Q4_K | 17408x5120 | 70 | 82 | **93** |
| Q4_K | 6144x5120 | 68 | 89 | **100** |
| Q4_K | 5120x6144 | 50 | **87** | 85 |
| Q6_K | 10240x5120 | 60 | 74 | **92** |
| NVFP4 | 17408x5120 | 65 | 77 | **85** |
| NVFP4 | 6144x5120 | 64 | 82 | **99** |

BM=512 potrzebuje T >= 512, żeby mieć czym wypełnić kafel, więc wybór należy do
launchera, który zna długość chunka.

**Nazwa kafla musi nieść jego geometrię.** Obraz HSACO jest związany z
architekturą, a zestaw gfx1100 jest już zbudowany i ma pod `_bm256` kafel BN=64.
Gdyby nowa geometria weszła pod starą nazwą, launcher liczyłby siatkę z BN=128
dla kernela kafelkującego po 64 i po cichu pomijałby połowę wierszy. Stąd
`_bm256_bn128` i `_bm512_bn128` obok zachowanego `_bm256`.

### 3.4 Q6_K: przesunięcie o 32 ściąga się na liczbie całkowitej

Pierwsza wersja liczyła wagę jako `q6 * scale - 32 * scale` w f16, tak jak Q4_K
i Q5_K ściągają swój człon `dmin`. Odejmowanie przed skalowaniem jest dokładne
(kod ma sześć bitów) i zostawia jedno zaokrąglenie zamiast trzech, więc zostało —
ale **na tych danych nie zmieniło wyniku o ani jedną cyfrę**, więc nie jest to
poprawka wydajności ani dokładności, tylko tańszy zapis tej samej wartości.

Szukanie tego było skutkiem BŁĘDNIE SKALIBROWANEGO TESTU, i to jest tu lekcja.
`gemm_q6_k_matches_canonical_dequant_shapes` porównywał wynik GPU z referencją,
która kwantyzowała aktywacje do int8 — bo do tej pory Q6_K szedł na AMD kaflem
`dot4`, więc obie strony kwantyzowały tak samo. Po przejściu na WMMA referencja
opisywała już inną arytmetykę niż kernel. Po wyrównaniu jej do faktycznie
wybranego kafla zostało 3% błędu WZGLĘDNEGO w jednym elemencie na 16384 — i to
też nie jest usterka: zmierzone `want` wynosi tam 1,8e-3 przy członach iloczynu
rzędu 1, czyli iloczyn skalarny kasuje się prawie do zera, a zaokrąglenie wagi do
f16 (robi to CAŁA rodzina kafli WMMA) zostaje w wyniku jako kilka procent.
`golden.rs` ma na takie ścieżki próg 0,05 i ten test dostał ten sam.

Przy okazji usunięty został predykat `Kernels::int8_batch_activations()`: mówił
„na AMD batch kwantyzuje aktywacje", co po wejściu kafli WMMA przestało być
prawdą, a jedyne żywe użycie było właśnie w tej referencji.

### 3.5 Roofline karty — zmierzony, nie z papieru

Bez tych liczb nie da się powiedzieć, czy kernel jest szybki. Wszystkie zmierzone
na tej R9700 (`bench-amd/bench_wmma_gfx11.mojo`, `bench_roofline_gfx.mojo`):

| jednostka | przepustowość | wobec f16 |
|---|--:|--:|
| f16 WMMA 16x16x16 | 179 TFLOPS | 1,0x |
| int8 WMMA 16x16x16 | 357 TOPS | 2,0x |
| **fp8 WMMA 16x16x16** | **378 TFLOPS** | **2,1x** |
| **iu4 WMMA 16x16x32** | **743 TOPS** | **4,2x** |
| odczyt DRAM | 551 GB/s | |
| odczyt Infinity Cache (64 MiB) | 1828 GB/s | 3,3x DRAM |

`iu4` sprawdzony na przypadku ujemnym (−32 zamiast cichego 15 wariantu bez znaku).

### 3.6 Gdzie naprawdę jest sufit kafla f16

Kafel Q4_K liczy 73–104 TFLOPS zależnie od kształtu. Trzy pomiary rozstrzygają,
co go trzyma — i dwa pierwsze obalają hipotezy, które wyglądały oczywiście:

1. **Dekwantyzacja nie kosztuje nic.** Ten sam kafel na wagach JUŻ w f16 daje
   97 TFLOPS wobec 97–104 dla Q4_K. Rozpakowanie superbloku chowa się całkowicie
   za mnożeniami.
2. **To nie jest ruch globalny.** Fragment `a` czyta szesnaście wierszy odległych
   o `n_cols * 2` bajtów (10 KB), czyli zupełnie nieskoalescowanie — ale
   przepuszczenie aktywacji przez LDS jest DWA RAZY WOLNIEJSZE (35–67 TFLOPS).
3. **To ściana rejestrów.** Kafel ma 189 z 256 VGPR: 16 akumulatorów po 8 f32 to
   128, a każdy fragment f16 zajmuje 8. Każda zmiana, która potrzebuje więcej —
   potokowanie odczytu następnego podkroku (18 TFLOPS), MTILE=8 (17 TFLOPS) —
   kończy się zrzutem rejestrów. Sama instrukcja nie jest winna: mikrobenchmark
   z szesnastoma niezależnymi akumulatorami trzyma 180 TFLOPS.

**Wniosek: f16 nie da się poprawić kafelkowaniem, a formaty z blokową skalą nie
pomogą.** Przy skali zmieniającej się co 32 kolumny (Q4_K, Q6_K, NVFP4)
akumulator int32 trzeba zrzucić do f32 w środku pętli: dla `iu4` to około 26
cykli na kafel wobec 13,6 cyklu samej instrukcji, czyli mimo 4,2x szybszej
instrukcji rachunek wychodzi gorzej niż f16. Dlatego wcześniejsza próba
`gemm_q4_k_i8wmma` wyszła 3,3x wolniej — i dlatego kolejna próba w tę stronę też
by wyszła.

### 3.7 FP8 zdejmuje ścianę

`src/gemm_fp8_wmma.mojo` ma tę samą geometrię, ale fragment operandu zajmuje
**2 VGPR zamiast 8**, a skale są per wiersz wagi i per token — czyli stałe wzdłuż
K, więc w pętli wewnętrznej NIE MA żadnego zrzucania akumulatora. Zmierzone
(TFLOPS, T=1024):

| kształt | kafel f16 | **fp8 BM512/BN128** | **fp8 BM256/BN128** |
|---|--:|--:|--:|
| 17408x5120 (`ffn_gate`/`ffn_up`) | 97 | **203** | 172 |
| 5120x6144 (`ssm_out`) | 79 | 139 | **184** |

**2,1–2,3x na najgrubszym kernelu prefillu**, przy błędzie względnym 1,5e-5 wobec
referencji hosta (test złoty `tests_amd_fp8_wmma.mojo`, trzy kafle). To jest
pierwsza wersja, bez potokowania — na które teraz jest miejsce w rejestrach.

### 3.8 Wynik

| krok | Q4_K_M prefill | NVFP4 prefill |
|---|--:|--:|
| stan wyjściowy | 842,9 | 1013,4 |
| WMMA dla Q6_K | 975,8 | — |
| rozsunięcie LDS | 1008,9 | 1036,1 |
| kafel BN=128 | 1261,7 | 1282,2 |
| DeltaNet równolegle po tokenach + wąskie kafle | **1442,4** | 1283,8 |
| **razem** | **+71,1%** | **+26,7%** |
| **wobec llama.cpp** | **1,40x** | **1,38x** |

Suma SHA 128 wygenerowanych tokenów jest przez całą tę drogę ta sama
(`0bf2b86b…`) i identyczna dla obu kwantyzacji.

### 3.9 DeltaNet: 8,8x z samego kształtu siatki

`deltanet_prepare_dynamic_f16` miał JEDEN BLOK NA GŁOWĘ i przechodził wszystkie
tokeny w pętli — dla 27B to 64 bloki na karcie o 64 CU, czyli dwie fale na SIMD
przy 1024 iteracjach szeregowo. Tymczasem jedyna zależność między tokenami to
przyczynowy splot o oknie `d_conv - 1`, którego wejście już leży w pamięci.
Druga oś siatki (32 tokeny na blok) daje 3036 -> 346 us na warstwę, **bitowo ten
sam wynik** (0 rozbieżnych wartości), czyli 135 -> 17 ms w prefillu.

Tą samą drogą poszły wąskie projekcje: bramki `ssm_alpha`/`ssm_beta` mają 48
wierszy, a kafel prefillowy pokrywa 128, więc dostawały DWA bloki robocze na całą
kartę — 222 us na wywołanie, 96 wywołań, 21 ms zmarnowane. Mały kafel ma tam 32
bloki: 21 -> 8,9 ms.

## 4. MTP dla Q4_K_M — czego brakowało

llama.cpp wyciągał z MTP na tym pliku 1,33x, my nie startowaliśmy w ogóle. To
nie była luka wydajnościowa, tylko trzy brakujące kawałki — w Q4_K_M inne
tensory są w innych formatach niż w wariancie NVFP4:

| tensor | NVFP4 | Q4_K_M | co było potrzebne |
|---|---|---|---|
| `token_embd` | NVFP4 | **Q4_K** | gather Q4_K (wsadowy i jednowierszowy) |
| `nextn.eh_proj` | Q8_0 | **Q4_K** | przekwantowanie przy ładowaniu |
| `output` (głowa) | Q8_0 | **Q6_K** | batchowa głowa logitów z wyjściem f32 |

1. **Gather embeddingu.** 715 MB tablicy — przekwantowanie odpada, więc doszły
   `gather_q4_k_rows_f16` i `gather_q4_k_row_f16`, ta sama formuła co
   `gemm_q4_k_wmma`. Test złoty porównuje oba z kanoniczną dekwantyzacją
   `forge-formats` i sprawdza, że ID spoza zakresu daje wyzerowany wiersz.
2. **`eh_proj`.** Cała ścieżka MTP (`mtp_prepare_f16`, `mtp_project_joined_q8_f16`)
   czyta tę jedną macierz kernelem Q8_0. Przekwantowanie przy ładowaniu kosztuje
   26 MiB VRAM na całą głowę i jest tańsze niż drugi komplet kerneli o innej
   arytmetyce — stąd `MtpTensorLoader::matrix_q8`, używane wyłącznie dla
   `eh_proj`.
3. **Głowa logitów Q6_K.** Weryfikacja MTP potrzebuje logitów dla T tokenów
   naraz, a `logits_gemm` miał dla K-kwantów tylko przemiat per token — czyli
   odczyt CAŁEJ głowy (1,27 GiB) raz na token draftu. Kernel batchowy Q6_K
   istniał, ale wyłącznie z wyjściem f16; sampling weryfikatora wymaga f32.
   Wariant f32 powstał przez sparametryzowanie tego samego kernela typem zapisu
   (`OUT: DType`, wzorzec z `gemm_dot.mojo`), więc matematyka i odczyt wag są te
   same — test złoty wiąże go wprost z wariantem f16.

## 5. Co dalej — z pomiarem, nie z intuicji

**Decode jest domknięty sprzętowo.** 16,8 GB wag na token w 33,6 ms to 500 GB/s
z osiągalnych 551 — **91% roofline'u DRAM**. Nie ma tam czego kafelkować: jedyna
dźwignia to CZYTAĆ MNIEJ BAJTÓW NA TOKEN, czyli akceptacja spekulacji.

**Prefill po tych zmianach** (per przebieg, Q4_K_M, T=1024):

| kernel | ms | udział |
|---|--:|--:|
| `gemm_q4_k_wmma` | 438,7 | 62% |
| `gemm_q6_k_wmma` | 101,2 | 14% |
| `deltanet_value_key` (skan) | 68,0 | 9,6% |
| `attn_prefill` | 41,0 | 5,8% |
| `deltanet_prepare` | 17,1 | 2,4% |
| reszta | ~34 | 5% |

Kolejność prac, jaką wyznaczają te liczby:

1. **FP8 strumieniowo — ZMIERZONE I ODRZUCONE.** Ścieżka powstała w całości
   (paker NVFP4->e4m3, dwa gniazda, drugi strumień, zdarzenia `packed`/
   `consumed`) i dała **970,0 tok/s wobec 1311,2** na NVFP4 p1024, czyli
   regresję o 26%. Projekcja „0,76 ms przepakowania chowa się za 5,0 ms GEMM-u"
   była błędna z dwóch powodów naraz:

   - **Przepakowanie robi tę samą pracę co GEMM, tylko dwa razy.** Paker
     dekwantyzuje każdą wagę w przebiegu absmax i drugi raz przy kodowaniu, a
     GEMM rozpakowywał ją raz. Dla `17408 x 5120` to 267 M wartości na projekcję
     i 3 projekcje na warstwę.
   - **Drugi strumień niczego nie chowa, bo obie prace są PRZEPUSTOWOŚCIOWE.**
     Nakładanie ukrywa opóźnienie, nie zajętość. Nie ma wolnych jednostek, w
     których cień mógłby się zmieścić, więc czas się sumuje: +345 ms
     przepakowania wobec ~70 ms oszczędności na GEMM-ach.

   Sedno jest strukturalne: **każda paczka służy dokładnie jednemu GEMM-owi**,
   więc jej koszt nigdy się nie amortyzuje. FP8 wygrywa TYLKO jako rezydencja,
   gdzie pakuje się raz przy ładowaniu i używa w każdym prefillu — a wtedy
   ogranicza je VRAM (pełna kopia e4m3 FFN to 17,4 GB przy modelu 17 GB na
   karcie 32 GiB). Realny wariant to rezydencja CZĘŚCIOWA: tyle warstw, ile
   mieści się w zapasie, bez kosztu przepakowania po rozgrzaniu.

   Z tej pracy ZOSTAJE `pack_nvfp4_gguf_fp8`: paker czyta surowe bloki 36 B GGUF
   NVFP4 i wkłada `output_scale` tensora w skalę wierszową paczki (GEMM e4m3 nie
   ma mnożnika wyniku). Jest bramkowany złotym testem
   `pack_gguf_fp8_matches_cpu_pack` w pięciu wariantach, w tym NVFP4 z
   mnożnikiem 0,0625. To on jest warunkiem wariantu rezydentnego.

2. **Uwaga prefillu na kaflu macierzowym — ZROBIONE.** `head_dim` jest
   parametrem kompilacji (`attn_prefill_wmma_impl[HD]`, instancje 128 i 256), a
   brakującym ogniwem był kontrakt pozycji bazowej: prefill layer-major podaje
   ją jako BUFOR GPU, a kafel WMMA brał SKALAR HOSTA. Wariant
   `attn_prefill_wmma_pos_hd256` czyta `base_pos[0]` i wchodzi w launcherze
   `attn_prefill_device_pos_f16_hd256`; bez artefaktu zostaje ścieżka skalarna,
   więc pozostałe karty nic nie tracą.

   | model | przed | po |
   |---|--:|--:|
   | Q4_K_M p1024 | 1446,7 | **1481,2** |
   | NVFP4 p1024 | 1295,9 | **1311,2** |

   Suma SHA bez zmian — kafel jest bitowo zgodny ze ścieżką skalarną.

3. **`deltanet_value_key`** — sprawdzona i ODRZUCONA tania hipoteza: skan ma na
   token dwie redukcje warpowe spięte zależnością `predicted -> delta -> state`,
   więc wyglądał na ograniczony opóźnieniem. Cztery kolumny na falę zamiast
   dwóch (cztery przeplatające się łańcuchy zamiast dwóch, wynik bitowo ten sam)
   dały **1475,2 tok/s wobec 1481,2** — czyli nic, w granicach szumu. Skan nie
   stoi na ILP redukcji. Zostaje chunkowa postać macierzowa (chunked linear
   attention), która zamienia go na GEMM-y — duża zmiana algorytmiczna.

4. **Akceptacja draftu MTP na NVFP4 — ZAMKNIĘTE, nie było czego naprawiać.**
   Przemierzone na jednym protokole (§2): akceptacja to 1,87/krok na NVFP4 i
   1,96/krok na Q4_K, czyli praktycznie tyle samo, a NVFP4 z MTP wygrywa z
   llama.cpp 56,8 wobec 54,1. Lekcja jest o pomiarze, nie o kernelu: „przegrana
   komórka" pochodziła z porównania dwóch różnie obramowanych przebiegów.
5. Rozsunięcie LDS i kafel BN=128 wpisano do Q4_K, Q6_K i NVFP4. **Q2_K, Q3_K i
   Q5_K mają ten sam defekt** i czekają na własny pomiar — ten checkpoint ich nie
   używa.
6. `test_catalog_matches_committed_manifest` jest CZERWONY dla `gfx1100` i
   `gfx1030` i był czerwony już przed tą pracą (`gemm_q8_0_wmma_128x128` z
   poprzedniej sesji). Przyczyna jest strukturalna: HSACO jest związany z
   architekturą, więc katalog danej karty da się zbudować TYLKO na niej, a 7900 XT
   i 6900 XT nie ma już w maszynie. Kernele przenośne dodane w tej pracy
   (`gather_q4_k_*`, `deltanet_prepare_tokens_f16_t32`,
   `gemv_q6_k_dp4a_batch_out_f32_*`) należą do wszystkich zestawów i trafią tam
   przy najbliższym buildzie na tamtych kartach. gfx1201 przechodzi.

## Sufit pasma i rola Infinity Cache (zmierzone)

R9700 ma 64 MB Infinity Cache (L3), 8 MB L2, 64 CU. Zmierzona przepustowość
odczytu zależy od tego, czy zbiór roboczy MIEŚCI SIĘ w L3 i jest ponownie
używany:

| zbiór roboczy | przepustowość |
|---|--:|
| 48 MiB | **1033 GB/s** |
| 64 MiB | **1124 GB/s** |
| 256 MiB | 572 GB/s |
| 1 GiB | 519 GB/s |
| 4 GiB | **552 GB/s** |

Wniosek jest jednoznaczny: Infinity Cache daje około **dwukrotność** pasma
VRAM, ale WYŁĄCZNIE przy ponownym użyciu danych mieszczących się w 64 MB.

**Dekodowanie tego nie spełnia i spełnić nie może.** Krok dekodu czyta ~16,1 GB
wag — 250 razy więcej, niż mieści L3 — i czyta każdy bajt DOKŁADNIE RAZ. Zanim
kolejny token sięgnie po tę samą wagę, przez cache przepłynęło 16 GB, więc jest
ona dawno wyparta. Cache nie jest tu „źle użyty": jest zarządzany sprzętowo i po
prostu nie ma czego przyspieszyć.

Sufitem dekodowania jest więc strumieniowe pasmo VRAM, ~552 GB/s:

    16,1 GB / 552 GB/s = 29,2 ms/token = 34,1 tok/s

To jest TWARDA granica dla Q4_K_M na jednej karcie, nie cel do pobicia.
Zmierzone: FORGE 29,2 tok/s (86% sufitu), llama.cpp Vulkan 31,95 (94%).
Dwie karty dają 36,0 tok/s, czyli WIĘCEJ niż sufit jednej — bo dzielenie wag
podnosi sam sufit.

Prefill korzysta z L3 naprawdę: macierz jest tam używana wielokrotnie w kaflu
GEMM, a 48-59 MB mieści się w 64 MB. Stąd 1322 tok/s.

Praktyczny wniosek: pojedynczego dekodowania nie da się rozpędzić powyżej
~34 tok/s żadną optymalizacją kerneli. Przez tę ścianę przechodzi się TYLKO
podnosząc intensywność arytmetyczną — spekulacją (MTP weryfikuje T tokenów na
jednym odczycie wag) albo batchem. Dlatego MTP daje 79,5 tok/s przy tym samym
paśmie.

## Q4_K_M: stan i następny krok (zmierzone)

| konfiguracja | początek | po zmianach |
|---|--:|--:|
| 1 karta, decode | 28,3 | **30,1** |
| 2 karty, decode | 35,3 | **37,3** |
| 1 karta, prefill | ~1315 | **1325,2** |

Odniesienie llama.cpp `ea63b4d` (30.07) na tych samych kartach: Vulkan 31,95 na
JEDNEJ karcie, ROCm 28,1. Obu backendom `-sm row` odmawia startu
(`device does not support split buffers`), więc druga karta nie daje im nic:
Vulkan spada do 26,9, ROCm do 27,9.

Co dało zysk:
- `gemv_q4_k_dp4a_group4_f16` — grupowanie jednorodne,
- `gemv_mixed_dp4a_group4_f16` — grupowanie MIESZANE, bo `Q4_K_M` dobiera
  format per tensor (`q`/`k` w Q4_K obok `v` w Q6_K, `attn_qkv` w Q6_K obok
  bramki w Q4_K); bez tego najliczniejsze trójki i czwórki szły pojedynczo,
- dp4a dla Q6_K w dekodowaniu.

Uruchomienia spadły z 1287 do 1046 na token, czas z 33,0 do 31,8 ms.

**Następny krok, z liczbami.** `ffn_down` to teraz 64 uruchomienia i 7,10 ms na
token (22% kroku), a jego wariant Q6_K osiąga tylko **459 GB/s** przy suficie
~620. Powód: przy 17408 kolumnach nie mieści się w oknie aktywacji LDS
(`X_MAX = 16384`), więc idzie ścieżką bez stagingu — każdy blok wierszy czyta
aktywację z pamięci od nowa, co przy 640 blokach dokłada ~22 MB do 58,9 MB wag.

Podnoszenie `X_MAX` jest ODRZUCONE pomiarem (29,2 -> 28,1: większy bufor zabiera
zajętość wszystkim kernelom dp4a).

**Staging w kawałkach też ODRZUCONY — zaimplementowany i zmierzony.**
Powstały `gemv_q4_k_dp4a_wide_f16` i `gemv_q6_k_dp4a_wide_f16`: aktywacja
stagingowana w dwóch turach po <= 16384 kolumn, iloczyn akumulowany między nimi,
LDS nietknięte. Kernele są POPRAWNE — dają ten sam SHA co `ffn_down` przez dp4a
z podniesionym `X_MAX`, czyli inna droga i identyczna matematyka. Ale nie dają
NIC: Q6_K `ffn_down` 131,3 us wobec 128,2 us ścieżką f16, całość 30,2 wobec 30,1
tok/s. Kod usunięty.

Hipoteza, która to napędzała, była BŁĘDNA. Zakładałem, że wariant f16 marnuje
pasmo, bo każdy z 640 bloków wierszy czyta aktywację z pamięci od nowa — ~22 MB
dołożone do 58,9 MB wag. Ale aktywacja to 34,8 KiB i MIEŚCI SIĘ W CACHE: te
odczyty nigdy nie szły do VRAM. Nie było czego odzyskiwać. To jest ten sam
Infinity Cache, o którym wyżej napisano, że dekodowaniu nie pomaga — pomaga, ale
małym danym wielokrotnie czytanym, nie strumieniowi wag.

Co naprawdę ogranicza `ffn_down` (449-535 GB/s wobec ~620): wiersz ma 17408
kolumn, czyli 68 superbloków, a macierz tylko 5120 wierszy, czyli 640 bloków
roboczych. Długi szeregowy spacer po wierszu przy małej liczbie bloków nie ma
czym ukryć opóźnienia. Lekarstwem jest podział wiersza między bloki (split-K) z
redukcją, a nie zmiana sposobu stagingu aktywacji.

## Dlaczego część kerneli stoi na 430-530 GB/s: ZA MAŁO BLOKÓW

Zmierzone GB/s zestawione z liczbą bloków roboczych (8 wierszy na blok):

| tensor | bloków | MB | us | GB/s |
|---|--:|--:|--:|--:|
| `ffn_gate`+`up` (grupa) | 4352 | 95,6 | 146,1 | **654** |
| `lm_head` Q6_K | 31040 | 1043,0 | 1657,1 | **629** |
| `ffn_down` Q4_K | 640 | 50,1 | 94,2 | 532 |
| `ffn_down` Q6_K | 640 | 58,9 | 131,3 | **449** |
| `ssm_out` Q4_K | 640 | 16,9 | 39,2 | **431** |

To nie jest kwestia formatu ani długości wiersza — `ssm_out` ma wiersze KRÓTKIE
(24 superbloki) i też stoi na 431 GB/s. Wspólnym mianownikiem jest liczba bloków:
macierze o 5120 wierszach dają 640 bloków, czyli 10 na 64 CU. Za mało, żeby ukryć
opóźnienia pamięci. Powyżej ~4000 bloków wszystko siada na suficie.

Dotyczy to `ffn_down`, `ssm_out` i `attn_output` — razem około 4,9 GB z 16,1 GB
czytanych na token, czyli 30% wag, chodzących ~470 zamiast ~630 GB/s. Przy
suficie to 7,8 ms zamiast 10,4 ms, czyli ~2,6 ms na token; z 32 ms kroku daje to
około +8%, czyli okolice 32,5 tok/s.

**SPLIT-K SPRAWDZONY I ODRZUCONY.** Zaimplementowany w całości:
`gemv_q4_k_dp4a_splitk_partial`, `gemv_q6_k_dp4a_splitk_partial` i
`splitk_reduce_f16` — wiersz dzielony na `n` zakresów kolumn liczonych przez
osobne bloki, każdy stagingujący tylko swój wycinek aktywacji, sumy cząstkowe w
f32 sumowane i zawężane do f16 jednym zaokrągleniem.

Zmierzone:

| wariant | decode tok/s |
|---|--:|
| bez split-K | **30,0** |
| `splits = 2` | 30,0 |
| `splits = 4` | 29,5 |

Na poziomie kernela zysk JEST, ale mały: Q6_K `ffn_down` 449 -> 478 GB/s
(123,3 us wobec 131,3), Q4_K bez zmian. Suma czasu kerneli spadła 32,09 -> 31,80
ms, czyli 0,29 ms — i to ginie w czasie ściennym. Przy czterech podziałach
koszt stagingu (każdy blok staginguje swój wycinek osobno) przeważa i wynik
spada. Kod usunięty.

Wniosek: sama liczba bloków NIE jest wąskim gardłem `ffn_down`. To już TRZECIA
odrzucona hipoteza dla tej macierzy — po podniesieniu `X_MAX` i stagingu w
kawałkach. Kolejnym podejrzanym jest sam format: superbloki Q6_K mają 210 bajtów,
więc są wyrównane tylko do 2 bajtów, co wymusza odczyty par `uint16` zamiast
szerokich, wyrównanych wektorów. Q4_K (144 B, wyrównane do 16) mierzy na tym
samym kształcie 532 GB/s wobec 449 GB/s Q6_K — różnica 18% przy identycznej
geometrii wskazuje właśnie na koszt układu bajtów, a nie na równoległość.

## Dekodowanie: gdzie NAPRAWDĘ jest sufit (2026-07-31)

Poprzednie sekcje szukały wolnych kerneli. Pomiar mówi, że ich nie ma — a
sufit jest niżej, niż zakładał rachunek `16,1 GB / 552 GB/s = 34,1 tok/s`.

### Każdy kernel GEMV płaci stały narzut rozbiegu, rosnący gdy jest krótki

Zestawienie z profilu (`rocprofv3`, Q4_K_M, kontekst 128) z bajtami liczonymi z
nagłówka GGUF, obok pomiaru tych samych kształtów na ZIMNYM DRAM
(`bench-amd/bench_decode_cold.mojo` — każda iteracja czyta inny wycinek bufora
4 GiB, więc Infinity Cache nie ma czego powtórzyć):

| kernel | MB | us w modelu | GB/s | us mikro (zimny) |
|---|--:|--:|--:|--:|
| `lm_head` Q6_K | 1042,9 | 1657,8 | **629** | — |
| `ffn_gate`+`up` (grupa) | 100,3 | 176,8 | 567 | 172 |
| `ffn_down` Q6_K | 73,1 | 128,2 | 570 | — |
| `ffn_down` Q4_K | 50,1 | 93,8 | 534 | — |
| DeltaNet in_proj+gate (mieszana) | 60,8 | 106,7 | 570 | — |
| uwaga q/k/v (grupa) | 41,3 | 75,8 | 545 | — |
| `ssm_out` / `attn_output` | 17,7 | 39,3 | **450** | 36 |

Mikrobenchmark odtwarza czasy z modelu co do mikrosekundy, więc **żaden kernel
nie jest wolny — są za krótkie.** Sweep po liczbie wierszy przy stałej długości
wiersza pokazuje monotoniczny wzrost pasma z czasem trwania kernela: 14,7 MB →
473 GB/s, 100 MB → 582, 401 MB → 597. To rozbieg podsystemu pamięci, nie ogon
fal grup roboczych — bo siatka TRWAŁA (blok przechodzi po kaflach krokiem
siatki, `bench-amd/bench_persist_grid.mojo`) odzyskuje najwyżej 9% na wąskich
macierzach i DOKŁADNIE ZERO na `17408 x 5120`.

**PUŁAPKA POMIAROWA, na którą sam wpadłem:** pierwszy mikrobenchmark czytał w
kółko ten sam bufor i pokazywał 980 GB/s dla `attn_qkv` Q6_K — bo 43 MB mieści
się w 64 MB Infinity Cache. Każdy pomiar pasma kernela dekodowania MUSI czytać
dane spoza cache, inaczej mierzy L3.

Rachunek sufitu, uczciwie: 15,82 GB czytane na token (16,80 GB pliku minus
`token_embd`, z którego decode bierze jeden wiersz, minus blok MTP). Przy
najlepszym zmierzonym paśmie kernela strumieniowego (629 GB/s, `lm_head`) to
25,1 ms. Ale tego pasma dosięga WYŁĄCZNIE kernel trwający 1,7 ms; 257 uruchomień
GEMV na token trwa po 39-177 us i siedzi na 450-570 GB/s. Realny sufit tej
ścieżki to **~28,5 ms samych GEMV-ów**, czyli około 33-34 tok/s ŁĄCZNIE z resztą
kroku — a nie 34 tok/s dla samego odczytu wag.

### Druga składowa: 3,5 us przestoju na KAŻDE uruchomienie

Rozkład przerw między kernelami na token (profil, 30 tokenów):

| przerwa | sztuk/token | ms/token |
|---|--:|--:|
| 2-5 us | 1027,9 | 3,636 |
| 5-10 us | 2,1 | 0,016 |
| >10 us | 2,0 | 0,326 |

Rozkład jest jednorodny: **nie ma pojedynczego wąskiego gardła, jest podatek od
liczby uruchomień.** Graf HIP tego nie zdejmuje (zmierzone wcześniej: 1,7%), bo
koszt jest po stronie GPU — bariera i drenaż fal między dyspozycjami, nie
wysyłka z hosta. Jedyne lekarstwo to MNIEJ KERNELI.

### Co z tego zrobiono

1. **Scalony wstęp kroku DeltaNet — 7 uruchomień na warstwę w 1.**
   `deltanet_step_prepare_f16` robi splot+SiLU, wycięcie v, obie normalizacje L2
   głowic q/k, powielenie GQA, log-decay i bramkę beta naraz. Podział bez
   zależności między blokami: siatka to głowice K, a blok `h` obsługuje kanały
   swojej głowicy q, swojej k i głowic V `h + r*n_k` — bo powielenie GQA mapuje
   głowicę V na `v % n_k`. Suma kanałów to dokładnie `conv_dim`.
   Blok ma `d_state` wątków, więc `block_reduce_sum` redukuje tyle samo wartości
   w tej samej kolejności co `l2norm_heads_f16` — **bitowo ten sam wynik**.
   Efekt: 1033 -> 745 uruchomień na token, `copyBuffer` 53 -> 5.
2. **Szerszy blok normy wiersza przy dekodowaniu jednego wiersza.** Krok dekodu
   normalizuje JEDEN wiersz, więc siatka miała jeden blok na 64 CU: 5,06 us.
   Blok dobrany do liczby kolumn daje 3,94 us.
   **REGRESJA ZŁAPANA PRZY POMIARZE:** pierwsza wersja warunku brzmiała
   `rows <= 8` i objęła weryfikację MTP (T=4). Szerszy blok zmienia KSZTAŁT
   redukcji, więc ta sama sekwencja policzyła inne ostatnie bity — suma SHA
   ścieżki Q4_K+MTP przestała się zgadzać, a przepustowość spadła 58,8 -> 56,9.
   Warunek to teraz `rows == 1`. Lekcja: każda zmiana szerokości bloku, w którym
   siedzi redukcja, jest zmianą ARYTMETYKI i musi być bramkowana sumą SHA
   WSZYSTKICH ścieżek, nie tylko tej, którą się stroi.
3. **Siatka trwała dla wąskich GEMV-ów** (`ssm_out`, `attn_output` — 640 kafli).
   Zmierzone +0,3% w modelu wobec 9% w izolacji; zostaje, bo jest bitowo zgodna
   i mierzalnie na plus, ale to już koniec tej dźwigni. Powyżej 2048 kafli
   kernel trwa dość długo, żeby rozbieg się zamortyzował — tam siatka trwała
   mierzy się na remis i nie jest używana.
4. **Scalony wstęp warstwy uwagi — 5 uruchomień w 1.** `attn_prepare_qk_f16`
   robi rozplecenie bramkowanej projekcji Q, RMSNorm głowic q, RMSNorm głowic k
   oraz oba częściowe RoPE. Siatka to głowice: bloki `[0, n_heads)` obsługują q,
   dalsze głowice k; blok ma `head_dim` wątków, więc `block_reduce_sum` redukuje
   tyle samo wartości w tej samej kolejności co `rmsnorm_f16`. Rozplecenie jest
   czystym ruchem f16, więc czytanie normy wprost z `q_full` daje tę samą
   wartość co czytanie zapisanego `qc` — **bitowo ten sam wynik**. Bariera po
   zapisie normy jest konieczna, bo RoPE łączy indeksy `j` i `j + n_rot/2`,
   czyli dwa różne wątki. 8,0 -> 2,34 us na warstwę.

Rachunek uruchomień i przestoju przez całą tę drogę (profil, kontekst 128):

| stan | uruchomienia / token | przestój / token |
|---|--:|--:|
| wyjściowy | 1033 | 3,98 ms |
| po scaleniu wstępu DeltaNet | 745 | 2,97 ms |
| po scaleniu wstępu uwagi | **681** | **2,74 ms** |

Zajętość kerneli nie drgnęła (30,65 -> 30,35 ms) i tak miało być: te fuzje nie
przyspieszają liczenia, tylko zdejmują podatek od liczby dyspozycji.

### Wynik, wszystkie cztery komórki z tą samą sumą SHA `0bf2b86b…`

Protokół: `forge bench --prompt-tokens 1024 --tokens 128 --reps 3
--prefix-cache off`, jedna karta (`HIP_VISIBLE_DEVICES=0`), stan przed zmierzony
tym samym poleceniem na `d5c8ffc5`.

| model | tryb | przed | po | |
|---|---|--:|--:|--:|
| Q4_K_M | bez spekulacji | 30,0 | **31,0** | +3,3% |
| Q4_K_M | MTP K=3 | 58,8 | **58,9** | +0,2% |
| NVFP4 | bez spekulacji | 30,0 | **30,9** | +3,0% |
| NVFP4 | MTP K=3 | 70,3 | **70,4** | +0,1% |
| NVFP4, 2 karty | bez spekulacji | 39,7 | **40,0** | +0,8% |

Ścieżki MTP zyskują minimalnie i to jest spójne: weryfikacja T=4 nie używa
miksera jednotokenowego, tylko `deltanet_prepare_f16`, który ma te kernele
scalone od początku — więc zostaje im wyłącznie to, co scalił wstęp uwagi.

UWAGA do wcześniejszych wpisów: notowane tu `NVFP4 decode 29,1` było
NIEAKTUALNE — ta sama binarka na `d5c8ffc5` mierzy dziś 30,0. Porównania
„przed/po" wolno robić wyłącznie wobec przebiegu wykonanego tego samego dnia na
tej samej maszynie, a nie wobec liczby z dokumentu.

### Czego z tego NIE da się wycisnąć więcej

Po zmianach: 32,2 ms na token, z czego 28,6 ms to same GEMV-y siedzące na
rozbiegu pamięci, ~1,7 ms reszta pracy i ~1,9 ms przestoju na 681 uruchomieniach.
Zostało 128 uruchomień w zasięgu tej samej metody (`silu_mul` 64,
`gated_rmsnorm` 48, `sigmoid_mul` 16), warte razem około 0,5 ms, czyli okolice
31,5 tok/s.

**`gated_rmsnorm` do skanu DeltaNet — ODRZUCONE ANALIZĄ, nie próbą.** Kształt
kusi, bo oba są per głowica V, ale skan ValueKey ma `COLUMN_TILES = 32`, czyli
128 kolumn jednej głowicy liczą 32 OSOBNE bloki. Redukcja normy po głowicy
wymagałaby synchronizacji całej siatki, a nie bloku.

**Fuzja normy rezyduum w SZEROKI GEMV jest odrzucona rachunkiem, nie próbą.**
`rmsnorm_residual` kosztuje 3,94 us pracy plus ~3,5 us przestoju. Gdyby liczył go
każdy blok konsumenta, przy 4352 blokach grupy `gate`/`up` doszłoby 43 MB ruchu
L2 na jedno wywołanie — 24 us przy 1800 GB/s. Norma jest tania właśnie dlatego,
że liczy się RAZ; wąskim gardłem jest tu uruchomienie, ale lekarstwo nie może
kosztować więcej niż choroba. Do tego dochodzi wyścig: bloki, które czytają
rezyduum PO tym, jak inny blok je zaktualizował, dodałyby deltę dwa razy.

**34 tok/s na jednej karcie dla tego checkpointu nie jest osiągalne.** Przez tę
ścianę przechodzi się wyłącznie podnosząc intensywność arytmetyczną — spekulacją
(MTP daje 58,6 i 70,0) albo drugą kartą.

## Dwie karty: co dokładnie kosztuje (2026-07-31)

NVFP4, p1024/tg128, `--tp-cards 1`: decode **39,7 tok/s** wobec 30,7 na jednej
karcie (+29%), MTP **78,6** wobec 70,0 (+12%). Suma SHA bez zmian.

Podział obejmuje dziś 88,5% bajtów czytanych na token (FFN, obie wejściowe
projekcje DeltaNet, głowa logitów), więc karta modelu powinna czytać 8,8 GB i
kończyć krok w okolicach 20 ms. Mierzymy 25,2 ms. Profil (`rocprofv3`, kontekst
128), zestawiony z przebiegiem JEDNOKARTOWYM TEGO SAMEGO modelu:

| | 1 karta | karta 0 z 2 | karta 1 z 2 |
|---|--:|--:|--:|
| uruchomienia / token | 1001 | **1290** | 626 |
| zajętość / token | 30,04 ms | 20,14 ms | 13,02 ms |
| przestój / token | 3,81 ms | **7,08 ms** | — |
| `copyBuffer` / token | 5 | **182** | 161 |
| czas ścienny / token | 33,85 ms | **27,22 ms** | |

Nadwyżka przestoju karty 0 rozkłada się na dwie pozycje: **+289 uruchomień**
(kopie, dodawania sum cząstkowych, projekcje liczone osobno zamiast grupowo)
kosztuje przy 3,5 us około **1,0 ms**, a pozostałe **~2,3 ms to czekanie na
kartę 1**.

Dwie mierzalne wady, obie wynikające z ASYMETRII podziału:

1. **343 kopie D2D na token.** Jedna karta trzyma stan i KV, więc każda
   pomocnicza projekcja wraca do niej. Na jednej karcie ten sam krok ma 5 kopii.
2. **Karta 0 wykonuje 55% więcej pracy niż karta 1.** To NIE jest zły stosunek
   podziału — sweep `--tp-split` od 8704/8704 do 6656/10752 pokazuje, że równy
   podział jest NAJLEPSZY, a każde przesunięcie w stronę karty 1 pogarsza wynik
   (3172 -> 3372 ms). Nadwyżka karty 0 to praca NIEPODZIELONA: projekcje uwagi,
   `ssm_out`, cały mikser DeltaNet, normy i sampling.

**Ile jest do wzięcia, liczone z tych pomiarów.** Gdyby podział objął CAŁĄ
warstwę, zajętość rangi zeszłaby do około 15,8 ms (połowa jednokartowych 30,04
plus redukcje), a przestój NIE dzieli się przez dwa — zostaje jednokartowe
3,81 ms plus synchronizacja, czyli ~5 ms. Krok wychodzi ~21 ms, czyli około
**47-48 tok/s wobec dzisiejszych 39,7**. To jest cała pula SPMD dla dwóch kart —
nie 2x, bo podatek od liczby uruchomień jest wspólny dla obu rang i nie maleje
od dołożenia karty.

UWAGA METODOLOGICZNA: pierwsza wersja tego akapitu zestawiała 1290 uruchomień
karty 0 (NVFP4) z 745 uruchomieniami jednokartowymi zmierzonymi na **Q4_K_M** i
wychodziło z tego +545. To było porównanie dwóch różnych kwantyzacji. Właściwa
baza dla NVFP4 to 1001, a nadwyżka wynosi +289 — czyli dokładnie tyle, ile
notował `TENSOR_PARALLEL_DESIGN.md`.

Wniosek jest ten sam, co w `TENSOR_PARALLEL_DESIGN.md`, tylko teraz z liczbami
po obu stronach: dosypywanie kolejnych macierzy do dzisiejszej protezy nie
zadziała, bo każda dołożona macierz DOKŁADA kopie do tych 343. Dopiero SPMD —
gdzie ranga liczy swoje głowice do końca miksera i wymienia wyłącznie sumę
cząstkową — zdejmuje jedno i drugie naraz.
