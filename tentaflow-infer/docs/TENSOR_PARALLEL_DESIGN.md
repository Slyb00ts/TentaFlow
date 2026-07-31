# Tensor parallel — właściwa architektura

Ten dokument opisuje, jak podział na karty ma wyglądać docelowo, i dlaczego to,
co jest w drzewie dzisiaj (`--tp-cards`), jest kształtem BŁĘDNYM, który należy
zastąpić, a nie rozbudowywać.

## Co jest dziś i dlaczego to jest złe

Obecny podział jest ASYMETRYCZNY: karta 0 „jest modelem" i trzyma wszystkie
wagi, karta 1 dostaje kopie wybranych macierzy i jest doraźnie proszona o pomoc
w konkretnych MIEJSCACH WYWOŁANIA. Konsekwencje są mierzalne, nie teoretyczne:

- **Każda ścieżka wymaga osobnego wpięcia.** Blok FFN jest w tym pliku
  powielony ~20 razy (22 dopasowania `GateUpWeights::Split`, 20 wywołań
  `glu_mul_f16`). Dekodowanie hybrydowe, prefill layer-major, weryfikacja MTP,
  prefill batchowy i ścieżka gęsta to osobne kopie tej samej matematyki.
- **Kod może cicho nie być wykonywany.** Wariant batchowy podziału został wpięty
  kolejno w dwa bloki weryfikacji MTP (`b2`/`TOTAL` w okolicy linii 15424 i
  `pb`/`total` w okolicy 12128) i ŻADEN z nich nie jest wykonywany przy
  `--speculative mtp` na jednej sekwencji. Żywa ścieżka to trzeci blok, w
  `run_hybrid_batch_layers` (linia 11458). Bez logu diagnostycznego wyglądałoby
  to na „podział daje mało", a nie na „podział się nie uruchamia".
- **Prefill wymagał ODDZIELNEGO mechanizmu** (podział po tokenach), bo podziału
  po wagach nie dało się użyć bez ruszania tych samych zduplikowanych bloków.
  Mechanizm okazał się nieopłacalny i został usunięty.
- **Aktywacje wracają na kartę 0** po każdej pomocniczej projekcji, bo karta 1
  nie ma ani stanu, ani KV, ani reszty warstwy.

Zysk, jaki to dało, jest realny (dekodowanie 30,0 -> 38,6 tok/s), ale to jest
PROTEZA. Docelowa architektura ją zastępuje w całości — nie współistnieje z nią.

## Właściwy kształt: SPMD, podział raz, ta sama pętla na każdej karcie

Tak robi to vLLM i to jest jedyny kształt, który się skaluje:

1. **Model jest dzielony RAZ, przy ładowaniu.** Nie ma „karty modelu" i „karty
   wspierającej" — jest N rang, każda trzyma swój fragment KAŻDEJ warstwy.
2. **Każda ranga wykonuje TĘ SAMĄ pętlę warstw** na swoim fragmencie. Nie ma
   miejsc wywołania do wpinania: prefill, dekodowanie, draft MTP, weryfikacja
   MTP i batch przechodzą przez ten sam kod, więc dostają podział ZA DARMO.
3. **Dokładnie DWIE redukcje na warstwę**: po projekcji wyjściowej miksera i po
   projekcji `down` FFN. Nic więcej nie przechodzi między kartami.

Każda macierz jest albo *kolumnowo równoległa* (dzielony wymiar WYJŚCIA, wynik
zostaje lokalny, brak komunikacji), albo *wierszowo równoległa* (dzielony wymiar
WEJŚCIA, wynik to suma cząstkowa, kończy się redukcją).

## Podział dla `qwen35` (ThinkingCap-Qwen3.6-27B)

Model: 64 bloki, 48 DeltaNet + 16 uwagi, hidden 5120, inter 17408, 24 głowice Q
/ 4 KV, head_dim 256, 48 głowic V DeltaNet, d_state 128.

