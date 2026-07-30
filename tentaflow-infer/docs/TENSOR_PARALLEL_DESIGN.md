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
