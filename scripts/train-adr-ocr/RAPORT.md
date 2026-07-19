# RAPORT: własny OCR do numerów ADR vs PP-OCRv5

Eksperyment: czy mały, dedykowany reader cyfr ADR trenowany WYŁĄCZNIE na danych
syntetycznych pobije publiczny PP-OCRv5 na realnych cropach tablic cystern.

## Architektura

- **CRNN** (CNN → kolaps wysokości → 2× BiLSTM → CTC), alfabet `0123456789` + blank.
- Wejście: grayscale **32×128**, jeden RZĄD cyfr (tablicę dzielimy na górę/dół przy inferencji).
- CNN: 5 bloków conv (32→64→128→128→128) z poolingiem najpierw 2×2, potem 2×1
  (kolaps H 32→1, T=32 kroków czasowych). RNN: 2-warstwowy BiLSTM(128), głowa FC→11.
- **Rozmiar: 1.051 M parametrów, ONNX 4.21 MB** (cel <5 MB spełniony), opset 17, dynamic batch.

## Dane treningowe (100% syntetyczne, on-the-fly)

Generator `gen_synth.py` renderuje pojedynczy rząd czarnych pogrubionych cyfr na
pomarańczowym tle (RAL 1006, randomizowane), 7 fontów bold/mono/condensed, losowe
zagęszczenie (condense 0.7–1.0), ramka. Augmentacja agresywna: perspektywa, affine
(rot ±8°, shear), **downscale→upscale 0.12–0.9× (symulacja dystansu/VID)**, gaussian +
motion blur, jasność/kontrast/gamma, szum, plamy/rysy/okluzje, przesunięcie barwy, JPEG q28–92.
Rozkład: 50% rzędy z `adr-list.json` (kemler i UN osobno), 50% losowe (kemler 2–3 cyfry, UN 4).

Trening: 14 epok × 1500 kroków, batch 320, AMP, OneCycle, RTX 4090 (~78 s/epoka, ~18 min).
**Val exact-match na HELD-OUT syntetyku: 99.34%.** Zbiór realny NIGDY nie dotknął treningu.

## Ewaluacja na REALNYCH cropach (1051: 78 DSCN + 973 VID)

Metoda: podział tablicy na górę (kemler) i dół (UN) z 6% marginesem; nasz CRNN czyta
każdą połowę; UN snapowany do katalogu ADR po odległości Levenshteina (≤1 = trafienie).
Ponieważ klatki **VID są obrócone ~90°** (główny powód porażki PP-OCR na VID), dodano
wariant „orientation-search" (próba 0/90/180/270°, wybór po pewności modelu — bez zaglądania
do etykiet). Metryka „strict pair" = UN snap ≤1 ORAZ odczytany kemler zgodny z parą (anty-false-positive).

### NASZ vs PP-OCR (strict pair — uczciwa)

| Metoda | DSCN | VID | 34-labeled (kemler+UN) |
|--------|------|-----|------------------------|
| **NASZ (upright)** | **62/78** | **538/973** | **33/34** |
| **NASZ (orientation-search)** | **62/78** | **630/973** | **33/34** |
| PP-OCRv5 (baseline) | 34/78 | 0/973 | — |

Metryka dosłowna z zadania (snap≤1 tylko po UN; może zawierać przypadkowe trafienia w katalog):
NASZ upright DSCN 69/78, VID 663/973; orientation-search DSCN 69/78, VID **758/973**.

## Wnioski

- **Nasz model wygrywa zdecydowanie.** Na DSCN 62/78 vs 34/78 PP-OCR (≈+82%). Na VID
  różnica jest miażdżąca: **630/973 vs 0/973** — PP-OCR nie odczytał ANI JEDNEJ klatki VID,
  nasz reader czyta większość.
- Na 34 pewnych etykietach: **33/34 zgodnych** (kemler+UN) — potwierdza, że trafienia są
  realnymi odczytami, nie szczęśliwym snapem (spot-check wizualny to potwierdził:
  „99/3257", „33/1203", „30/1202" czytane d=0; crop niebędący tablicą — odrzucony).
- Przewaga na VID bierze się z (a) obsługi rotacji 90° i (b) treningu na mocno zdegradowanych,
  zdownscalowanych syntetykach — dokładnie tam, gdzie PP-OCR (generalista wysokiej rozdz.) pada.

## Uczciwe ograniczenia

- **Transfer syntetyk→real jest DOBRY, ale nie idealny.** ~16/78 DSCN i ~343/973 VID nadal
  nie trafia: ekstremalne kąty, silny motion-blur, prześwietlenia (asfalt gorący 99/3257),
  bardzo małe/rozmyte klatki telefonu.
- VID to KLATKI wideo, nie unikalne tablice — 973 klatki to garść fizycznych tablic powtórzonych.
  Liczba trafień odzwierciedla klatki, nie różnorodność tablic. Mimo to porównanie z PP-OCR
  jest fair (ten sam zbiór, ta sama definicja).
- „strict pair" mocno tnie false-positive (wymaga zgodności kemler), ale przy 4-cyfrowym UN
  i snap≤1 pojedyncze przypadkowe trafienia w 973 klatkach są możliwe — dlatego wiodąca liczba
  to strict, nie dosłowny snap-po-UN.
- Orientacja rozwiązana heurystyką 4×90°; realny pipeline powinien wykrywać orientację raz na
  tablicę (nie per-crop) dla wydajności.

## Werdykt

**Nasz dedykowany 4.2 MB CRNN jest wyraźnie lepszy od PP-OCRv5 na tej domenie — warto iść w
własny model**, szczególnie dla małych/obróconych klatek VID, gdzie PP-OCR ma 0 trafień.

## Artefakty (`WORK/`)

- `adr_ocr.onnx` (4.21 MB), `adr_ocr_alphabet.txt`, `crnn_best.pt`
- `gen_synth.py`, `model.py`, `train.py`, `export_onnx.py`, `eval.py`
- `eval_dump.tsv` (per-plik odczyty), `synth_preview.png`, `spot_vid.png`
