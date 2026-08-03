# FORGE — plan naprawy i rozszerzenia

**Czym jest ten dokument.** Planem przebudowy `tentaflow-infer` (FORGE) w miejscu, w trzech
wymiarach naraz:

1. **Wydajność** — zamknięcie zmierzonych luk (największa: 14,6× na przepustowości agregatowej).
2. **Układ kodu** — rozbicie monolitów na moduły z jednoznaczną odpowiedzialnością i **pięć
   jawnych punktów wpięcia**, tak żeby dodanie karty, modelu, formatu kwantyzacji albo kernela
   było wpisem w danych, a nie kolejną gałęzią w `model.rs`.
3. **Apple** — backend Metal i obsługa modeli MLX, przy czym **modele MLX mają działać
   wszędzie**: na Apple, AMD i NVIDII. Poprzeczką na Apple jest `mlx-swift`.

Każda diagnoza i każdy cel stoi na **zmierzonej liczbie**. Liczby wyprowadzone (nie zmierzone)
są oznaczone wprost i mają przypisany eksperyment domykający.

**Skąd wnioski.** Z trzech źródeł:

1. **Pomiary FORGE** — `docs/BENCH_R9700_27B.md`, `docs/BENCH_7900XT_LLAMACPP.md`,
   `docs/PROFILE_DECODE_2026-07-24.md`, `docs/STATUS.md`, `docs/CODEGEN_PROOF.md`, `MOJO_NOTES.md`.
2. **Dokumentacja TentaForge** (`critix@192.168.11.143:/home/critix/repos/rust/TentaForge/docs`,
   2026-08-02) — w tym `analiza-starego-silnika.md`, będący katalogiem błędów FORGE spisanym
   z pomiarów, oraz `pomiary/eks-1..4`: cztery eksperymenty sprzętowe wykonane na R9700
   **niezależnie od FORGE**.
3. **Pomiary i inwentaryzacja wykonane przy pisaniu tego dokumentu** — stan repozytorium,
   sprzęt tej maszyny, realne checkpointy MLX na dysku.

Odsyłacze: 「TF-ANALIZA §X」, 「TF-ARCH §X」, 「TF-SPEC-0n §X」, 「EKS-n」.

**Czym nie jest.** Nie jest propozycją przepisania FORGE na TentaForge — to dwa projekty
o różnym zakresie. Rozdział 12 wymienia, czego z TentaForge **nie wolno** przenosić.

---

## 0. Streszczenie

FORGE działa i w wielu miejscach wygrywa z llama.cpp. Problem nie brzmi „silnik jest zły",
tylko: **każda nowa karta, model, kwantyzacja i szerokość batcha dokładała gałąź w kodzie**,
a nie wpis w danych. Stąd 21 430 linii w `model.rs`, 20 430 w `launchers.rs`, 83 zmienne
środowiskowe i 469–535 kerneli w katalogu na architekturę. Ten sam mechanizm sprawia, że
Apple jest dziś nieobsługiwalny bez kolejnego równoległego świata w kodzie.

Dziesięć długów, uporządkowanych **według zmierzonej dźwigni**:

| # | Dług | Zmierzony koszt | Faza |
|---|---|---|---|
| **D3** | batch doklejony po fakcie | `serve` płaski **166 tok/s od B=1 do B=32**, gdy vLLM robi 2 430 (**14,6×**) | N4 |
| **D8** | brak backendu Apple, brak MLX | **0%** — silnik nie startuje na M-series, żaden checkpoint MLX się nie ładuje | NA1–NA3 |
| **D9** | brak punktów wpięcia | dodanie modelu = 6 bezgłośnych błędów (Gemma 4) i 7 nieistniejących mechanizmów (DeepSeek V4) | N3/N5 |
| **D1** | forward pisany ręcznie, brak IR i automatycznej fuzji | 681 uruchomień/token = **2,26 ms z 32 ms** przestoju; fuzje ręczne, zrobiono 5 | N2 |
| **D10** | prefill poniżej sufitu obliczeniowego | 1 481 tok/s wobec **2 800** (f16) i **5 500** (fp8) na tej samej karcie | N6 |
| **D6** | kernele kopiowane, nie generowane | 30 formatów → **~500 kerneli** zamiast ~10 rodzin; 55 min buildu | N3 |
| **D5** | Mojo jako jedyna ścieżka kerneli | codegen **36% sufitu** na int8-GEMM; brak cross-kompilacji; **ciche** złe artefakty; **nie celuje w Metal** | N3/NA2 |
| **D2** | HAL zaprojektowany pod CUDA | AMD jako emulacja ROCm; graf HIP zdejmuje **0%**; na Apple model `alloc+copy` jest **wprost szkodliwy** | N5/NA1 |
| **D4** | monolity i 83 zmienne środowiskowe | 11 plików > 1500 linii; **42** realne przełączniki ścieżki | N0/N1 |
| **D7** | metodyka pomiaru nieegzekwowana maszynowo | dwie „optymalizacje", które były błędem pomiaru (raportowane 1,94× → realnie **0,51×**) | N0 |

**Kolejność jest wnioskiem, nie preferencją.** Największa dziura to nie decode przy B=1
(zostało tam ~1,2×), tylko przepustowość agregatowa (~14×), brak Apple (∞) i prefill (~2–4×).

**Dwa rozstrzygnięcia, które oszczędzają kwartał pracy:**

- **Megakernel jest FORGE niepotrzebny.** EKS-2 zmierzył, że fuzja do ~65 uruchomień zabiera
  **93% zysku megakernela**; megakernel dokłada ponad to 0,22 ms z 32 ms = 0,7% (§2, D1).
- **Obsługa MLX to loader plus jedna rodzina kerneli, nie nowy silnik.** Z czterech trybów
  kwantyzacji MLX (`affine`, `mxfp4`, `mxfp8`, `nvfp4`) FORGE **ma już trzy**; nowy jest
  wyłącznie `affine` o dowolnej liczbie bitów — strukturalnie ten sam kształt co Q4_1/Q4_K (§7.4).

---

## 1. Punkt wyjścia — stan zmierzony

### 1.1. Repozytorium (pomiar z dzisiaj)

| Miara | Wartość |
|---|--:|
| Rust, `crates/**/*.rs` | **124 426** linii w 171 plikach |
| Pliki > 1500 linii | **11** |
| `model.rs` | **21 430** linii, 369 funkcji |
| `launchers.rs` | **20 430** linii, 387 funkcji publicznych |
| `weights.rs` / `server.rs` / `registry.rs` | 5 332 / 3 259 / 3 073 |
| Unikalnych zmiennych `FORGE_*` | **83** (41 to przełączniki ścieżki wykonania) |
| Źródła Mojo | 236 plików, **60 960** linii |
| Kernele w katalogu | gfx1030 411, gfx1100 436, **gfx1201 469**, sm_89 474, sm_121a 535 |
| Graph IR | **brak** — jedyny enum operacji w repo należy do `forge-onnx` |
| Backendy HAL | `cuda`, `hip`, `cpu` — **Metal nie istnieje** |
| Wystąpienia „Apple"/„Metal" w kodzie | 8, wszystkie w testach negatywnych i komentarzach |

`crates/forge-formats/arch/*.ron` (9 plików) to jedyne miejsce, w którym wariant jest już
danymi. To jest wzorzec do rozciągnięcia na resztę, nie wyjątek do usunięcia.

### 1.2. Sprzęt tej maszyny (pomiar)

| | |
|---|---|
| Układ | **Apple M4** (bazowy), macOS 26.5.2, Metal 4 |
| CPU | 10 rdzeni: 4 wydajnościowe + 6 energooszczędnych |
| GPU | **10 rdzeni**, bez akceleratorów neuronowych (te są dopiero w M5) |
| Pamięć | **16 GiB unified**, przepustowość **120 GB/s** |
| Modele MLX na dysku | Bielik-Minitron-7B-MLX-4bit (**4,207 GB**), Qwen3-VL-30B-A3B-4bit (17 GB), chatterbox-turbo-4bit |
| `mlx-swift` | obecny w drzewie głównym (`MLXBridge`, fork PrismML 0.31.x) — gotowa poprzeczka |

### 1.3. Wydajność — poprzeczka do utrzymania

R9700 (gfx1201), ThinkingCap-Qwen3.6-27B, `--prefix-cache off`, llama.cpp `3018a11e`
「TF-ANALIZA §1.1」:

| model | miara | FORGE | llama.cpp | stosunek |
|---|---|--:|--:|---|
| Q4_K_M | prefill p1024 | **1 481,2** | 1 027,9 | 1,44× |
| Q4_K_M | decode tg128 | **31,0** | 27,4 | 1,13× |
| Q4_K_M | decode + MTP K=3 | **58,9** | — | 1,90× nad własnym |
| NVFP4 | prefill p1024 | **1 311,2** | 929,0 | 1,41× |
| NVFP4 | decode + MTP K=3 | **70,4** | — | 2,28× nad własnym |
| Q4_K_M | SPMD `--tp 2` | **49,0** | — | +45,8% |

Przegrane, których nie wolno ukrywać: Mistral-7B Q4_K_M decode 67,0 vs **79,0** (0,85×),
Snake 3D z MTP 51,2 vs **58,5** (0,88×).

### 1.4. Ile zostało do wzięcia — tabela dźwigni

To jest najważniejsza tabela w dokumencie; ustala kolejność faz.

| Dźwignia | Stan | Sufit | Mnożnik | Skąd sufit |
|---|--:|--:|--:|---|
| **Apple / MLX** | nie działa | mlx-swift | **∞** | brak backendu |
| **Agregat przy współbieżności (dense)** | 166 tok/s | 2 430–2 562 | **~14,6×** | vLLM i llama.cpp na tym samym modelu |
| **Prefill, ścieżka fp8 (RDNA4)** | 97 TFLOPS | **203 TFLOPS** | **2,1×** | zmierzony kafel fp8 na `ffn_gate/up` |
| **Prefill systemowo (R9700)** | 1 481 tok/s | 2 800 (f16) / 5 500 (fp8) | 1,9–3,7× | roofline WMMA 「TF-ARCH §1.1a」 |
| **Decode B=1 (R9700)** | 31,0 (500 GB/s) | **37,3** (602 GB/s) | **1,20×** | ablacja praktycznego sufitu |
| Spekulacja MTP | 1,87–2,17 tok/krok | 2,5 z drzewami | 1,2–1,3× | 「TF-ARCH §1.1」 |
| Dwie karty, B=1 | 1,46× | 1,75–1,9× | 1,2–1,3× | 「TF-SPEC-04 §4」 |

**Korekta rachunku, którą trzeba przyjąć do wiadomości.** FORGE raportował decode na
„91% roofline'u DRAM", ale mianownikiem było 551 GB/s — **własny wynik FORGE**, nie sufit karty.
Ablacja TentaForge na realnym kernelu 「TF-ARCH §1」:

| wariant | GB/s | % z 644 |
|---|--:|--:|
| katalogowe 256 bit × 20 Gb/s | 644 | 100% |
| kernel wyłącznie z idealnie sklejonymi ładowaniami | **602** | 93,5% |
| ten sam wolumen, wzorzec dostępu GEMM, bez arytmetyki | 548 | 85,1% |
| pełny kernel GEMM | 549 | 85,2% |
| **FORGE** | **500** | **77,6%** |

Czyta się to tak: 6,5 pkt zabiera kontroler pamięci, 8,4 pkt wzorzec dostępu do wag,
**cała arytmetyka razem 1,2–1,5 pkt**. Wniosek: w ciele kerneli GEMV nie ma już czego
optymalizować — zostało 1,20× i leży w narzucie oraz wzorcu dostępu. To zamyka temat
„jeszcze jeden wariant kafla dla `ffn_down`".

---

## 2. Diagnoza — dziesięć długów

### D1. Forward pisany ręcznie, brak IR, fuzja jako praca ręczna

**Objaw.** `model.rs`: 21 430 linii, 369 funkcji; forward to `record_batch_forward`
i `record_hybrid_batch_forward` wypisujące uruchomienia po kolei. STATUS mówi wprost:
„brak deklaratywnego op-grafu, passów, autotunera. **Forward jest ręcznie napisany**".

**Dowód.** Podatek od uruchomienia kernela na AMD: **3,5–5 µs**, jednorodnie rozłożony
(1027,9 przerw 2–5 µs na token). Potwierdzone niezależnie przez EKS-2 na tym samym sprzęcie:
**3,83 µs** na dyspozycję, **18,20 µs** na powrót na hosta, **graf HIP zdejmuje 0,0%**
(2,55 vs 2,50 µs — szum). Praca ręczna dała 1033 → 681 uruchomień, przestój 3,98 → **2,74 ms**,
Q4_K_M 30,0 → 31,0 tok/s. **Pięć fuzji.** Zostało ~128 uruchomień w zasięgu tej samej metody,
wartych ~0,5 ms — i nikt ich nie zrobi ręcznie, bo każda to nowy kernel + nowy launcher +
nowa bramka SHA.

**Zastrzeżenie, które oszczędza kwartał.** EKS-2 §7.3 policzył zysk megakernela wobec ścieżki
**agresywnie zfuzowanej**:

| scenariusz | punktów sync | dyspozycje | megakernel | oszczędność/krok |
|---|--:|--:|--:|--:|
| stan FORGE (1001 uruchomień) | 1001 | 3,83 ms | 0,52 ms | **3,31 ms** |
| FORGE po fuzjach (681) | 681 | 2,61 ms | 0,35 ms | **2,26 ms** |
| ~65 etapów (cel fuzji) | 65 | 0,25 ms | 0,034 ms | **0,22 ms** |

