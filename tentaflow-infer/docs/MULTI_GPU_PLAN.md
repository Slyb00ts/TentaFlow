# Dwie różne karty jako jeden silnik — plan

Cel postawiony wprost: **nie chodzi o to, żeby działało jak najwolniejsza karta,
tylko żeby sumowało ich moc.** Ten dokument jest planem wykonawczym opartym na
pomiarach z tej maszyny, a nie na ogólnych zaleceniach.

## 1. Zmierzone fakty, na których stoi cały projekt

| | RX 6900 XT (gfx1030) | RX 7900 XT (gfx1100) | stosunek |
|---|--:|--:|--:|
| VRAM | 16 368 MiB | 20 464 MiB | 1 : 1,25 |
| odczyt DRAM (ciągły) | **336 GB/s** | **735 GB/s** | 1 : 2,19 |
| int8 `dot4` | **97 TOPS** | 43 TOPS | **2,26 : 1** |
| WMMA int8 / f16 | brak jednostki | 98 TOPS / 102 TFLOPS | — |

Połączenie (zmierzone `hipMemcpyPeer`, obie strony):

| pomiar | wartość |
|---|--:|
| P2P dostępne | TAK, dwukierunkowo |
| pasmo strumieniowe | 14,2 GB/s |
| kopia 10 KiB w strumieniu | **6,45 us** |
| wymiana 10 KiB + synchronizacja hosta obu strumieni | **35,2 us** |

### Wniosek, który przesądza o architekturze

**Stosunek mocy tych kart ZALEŻY OD RODZAJU PRACY i raz jest odwrotny.**
W dekodowaniu (ograniczonym pamięcią) 7900 XT jest 2,19x szybsza. W prefillu
liczonym na instrukcjach dot 6900 XT jest **2,26x szybsza** — RDNA3 zdegradowała
`dot4`. Dopiero WMMA odwraca to z powrotem na korzyść 7900 XT.

Dlatego **jeden statyczny podział jest z definicji zły**. Podział musi być
osobny dla prefillu i dla dekodowania, wyliczany z POMIARU, nie z nazwy karty.

## 2. Co która technika daje i czego nie daje

| | co dzieli | ruch na warstwę | zysk przy jednym strumieniu |
|---|---|---|---|
| **TP** (tensor parallel) | każdą macierz wag po wierszach | 2 wymiany hidden (10 KiB) | **TAK** — obie karty liczą tę samą warstwę |
| **PP** (pipeline parallel) | warstwy między karty | 1 wymiana hidden na granicę | **NIE** — token i tak przechodzi obie karty po kolei |
| **EP** (expert parallel) | ekspertów MoE | tokeny skierowane do eksperta | tak, gdy eksperci są rozrzuceni |

To jest kluczowe rozróżnienie i łatwo je przeoczyć: **pipeline parallel NIE
przyspiesza pojedynczego strumienia.** Sumę mocy przy jednym żądaniu daje
wyłącznie TP (albo EP w modelu MoE). PP daje pojemność (36 GB zamiast 20) i
przepustowość przy wielu równoległych żądaniach.

Skoro cel brzmi „nie jak najwolniejsza, tylko suma", to **TP jest techniką
pierwszą**, a PP dokładamy dla modeli, które nie mieszczą się na jednej karcie.

### Budżet komunikacji TP — czy to się w ogóle spina

27B, dekodowanie: 65 warstw x 2 punkty wymiany = 130 na token.

- naiwnie (synchronizacja hosta): 130 x 35,2 us = **4,6 ms** wobec ~15 ms liczenia → 30% narzutu, **nie do przyjęcia**
- na zdarzeniach urządzenia (bez powrotu do hosta): 130 x 6,45 us = **0,84 ms** → **5,4%**, akceptowalne

**Warunek konieczny TP: synchronizacja przez zdarzenia HIP między strumieniami
urządzeń, nigdy przez `hipStreamSynchronize` na hoście.** Bez tego cały zysk
znika w narzucie.

### Ile TP realnie da

Dekodowanie 27B jest ograniczone pamięcią i mierzy dziś 36,5 tok/s na 7900 XT
(95% jej sufitu). Przy podziale proporcjonalnym do pasma:

- suma pasm 336 + 735 = **1071 GB/s**
- czas = 19,55 GB / 1,071 = 18,3 ms + 0,84 ms komunikacji = 19,1 ms
- **~52 tok/s, czyli 1,43x wobec samej 7900 XT i 3,2x wobec samej 6900 XT**

To jest dokładnie „średnia ich wydajności" z pytania — a ściślej SUMA, bo praca
dzieli się proporcjonalnie do możliwości.

## 3. Serce projektu: model możliwości i zamknięta pętla

Podział NIE jest stałą w konfiguracji. Każde urządzenie ma profil:

```
struct DeviceCapability {
    stream_bytes_per_s: f64,   // zmierzone pasmo odczytu (decode)
    matmul_ops_per_s: f64,     // zmierzona przepustowość GEMM (prefill)
    free_bytes: usize,         // ile wag się zmieści
}
```

Dwa źródła:
1. **Kalibracja przy starcie** — krótki mikrobenchmark na każdej karcie
   (odczyt strumieniowy + jeden GEMM w formacie modelu). Ułamek sekundy.
2. **Korekta z obserwacji** — po każdym kroku znamy rzeczywisty czas etapu na
   każdej karcie. Jeśli karta A kończy wcześniej i czeka, jej udział rośnie.
   Wygładzanie wykładnicze, limit zmiany na krok, żeby nie oscylowało.

Podział wierszy `rows_i = round(rows * w_i / suma(w))`, gdzie `w_i` to
przepustowość właściwa dla RODZAJU pracy (pasmo dla decode, ops dla prefillu).
Reszta z zaokrąglenia idzie do karty o największym `w_i`.

**Ograniczenie pamięciowe:** udział karty nie może przekroczyć jej wolnego VRAM.
Przy 16 GB i 20 GB oraz podziale 31/69 model 27B zajmuje 5,7 GB i 12,5 GB —
mieści się z zapasem.

## 4. Kolejność wykonania

| krok | zawartość | dlaczego w tej kolejności |
|---|---|---|
| **M1** | HAL wielourządzeniowy: otwarcie N kart, włączenie P2P, kopia między urządzeniami, zdarzenia międzystrumieniowe | bez tego nie da się zrobić niczego |
| **M2** | `DeviceCapability` + planer podziału + kalibracja + pętla korekty, z testami | to jest odpowiedź na „nie jak najwolniejsza" |
| **M3** | TP dla dekodowania: podział kolumnowy qkv/gate/up, wierszowy o/down, redukcja na zdarzeniach | pierwszy realny zysk przy jednym strumieniu |
| **M4** | TP dla prefillu z WŁASNYM podziałem (inny stosunek mocy!) | prefill ma odwrotny stosunek kart |
| **M5** | PP dla modeli ponad 20 GB + mikrobatching | pojemność i przepustowość |
| **M6** | EP dla MoE na bazie istniejącej rezydencji ekspertów | MoE ma już warstwę migracji ekspertów |

## 5. Ryzyka nazwane wprost

- **Dwie architektury w jednym procesie.** Rejestr ma już wkompilowane oba
  zestawy artefaktów (`EMBEDDED_GFX1030` i `EMBEDDED_GFX1100`), a `Kernels::load`
  wybiera po architekturze urządzenia — więc to powinno działać, ale wymaga
  sprawdzenia, bo dotąd nikt nie tworzył dwóch `Kernels` w jednym procesie.
- **Kernele różnią się między kartami.** 6900 XT nie ma WMMA, więc ta sama
  warstwa liczy się tam innym kernelem. Wyniki muszą pozostać zgodne — bramką
  jest suma SHA tokenów, tak jak dotąd.
- **DeltaNet ma stan rekurencyjny.** Podział TP warstwy DeltaNet wymaga albo
  replikacji stanu, albo podziału po głowicach. Podział po głowicach jest
  naturalny (48 głowic), ale trzeba sprawdzić, czy skan się na to zgadza.
- **Kolejność redukcji zmienia bity.** Suma częściowych wyników z dwóch kart w
  innej kolejności niż dziś da inne ostatnie bity. Trzeba świadomie wybrać
  deterministyczną kolejność (zawsze urządzenie 0, potem 1) i udokumentować, że
  wynik TP nie musi być bitowo równy jednokartowemu.