| tensor | tryb | uwagi |
|---|---|---|
| `token_embd` | replikowany | dekodowanie pobiera JEDEN wiersz; dzielenie nic nie daje |
| `attn_norm`, `ffn_norm`, `output_norm` | replikowane | wektory, koszt pomijalny |
| `attn_q/k/v` | kolumnowo, **po GŁOWICACH** | ranga liczy tylko swoje głowice |
| **cache KV** | **dzielony po głowicach** | ranga trzyma KV swoich głowic; NIGDY nie wymieniane |
| `attn_output` | wierszowo | **redukcja 1** |
| `ssm_in_proj`, `ssm_gate`, `ssm_alpha`, `ssm_beta` | kolumnowo, po głowicach | q/k/v/z/α/β tej samej głowicy na tej samej randze |
| `ssm_conv1d`, `ssm_dt`, `ssm_a`, `ssm_norm` | dzielone po głowicach | idą za swoimi głowicami |
| **stan rekurencyjny DeltaNet** | **per ranga, po głowicach** | skan jest niezależny per głowica — ZERO wymiany w pętli rekurencyjnej |
| `ssm_out` | wierszowo | **redukcja 1** (wariant DeltaNet) |
| `ffn_gate`, `ffn_up` | kolumnowo | wynik lokalny, `glu` lokalnie |
| `ffn_down` | wierszowo | **redukcja 2** |
| `lm_head` | kolumnowo, po słowniku | all-gather logitów na randze próbkującej |
| bloki MTP/NextN | tak samo jak warstwa targetu | dziedziczą podział, nie mają własnej ścieżki |

**Ograniczenie dzielnika.** GQA ma 4 głowice KV, więc TP musi dzielić 4:
dopuszczalne TP to 1, 2 i 4. DeltaNet ma 48 głowic V, `inter` 17408 dzieli się
przez 64 (blok NVFP4), słownik 248320 — żadne z nich nie jest ciaśniejsze niż
KV. Kontrakt trzeba sprawdzać przy starcie i ODMAWIAĆ, a nie dobierać po cichu.

## Dlaczego to rozwiązuje wszystko naraz

Nie ma osobnego „podziału prefillu", „podziału weryfikacji MTP" ani „podziału
batcha". Jest jedna sharded warstwa. `T` (liczba tokenów) jest parametrem, a nie
osobną architekturą:

| ścieżka | T | ruch na warstwę (TP=2) |
|---|--:|--:|
| dekodowanie | 1 | 2 x 10 KiB |
| weryfikacja MTP | 3-4 | 2 x 30-40 KiB |
| prefill | 512 | 2 x 5,2 MiB |
| prefill | 4096 | 2 x 42 MiB |

Zmierzone łącze między kartami: 5,5 us podłogi opóźnienia, 21-27 GB/s dla
transferów od 0,5 MiB (`tests/cluster_peer.rs`). Dla dekodowania to 0,73 ms na
token wobec ~26 ms kroku. Dla prefillu 512 to ~32 ms wobec ~430 ms.

Kluczowe: **projekcje uwagi i DeltaNet przestają cokolwiek wymieniać.** Dziś
karta wspierająca odsyła policzone projekcje z powrotem; po podziale po
głowicach każda ranga liczy swoje głowice do końca miksera i wymienia dopiero
sumę cząstkową projekcji wyjściowej. To jest ta sama liczba wymian dla całej
warstwy, jaką ma dziś sam FFN.

## Dlaczego reasemblacja jest zakazana (i skąd biorą się DWIE redukcje)

To jest najważniejsze ograniczenie projektowe i najłatwiej je przeoczyć.

Macierz kolumnowo równoległa daje na karcie blok ZWARTY `[T, wiersze_rangi]`.
Bufor, którego oczekuje kolejny krok, jest ułożony token-major `[T, pełny_wymiar]`,
więc złożenie fragmentów z dwóch rang wymaga rozrzucenia Z KROKIEM — a tego
kernele GEMM nie robią, bo piszą wyjście zwarte. Dla `T = 1` problem nie
istnieje (krok równa się szerokości) i dlatego podział pojedynczego tokena
działa. Dla `T > 1` reasemblacja kosztuje albo `T` osobnych kopii, albo osobny
kernel rozrzucający — przy 48 warstwach zjada to cały zysk z podziału.