**Fuzja do ~65 uruchomień zabiera 93% zysku megakernela.** FORGE ma wziąć fuzję, nie megakernel.

### D2. HAL zaprojektowany pod CUDA

**Objaw.** `forge-hal/src/lib.rs` (528 linii) to niemal 1:1 CUDA Driver API:
`load_module(image) → Module → KernelHandle`, `create_stream`, `record_event`/`wait_event`,
`begin_capture`/`launch_graph`, `LaunchArgs` jako sloty 8-bajtowe. `hip.rs` (937 linii) to
emulacja tego API przez ROCm.

**Dowód.**
- Graf HIP, pod który to API jest ukształtowane, zdejmuje na AMD **0%** 「EKS-2 §7.1」.
- Capture przez **dwie karty jest na ROCm niemożliwy** — runtime przerywa asercją i zrzuca
  pamięć zamiast zwrócić błąd.
- Dwa błędy czasu życia w `hip.rs` dawały SIGSEGV na **pierwszym** uruchomieniu; na CUDA
  problemu nie było. **Abstrakcja ukrywała różnicę kontraktu między backendami.**
- `multiProcessorCount` na RDNA liczy **WGP, nie CU** (6900 XT: 40 vs 80) — każda heurystyka
  „bloki na SM" przeniesiona z CUDA jest tam po cichu błędna.
- `TieredWeightDevice` nie przekazywał `ordinal` ani `enable_peer_access`, **oba mają domyślną
  implementację w traicie** — więc brak przekazania nie był błędem kompilacji, tylko cichym
  zmyśleniem: karta zgłaszała numer 0 i „brak P2P".

**Nowy dowód, z Apple.** Na M-series pamięć jest **unified**: `alloc(device) + write(host→device)`
to nie jest kosztowna abstrakcja, to jest **kopiowanie danych, które nigdzie nie muszą jechać**.
Model HAL zaprojektowany pod dyskretną kartę wymusza na Apple pracę, której tam nie ma.
Szczegóły i konsekwencje w §7.3.

### D3. Batch doklejony po fakcie — **największa dziura wydajnościowa**

- `forge serve` przez długi czas **w ogóle nie skalował się z batchem**: agregat płaski
  **166 tok/s (Mistral) i 144 (Bielik) od N=1 do N=32 przy GPU na 100%**, gdy llama.cpp robił
  176 → **2 562 (14,5×)**, a vLLM 90 → **2 430 (27×)**.
- Ścieżka hybrydowa miała **sufit B=2 wpisany w kod**. Zdjęcie warunku — jedna linia — dało **1,40×**.
- Brakująca instancja `gemm_nvfp4_gguf_f16_b16_nvidia` dawała **klif**: grupa 8 → 70,8 tok/s,
  grupa 10 albo 16 → **37,5 i 39,5**. Jedna linia szablonu: 67,8 i 71,3.
- B=17..32 spadał na generyczny dequant-GEMM: TPOT **58 ms**, 533 tok/s. Kafel BM32: 2 493 (**4,7×**).
- `batched_matches_single_seq` failował „od dawna bez wyjaśnienia": B=1 szedł na kafel dopełniany
  do ≥64 tokenów, kwantyzujący aktywacje inaczej niż GEMV → **rel_l2 1,1e-2 od pierwszego kroku**.

**Wzorzec:** gdy szerokość batcha nie trafiała **dokładnie** w istniejący kernel, wydajność
spadała **skokowo**, a jakość wyjścia zaczynała zależeć od współbieżności. Jedna przyczyna:
batch jest przypadkiem szczególnym, a nie parametrem.

### D4. Monolity i 83 zmienne środowiskowe

83 zmienne `FORGE_*` = 83 osobne ścieżki. Skutek udokumentowany: pomiary porównywały **dwie
różne ścieżki zamiast dwóch stanów** — zestawienie `tp=1` z `tp=2` przy domyślnych ustawieniach
porównywało layer-major z batchem, czyli dwa różne algorytmy.

Rozkład: 24 to `FORGE_BENCH_*`/`FORGE_PROFILE_*`, 18 `FORGE_TEST_*`/`*_AUDIT`,
**42 to realne przełączniki ścieżki wykonania** (pomiar `cargo xtask env-inventory`, patrz `INWENTARZ_ENV.md`) — i to jest liczba do zbicia do zera.

### D5. Mojo jako jedyna ścieżka kerneli (ADR-0001)

Zysk był realny (jedno źródło → PTX i AMDGCN; 340 z 474 kerneli zbudowało się na gfx1030 po
dopisaniu jednego helpera). Koszty, wszystkie zmierzone:

| koszt | dowód |
|---|---|
| brak kontroli nad ISA | ten sam algorytm int8-MMQ: nvcc/ptxas **92% sufitu**, Mojo **36%**; ADR złamany dwa razy |
| brak cross-kompilacji | `MOJO_TARGET_ACCELERATOR` nie działa; katalog sm_89 był w tyle (474 z 529); katalogi gfx1100/gfx1030 na stałe czerwone |
| **cicha zła kompilacja** | `inlined_assembly` PTX na gfx1030: `unknown asm constraint 'h'` i **build kontynuowany z pominiętą instrukcją** — 1,1e-44 zamiast 1,5 |
| **cichy zły artefakt** | dwie nazwy w jednym `compile_function` → artefakt `hd256` z ciała `hd128`, pliki **bajtowo identyczne**, Gemma liczyła śmieci |
| czas | **~55 min** na 518 kerneli |
| dryf katalog↔manifest | 14 kerneli w manifeście bez katalogu i 13 odwrotnie; pełny build **usunąłby** żywy sampler |
| brak szeregowania instrukcji | „41,3 TOPS to praktyczny pułap, llama.cpp osiąga ~46" |
| **brak Metala** | Mojo nie emituje kodu na GPU Apple — ścieżka Apple **nie da się** zrealizować w obecnym ADR |

Ostatni wiersz jest nowy i rozstrzygający: dopóki „kernel" znaczy „plik `.mojo`", Apple jest poza
zasięgiem z definicji.

### D6. Kernele kopiowane zamiast generowane

469 kerneli na gfx1201, 535 na sm_121a. Analiza formatów mówi, że **30 typów kwantyzacji
sprowadza się do ~10 rodzin**, a prymityw `U4-SPLIT` jest identyczny w **11 formatach**.
FORGE ma zamiast tego iloczyn kartezjański `format × kształt × batch × dtype wyjścia × dostawca`,
wypisany ręcznie. Konsekwencja niewidoczna z liczby plików: **poprawka w dekodzie formatu wymaga
dotknięcia kilkunastu kerneli**, więc nikt jej nie robi i powstaje kolejny wariant.

Trzy koszty policzone wprost: `Q6_K` bez kernela macierzowego na AMD = **25,4% prefillu**;
wariant `out_f32` musiał powstać dla **wszystkich pięciu rodzin** kafli, choć różni się typem
wskaźnika i jedną linią zapisu (odblokował prefill TP z 17,12 s na **1,79 s**); nazwa kafla
musi nieść geometrię, bo inaczej launcher **po cichu pomija połowę wierszy**.

### D7. Metodyka pomiaru nieegzekwowana maszynowo

FORGE ma dobrą metodykę spisaną 「TF-ANALIZA §7.1」, ale nie ma jej w kodzie. Skutki:

- Dwa udokumentowane przypadki, w których „optymalizacja" była błędem pomiaru:
  „11154/1,94× było **fałszem** — złym pomiarem" (realnie 5742 → 4467, czyli regresja)
  i „11120/0,997× było błędem pomiaru" (realnie **0,51×**).
- `--prefix-cache on` domyślnie zawyżało prefill **3×** (44 537 vs 14 783 tok/s).
- `bench` rozjeżdżał się z `serve`: auto-fp8 włączał się tylko w `serve`, więc `bench` zaniżał
  prefill Mistrala **2,3×**.
- Sesja pomiarowa unieważniona przez niezapisany `pp_dpm_mclk` (456 z 1000 MHz).

Na Apple ta klasa błędu jest **groźniejsza**, nie łagodniejsza: throttling termiczny i praca
na baterii zmieniają wynik bardziej niż większość optymalizacji, które będziemy mierzyć.

### D8. Brak backendu Apple i brak obsługi MLX

**Stan zmierzony:** `forge-hal` ma `cuda`, `hip`, `cpu`. Metal nie istnieje.
**Ustalenie z 2026-08-02, korzystne:** cały workspace `cargo check --workspace` **przechodzi
na macOS** (cudarc jest budowany z `dynamic-loading`, więc nie wymaga CUDA w czasie budowy).
Backend Metal da się więc rozwijać na tej maszynie przy kompletnym, kompilującym się silniku
i z backendem CPU jako wyrocznią — faza NA2 nie potrzebuje osobnego stanowiska. Osiem wystąpień
słów „Apple"/„Metal" w całym kodzie to komentarze i **testy sprawdzające, że Apple czegoś
nie potrafi** (`assert!(!hybrid_prefill_b2_backend_capable(Vendor::Apple, 32))`).
`forge-formats` nie ma czytnika kwantyzacji MLX.

**Koszt biznesowy:** na Macu FORGE nie startuje, a modele MLX — najszybciej rosnąca rodzina
checkpointów dla Apple Silicon — są niedostępne **także na AMD i NVIDII**, bo brakuje samego
dekodera formatu, niezależnie od karty.

### D9. Brak punktów wpięcia — rozszerzenie kosztuje kilkanaście miejsc

**Dowód: dodanie jednego modelu.** Gemma 4 12B — działa, ale kosztowała **sześć bezgłośnych
błędów**: `qk_norm_over_hidden` porównujące długość normy z globalnym `head_dim` (norma leciała
po całej projekcji i **czytała poza bufor**), prefill i decode z globalnymi wymiarami dla
warstw o naprzemiennej geometrii, `gelu_mul_f16` liczące `tanh` przez `exp(2x)` (przelew do
`inf` → `NaN`, a **kwantyzacja aktywacji zamieniała NaN w ciche śmieci**), brak norm sandwich,
cache KV rozmiarowany jedną geometrią, brak maski `suppress_tokens`.

**Dowód: dodanie drugiego modelu.** DeepSeek V4 Flash — **siedem** mechanizmów, z których silnik
nie miał żadnego, i trzy klasy trudności, wszystkie bezgłośne: konwencje mylące się bez sygnału
(`weight_scale_2` **mnoży**, `weight_global_scale` **dzieli** — podstawienie daje wagi rzędu 10⁶
zamiast 10⁻², bez ostrzeżenia), detale grafu niewidoczne z metadanych, precyzja jako poprawność
(kompresor w f16 daje `inf`, softmax z `inf − inf` daje `NaN`; **pierwsze trzy tokeny zgadzały
się z referencją**).

**Dowód: dodanie karty.** `docs/NOWA_KARTA.md` istnieje jako lista kroków — czyli dodanie karty
jest procedurą dla człowieka, a nie wpisem w rejestrze. Skutkiem są bramki na dostawcy
postawione za szeroko: **trzy razy w jednym pliku**, koszt 16,4× na weryfikacji MTP i 26×
na prefillu hybrydowym.

**To jest dług, który blokuje wszystkie pozostałe**, bo Apple, MLX i każdy następny model wchodzą
dokładnie tymi punktami. Rozdział 6 jest odpowiedzią na niego.

### D10. Prefill poniżej sufitu obliczeniowego

Decode i prefill mają **przeciwne wąskie gardła** i mieszanie ich prowadzi do optymalizowania
nie tego, co trzeba. Prefill 1024 tokenów na 27B Q4_K to ~58,8 biliona operacji, przy odczycie
wag **26,8 ms raz na cały prefill** — czyli jest zdecydowanie ograniczony arytmetyką.

| | czas | tok/s |
|---|--:|--:|
| FORGE | 691 ms | **1 481** |
| sufit f16 WMMA | 328 ms | **3 118** |
| sufit fp8 WMMA | 155 ms | **6 585** |
| sufit int4 WMMA | 79 ms | **12 944** |

FORGE stoi na **~47% sufitu f16** i **~22% sufitu fp8**, mimo że **zmierzył u siebie kafel fp8
dający 203 TFLOPS wobec 97 dla f16** na `ffn_gate`/`ffn_up`. Ścieżka istnieje i jest szybsza —
nie jest domyślna. Osobno: na 7900 XT prefill był **płaski względem długości promptu**
(27,3 przy p128, 27,3 przy p1024), bo layer-major był zabramkowany na `Vendor::Nvidia`;
po zdjęciu bramki **27,3 → 843,9 tok/s (30,9×)**. Prefill jest miejscem, w którym pojedyncza
bramka kosztuje rząd wielkości.

---

## 3. Czego nie ruszać

Lista rzeczy zmierzonych i dobrych. Każda wchodzi do zestawu regresyjnego **przed** N1 —
refaktoryzacja bez tego jest hazardem.

1. **Natywne MTP/NextN** — Q4_K_M 31,0 → 58,9, NVFP4 30,9 → 70,4; akceptacja 1,87–2,17,
   pełne ID wobec greedy.
2. **Redukcja symetryczna dla TP=2** — +45,8%; wariant zbierający **regresował o 14%**.
3. **Warianty `_at` (offset bajtowy zamiast kopii D2D)** — 430 → 52,2 kopii/token. Zasada:
   **każdy konsument czyta przez przesunięcie, nigdy przez kopię do bufora „od zera"**.
