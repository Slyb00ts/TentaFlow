# Natywne MTP NVFP4 dla Qwen3.5/3.6

Raport stabilnego etapu natywnego MTP/NextN w FORGE dla gęstego hybrydowego
GGUF `qwen35`. Nazwa checkpointu używa Qwen3.6, natomiast identyfikator
architektury zapisany w GGUF to `qwen35`.

## Środowisko i model

- GPU: NVIDIA GeForce RTX 4090.
- Backend zweryfikowany wykonawczo: CUDA.
- Model: `protoLabsAI/ThinkingCap-Qwen3.6-27B-MTP-GGUF`, plik
  `ThinkingCap-Qwen3.6-27B-NVFP4-MTP.gguf`.
- Układ: 64 warstwy targetu, w tym 48 DeltaNet i 16 pełnej atencji, oraz jeden
  blok `nextn_predict_layers` używany przez MTP.
- Kwantyzacja: GGUF NVFP4 dla głównych projekcji oraz Q8_0/F32 dla tensorów,
  które checkpoint przechowuje w tych formatach.
- Tryb: pojedynczy strumień, greedy, bez cache prefiksu, `max_active=1`.

AMD/ROCm i Metal nie były uruchamiane. Kernele mają źródła Mojo i zachowują
podział odpowiedni do przyszłego codegenu AMDGPU/Metal, ale nie jest to dowód
zgodności ani wydajności na tych backendach.

## Zakres implementacji

Loader oddziela warstwę NextN od autoregresyjnego trunku, zachowuje natywny
układ NVFP4 i współdzieli embedding oraz głowę wyjściową targetu, gdy GGUF nie
zawiera ich dedykowanych odpowiedników. MTP generuje draft K=2 albo K=3 na GPU.
Target weryfikuje draft blokowo, wykonuje batched argmax i zatwierdza KV oraz
stan hybrydowego DeltaNet.

Końcowy wariant retained przechowuje checkpoint stanu dla każdej warstwy
DeltaNet podczas pierwszego skanu. Commit wybiera już obliczony stan odpowiadający
zaakceptowanemu prefiksowi, zamiast uruchamiać drugi skan 48 warstw. Sterowanie
cyklem i wybór długości pozostają na CPU; obliczenia modelu i sampling greedy są
na GPU.

`--speculative mtp` ustawia maksymalny budżet K=3 i adaptacyjnie porównuje tempo
K=2 oraz K=3. Dostępne są też jawne `mtp:2` i `mtp:3`. Każda próba benchmarku
porównuje pełną sekwencję tokenów z sekwencyjnym greedy i przerywa się przy
różnicy.

## Wyniki retained

| Silnik i tryb | raw128 | raw512 |
|---|---:|---:|
| llama.cpp, ten sam lokalny GGUF | 110,2 tok/s | 100,5 tok/s |
| FORGE, MTP K=3 | 59,8 tok/s | 57,7 tok/s |
| FORGE, MTP adaptacyjne K=2/K=3 | 58,4 tok/s | 56,8 tok/s |

`raw128` i `raw512` oznaczają długość surowego promptu użytego w porównaniu.
Liczby FORGE są wynikami po włączeniu retained checkpointów. Nie należy ich
łączyć ze starszymi pomiarami sprzed tej zmiany.

FORGE osiąga około 54% wyniku llama.cpp dla raw128 i około 57% dla raw512.
Retained commit usuwa zbędne obliczenia, ale nie domyka luki: głównym kosztem
pozostaje duża liczba uruchomień małych kerneli i kopii D2D w przygotowaniu
DeltaNet oraz niegrafowana weryfikacja T=3/T=4.

## vLLM 0.25.1

Nie ma liczby vLLM dla tego samego artefaktu. Próba uruchomienia vLLM 0.25.1 na
lokalnym jednoplikowym GGUF nie przeszła inicjalizacji modelu: loader potraktował
ścieżkę jako źródło wymagające konfiguracji Hugging Face i tokenizera, zamiast
przyjąć GGUF jako kompletny lokalny checkpoint. W efekcie zakończył pracę na
etapie rozwiązywania konfiguracji, przed załadowaniem wag i benchmarkiem.

Nie zastąpiono tego modelu checkpointem safetensors ani inną kwantyzacją, ponieważ
nie byłoby to porównanie tych samych wag i tego samego MTP. Brak wyniku vLLM
oznacza brak obsługi badanego lokalnego artefaktu w tym protokole, a nie wynik
wydajności równy zero.

## Ograniczenia

- Tylko greedy-exact: `temperature=0`, sampling GPU i brak repetition penalty.
- `max_active=1`, ponieważ stan SSM jest obecnie własnością modelu, a nie
  niezależnej sekwencji.
- Budżet natywnego MTP wynosi wyłącznie K=2 lub K=3.
- CUDA jest jedynym backendem sprawdzonym wykonawczo dla tego etapu.
- EAGLE, DFlash, DSpark, draft-model, n-gram jako rozszerzenie natywnego MTP,
  tree-attention i akceptacja stochastyczna pozostają poza tym etapem.

## Następny próg wydajności

Priorytetem jest połączenie przygotowania DeltaNet dla T=3/T=4: conv+SiLU,
podział Q/K/V, normalizacja, logiczne powtórzenie głów oraz log-decay/beta bez
pośrednich kopii D2D. Następnie stabilne grafy CUDA dla pełnego cyklu MTP i
weryfikacji. Ostatni krok proposera, który jedynie materializuje KV i hidden,
pomija już głowę logits.
Każda zmiana musi zachować porównanie token po tokenie z sekwencyjnym greedy.