Wniosek nie brzmi „napisz kernel rozrzucający", tylko: **po macierzy kolumnowo
równoległej NIE WOLNO składać wyniku.** Ma go skonsumować macierz wierszowo
równoległa NA TEJ SAMEJ randze. Wtedy jedyne, co przechodzi między kartami, to
suma cząstkowa na końcu.

FFN jest tego dowodem i działa: `gate`/`up` (kolumnowe) zostają lokalne,
bramkowanie jest lokalne, `down` (wierszowe) redukuje. Zero składania.

DeltaNet i uwaga muszą być zrobione tak samo: ranga liczy SWOJE głowice przez
splot, normy i skan rekurencyjny, i dopiero projekcja wyjściowa redukuje. Podział
samych projekcji wejściowych z odsyłaniem wyników na kartę 0 — czyli to, co robi
dzisiejsza proteza — jest właśnie tym zakazanym składaniem i dlatego nie da się
go rozszerzyć na `T > 1`.

Stąd biorą się DOKŁADNIE dwie redukcje na warstwę. Nie jest to wybór
optymalizacyjny, tylko konsekwencja tego, że każda ścieżka przez warstwę ma
postać: kolumnowa -> lokalne przetwarzanie -> wierszowa -> redukcja.

## Dowód liczbowy: połowa projektu nie wystarcza

`ssm_out` (9,7% odczytu na token) został zaimplementowany jako macierz wierszowo
równoległa — podział po kolumnach na granicy `d_state`, sumy cząstkowe w f32,
jedna redukcja. Sama ta zmiana ZMIERZYŁA SIĘ NA MINUS: dekodowanie 38,6 -> 37,0
tok/s. Kod usunięty.

Rachunek pokazuje dlaczego i jest to ten sam wniosek co wyżej. Dopóki mikser
DeltaNet liczy się w całości na karcie modelu, `normed` powstaje TAM, więc każda
z 48 warstw musi najpierw WYSŁAĆ wycinek wejścia i dopiero potem odebrać sumę
cząstkową: dwie wymiany plus zdarzenia plus redukcja, około 6 dodatkowych
uruchomień na warstwę. Przy 48 warstwach to ~1,3 ms na token — tyle samo, ile
warte jest zaoszczędzone 765 MiB odczytu (~1,4 ms).

Przy podziale po GŁOWICACH ranga liczy `normed` swoich głowic SAMA. Wysyłka
wejścia znika, zostaje wyłącznie redukcja — czyli połowa kosztu przy tym samym
zysku, i to jeszcze zanim doliczy się oszczędność na projekcjach wejściowych,
splocie i skanie, które też przestają być liczone dwa razy.

Wniosek praktyczny: `ssm_out` NIE nadaje się do wdrożenia osobno. Jest ostatnim
krokiem podziału DeltaNet po głowicach i ma sens dopiero razem z nim.

## Podziału po głowicach NIE WOLNO wdrażać po kawałku

To jest ograniczenie POPRAWNOŚCI, nie wydajności, i łatwo je przeoczyć.

Stan rekurencyjny DeltaNet jest wspólny dla wszystkich ścieżek: dekodowania,
prefillu i weryfikacji draftu MTP. Jeśli mikser zostanie podzielony po głowicach
tylko w JEDNEJ z nich, to karta modelu przestaje aktualizować stan głowic, które
policzyła ranga wspierająca — a każda inna ścieżka nadal czyta ten stan z karty
modelu i dostaje wartości NIEAKTUALNE. Nie ma z tego błędu ani asercji: model po
prostu liczy coś innego, a przy weryfikacji MTP jeszcze to zatwierdza.

Wniosek: podział DeltaNet po głowicach jest zmianą WSZYSTKO ALBO NIC. Stan musi
stać się per ranga w tym samym kroku, w którym mikser zaczyna być liczony per
ranga, i muszą to objąć wszystkie ścieżki naraz — łącznie z checkpointami i
rollbackiem MTP. Bramkowanie go „na razie tylko dla dekodowania bez spekulacji"
jest właśnie tym niebezpiecznym półśrodkiem.