4. **Radix prefix cache** — wspólny prefiks 2048 tok: prefill 68,8 → **14,8 ms (4,7×)**, ID bit-identyczne.
5. **Device-side grouped expert dispatch** (`_gidx`) — zero `synchronize()` na warstwę w decode MoE.
6. **Tablica wskaźników ekspertów zamiast sklejonego stosu** — blok z VRAM i blok z pamięci hosta
   wyglądają dla kernela identycznie; **mieszanie warstw nie wymaga ani jednej gałęzi w kodzie GPU**.
7. **Dwuprzebiegowy sampler top-k** — 560 → 2 496 tok/s.
8. **Split flash-decode z efektywnymi splitami** — 1 512 → 1 670 tok/s.
9. **Scheduler: FIFO serial prefill, jedna sekwencja na iterację** — TTFT med 937 → 330 ms.
10. **`sub_buffer` w HAL** — bez tego rezydencja ekspertów to dziesiątki tysięcy alokacji.
11. **Deklaratywny rejestr architektur** (`arch/*.ron`) — wzorzec do powielenia, nie do zmiany.

---

## 4. Architektura docelowa — pięć kontraktów

Każdy kontrakt zastępuje konkretną klasę gałęzi w kodzie.

### 4.1. `KernelSpec` / `VariantKey` — wariant jest danymi

```rust
// crates/forge-kernels/src/spec.rs
/// Pełne wejście doboru kernela. Dwa identyczne KernelSpec MUSZĄ dawać ten sam artefakt.
pub struct KernelSpec {
    pub op: OpKind,                 // GemmDequant, FlashAttn, RmsNorm, Rope, SiluMul, Sample, ...
    pub shape: ShapeBucket,         // kubełki, nie dokładne kształty
    pub weight_format: QuantFormat,
    pub act_dtype: DType,
    pub out_dtype: DType,           // to, co dziś jest osobną rodziną `*_out_f32`
    pub target: Target,             // sm_89 | sm_121a | gfx1100 | gfx1201 | apple_g9 | apple_g10
    pub batch: BatchBucket,         // 1,2,4,8,16,32,64,128,256,512
    pub tile: TileConfig,
    pub flags: KernelFlags,         // FUSE_NORM | FUSE_SILU_MUL | FUSE_RESIDUAL | PEER_WEIGHTS
}
```

**Co to kasuje:** `if vendor == Nvidia` (→ `target`), rodzinę `*_out_f32` (→ `out_dtype`),
`FORGE_GEMM`/`FORGE_ATTN`/`FORGE_NVFP4_CT_LAYOUT` (→ rejestr), osobne kernele `b4`/`b8`/`b16`
(→ `batch`), fuzje jako osobne pliki (→ `flags`).

Rejestr: `VariantKey` (= `KernelSpec` bez `TileConfig`) → do 8 slotów, każdy z wynikiem golden
i zmierzonym czasem. **Slot bez zaliczonego golden nie może być użyty**, a golden idzie
**przed** pomiarem — slot liczący szybko i źle nie trafia do rejestru nawet na chwilę.
To jest bezpośrednia mitygacja „cichego złego artefaktu" z D5.

Brak wariantu → **wariant większego kubełka** (zawsze poprawny, tylko wolniejszy) plus generacja
w tle. Nigdy błąd, nigdy klif.

### 4.2. `BatchPlan` — batch jest długością tablicy

```rust
pub struct BatchPlan {
    pub tokens: Vec<TokenId>,       // płaska tablica, wszystkie elementy pracy razem
    pub seq_of_token: Vec<u16>,
    pub pos_of_token: Vec<u32>,
    pub seqs: Vec<SeqId>,
    pub mask: MaskDesc,             // Causal | Tree{ancestors} | Bidirectional(rezerwa)
    pub tile_width: u32,            // parametr geometrii, NIE liczba sekwencji
}
```

**Niezmiennik I-SCHED-1:** w ścieżce planowania **nie istnieje** gałąź na `seq_count() == 1`
ani `token_count() == seq_count()`. Test: ten sam ślad wykonania dla B=1 i B=2.

**Konsekwencja:** decode, prefill i weryfikacja draftu to **ta sama ścieżka kodu** z innym
kształtem wejścia; chunked prefill wchodzi do tego samego batcha co decode.

**Doprecyzowanie.** „Nie ma kernela GEMV, jest GEMM degenerujący się dla B=1" jest **nieprawdą**
i pomiar to pokazał: przy B=1 kafel 16×16 dostaje 16 kopii tego samego wiersza, 15/16 mnożenia
jest redundantne. Dopuszczalne jest **drugie odwzorowanie kontrakcji** pod czterema warunkami:
to samo źródło i prymitywy, wybór należy do rejestru (nigdy do `if B == 1` w kodzie wywołującym),
`no_perf_cliff` obowiązuje **także na granicy między odwzorowaniami**, limit liczby kerneli wiąże.

### 4.3. IR grafu i plan kroku

Lekki IR (~30 opów), na nim passy: zwijanie stałych, **fuzja**, planowanie układu danych,
planowanie pamięci, punkty komunikacji TP/PP/EP. Wyjściem jest **plan kroku** — lista etapów
z tablicą argumentów, a nie 21 tysięcy linii wywołań.

Kryterium sukcesu jest arytmetyczne: **≤ 65 uruchomień na krok decode** (dziś 681), co przy
3,31 µs na usunięty punkt synchronizacji daje ~2,0 ms z 32 ms i zabiera 93% zysku megakernela.

Fuzje, które IR musi umieć bez pytania człowieka (wszystkie mają zmierzony precedens):
`norm + linear`, `dequant + GEMM`, `SiLU-mul`, `RoPE + pakowanie attention`, `residual` w epilogu,
`gate|up` scalone przy ładowaniu.

**Warunek bitowej zgodności wpisany w pass fuzji:** blok musi mieć tyle wątków, ile redukcja
miała wartości. Każda zmiana szerokości bloku, w którym siedzi redukcja, **jest zmianą arytmetyki**
i wymaga bramki SHA wszystkich ścieżek — FORGE ma na to regresję: rozszerzenie `rows == 1`
na `rows <= 8` objęło weryfikację MTP, SHA przestało się zgadzać, przepustowość 58,8 → 56,9.

### 4.4. Jeden typ bloku pamięci

Dziś tiering ekspertów (`moe_residency.rs` 868 + `expert_spill.rs` 203 + `weight_tier.rs` 212),
KV (`kv.rs` 756), prefix cache (`prefix.rs` 374) i stan hybrydowy konkurują o tę samą pamięć
i to samo pasmo **nie wiedząc o sobie**.

Jeden nieprzezroczysty blok: bajty + tier + refcount + znacznik użycia + koszt sprowadzenia.
Semantyka (strona KV / stan SSM / ekspert / waga) jest warstwą **nad** nim. Jeden arbiter pasma.

Dowód, że to nie kosmetyka: pierwsza wersja rezydencji ekspertów rozdzielała pamięć **w kolejności
ładowania** — ostatnie warstwy w całości na dysku, więc każdy token trafiał **gwarantowanym
chybieniem** w każdą z nich; przy budżecie 61% model po prostu się nie ładował.

**Na Apple ten kontrakt ma dodatkowe znaczenie:** przy pamięci unified „tier VRAM" i „tier RAM"
to ten sam fizyczny zasób, więc menedżer musi umieć wyrazić tier o **zerowym koszcie
sprowadzenia** — inaczej cała maszyneria migracji będzie tam mieliła powietrze (§7.3).

### 4.5. Jeden plik konfiguracyjny

`forge.toml`, typowany, jedno miejsce definicji, **zero odczytów `env` poza modułem konfiguracji**
(lint: `std::env::var` dozwolone w dokładnie jednym pliku). Likwiduje 42 przełączniki ścieżki;
`FORGE_BENCH_*` i `FORGE_TEST_*` przechodzą na flagi CLI i atrybuty testów.

---

## 5. Układ kodu — docelowy podział na moduły

Zasada nadrzędna: **plik ma jedną odpowiedzialność i ≤ 1500 linii**; przekroczenie limitu jest
sygnałem, że moduł robi za dużo, a nie powodem do podniesienia limitu.

### 5.1. Warstwy i uprawnienia

```
forge-types      typy bazowe: DType, kształty, błędy, opis możliwości sprzętu
forge-quant      dekodery kwantyzacji + referencja CPU + repack     (zero zależności od HAL)
forge-formats    GGUF / safetensors / compressed-tensors / MLX + rejestr architektur
forge-graph      IR modelu, passy, plan kroku                       (NIE WIE o HAL)
forge-hal        jedyny styk ze sprzętem: cuda | hip | metal | cpu
forge-kernels    rejestr wariantów, autotuner, cache, fasada wykonawcza
forge-state      KV stronicowany, stan SSM, prefix cache, bloki
forge-sched      continuous batching, admission control, SLO
forge-spec       spekulacja: proposerzy, drzewa, weryfikacja
forge-model      architektury: dense / hybrid / MoE; budowa grafu z wag
forge-io         tokenizery, detokenizacja, ekstraktory cech
forge-server     HTTP/OpenAI API          forge-cli   run | bench | inspect | probe
```

**Dwie niezależne reguły, których nie wolno mylić:**

- **Reguła 1 — kolejność.** Zależności idą wyłącznie w dół listy.
- **Reguła 2 — granica sprzętowa.** `forge-hal` widzą **wyłącznie** `forge-kernels`,
  `forge-state` i `forge-cli`. `forge-graph`, `forge-model`, `forge-sched` i `forge-spec`
  **nie wiedzą, jaki sprzęt jest pod spodem**.

Obie egzekwowane testem w `xtask`, z osobnym testem na każdą, żeby komunikat mówił, która
została złamana. **Backend CPU jest utrzymywany jako wyrocznia i weryfikator granicy**: jeśli
cokolwiek specyficznego dla dostawcy przecieknie wyżej, CPU przestanie się kompilować i dowiemy
się o tym w CI, a nie po wyniku modelu.

`forge-quant` leży **przed** `forge-kernels` celowo: opis formatu jest wejściem generatora
kerneli, a referencja CPU jest wyrocznią testów golden. Przy odwrotnej kolejności opis formatu
musiałby istnieć w dwóch miejscach bez możliwości przetestowania ich zgodności.

### 5.2. Rozbicie `model.rs` (21 430 linii)

Podział przebiega po granicach, które już w tym pliku są:

```
forge-model/src/
  lib.rs              (≤150)  fasada: ModelHandle, budowa z wag
  common/{norm,rope,attn_common,residual}.rs   wspólne fragmenty grafu
  dense/{build,layer}.rs
  hybrid/{build,layer,deltanet,state}.rs
  moe/{build,router,dispatch}.rs
  mtp/{build,verify,commit}.rs
  prefill/{plan,chunk,layer_major}.rs
  decode/{plan,step}.rs
  sampling/{order,penalties,topk}.rs
```

**Warunek fazy N1:** każdy commit ma **identyczną macierz SHA**. Zmiana sumy SHA przy
przenoszeniu kodu oznacza, że przenoszenie nie było mechaniczne — cofamy.

### 5.3. Rozbicie `launchers.rs` (20 430 linii, 387 funkcji)

Docelowo ten plik **przestaje istnieć**. 387 funkcji to objaw D6: każdy wariant kernela ma własny
launcher, bo siatka i argumenty są wypisane ręcznie. Po N3 launcher jest jeden i generyczny —
siatkę liczy się z `TileConfig` wariantu, a argumenty z `KernelSpec`:

```
forge-kernels/src/
  spec.rs        (≤400)  KernelSpec, VariantKey, TileConfig, ShapeBucket, BatchBucket
  registry/{mod,cache,fingerprint,autotune}.rs
  dispatch.rs    (≤300)  JEDEN launcher: (wariant, argumenty) → uchwyt zakończenia
  grid.rs        (≤200)  siatka liczona z TileConfig — nigdy ręcznie
  artifacts/{mojo,cuda_src,msl,embedded}.rs   źródła artefaktów
```

**Test, który tego pilnuje:** `kernel_count_bound` (górna granica liczby kerneli w katalogu,
malejąca wraz z postępem) oraz lint sprawdzający, że **nazwa wariantu niesie geometrię**
(`_bm256_bn128`) — bez tego launcher liczy siatkę z BN=128 dla kernela kafelkującego po 64
i po cichu pomija połowę wierszy.

---

## 6. Rozszerzalność — pięć punktów wpięcia

To jest odpowiedź na D9 i warunek, żeby Apple, MLX i każdy następny model dały się dodać bez
przebudowy. **Reguła nadrzędna: rozszerzenie jest wpisem w danych plus co najwyżej jedną
implementacją wąskiego traitu. Jeśli wymaga dotknięcia więcej niż dwóch miejsc — punkt wpięcia
jest źle postawiony i to on jest do naprawy, nie rozszerzenie.**

### 6.1. Punkt 1 — nowa karta / nowa architektura GPU

**Dziś:** procedura dla człowieka (`docs/NOWA_KARTA.md`) plus bramki `if vendor == …` rozsiane
po kodzie, z których trzy były postawione za szeroko w jednym pliku (koszt 16,4× i 26×).

**Docelowo:** rejestr możliwości jako dane + jeden backend HAL.

```rust
// forge-hal/caps/<target>.ron — DANE, nie kod
DeviceCaps(
    target: "apple_g9",              // rodzina, nie model handlowy
    vendor: Apple,
    unified_memory: true,            // zmienia model kopiowania (§7.3)
    compute_units: 10,               // jednostka natywna platformy
    cu_semantics: GpuCore,           // CU | WGP | SM | GpuCore — koniec pomyłki WGP/CU
    simd_width: 32,
    matrix: [
        MatrixUnit(shape: (8,8,8), a: F16, acc: F32, kind: SimdgroupMatrix),
        MatrixUnit(shape: (8,8,8), a: BF16, acc: F32, kind: SimdgroupMatrix),
    ],
    dot_int8: None,                  // brak v_dot4 — nie zgaduj, zadeklaruj
    threadgroup_mem: 32768,
    peak_bandwidth_hint: 120e9,      // HINT — model kosztu i tak kalibruje pomiarem
)
```

**Trzy zasady, każda z zapłaconej ceny:**

- **Możliwość jest deklarowana, nie wywnioskowana z błędu kompilacji.** „Nie zgaduj zasięgu
  z tego, że coś się nie kompiluje" — zasięg architektury jest wpisem, a wykrycie następuje
  **przed** kompilacją.
- **Jednostka zliczania jest częścią deklaracji.** `multiProcessorCount` na RDNA liczy WGP,
  nie CU (40 vs 80). Pole `cu_semantics` sprawia, że heurystyka „bloki na jednostkę" nie może
  po cichu znaczyć czegoś innego na innej karcie.
- **Wartości wydajnościowe są hintem, nie prawdą.** „Stosunku mocy kart nie da się wyprowadzić
  z parametrów": z instrukcji `dot4` wynikałoby, że w prefillu wygrywa 6900 XT (97 vs 43 TOPS),
  a przegrywa **ośmiokrotnie**. Model kosztu kalibruje się mikrobenchmarkiem przy starcie.

**Definicja ukończenia dla nowej karty:** wpis w rejestrze + sonda startowa potwierdzająca
każdą zadeklarowaną możliwość + zielona macierz SHA na tej karcie. Zero zmian w `forge-graph`,
`forge-model`, `forge-sched`.

### 6.2. Punkt 2 — nowy model / nowa architektura sieci

**Dziś:** częściowo zrobione dobrze (`arch/*.ron`), ale opis kończy się na mapowaniu nazw
tensorów; geometria naprzemienna, warianty RoPE i normy trafiają do kodu.

**Docelowo:** rejestr opisuje **role i geometrię per warstwa**, a nie tylko nazwy:

```ron
Arch(
    name: "gemma4",
    layers: 48,
    // Geometria naprzemienna JAKO DANE — to jest dokładnie miejsce, w którym
    // Gemma 4 kosztowała cztery z sześciu bezgłośnych błędów.
    layer_pattern: [ Local(window: 1024), Local(1024), Local(1024), Local(1024), Local(1024), Global ],
    head_count_kv: [8, 8, 8, 8, 8, 1],
    key_length: [256, 256, 256, 256, 256, 512],
    v_from_k_when: Global,            // warstwy globalne nie mają projekcji V
    norm: Rms(eps: 1e-6, sandwich: true),
    rope: NeoX(theta: 1e6, partial: 1.0),
    qk_norm: PerHead,                 // NIE „po hidden" — źródło czytania poza bufor
    suppress_tokens: FromTokenizer,
)
```

**Walidacja ról jest obowiązkowa i wykonuje się przy ładowaniu**, nie przy pierwszym tokenie:
każdy tensor pliku musi rozwiązać się do roli, każda rola wymagana przez architekturę musi mieć
tensor, a kształty muszą się zgadzać **per warstwa**, nie globalnie. TentaForge pokazał, że to
działa: wpisy `qwen3_5` i `deepseek_v4` rozwiązują **wszystkie** tensory bez reszty —
866/866, 2687/2687, 135 235/135 235.