Praktycznie: `SsmState` (bufory `conv` i `state`) jest czytany bezpośrednio w
~60 miejscach POZA pulą i w 7 miejscach wewnątrz niej. Rozsądna droga to nadać
mu dostęp per ranga, zostawiając dzisiejsze `.conv`/`.state` jako rangę zero —
wtedy istniejące miejsca zostają nietknięte, a zmiana skupia się w puli i w
mikserze. Ale przełączenie musi objąć wszystkie ścieżki w jednym kroku.

## Ile to jest warte — zmierzone, nie oszacowane

Profil `rocprofv3` dekodowania (prompt 128, 64 tokeny, warmup + 1 przebieg):

| | kernele karty 0 | uruchomienia karty 0 | kernele karty 1 |
|---|--:|--:|--:|
| 1 karta | 4260,5 ms | 165 057 | — |
| 2 karty | 2998,6 ms | **201 693** | 1669,2 ms |

Karta modelu wykonuje 29,6% mniej pracy, ale ma o 22% WIĘCEJ uruchomień, czyli
około 290 dodatkowych na token. To dokładnie 48 warstw DeltaNet razy sześć
operacji rozgłoszenia, zbiórki i zdarzeń — koszt składania wyników projekcji.
Przy zmierzonych ~4,5 us na uruchomienie to ~1,3 ms na token przy kroku ~26 ms.

Podział po głowicach wygrywa więc DWA RAZY: usuwa te same ~288 uruchomień (bo
nie ma czego składać) i dopiero wtedy pozwala podzielić `ssm_out` (9,7% odczytu,
~1,4 ms). Razem ~2,7 ms z ~26 ms, czyli oczekiwane ~+11% dekodowania — i tyle
samo na ścieżce MTP, której dzisiejszy podział projekcji w ogóle nie dotyczy.

Kontrola: sam podział projekcji DeltaNet zmierzony A/B daje 38,4 wobec 37,7
tok/s. Jest na plus, ale skromnie — bo połowa zysku z odczytu wraca jako koszt
składania. To jest ta sama liczba widziana z drugiej strony.

## KOREKTA: podział DeltaNet po głowicach nie jest tak czysty, jak zakładano

Powyższa tabela mówiła „ssm_* dzielone po głowicach" i to było uproszczenie,
które ZAIMPLEMENTOWANO i które NIE DZIAŁA. Dwa ustalenia z realnego kodu:

**1. GGUF przypisuje głowicę V do głowicy K przez MODULO, nie blokowo.**
`deltanet_repeat_qk_f16` liczy `source = index % n_elems`, czyli głowica V `h`
używa głowicy K `h % n_k`. Ciągły zakres głowic V potrzebuje więc WSZYSTKICH
głowic K — chyba że jest wyrównany do wielokrotności `n_k`. Dla tego modelu
`n_v = 48`, `n_k = 16`, więc jedyne dopuszczalne podziały na dwie karty to 16/32
albo 32/16: z natury nierówne (33%/67%). Alternatywa to replikacja q/k na obu
kartach, czyli 40% wierszy `in_proj` czytanych dwa razy.

Implementacja, która tego nie uwzględniała (karta 0 dostała głowice V 0-20 i K
0-6), produkowała TEKST-ŚMIEĆ — głowica V nr 7 sięgała po głowicę K nr 7, której
ta karta nie miała. Wykryte dopiero po obejrzeniu wygenerowanego tekstu; sam SHA
pokazywał tylko „inaczej", a spadek przepustowości wyglądał jak zwykły narzut.

**2. Kernele miksera są ograniczone OPÓŹNIENIEM, nie pasmem.** Splot, l2norm,
powielanie q/k, log-decay, sigmoid bety, skan i bramkowany RMSNorm operują na
kilkudziesięciu głowicach po 128 wartości. Podzielenie ich na pół nie skraca
ich czasu, a podwaja liczbę uruchomień, bo teraz liczą je obie karty. Zmierzone:
dekodowanie 38,5 -> 32,7 tok/s, czyli 15% W DÓŁ.