**Wyrocznia numeryczna dla modeli, których nie da się sprawdzić „na oko".** Przy DeepSeeku V4
uratował projekt `oracle` z kodem skopiowanym **dosłownie** z referencji („to ma być oracle,
a nie druga interpretacja tego samego kodu") i 17 fragmentów warstwy przypiętych na 1e-6…1e-7.
To wchodzi do procedury dodawania modelu jako krok obowiązkowy, gdy model nie mieści się w pamięci
albo nie ma punktu odniesienia.

**Definicja ukończenia dla nowego modelu:** wpis w rejestrze + walidacja ról 100% + zgodność
z wyrocznią na warstwie + wpis w macierzy SHA. **Zero zmian w kernelach**, jeśli model nie wnosi
nowego mechanizmu; jeśli wnosi — nowy op w IR, nie nowa gałąź w `model.rs`.

### 6.3. Punkt 3 — nowy format kwantyzacji

**Dziś:** dekoder + od kilku do kilkunastu kerneli + wpis w launcherach.

**Docelowo:** jeden wpis opisowy, z którego generator składa kernele:

```rust
QuantDesc {
    name: "mlx_affine",
    bits: 4,                      // 2 | 3 | 4 | 5 | 6 | 8
    group: 64,                    // wzdłuż K
    packing: U32LsbFirst,         // 8 nibbli w u32, element i na bitach 4*(i%8)
    scale: PerGroup(BF16),
    bias:  PerGroup(BF16),        // affine: w = q * scale + bias
    zero_point: None,
    layout: RowMajorGroupsAlongK,
}
```

Z takiego opisu powstaje ścieżka dekodu przez **prymitywy** (`U4-SPLIT`, `SCALE-AFFINE`),
a nie przez nowy plik kernela. Jedna poprawka w prymitywie poprawia wszystkie formaty, które
go używają — `U4-SPLIT` jest wspólny dla **11 formatów**.

**Test, który tego pilnuje:** referencja CPU **bit w bit** dla dequantu każdego formatu (to
całkowitoliczbowe rozpakowanie plus jedno mnożenie — różnica oznacza błąd, nie zaokrąglenie)
oraz `kernel_count_bound`.

### 6.4. Punkt 4 — nowy kernel albo nowe źródło kerneli

**Dziś:** „kernel" znaczy „plik `.mojo` zarejestrowany w builderze" — i to jest powód, dla
którego Apple jest poza zasięgiem (D5).

**Docelowo:** kontrakt brzmi `KernelSpec → artefakt`, a **źródło artefaktu jest wymienne**:

```rust
trait KernelSource {
    fn can_build(&self, spec: &KernelSpec, caps: &DeviceCaps) -> bool;
    fn build(&self, spec: &KernelSpec, caps: &DeviceCaps) -> Result<Artifact>;
    fn provenance(&self) -> Provenance;   // do `inspect`: skąd wziął się ten bajt
}
```

Implementacje: `MojoSource` (PTX/HSACO — stan obecny), `CudaSource` (wendorowane `.cu`, dziś
dwa wyjątki od ADR), `MslSource` (Metal Shading Language — nowe, §7.5), `EmbeddedSource`
(artefakty w binarce). Dołożenie piątego źródła nie dotyka rejestru ani dispatchu.

**Trzy bramki, każda z zapłaconej ceny:**
- **golden przed pomiarem** — artefakt bez zaliczonego golden nie wchodzi do rejestru
  (mitygacja „cichego złego artefaktu": `hd256` zbudowany z ciała `hd128`, pliki **bajtowo
  identyczne**, model liczył śmieci);
- **manifest jest generowany z katalogu**, nie utrzymywany równolegle — koniec dryfu
  (14 kerneli w manifeście bez katalogu, 13 odwrotnie, a pełny build **usunąłby** żywy sampler);
- **`provenance` w `inspect`** — dla każdej operacji widać, który wariant, z jakiego źródła,
  z jakim wynikiem golden i jakim czasem. Brak panowania nad wariantami bierze się
  z niewidoczności wyboru, nie z jego złożoności.

### 6.5. Punkt 5 — nowe wejście/wyjście i nowa modalność

Jednostką pracy schedulera **nie jest sekwencja generująca tokeny**, tylko ogólny element pracy
z własnym modelem kosztu i warunkiem zakończenia. Embeddingi nic nie generują, STT konsumuje
ramki audio, TTS produkuje kody akustyczne. Bez tego uogólnienia scheduler zarasta warunkami
per modalność.

Wejście i wyjście modelu są wymienne: tokenizer BPE / ekstraktor mel / patchowanie obrazu
z przodu, sampling / wektor znormalizowany / kody akustyczne z tyłu. Wieża wizyjna
(Qwen3-VL jest już na dysku) jest pierwszym realnym testem tego interfejsu — jeśli ViT wpina się
czysto, encoder STT też się wepnie.

**Zasada rozstrzygająca spory o rezerwy:** rezerwa jest dopuszczalna wtedy i tylko wtedy, gdy
jej brak wymusiłby później **zmianę kontraktu warstwy**, i musi być jawnie oznaczona
z odsyłaczem. Rezerwa „na wszelki wypadek" jest zakazana.

### 6.6. Test rozszerzalności — mierzalny, nie deklaratywny

Trzy testy w CI, każdy tworzący rozszerzenie **w czasie swojego biegu**:

1. `add_arch_without_compiling` — buduje opis architektury w runtime i wczytuje nim realny plik.
   (TentaForge ma dokładnie taki test dla `olmoe`; udowadnia, że rejestr jest naprawdę danymi.)
2. `add_quant_format_reaches_kernel` — nowy `QuantDesc` przechodzi przez generator do artefaktu
   i zgadza się z referencją CPU bit w bit.
3. `add_target_is_data_only` — nowy wpis `DeviceCaps` przechodzi walidację i planowanie zasobów
   bez ani jednej zmiany w `forge-graph`/`forge-model`.

---

## 7. Apple: backend Metal i modele MLX

Wymaganie ma dwie części, które trzeba trzymać osobno:

- **(A) FORGE działa na Apple Silicon** — nowy backend HAL i nowe źródło kerneli.
- **(B) Modele MLX działają wszędzie** — na Apple, AMD i NVIDII. To jest **wyłącznie sprawa
  loadera i formatu kwantyzacji**, niezależna od (A).

Rozdzielenie jest istotne, bo (B) da się dowieźć **przed** (A) i zweryfikować na R9700.

### 7.1. Fakty o sprzęcie M-series i ich konsekwencje

| Fakt | Źródło | Konsekwencja projektowa |
|---|---|---|
| Pamięć **unified**, CPU i GPU widzą tę samą | architektura Apple | `write(host→device)` to kopia danych, które nigdzie nie jadą. Model HAL musi mieć **tier o zerowym koszcie sprowadzenia** |
| **`simdgroup_matrix` 8×8×8** f16 i bf16, od M1 | Metal | to jest odpowiednik WMMA. Kafle GEMM projektujemy na 8×8, nie 16×16 |
| Na M1–M4 macierze liczą się **na zwykłych rdzeniach shaderów**, bez dedykowanej ścieżki | Rigel (M4 Max) | brak „darmowego" mnożnika; prefill skaluje się z liczbą rdzeni GPU i zegarem |
| **M5 ma Neural Accelerator w każdym rdzeniu GPU**, sterowany przez Metal 4 `mpp::tensor_ops::matmul2d` | Metal 4 / BaseRT | to **osobna możliwość w rejestrze** (`apple_g10`), nie flaga „nowszy Apple" |
| Na M4 Max `matmul2d` bije `simdgroup_matrix` tylko **1,05–1,21×** | Rigel | na M4 nie ma po co przepisywać na Metal Performance Primitives; na M5 ma |
| **fp8 (E4M3) w Metal 4.1 jest emulowany**: 0,94× przepustowości fp16 mimo połowy bajtów operandu | Rigel | **na Apple fp8 nie jest dźwignią prefillu.** Na RDNA4 była (2,1×). Ta sama optymalizacja, przeciwny wynik — dlatego akceleracja jest wpisem w rejestrze możliwości, a nie założeniem |
| Ten M4: **10 rdzeni GPU, 16 GiB** | pomiar | sufity w §7.7 |
| **Realna przepustowość pamięci: 102,4 GB/s = 85,3% ze 120 katalogowych** | **EKS-A1** | mianownik celów dekodowania; liczba katalogowa jest zawyżona o 17% |
| **Reguła „≥ 8 akumulatorów" z AMD NIE przenosi się** — najlepszy wynik przy jednym | **EKS-A1** | kernele strumieniowe Metal: prosta pętla, szeroki odczyt; wymaganie ILP jest własnością targetu, nie regułą globalną |
| **Dyspozycja w jednym command bufferze: 0,61 µs** (AMD: 3,83 µs) | **EKS-A3** | **fuzja nie jest na Apple dźwignią** — 681→65 dyspozycji to 0,7% kroku |
| **Osobny command buffer na dyspozycję: 19,6 µs** | **EKS-A3** | command buffer per warstwa zakazany (1,27 ms/token przy 65 warstwach) |
| **Powrót na hosta: ~94 µs** (AMD: 18,2 µs) | **EKS-A3** | oczekiwanie hosta per warstwa zakazane bezwzględnie (6,12 ms/token) |
| **`simdgroup_matrix` f16: 3,94 TFLOPS; zwykłe FMA FP32: 3,07** | **EKS-A2** | instrukcja macierzowa daje **1,28×**, nie rząd wielkości — to nie jest rdzeń tensorowy |
| **Akumulacja f32 kosztuje 1,3%, bf16 tyle co f16** | **EKS-A2** | kernele akumulują w f32 domyślnie; skale MLX w BF16 nie wymagają konwersji |

**Wniosek, który zmienia priorytety na Apple:** dźwignią nie jest niższa precyzja (fp8 nic nie
daje, a fp4 sprzętowo nie istnieje), tylko **unikanie kopii, pełne wykorzystanie
`simdgroup_matrix` i zamykanie kroku w jednym command bufferze**. Ścieżka int4/fp8, która
na RDNA4 jest główną dźwignią prefillu, na M4 jest ślepą uliczką.

**Korekta po EKS-A3 (2026-08-02).** Wcześniejsza wersja tego akapitu wymieniała wśród
dźwigni Apple także **fuzję**. Pomiar to obalił: dyspozycja wewnątrz command buffera
kosztuje 0,61 µs, więc zejście z 681 do 65 dyspozycji oszczędza 0,37 ms z ~50 ms kroku,
czyli **0,7%**. Proporcje wobec AMD są odwrócone — dyspozycja jest 6,3× tańsza, a powrót
na hosta 5,2× droższy — więc i optymalizacja jest inna: **jeden command buffer na krok,
zero oczekiwań hosta w pętli warstw**. Fuzja zostaje uzasadniona mniejszym ruchem przez
pamięć, ale nie kosztem dyspozycji. Pełny raport: `pomiary/eks-a1-a3-apple-m4.md`.

### 7.2. Stan wyjściowy: zero

`forge-hal` nie ma Metala; osiem wystąpień „Apple"/„Metal" w kodzie to komentarze i **testy
sprawdzające, że Apple czegoś nie potrafi**. Te testy zostają — jako testy — ale ich treść
przestanie być prawdziwa i dlatego wchodzą do zestawu regresyjnego jako pierwsze do odwrócenia.

### 7.3. Co w HAL jest wprost szkodliwe na Apple

Cztery elementy obecnego kontraktu `Device`:

1. **`alloc(bytes, kind, pool)` + `write(src, dst)` + `read(src, dst)`** — model dyskretnej karty.
   Na Apple bufor tworzy się z pamięci, którą host już ma (`newBufferWithBytesNoCopy`),
   a `write`/`read` degenerują się do zapisu i odczytu tego samego wskaźnika. Kontrakt musi
   dopuszczać `map()` zwracające wskaźnik hosta, żeby ładowanie modelu 4,2 GB nie kopiowało
   4,2 GB bez powodu.
2. **`create_stream` / `record_event` / `wait_event`** — Metal ma `MTLCommandQueue`
   i `MTLCommandBuffer` z `addCompletedHandler` oraz `MTLEvent`. Odwzorowanie jest możliwe,
   ale **jednostką zlecania jest command buffer**, nie pojedynczy dispatch — i to jest cecha,
   którą warto wykorzystać, a nie ukryć: wiele etapów w jednym buforze to dokładnie ta sama
   dźwignia co fuzja z §4.3.
3. **`begin_capture`/`launch_graph`** — nie ma odpowiednika. Ścieżka grafu musi być
   **możliwością w rejestrze**, a nie założeniem (na AMD i tak daje 0%).
4. **`LaunchArgs` jako sloty 8-bajtowe** — Metal wiąże bufory przez `setBuffer:offset:atIndex:`
   i stałe przez `setBytes:`. Kontrakt argumentów musi rozróżniać **bufor + offset** od
   **wartości skalarnej**, zamiast pakować oba w `u64`. To jest zresztą naprawa niezależna
   od Apple: wariant `_at` (offset bajtowy zamiast kopii) już dziś żyje w FORGE jako obejście
   tego samego ograniczenia.

**`sub_buffer` przenosi się czysto** (`MTLBuffer` + offset), więc rezydencja ekspertów
i tablica wskaźników działają na Apple bez zmian koncepcyjnych.

### 7.4. Formaty MLX — działają wszędzie, nie tylko na Apple

**Ustalenie faktograficzne (zweryfikowane na realnych checkpointach z dysku).**
MLX ma cztery tryby kwantyzacji: **`affine`, `mxfp4`, `mxfp8`, `nvfp4`**. Oba modele lokalne
(`Bielik-Minitron-7B-MLX-4bit`, `Qwen3-VL-30B-A3B-Instruct-4bit`) deklarują:

```json
"quantization": { "group_size": 64, "bits": 4, "mode": "affine" }
```

Układ tensorów (odczytany z nagłówka safetensors):

```
model.layers.0.mlp.gate_proj.weight   U32   [11264, 512]   // 4096 kolumn / 8 nibbli na u32
model.layers.0.mlp.gate_proj.scales   BF16  [11264,  64]   // 4096 / group_size 64
model.layers.0.mlp.gate_proj.biases   BF16  [11264,  64]
lm_head.weight / .scales / .biases                          // głowa też skwantyzowana
model.embed_tokens.weight / .scales / .biases               // embedding też
```

Dekod: `w = q * scale + bias`, grupa 64 wzdłuż K, `q` bez znaku z 4 bitów.

**Trzy wnioski, które czynią to tanim:**

1. **Trzy z czterech trybów FORGE już ma.** `nvfp4` jest ścieżką produkcyjną (ThinkingCap,
   DeepSeek V4), `mxfp4` jest w tabeli formatów. Nowy jest wyłącznie `affine`.
2. **`affine` jest strukturalnie tym, co FORGE już liczy.** To asymetryczna kwantyzacja
   grupowa ze skalą i przesunięciem — dokładnie kształt `Q4_1` i drugiego członu `Q4_K`
   (`w = d*sc*q − dmin*m`). Różnice: grupa 64 zamiast 32, przesunięcie **dodawane** zamiast
   odejmowanego, upakowanie 8 nibbli w `u32` zamiast bajtów, skale w BF16 zamiast F16.
   **Wszystkie cztery to parametry `QuantDesc` z §6.3, nie nowe kernele.**
3. **Uwaga na kierunek znaku — to jest dokładnie klasa błędu, która kosztowała DeepSeeka.**
   `weight_scale_2` DeepSeeka **mnoży**, `weight_global_scale` compressed-tensors **dzieli**;
   podstawienie jednego pod drugie daje wagi rzędu 10⁶ zamiast 10⁻², **bez błędu i bez
   ostrzeżenia**. MLX-owe `bias` jest **dodawane**. Bramką jest referencja CPU bit w bit
   plus porównanie z `mlx` na kilkunastu tensorach — nie „wygląda sensownie".

**Dwie rzeczy, które trzeba zrobić poza dekoderem:**

- **Skwantyzowany embedding i głowa.** `embed_tokens` i `lm_head` są w tym samym formacie,
  więc potrzebny jest **gather-dequant** (embedding) i GEMM z dequantem na głowie. FORGE ma
  precedens obu (gather Q4_K wsadowy i jednowierszowy, batchowa głowa Q6_K z wyjściem f32) —
  to jest parametryzacja, nie nowa praca.
- **Mapowanie nazw.** MLX używa nazw HuggingFace (`model.layers.N.self_attn.q_proj`), a nie
  GGUF (`blk.N.attn_q`). To jest **wpis w rejestrze architektur**, czyli dokładnie punkt
  wpięcia z §6.2 — a nie kod.

**Kryterium wyjścia (B), świadomie postawione na AMD:** `Bielik-Minitron-7B-MLX-4bit` generuje
poprawny tekst **na R9700**, z sumą SHA identyczną jak na M4. Model MLX, który działa tylko
na Apple, nie spełnia wymagania.

### 7.5. Kernele na Metal — skąd się biorą

Mojo nie emituje kodu na GPU Apple, więc `MojoSource` tam nie sięga (D5). Rozstrzygnięcie:
**`MslSource` — kernele w Metal Shading Language, kompilowane w runtime** przez
`newLibraryWithSource:` z cache artefaktów, dokładnie tak jak dziś działa cache wariantów.
Uzasadnienie: MSL jest jedynym publicznym, stabilnym wejściem do GPU Apple; kompilacja
w runtime jest tam tania i jest zwyczajową praktyką; a cache wariantów z §4.1 już istnieje
w projekcie i nie wymaga osobnego mechanizmu.

**Zakres kerneli na start (dokładnie ten, którego wymaga model dense 4-bit):**
`gemm_dequant` (affine 4/8-bit → f16, `simdgroup_matrix` 8×8), `gemv_dequant` dla wąskich
batchy, `rmsnorm` z rezyduum, `rope`, `flash_attention` (prefill i decode), `silu_mul`,
`sampling` (dwuprzebiegowy top-k), `gather_dequant` dla embeddingu.
**Osiem rodzin, nie osiemdziesiąt** — i to jest test, czy §6.3 i §6.4 zadziałały.

**Czego nie robimy w v1:** MPS/MPSGraph jako backendu (uzależnia od czarnej skrzynki
i uniemożliwia fuzję), `mpp::tensor_ops` (na M4 daje 1,05–1,21×; wchodzi razem ze wsparciem
M5), ANE (dostępny wyłącznie przez CoreML, nie da się użyć w pętli inferencyjnej z własnym KV).

### 7.6. `mlx-swift` jako poprzeczka — protokół porównania

`mlx-swift` jest w drzewie głównym (`tentaflow-desktop/macos/swift/MLXBridge`, fork PrismML
0.31.x), więc porównanie jest odtwarzalne od pierwszego dnia. Obowiązuje **sześć zasad uczciwego
porównania** z §10, a szczególnie:

1. **Ten sam plik modelu**, ten sam SHA — nie „ten sam model". Format wag decyduje o wyniku
   bardziej niż cokolwiek innego (ten sam checkpoint 27B: 28,2 tok/s prefillu w Q4_K_M i 770,5
   w NVFP4 — **27×**).
2. **Te same identyfikatory tokenów**, nie ten sam tekst — prompt tokenizowany raz, naszym
   tokenizerem, podany obu silnikom jako ciąg ID.
3. **Prefill i decode raportowane osobno**, prefix cache wyłączony po obu stronach.
4. **Przeplot A/B/A/B, minimum trzy pary** — Mac pod obciążeniem dryfuje termicznie mocniej
   niż stanowisko z dyskretną kartą; przeplot znosi dryf liniowy.
5. **Zapis stanu termicznego i źródła zasilania.** Odpowiednik reguły „zapisuj `pp_dpm_mclk`":
   na Macu pomiar na baterii albo po throttlingu jest nieporównywalny, a asymetria „prefill
   spadł, decode nie" jest sygnaturą throttlingu GPU.
6. **Wynik publikowany także wtedy, gdy mlx-swift wygrywa.** Najbardziej wartościowe wpisy
   w tabeli poprzednika to te, w których llama.cpp był szybszy.

### 7.7. Cele liczbowe na tym M4 (10 rdzeni GPU, 120 GB/s)

Bielik-Minitron-7B-MLX-4bit, plik **4 206 804 396 B = 4,207 GB**:

| | rachunek | wartość |
|---|---|--:|
| Sufit decode (pamięć) | **102,4 GB/s zmierzone** ÷ 4,207 GB | **24,3 tok/s** |
| Cel decode v1 | 80% sufitu | **≥ 19 tok/s** |
| Cel decode rozciągnięty | 86% sufitu (tyle wyciąga kernel strumieniowy) | 21 tok/s |
| Sufit obliczeniowy prefillu | **3,94 TFLOPS zmierzone** ÷ ~15 GFLOP/token | **~260 tok/s** |
| Cel prefill v1 | 70% sufitu | **≥ 175 tok/s** |

**Korekta po EKS-A5 (2026-08-03): oba cele v1 są ZANIŻONE wobec poprzeczki.**
MLX zmierzony na tej maszynie, tym modelu i tym promptcie robi **219,0 tok/s
prefillu i 22,4 tok/s dekodowania** (`docs/pomiary/eks-a5-wobec-mlx-m4.md`).
Progi 175 i 19 to były procenty sufitu sprzętowego, a nie prędkość konkurenta,
więc ich przekroczenie NIE oznacza spełnienia warunku „≥ 1,0× mlx" z §8.4.
Wiążący jest ten drugi. MLX wyciąga 92% sufitu pamięciowego w dekodowaniu i 84%
obliczeniowego w prefillu — to są liczby do pobicia.

**Korekta po EKS-A1 (2026-08-02).** Pierwsza wersja tej tabeli miała **dwa błędy naraz**:
rozmiar modelu wzięty z `du` w GiB i wpisany jako GB (3,9 zamiast 4,207) oraz przepustowość
**katalogowa** 120 GB/s zamiast zmierzonej 102,4. Iloczyn zawyżał sufit o 27% i dawał cel
24 tok/s, czyli **98,6% realnego sufitu** — nieosiągalny. To jest dokładnie ten błąd
metodyczny, który ten dokument zarzuca poprzednikowi w §1.4 (liczba katalogowa w mianowniku),
popełniony przy pisaniu planu i wychwycony przez pierwszy eksperyment.

**Status tych liczb po zamknięciu eksperymentów.** Oba sufity są **zmierzone na tej maszynie**:
decode przez EKS-A1 (102,4 GB/s, cztery uruchomienia), prefill przez EKS-A2 (3,94 TFLOPS →
260 tok/s). Wcześniejsze oszacowanie prefillu ze skalowania po rdzeniach GPU okazało się
trafne z dokładnością 6%. Otwarty zostaje wyłącznie odstęp między sufitem instrukcji
a realnym kernelem GEMM — to rozstrzyga faza NA2.

- **EKS-A1 — przepustowość pamięci. ZAMKNIĘTY 2026-08-02: 102,4 GB/s = 85,3%** ze 120
  katalogowych; sweep ILP wypada odwrotnie niż na AMD (najlepiej przy jednym akumulatorze).
- **EKS-A2 — przepustowość `simdgroup_matrix`. ZAMKNIĘTY 2026-08-02: 3,94 TFLOPS** (f16),
  3,89 (f16→f32), 3,90 (bf16→f32), wobec **3,07 TFLOPS** zwykłego FMA — przewaga instrukcji
  macierzowej to **1,28×**. Sufit prefillu **~260 tok/s**, czyli oszacowanie ze skalowania
  po rdzeniach było trafne z dokładnością 6%. Cel ≥ 175 tok/s = 67% sufitu **przestaje być
  hipotezą**. Raport: `pomiary/eks-a2-simdgroup-matrix-m4.md`.
- **EKS-A3 — koszt dyspozycji i koszt command buffera. ZAMKNIĘTY 2026-08-02:** dyspozycja
  w jednym buforze **0,61 µs**, osobny command buffer **19,6 µs**, powrót na hosta **~94 µs**.
  Rozstrzygnięcie: **fuzja nie jest na Apple dźwignią**, a ścieżka dekodowania ma zamykać
  krok w jednym command bufferze i synchronizować się z hostem raz na krok.

Raport: `pomiary/eks-a1-a3-apple-m4.md`. Kod: `tools/eks-apple/`.

---

## 8. Prefill — osobna ścieżka, osobne wąskie gardło

Prefill i decode mają **przeciwne** wąskie gardła; mieszanie ich jest źródłem złych priorytetów.
Decode czyta 15,8 GB wag na token i liczy 0,3 ms — jest pamięciowy. Prefill 1024 tokenów
wykonuje ~58,8 biliona operacji, czytając wagi **raz** (26,8 ms) — jest obliczeniowy.

### 8.1. Drabinka na każdej platformie

| Platforma | dziś | dźwignia | sufit | uwaga |
|---|--:|---|--:|---|
| RDNA4 (R9700) | 1 481 tok/s | f16 → **fp8** | 3 118 → **6 585** | kafel fp8 **zmierzony u nas**: 203 vs 97 TFLOPS |
| RDNA4 | | fp8 → int4 | **12 944** | blokują skale blokowe (§8.3) |
| NVIDIA (sm_89/121a) | — | fp8, int8-MMQ | — | codegen Mojo daje **36% sufitu** — tu wygrywa `CudaSource` |
| **Apple M1–M4** | brak | **wyłącznie f16/bf16 `simdgroup_matrix`** | **260 tok/s zmierzone** (ten M4) | **fp8 emulowany (0,94×), a macierz daje tylko 1,28× nad ALU — prefill jest ALU-bound** |
| Apple M5+ | brak | `mpp::tensor_ops` + Neural Accelerators | — | osobna możliwość w rejestrze |

**To jest najlepszy dowód, po co jest rejestr możliwości z §6.1:** ta sama optymalizacja (fp8)
daje 2,1× na jednej platformie i 0,94× na drugiej. Wpisana jako założenie w kodzie, byłaby
regresją na Apple.

### 8.2. Trzy błędy prefillu do niepowtórzenia

1. **Prefill płaski względem długości promptu** — sygnatura ścieżki chunkowanej na T=16:
   27,3 tok/s przy p128, p512 i p1024, bo layer-major był zabramkowany na `Vendor::Nvidia`.
   Po zdjęciu bramki **843,9 tok/s (30,9×)**. **Test regresyjny: prefill musi rosnąć z długością
   promptu** — płaskość jest błędem, nie właściwością.
2. **Klif między szerokościami chunka** — T=16 → 37,25 s, T=32 → 3,84 s (**9,7×**),
   T=128 → 2,57 s, layer-major T=128 → 1,71 s. Chunk jest parametrem kubełkowanym, nie stałą.
3. **Prefill pod podziałem szedł token po tokenie** — 820 tokenów w 17,1 s wobec 1,7 s na jednej
   karcie. Odblokował go **jeden wariant kernela z epilogiem f32**, różniący się typem wskaźnika
   wyjścia i jedną linią zapisu: 17,12 → **1,79 s**.

### 8.3. Bariera, o której trzeba wiedzieć przed próbą int4

Przy skali zmieniającej się co 32 kolumny (Q4_K, Q6_K, NVFP4) akumulator int32 trzeba zrzucić
do f32 **w środku pętli**: dla `iu4` to ~26 cykli na kafel wobec 13,6 cyklu samej instrukcji.
Mimo 4,2× szybszej instrukcji rachunek wychodzi **gorzej niż f16** — `gemm_q4_k_i8wmma` wyszedł
**3,3× wolniej**. FP8 zdejmuje tę ścianę (operand w 2 VGPR zamiast 8, skale stałe wzdłuż K),
dlatego to fp8, a nie int4, jest następnym krokiem na RDNA4.

**Przestroga równoległa:** FP8 **strumieniowo** (przepakowanie NVFP4→e4m3 w locie) dało
**regresję 26%** — każda paczka służy dokładnie jednemu GEMM, więc jej koszt nigdy się nie
amortyzuje. **FP8 wygrywa wyłącznie jako rezydencja.**

### 8.4. Cel

| Platforma | dziś | cel v1 |
|---|--:|--:|
| R9700, 27B Q4_K, p1024 | 1 481 tok/s | **≥ 2 800** (f16), **≥ 4 500** (fp8 jako rezydencja) |
| Apple M4 (10 rdzeni), 7B 4-bit | brak | **≥ 175 tok/s** i **≥ 1,0× mlx-swift** |
| dowolna platforma | — | **prefill rośnie z długością promptu** (test regresyjny) |

---

## 9. Plan faz

Każda faza kończy się czymś **zmierzonym**. Faza bez pomiaru jest niezaliczona. Ścieżka Apple
(NA1–NA3) jest **równoległa** do N1–N6 i zależy tylko od N0 oraz od punktów wpięcia z N3/N5.

### N0 — bramki, zanim cokolwiek ruszymy

1. **Macierz SHA e2e**: `{dense, hybrid, MoE} × {Q4_K_M, NVFP4} × {B=1, B=64} ×
   {spek: off, MTP} × {karty: 1, 2}` = 48 kombinacji × 32 tokeny. Suma SHA identyfikatorów,
   nie „wygląda sensownie". Uzasadnienie: trzy bramki „NVIDIA-only postawione za szeroko
   **w jednym pliku**", koszt 16,4× i 26× — takie rzeczy widać **wyłącznie** w macierzy.
2. **Protokół pomiaru jako kod**: rozgrzewka 300 iteracji, proces ciepły, 5 przebiegów
   z odrzuceniem pierwszego, mediana + IQR, `valid: false` gdy `IQR/mediana > 3%` albo gdy
   zegar spadł w trakcie. `prefix_cache = off` wymuszone. Automatyczne wykrycie sygnatury
   zaniżonego zegara: **decode spadł > 10%, prefill < 3%**.
3. **Regresja tylko gdy** mediana gorsza o > 2% **i** przedziały IQR rozłączne.
4. **Linty w CI**: limit 1500 linii; `std::env::var` tylko w module konfiguracji; zakaz
   placeholderów; `clippy -D warnings`; **warunek na dostawcy > 20 linii wymaga uzasadnienia**;
   **nazwa wariantu niesie geometrię**; `bench` i `serve` na tej samej ścieżce.
5. **Skaner antywzorca „kernel jednotokenowy we wsadzie"** — trzy kryteria, które znalazły
   **cztery** wystąpienia tego samego błędu: siatka `(wiersze/X, n_tokens)`; jeden workgroup
   na wiersz bez kafelkowania; pętla hosta wysyłająca wiele launchy zamiast jednego GEMM.

**Wyjście:** macierz SHA zielona, bramki zapisane w `bench/baselines/`, linty czerwone dla
obecnego stanu z wygasającą listą wyjątków. **Ryzyko: żadne.**

### N1 — rozbicie monolitów bez zmiany zachowania

Podział wg §5.2 i §5.3. **Warunek: identyczna macierz SHA w każdym commicie.**
**Wyjście:** zero plików > 1500 linii, brak regresji.

### N2 — IR grafu i automatyczna fuzja *(D1)*

IR powstaje **równolegle** do istniejącej ścieżki, przełączany jednym polem konfiguracji,
bramką jest identyczne SHA. Po zielonej macierzy stara ścieżka jest **usuwana**, nie zostawiana
jako fallback.

**Wyjście:** ≤ 65 uruchomień/krok (dziś 681); decode Q4_K_M ≥ 33,0 tok/s przy niezmienionym SHA;
fuzje wykonywane przez pass, nie przez człowieka; **[2K]** to samo pod `--tp 2`.
**Ryzyko: największe w planie.** Mitygacja: równoległa ścieżka + macierz SHA + bezwzględny zakaz
„przy okazji poprawiłem też…".

### N3 — punkty wpięcia: rejestr wariantów i źródła kerneli *(D6, D9, część D5)*

1. Rejestr wariantów z golden przed pomiarem i fallbackiem na większy kubełek — **wchodzi
   pierwszy**, działa na istniejącym katalogu i od razu kasuje klify.
2. `QuantDesc` jako dane (§6.3) i parametryzacja zamiast kopiowania (`out_dtype`, `batch`).
3. `KernelSource` (§6.4) — `MojoSource` i `CudaSource` jako pierwsze implementacje.
4. Manifest **generowany** z katalogu; test odmawiający buildu przy różnicy.
5. `kernel_count_bound` i trzy testy rozszerzalności z §6.6.

**Wyjście:** ≤ 150 kerneli/architekturę (dziś 469–535), `no_perf_cliff` zielony, build katalogu
< 15 min, SHA bez zmian.

### NA1 — modele MLX na istniejących backendach *(D8, część B — zaczyna się po N3)*

`QuantDesc` dla `mlx_affine` (2/3/4/5/6/8 bitów, dowolna grupa), czytnik `quantization` z
`config.json`, mapowanie nazw HF w rejestrze architektur, gather-dequant dla skwantyzowanego
embeddingu, głowa skwantyzowana.

**Wyjście:** `Bielik-Minitron-7B-MLX-4bit` generuje poprawny tekst **na R9700 i na NVIDII**;
dequant zgodny z referencją CPU **bit w bit**; tensory porównane z `mlx` numerycznie.
**Zero kodu specyficznego dla Apple w tej fazie.**

**Stan 2026-08-02 — warstwa formatu gotowa i zabramkowana.** `crates/forge-formats/src/mlx.rs`:
parser bloku `quantization` (cztery tryby, `affine` zdekodowany, pozostałe odrzucane
z nazwą trybu w komunikacie), walidacja kształtów przed odczytem, dekoder afiniczny
2/3/4/5/6/8 bitów o dowolnej grupie. Wyrocznią jest **sam MLX 0.31.2**
(`tools/mlx-oracle/gen_fixtures.py`), a nie druga interpretacja wzoru; test
`mlx_affine_golden` sprawdza siedem realnych tensorów Bielika, w tym skwantyzowany
embedding i głowę, dwoma niezależnymi kryteriami:

1. **samo rozpakowanie liczb całkowitych bit w bit** (skala 1, przesunięcie 0) — przypina
   kolejność bitów, czyli jedyną własność formatu, której nie ma w `config.json`;
2. **pełny dequant bit w bit po jednym zaokrągleniu do bf16** — przypina kierunek działania
   przesunięcia. Bez zaokrąglenia zgadza się 89,5% wartości, co jest sygnaturą szerokości
   akumulatora, nie błędu wzoru; po zaokrągleniu **zero rozbieżności**.

**Mapowanie checkpointu — zrobione i zabramkowane.** Rejestr architektur znał już nazwy HF
(`arch/llama.ron`), więc brakowało wyłącznie tego, że w MLX każda skwantyzowana waga to
**trójka** `.weight` / `.scales` / `.biases`. `map_checkpoint` dopasowuje listę tensorów pliku
do opisu **w obie strony** i rozróżnia cztery wyniki: waga skwantyzowana (pełna trójka),
tensor nieskwantyzowany (sama waga), tensor, którego opis nie obejmuje, oraz **niepełna
trójka** — ta ostatnia jest błędem, a nie brakiem, bo dekodowanie wagi bez skal czyta
sąsiednią pamięć.

Bramka na realnym pliku: `Bielik-Minitron-7B-MLX-4bit` rozwiązuje się **927/927 tensorów,
zero reszty w obie strony** (282 wagi skwantyzowane = 40 warstw × 7 projekcji + embedding
i głowa; 81 tensorów nieskwantyzowanych = 40 × 2 normy + norma końcowa). Osobny test usuwa
jeden tensor skal i sprawdza, że zgłasza się to jako niepełna trójka, a nie przechodzi bokiem.

**Luka znaleziona przez tę bramkę:** czytnik safetensors **nie znał typu `U32`**, czyli
dokładnie tego, w którym MLX trzyma upakowane wagi — checkpoint MLX nie dawał się w ogóle
otworzyć. Tabela typów została uzupełniona do kompletu, który definiuje format
(`U16`, `U32`, `U64`, `I16`, `F64`, `BOOL`).

**Granica pokrycia, zapisana testem.** Drugi lokalny model MLX
(`Qwen3-VL-30B-A3B-Instruct-4bit`) jest poprawnie rozpoznawany jako MLX affine, ale zatrzymuje
się na rejestrze: modele wielomodalne trzymają wymiary wieży tekstowej w zagnieżdżonym
`text_config`, którego `HfConfig` nie czyta. To jest praca na danych i konfiguracji, nie
na dekoderze — czyli dokładnie ten punkt wpięcia, o którym mówi §6.2.

**Whisper w MLX — drugi realny checkpoint przez tę samą bramkę.**
`mlx-community/whisper-large-v3-turbo-4bit` (1053 tensory, 463 MB) jest już na dysku
w cache ścieżki MLX drzewa głównego. Przejście przez niego wymusiło dwie poprawki i ujawniło
trzy własności formatu, których model gęsty nie pokazywał:

| własność | Bielik (mlx-lm) | Whisper (mlx-whisper) | konsekwencja |
|---|---|---|---|
| typ skal i przesunięć | **bf16** | **f16** | dekoder nie może zakładać jednego typu — `MlxParams` niesie typ razem z danymi |
| pole `mode` w konfiguracji | obecne | **nieobecne** | wartość domyślna `affine` jest wymagana, nie kosmetyczna |
| nazewnictwo | HF (`model.layers.N.self_attn.q_proj`) | **OpenAI** (`encoder.blocks.N.attn.query`) | `forge-whisper` oczekuje HF, więc tego pliku jeszcze nie wczyta |
| sploty wejściowe | — | **nieskwantyzowane F16** | kwantyzacja MLX obejmuje wyłącznie warstwy liniowe |
| kolizja nazw | brak | **`attn.out.bias` obok `attn.out.biases`** | patrz niżej |

**Pułapka nazewnicza, która żyje w realnym pliku.** Whisper trzyma jednocześnie
`attn.out.bias` (wektor przesunięcia warstwy liniowej, F16 `[1280]`) i `attn.out.biases`
(zera kwantyzacji, F16 `[1280, 20]`) — **272 tensory w liczbie pojedynczej i 233 w mnogiej**.
Różnią się jedną literą, a trafiają w zupełnie różne miejsca obliczenia. Dekoder, który
obcina zły przyrostek, czyta nie ten tensor i nie zgłasza przy tym żadnego błędu. Bramką jest
test na realnym pliku sprawdzający, że `.bias` NIE jest uznawany za składnik trójki.

Bramki dla Whispera: 233 pełne trójki, sploty pozostają pojedyncze i F16, `mode` domyślnie
`affine`, a wektory wzorcowe z `mx.dequantize` przechodzą oba kryteria (rozpakowanie bit
w bit, pełny dequant bit w bit po zaokrągleniu do typu checkpointu) — na sześciu tensorach
obejmujących enkoder, dekoder, cross-attention i embedding tokenów.

**Loader Whispera obsługuje oba warianty — zrobione i sprawdzone na realnym pliku.**
`forge-whisper/src/flavour.rs` opisuje wariant checkpointu **jako dane**: dwie stałe tabele
nazw i układów, wybierane po kluczach `config.json` (`d_model` → HF, `n_audio_state` → OpenAI),
nigdy po nazwie katalogu. Dodanie trzeciej konwersji to nowa stała, nie nowa gałąź w loaderze.

Trzy różnice okazały się semantyczne, nie kosmetyczne — i każda jest cichym złym wynikiem,
jeśli się ją przeoczy:

1. **Osadzenie pozycji enkodera jest w HF przechowywane, a w MLX generowane** (stałe sinusoidy).
   Loader, który tylko go szuka, nic nie znajduje.
2. **Sploty mają inną kolejność osi**: `[out, in, k]` w HF wobec `[out, k, in]` w MLX.
   Przestawiony splot nie daje ani błędu, ani `NaN` — model po prostu przesłyszy się na całej linii.
3. **Wymiary są w innym schemacie**, a identyfikatory tokenów startu i końca w MLX **nie istnieją
   w `config.json`** — jedynym źródłem jest `generation_config.json`. Ich brak jest błędem,
   nie wartością domyślną: zły token startowy daje płynne wyjście w złym języku, a nie awarię.

Bramka: `mlx-community/whisper-large-v3-turbo-4bit` (463 MB, 1053 tensory) **wczytuje się
w całości na backendzie CPU**, więc ścieżka jest sprawdzona bez GPU. Wagi po dekwantyzacji
są porównywane z wartościami z `mx.dequantize` — „model się wczytał" nie jest dowodem, że
wczytał właściwe liczby.

**Luka znaleziona przy okazji:** czytanie tensorów zakładało wyrównanie wskaźnika (`cast_slice`
po zmapowanych bajtach). Safetensors umieszcza dane pod dowolnym offsetem, więc szeroki rzut
**panikuje** zamiast zwrócić błąd — i to na dokładnie tych tensorach, dla których ta ścieżka
istnieje. Odczyt jest teraz elementowy, w loaderze Whispera i w `tensor_f32`.

Zostaje: gather-dequant skwantyzowanego embeddingu w ścieżce LLM, obsługa `text_config`
plus wpis `qwen3_vl_moe`, oraz przebieg end-to-end na karcie — ten ostatni wymaga sprzętu.

### NA2 — backend Metal *(D8, część A)*

**Rozpoczęte 2026-08-02 — warstwa niska działa.** `forge-hal/metal/forge_metal_shim.m`
(shim Objective-C budowany przez `build.rs` pod cechą `metal`, tym samym wzorcem co shim HIP)
plus `forge-hal/src/metal.rs`. Dwie własności API wynikają wprost z EKS-A3, nie z gustu:

- **bufory są `Shared`** — alokacja „urządzenia" JEST pamięcią hosta i oddaje wskaźnik;
  kopiowanie host→device nie istnieje, więc ładowanie modelu nie przenosi gigabajtów bez powodu;
- **command buffer jest obiektem o własnym czasie życia**, a nie szczegółem ukrytym w
  „uruchom kernel". Dyspozycja w otwartym buforze kosztuje 0,61 µs, własny bufor 19,6 µs,
  a powrót na hosta ~94 µs — więc API czyni pakowanie rzeczą domyślną, a powrót na hosta
  osobnym, nazwanym wywołaniem.

Sprawdzone czterema testami na tym M4: realny kernel MSL liczy się przez shim, trzy
dyspozycje dzielą jeden bufor poleceń, błąd kompilacji MSL wraca z komunikatem kompilatora,
a grupa robocza ponad limit kernela jest odrzucana przed dyspozycją.

**Trait `Device` zaimplementowany** (`forge-hal/src/metal_device.rs`) — silnik widzi Metal
przez ten sam kontrakt co CUDA. Trzy punkty kontraktu różnią się świadomie:

- `alloc` zwraca pamięć **widoczną dla hosta**: na pamięci unified nie ma transferu,
  więc `write`/`read` to memcpy pod tym samym adresem, a ładowanie modelu nie przenosi nic;
- strumień trzyma **jeden otwarty command buffer**, do którego dokładają się kolejne
  dyspozycje aż do wymuszenia wysyłki — cała reszta kształtu backendu wynika z tej liczby;
- **przechwytywanie grafu jest odrzucane, nie emulowane**. Nic tu nie kupuje, bo pakowanie
  usuwa dokładnie to, co usunąłby graf, a udawana kompilacja grafu byłaby gorsza od błędu.

`DeviceCaps` mówi to, co **zmierzono**: `fp8_native: false` (Metal 4.1 emuluje, 0,94× f16),
`fp4_native: false`, `bf16_native: true` (kosztuje tyle co f16), `supports_graph_capture: false`,
`sm_count: 0` — Metal nie podaje liczby rdzeni GPU i wpisanie zmyślonej zepsułoby każdą
heurystykę siatki.

Przy okazji naprawiony kontrakt z §7.3 pkt 4: `LaunchArgs` zapisuje teraz **rodzaj każdego
slotu** (`ArgKind`), bo surowy adres wystarcza API przyjmującemu wskaźniki, a nie wystarcza
API wiążącemu bufory po indeksie. Odtwarzanie tego przez porównywanie wartości slotów
z adresami buforów byłoby zgadywaniem. Zmiana jest addytywna — `slots()` i `retained()`
działają jak dawniej, więc CUDA i HIP są nietknięte.

Sprawdzone dziewięcioma testami przez publiczny trait: dyspozycja kernela, wiele dyspozycji
w jednym buforze poleceń, pod-bufor jako okno rodzica, argument z przesunięciem bajtowym,
zdarzenia, oraz to, że ścieżki nieobsługiwane **mówią o tym wprost** zamiast udawać.

Zostaje: `DeviceCaps` dla rodziny GPU jako wpis w rejestrze (dziś `arch` bierze się z nazwy
urządzenia) i rodziny kerneli z §7.5.

`forge-hal/metal`: `Device`, `MTLCommandQueue` jako strumień, `MTLEvent`, bufory bez kopii
(`newBufferWithBytesNoCopy`), `sub_buffer` przez offset. `MslSource` z cache artefaktów.
Osiem rodzin kerneli z §7.5. `DeviceCaps` dla `apple_g9` jako **dane**.

**Wyjście:** EKS-A1..A3 zamknięte i zapisane; model dense 4-bit generuje na M4 tekst
o **sumie SHA identycznej jak na R9700**; `inspect` pokazuje wybrany wariant i jego pochodzenie
dla każdej operacji.

### NA3 — wydajność na Apple i porównanie z mlx-swift

**Jeden command buffer na krok dekodowania i zero oczekiwań hosta w pętli warstw** — to jest
główna dźwignia wg EKS-A3, nie fuzja. Kafle `simdgroup_matrix` 8×8 dobrane autotunerem,
prefill chunkowany, zero kopii przy ładowaniu (pamięć unified).

**Wyjście:** decode **≥ 19 tok/s** (80% zmierzonego sufitu 24,3) i prefill **≥ 175 tok/s**
na tym M4 dla Bielika 7B 4-bit;
`bench compare` wobec `mlx-swift` wykonany protokołem z §7.6 i **opublikowany także tam,
gdzie przegrywamy**.

### N4 — batch jako parametr *(D3 — największa dźwignia)*

`BatchPlan`, kubełkowanie z maskowaniem zamiast osobnych kerneli, scheduler iteracyjny
z chunked prefill w tym samym batchu co decode, admission control z projekcją zapotrzebowania.
Model kosztu **kalibrowany pomiarem przy starcie**, nie zaszyty.

**Kolano skalowania jest realne:** dla hybrydy stan rekurencyjny to 303 MiB ruchu na sekwencję
na krok wobec 16,3 GiB wag czytanych raz na batch → kolano **B≈55 (ctx≈0), B≈39 (2048),
B≈20 (8192)**; optymalna szerokość **maleje** z kontekstem. Dla modeli gęstych batch skaluje się
normalnie i tam obowiązuje cel wobec vLLM.

**Wyjście:** agregat dense przy B=32 **≥ 2 000 tok/s** (dziś 166); **żaden krok w dół > 10%**
dla B ∈ {1..64}; `batched_matches_single_seq` zielony; **[2K]** to samo pod `--tp 2`.

### N5 — HAL: kontrakt zamiast kalki z CUDA *(D2)*

Domyślne implementacje znikają dla wszystkiego, co dotyczy tożsamości i możliwości sprzętu.
Pojemność liczona z **WGP na RDNA** (granulat VGPR 24, LDS 128 KiB/WGP; **nie**
`hipOccupancyMaxActiveBlocksPerMultiprocessor` — zaniża o czynnik 2 przy ograniczeniu LDS).
Argumenty rozróżniają bufor+offset od skalara. Ścieżka grafu jako możliwość, nie założenie.

**Wyjście:** brak metody `Device` z domyślną implementacją udającą sukces; zajętość zgodna
z sondą startową; SHA bez zmian.

### N6 — prefill *(D10)*

fp8 **jako rezydencja** na RDNA4 (nie strumieniowo), `CudaSource` dla int8-MMQ na NVIDII,
chunk jako parametr kubełkowany, test „prefill rośnie z długością promptu".

**Wyjście:** R9700 27B Q4_K p1024 **≥ 2 800 tok/s**; brak klifu między szerokościami chunka;
**[2K]** prefill pod `--tp 2` bez regresji.

### N7 — spekulacja: drzewa i stochastyka

Wspólny interfejs proposera, typowane drzewo, weryfikacja **batchowana** (dziś sloty przeplatane
seryjnie), akceptacja stochastyczna dla `temp > 0`. **Ograniczenie do uszanowania:** K powyżej 3
wychodzi na zero — każdy krok draftu to pełny odczyt głowy (+5,7% ruchu) za ~0,2 tokena (+8%);
głowa jest czytana **cztery razy na cykl MTP** = 22% ruchu. Dźwignią jest drzewo, nie większe K.

**Wyjście:** akceptacja ≥ 2,5 tok/krok przy zachowaniu identyczności z trybem bez spekulacji;
żadne żądanie nie spada **cicho** do zwykłego dekodowania.

### N8 — pamięć, tiering, multi-GPU pod jednym menedżerem

Kontrakt 4 wdrożony: KV, prefix cache, stan hybrydowy, rezydencja ekspertów i offload kontekstu
na wspólnych blokach, jeden arbiter pasma, tier o zerowym koszcie sprowadzenia dla pamięci
unified. `world > 2` w TP. Kolektyw drzewiasty.

**Wyjście:** model przekraczający pamięć działa z **określonym** spadkiem, nie z awarią;
`--tp 4` z poprawnym SHA; nasycenie łącza jako metryka pierwszej klasy.

---

## 10. Zestaw regresyjny — pułapki do zakodowania jako testy

Każda pozycja kosztowała realny czas i każda jest **bezgłośna**.

**Sprzęt i instrukcje**
1. `v_dot4_i32_i8` liczy **bez znaku** — `(-1,-2,-3,-4)·(4,3,2,1)` dawało 2540 zamiast −20.
   Poprawna forma kosztuje (55 → 43 TOPS). Bajty identyczne na gfx1100 i gfx1201 → dotyczy
   **obu** generacji. **Test:** każdy mikrobenchmark instrukcji całkowitoliczbowej ma przypadek
   z wartościami ujemnymi.
2. `v_dot2_f32_f16` na gfx11 niedokładne o 1 ULP tam, gdzie RDNA2 jest dokładne.
3. Sumator WMMA **nie jest zgodny z IEEE-754** — **nie wolno wymagać bit w bit dla GEMM na f32**
   (tolerancja 2 ULP); bit w bit obowiązuje dla dequantu i arytmetyki całkowitej.
4. Zegar pamięci zatrzaskuje się na niskim DPM po błędach GPU. **Na Apple odpowiednik:
   throttling termiczny i praca na baterii.**
5. `HIP_VISIBLE_DEVICES=0` obowiązkowe przy porównaniach z llama.cpp.

**Kernele i launchery**
6. Nazwa wariantu niesie geometrię — inaczej launcher **po cichu pomija połowę wierszy**.
7. Liczba wątków bloku **musi wynikać z wymiarów kafla**. Trzy wpisy „128x64" przekazywały
   512 wątków do kerneli na 256 → **inne tokeny w kolejnych powtórzeniach**.
8. Każdy mierzony wariant ma **kontrolę poprawności**. Kafel 192×128 wychodził „najszybszy",
   bo kernel **wcale nie wnosił wag**.
9. Bufory K i V muszą sięgać końca ostatniego bloku — kernel maskuje wynik, nie ładowanie.
10. Referencja golden musi opisywać **tę samą arytmetykę** co kernel.
11. Testy GPU **nie dzielą zasobów** (40/42 równolegle, 42/42 przy `--test-threads=1`).

**Bramki i ścieżki**
12. **Bramka na dostawcy postawiona za szeroko — trzy razy w jednym pliku**, koszt 16,4× i 26×.
13. Dobór kerneli pod TP **identyczny** jak jednokartowy — inaczej podział bywa *dokładniejszy*
    od odniesienia i wygląda jak błąd (rel 2,8e-1).
14. Ładowanie code objectu **nie unieważnia cache instrukcji** — ma to bezpośrednie znaczenie
    dla cache wariantów, który z założenia podmienia obrazy w pracy.
15. `Drop` musi zatrzymać wątek roboczy — inaczej `double free` z wnętrza runtime'u.

**Formaty i modele**
16. **Kierunek działania skali:** DeepSeek **mnoży**, compressed-tensors **dzieli**, MLX
    **dodaje** bias. Podstawienie daje wagi rzędu 10⁶ zamiast 10⁻², bez ostrzeżenia.
17. **Ten sam model w dwóch kwantyzacjach ma inne tensory w innych formatach** — z tego powodu
    MTP dla Q4_K_M w ogóle nie startowało.
18. **Precyzja jako poprawność:** kompresor w f16 daje `inf`, softmax z `inf − inf` daje `NaN`,
    a kwantyzacja aktywacji zamienia `NaN` w **ciche śmieci**. Objaw: pierwsze trzy tokeny zgodne.

**Pomiar**
19. Każdy pomiar decode czyta **spoza cache**.
20. **ILP jest warunkiem pomiaru rooflinu na AMD i NVIDII** — przy 4 łańcuchach pomiar
    pokazuje **dokładnie połowę** sufitu; wypłaszcza się na ośmiu. **Na Apple reguła nie
    obowiązuje** i działa odwrotnie (EKS-A1, EKS-A2) — wymaganie ILP jest własnością targetu.
20a. **Mikrobenchmark arytmetyki na Apple musi mieć operand zależny od identyfikatora wątku
    i kontrolę skalowania z rozmiarem siatki.** Arytmetyka jednakowa dla wszystkich lane'ów
    jest przenoszona na ścieżkę skalarną: pierwsza wersja EKS-A2 raportowała **409 TFLOPS**
    na dziesięciordzeniowym GPU wobec realnych 3,07, a sygnaturą był czas niezmieniający się
    przy szesnastokrotnym zwiększeniu siatki.
21. `llama-bench tg128` dekoduje z **pustym** kontekstem — nie służy do porównania decode.
22. Ten sam **plik** modelu, ten sam SHA, te same identyfikatory tokenów.

---

## 11. Optymalizacje zmierzone i odrzucone — nie powtarzać

**GEMV / decode:** naiwna wektoryzacja SIMD-load (**regresja**), unroll 2/4, ładowania 16 B,
aktywacja w LDS (**493 → 326 GB/s**), arytmetyczne rozpakowanie e2m1, ciągły przedział grup
na lane (**gorzej**), podniesienie `X_MAX` (29,2 → 28,1), staging aktywacji w kawałkach
(poprawny, daje **nic**), **split-K dla wąskich GEMV** (trzecia odrzucona hipoteza dla tej samej
macierzy), siatka trwała (+0,3%), `ssm_out` wierszowo równoległy (38,6 → **37,0**), podział
kerneli miksera DeltaNet (**38,5 → 32,7**), fuzja normy w GEMV na gfx1030 (**181 vs 466 GB/s**).

**GEMM / prefill:** `.maxnreg` dla occupancy (**regresja** — kernel jest ILP-bound), głęboki
comptime K-unroll (≤ 8%), potokowanie odczytów LDS, **trzynaście** kształtów kafla int8
(wszystkie 34–36 TOPS = sygnatura wysyconego zasobu), replikacja kształtu llama.cpp
(**7% wolniej**), BN=256 (nie startuje), **Marlin W4A16 (no-go)**, `gemm_q4_k_i8wmma`
(**3,3× wolniej**; rachunek mówi, że kolejna próba w tę stronę też wyjdzie źle),
**FP8 strumieniowo (regresja 26%)**, tania dekwantyzacja bitowa w WMMA (**503 → 439 tok/s**),
WMMA dla Q8_0 na RDNA4 (**−10%**).

**Batch / spekulacja:** T=2..16 na kafel `bm32` (**51,8 → 27,6**), B8 do BM32
(**82,13 → 38,66**), głowa logitów FP8 w batchowym decode (**quality-fail** — e4m3 ma 3-bitową
mantysę w warstwie wybierającej token), większy kafel dla głowy F16 (**~6% gorzej**),
grafowanie proposera MTP (akceptacja **74,2% → 40–54%**), carry MTP w F32 (**−5,8%**),
`FORGE_MTP_DRAFT_HEAD=nvfp4` na 7900 XT (**53,7 → 42,1**, a na 4090 ten sam wariant był wygraną
1,84× — **wynik zależy od karty**).

**Jakość:** **W4A8 (QServe)** — 2,2× TOPS, **bełkot na wszystkich promptach**; SmoothQuant
zmierzony jako **regresujący**. Fuzja SwiGLU→q8_1 — token-identyczna i 2% szybsza, ale psuje
bit-identyczność → cofnięta. **GigaToken** — udział tokenizacji w TTFT to 0,90% na NVIDII
i 0,09% na AMD. Przekwantyzowanie DeepSeeka na Q8_0 kosztowało **11 000×** więcej błędu przy
tym samym bajcie na wagę.

**Apple (zmierzone tutaj):** więcej akumulatorów w pętli — **szkodzi** (FMA 3,07 → 2,55 przy
2 → 16 akumulatorów; kernel strumieniowy 103,3 → 81,1 GB/s przy 1 → 8). Fuzja kerneli jako
sposób na narzut dyspozycji — **0,7% kroku**, nie warta ryzyka (EKS-A3).

**Apple (z pomiaru zewnętrznego):** **fp8 (E4M3) w Metal 4.1 jest emulowany** —
0,94× fp16 mimo połowy bajtów operandu. Optymalizacja, która na RDNA4 daje 2,1×, na M4 daje
**stratę**. `mpp::tensor_ops` na M4 Max bije `simdgroup_matrix` tylko 1,05–1,21× — nie warto
przepisywać przed wsparciem M5.

**Zasada z tego wszystkiego:** jeśli hipoteza brzmi „zróbmy szerszy/większy/więcej naraz",
prawdopodobieństwo, że jest już zmierzona i odrzucona, jest wysokie. Sprawdź tę listę przed
napisaniem kodu.

---

## 12. Czego nie przenosić z TentaForge

1. **Własny enkoder ISA i praca przez `/dev/kfd` bez ROCm** — sensowne przy celu „wyłącznie AMD,
   jedna binarka bez zależności". FORGE jest wieloplatformowy; to byłby nowy projekt.
2. **Megakernel (Teza 1)** — dla ścieżki zfuzowanej do ~65 etapów daje **0,7%**.
3. **Cel „≤ 4 uruchomienia na krok"** — wynika z megakernela; dla FORGE właściwe jest **≤ 65**.
4. **Bezwarunkowe „nie ma kernela GEMV"** — obalone pomiarem przez sam TentaForge.
5. **Liczby wyprowadzone, a nie zmierzone** — bierzemy wyłącznie pozycje oznaczone jako pomiar.
   *Ta sama reguła obowiązuje wobec liczb Apple w §7.7, dopóki EKS-A1..A3 nie zostaną zamknięte.*

---

## 13. Pierwszy tydzień

1. **Macierz SHA e2e** (48 kombinacji) jednym poleceniem, wynik zapisany jako baza.
   Bez tego nie wolno dotknąć ani jednej linii.
2. **Protokół pomiaru jako kod** + kryterium regresji „2% i rozłączne IQR" + zamrożenie
   obecnych wyników.
3. **Linty w CI z listą wyjątków** obejmującą dzisiejszy stan; lista ma tylko maleć.
4. **Skaner antywzorca** przepuszczony przez cały katalog — znalazł już cztery wystąpienia
   tego samego błędu, piąte pewnie tam jest.
5. **Inwentarz 83 zmiennych** → `forge.toml` ze schematem.
6. **EKS-A1 i EKS-A3 na tym M4** — przepustowość pamięci i koszt dyspozycji.
   **ZROBIONE 2026-08-02**, raport `pomiary/eks-a1-a3-apple-m4.md`. Odpowiedź na pytanie
   „czy fuzja jest na Apple dźwignią" brzmi **nie** — i dobrze, że nie przeniesiono tu
   wniosku z AMD.
7. **EKS-A2** — przepustowość `simdgroup_matrix`. **ZROBIONE 2026-08-02**: 3,94 TFLOPS,
   przewaga nad zwykłym FMA tylko **1,28×**. Prefill na Apple jest ALU-bound, więc
   kwantyzacja daje tam wyłącznie oszczędność pamięci.

Dopiero potem N1.

### Stan realizacji

| punkt | stan | artefakt |
|---|---|---|
| 1. Macierz SHA e2e | **do zrobienia** — wymaga maszyny z działającym backendem | — |
| 2. Protokół pomiaru jako kod | **częściowo** — zaimplementowany w harnessie EKS-A | `tools/eks-apple/` |
| 3. Linty w CI z listą wyjątków | **zrobione** | `cargo xtask lint`, `xtask/baseline.tsv` (46 wpisów) |
| 4. Skaner antywzorca | **zrobione** — 41 trafień w 15 plikach, w tym jedno w `launchers.rs` | reguła `batch_antipattern` |
| 5. Inwentarz zmiennych | **zrobione** — 83 zmienne: 42 przełączniki ścieżki, 25 oprzyrządowania, 16 testów | `INWENTARZ_ENV.md` |
| 6. EKS-A1 / EKS-A3 | **zrobione** | `pomiary/eks-a1-a3-apple-m4.md` |
| 7. EKS-A2 | **zrobione** | `pomiary/eks-a2-simdgroup-matrix-m4.md` |

---

## 14. Cele mierzalne — zbiorczo

| Metryka | Dziś | Cel | Faza |
|---|--:|--:|---|
| Pliki > 1500 linii | 11 | **0** | N1 |
| Zmienne środowiskowe przełączające ścieżkę | 42 | **0** | N1 |
| Uruchomienia kerneli / krok decode | 681 | **≤ 65** | N2 |
| Decode B=1, 27B Q4_K_M, R9700 | 31,0 tok/s | **≥ 33,0** | N2 |
| Kernele w katalogu / architekturę | 469–535 | **≤ 150** | N3 |
| Czas budowy katalogu | ~55 min | **< 15 min** | N3 |
| Dodanie architektury modelu | zmiana kodu | **wpis w danych** (test w runtime) | N3 |
| Dodanie karty | procedura ręczna | **wpis w rejestrze możliwości** | N3/N5 |
| **Modele MLX na AMD/NVIDII** | nie działają | **poprawny tekst, SHA = Apple** | NA1 |
| **FORGE na Apple M4** | nie startuje | **SHA identyczne jak na R9700** | NA2 |
| **Decode Bielik 7B 4-bit, M4** | brak | **≥ 19 tok/s** (80% ze zmierzonych 24,3) | NA3 |
| **Prefill Bielik 7B 4-bit, M4** | brak | **≥ 175 tok/s** i **≥ 1,0× mlx-swift** | NA3 |
| Agregat dense, B=32 | 166 tok/s | **≥ 2 000** | N4 |
| Klify przy niedopasowanej szerokości | do 1,9× w dół | **żaden krok > 10%** | N4 |
| Prefill p1024, 27B Q4_K_M, R9700 | 1 481 tok/s | **≥ 2 800** (f16) | N6 |
| Prefill rośnie z długością promptu | — | **test regresyjny** | N6 |
| Akceptacja spekulacji | 1,87–2,17 tok/krok | **≥ 2,5** | N7 |
| Skalowanie 2 karty, B=1 | 1,46× | **≥ 1,75×** | N2/N8 |

Każdy wiersz mierzony tym samym narzędziem, tym samym protokołem, z zapisem do repozytorium
wyników. **Cel bez pomiaru jest nieosiągnięty.**