**Wniosek, który unieważnia wcześniejsze oszacowanie „+11%".** Podział po
głowicach opłaca się tylko dla części CIĘŻKIEJ WAGAMI (`in_proj`, `gate`,
`ssm_out`), a nie dla samego miksera. Sensowny wariant do sprawdzenia to podział
głowic V wyrównany do `n_k`, z REPLIKACJĄ q/k i całej lekkiej ścieżki q/k na obu
kartach — czyli dzielenie tylko strony V. Nie zostało to zmierzone i nie wolno
zakładać, że wygra: punktem odniesienia jest 38,4 tok/s, a poprzednia próba
przegrała o 15%.

## Zgodność bitowa

- **Kolumnowo równoległe** (podział wyjścia): każdy element wyniku liczony w
  całości przez jedną rangę, tym samym kernelem — bitowo identycznie.
- **Wierszowo równoległe** (`attn_output`, `ssm_out`, `ffn_down`): sumy cząstkowe.
  Kontrakt: każda ranga akumuluje swój fragment w f32, redukcja idzie w f32, i
  DOPIERO wynik jest zawężany do f16 — jedno zaokrąglenie, tak jak na jednej
  karcie. Zostaje różnica KOLEJNOŚCI sumowania, więc bitowa zgodność z jedną
  kartą nie jest gwarantowana i nie wolno jej obiecywać. Bramką jakości jest
  perplexity i zgodność wygenerowanego tekstu, nie SHA.

Kernele redukcji już są: `gemm_nvfp4_gguf_out_f32_batch` (2/4/8/16 tokenów),
`gemv_nvfp4_gguf_q8_1_out_f32` (jeden token), `add_f32`, `add_f32_out_f16`.
Brakuje wariantu f32 dla dużych `T` (prefill) — to jedyna nowa praca po stronie
Mojo.

## Kształt implementacji: ranga to OSOBNY `Model`, nie parametr w miksarze

Pierwsza wersja tego dokumentu opisywała krok 2 jako „wydzielić `Rank`" i
sugerowała przewleczenie zakresu głowic przez miejsca, które indeksują stan.
Przegląd drzewa mówi, że to zła droga, i warto zapisać dlaczego, zanim ktoś
zacznie: `model.rs` ma 34 odczyty `.state` i 22 `.conv`, ale to NIE jest cała
powierzchnia — ranga potrzebuje też własnego KV, własnych buforów aktywacji,
własnych wag norm i własnej głowy logitów. Przewleczenie zakresu głowic załatwia
jedną z tych pięciu rzeczy.

Tańszy kształt wychodzi z obserwacji, że `Model` UMIE JUŻ wszystko, czego
potrzebuje ranga: wczytać wagi, zaalokować KV, zaalokować stan DeltaNet i
przejść pętlę warstw. Ranga to po prostu `Model` zbudowany na karcie `r` z
deskryptora, w którym liczby głowic, `inter` i słownik są PODZIELONE. Wtedy:

- nie ma zakresu głowic do przewlekania — ranga ma po prostu mniej głowic i całą
  resztę kodu widzi bez zmian;
- prefill, dekodowanie, weryfikacja MTP, batch i checkpointy dziedziczą podział
  ZA DARMO, bo to ten sam kod na mniejszym kształcie — czyli znika ograniczenie
  „wszystko albo nic", które wywróciło poprzednie podejście;
- nowy kod ogranicza się do trzech rzeczy: ładowania fragmentu tensora zamiast
  całego, dwóch redukcji na warstwę i sterownika, który zakolejkowuje warstwę na
  każdej randze, zanim zsynchronizuje.

Czego to wymaga i co jest w tym ryzykowne:

1. **Loader musi umieć wczytać FRAGMENT tensora.** Dziś `load_row_split` czyta
   plik po raz drugi — to proteza asymetrii. Docelowo ranga czyta wyłącznie
   swoje wiersze, raz.
2. **Deskryptor per ranga.** `n_v_heads`, `n_k_heads`, `head_count`,
   `head_count_kv`, `feed_forward_length` i `vocab_size` różnią się między rangą
   a modelem. Trzeba sprawdzić przy starcie, czy dzielą się przez liczbę rang, i
   ODMÓWIĆ, zamiast dobierać po cichu. GQA ma tu 4 głowice KV, więc dopuszczalne
   TP to 1, 2 i 4.
3. **Dwie redukcje na warstwę** (po projekcji wyjściowej miksera i po `down`),
   sumy cząstkowe w f32, zawężenie do f16 dopiero po sumie. Kernele redukcji
   już są (`add_f32`, `add_f32_out_f16`, warianty `out_f32` GEMV-ów).
4. **Strumień rezydualny replikowany.** Po redukcji obie rangi mają ten sam
   wektor, więc każda liczy swoją normę lokalnie i nie ma rozgłaszania `x`.
5. **Głowa logitów** kolumnowo, z all-gather na randze próbkującej.
6. **Przechwytywanie grafu** sprawdzić osobno: ROCm przerywa asercją przy
   przechwytywaniu rozwidlenia strumienia MIĘDZY kartami, więc albo każda ranga
   ma własny graf z redukcjami poza nim, albo krok idzie jawnym łańcuchem
   (zmierzony koszt utraty grafu na ścieżce hybrydowej: 1,7%).

**Ile to jest warte — z pomiaru z 2026-07-31, nie z oszacowania.** Przebieg
jednokartowy NVFP4 to 1001 uruchomień, 30,04 ms zajętości i 3,81 ms przestoju na
token. Przy pełnym podziale zajętość rangi schodzi do ~15,8 ms, ale przestój NIE
dzieli się przez dwa — podatek od liczby uruchomień jest wspólny dla obu rang.
Krok wychodzi ~21 ms, czyli **47-48 tok/s wobec dzisiejszych 39,7**. Tyle jest do
wzięcia i nie więcej; kto obiecuje 2x, nie policzył przestoju.

## Sterownik: WDROŻONY (2026-07-31)

Sterownik jest w drzewie i zmierzony na dwóch R9700: `forge run --tp 2` daje
33,7 -> 42,4 tok/s (NVFP4) i 33,6 -> 45,5 tok/s (Q4_K_M), przy tekście
IDENTYCZNYM co do znaku wobec jednej karty. Liczby i protokół:
`BENCH_R9700_27B.md` §5.

Wyszło to na trzech elementach opisanych niżej i na jednym ustaleniu, którego
nie było w planie: `Model::logits_weight_gemv` miał już gotowy, ogólny dyspozytor
GEMV z wyjściem f32 obsługujący Q4_K, Q6_K i NvFp4Gguf. Wystarczyło wydzielić z
niego `gemv_out_f32` (cap i maska zostały u wołającego) — ŻADEN nowy kernel Mojo
nie był potrzebny.

Czego sterownik NIE prowadzi i co przez to ODMAWIA przy starcie: natywnego MTP,
tieringu KV, MoE, modeli niehybrydowych, `--tp` niedzielącego głowic KV oraz FFN
dla wielu tokenów. Prefill idzie token po tokenie, bo warianty layer-major i
batchowy liczą warstwę własnym kodem, poza dwoma punktami redukcji. Głowa
logitów jest replikowana (`load_hybrid` nie tnie `lm_head`), więc podział jej
jeszcze nie przyspiesza — to następny krok, nie regresja.

Dzisiejsza proteza `--tp-cards` NADAL JEST w drzewie obok sterownika. Nie wolno
zostawić tego na stałe: to dwie implementacje tej samej rzeczy, a proteza jest tą
gorszą (patrz cały początek tego dokumentu). Usunąć ją razem z
`tensor_parallel.rs`, `tp_ffn` i przykładami `tp_*_probe`, gdy sterownik obejmie
prefill.

## Zapis stanu sprzed wdrożenia

Fundament był w drzewie i zabramkowany (kontrakt podziału, krojenie macierzy,
plan per rola, loader zweryfikowany na realnym checkpoincie, prymityw redukcji,
warstwa rozcięta w dwóch punktach redukcji). Zostaje sterownik — i on NIE DZIELI
SIĘ na mniejsze kroki, bo każdy z jego trzech elementów osobno jest kodem,
którego nic nie woła:

1. bufory sum cząstkowych i wyjście f32 dla `out_proj` oraz `ffn_down` per ranga
   (warianty `gemv_*_out_f32` już istnieją),
2. noga rozgłoszenia w redukcji — `reduce_partials` ZBIERA na jednej karcie, a
   w SPMD OBIE rangi potrzebują zsumowanego wektora, żeby policzyć własny
   `rmsnorm_residual` na własnym `bufs.h`; dla dwóch rang to zbiórka plus jedna
   kopia,
3. pętla nad `Vec<Model>`: część 1 na każdej randze -> redukcja -> część 2 ->
   redukcja -> część 3.

**Strumień rezydualny jest replikowany ZA DARMO** i to jest miła konsekwencja
kształtu: każda ranga to pełny `Model` z własnym `bufs.h`, a wagi norm ładują się
jako `Replicated`. Gdy zredukowany wektor trafi na każdą rangę, rozgłaszanie `x`
przed każdą macierzą znika bez ani jednej dodatkowej linijki.

**PUŁAPKA WŁASNOŚCI, do rozstrzygnięcia PRZED pisaniem.** `Cluster::attach`
otwiera karty pomocnicze SAM i tworzy dla każdej własny strumień oraz własny
komplet artefaktów kerneli. `Model::load_gguf` robi dokładnie to samo dla karty,
którą dostanie. Zbudowanie rangi na karcie klastra da więc na tej karcie DWA
zestawy kerneli i DWA strumienie — dokładnie to, przed czym ostrzega komentarz
przy `attach` w odniesieniu do karty głównej. Sterownik ma budować `Model`
najpierw, a klaster spinać nad ICH urządzeniami (`Model::device`), a nie
odwrotnie; inaczej każda karta płaci drugim modułem kerneli i drugim strumieniem,
a bufory, na których liczy redukcja, nie będą tymi, na których liczy ranga.

## Co trzeba zrobić w tym drzewie

Kolejność jest wymuszona zależnościami, nie preferencją:

1. **Ujednolicić zduplikowane bloki warstwy.** Dopóki FFN i miksery są przepisane
   ~20 razy, każdy podział trzeba wpinać ~20 razy i każde wpięcie może być
   martwe. To jest PIERWSZY krok i sam w sobie nie dotyczy wielu kart.
2. **Wydzielić `Rank`**: urządzenie, strumień, kernele, fragment wag, własny KV,
   własny stan DeltaNet, własne bufory. `Model` staje się rangą numer 0.
3. **Dzielić przy ŁADOWANIU**, nie doczytywać pliku po raz drugi. Dzisiejsze
   `load_ffn_shards_gguf` / `load_delta_projection_source` czytają GGUF ponownie
   — to proteza wynikająca z asymetrii i znika razem z nią.
4. **Prymityw redukcji** ponad `Cluster` (punkt-punkt dla 2 rang, pierścień dla
   większych) z akumulacją w f32.
5. **Przełączyć pętlę warstw na rangi** i USUNĄĆ dotychczasowe wpięcia
   (`forward`, `forward_batch`, `forward_delta_projections`, `forward_logits`).
   Dwie implementacje tej samej rzeczy nie mogą współistnieć.
6. **Sprawdzić przechwytywanie grafów.** Zmierzone: ROCm przerywa asercją we
   własnym runtime przy przechwytywaniu rozwidlenia strumienia MIĘDZY kartami.
   Przy SPMD każda ranga ma własny graf, a punkty redukcji są jedynymi
   operacjami międzykartowymi — trzeba sprawdzić, czy dają się przechwycić, a
   jeśli nie, wykonywać krok jawnym łańcuchem (zmierzony koszt utraty grafu na
   ścieżce hybrydowej: 1,7%).

## Czego ten dokument NIE obiecuje

Nie obiecuje przyspieszenia 2x. Zmierzone ograniczenia tego stanowiska zostają w
mocy: przy ~1270 uruchomieniach kerneli na token i ~4,5 us narzutu na
uruchomienie sama dyspozycja to ~5,7 ms z ~34 ms kroku i NIE maleje od dołożenia
karty. Podział wag zmniejsza tę część kroku, która jest ograniczona odczytem
wag, i tylko ją.
