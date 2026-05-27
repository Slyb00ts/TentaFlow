# Snapshot okna parowania Mesh

Data zapisu: 2026-05-27.

Ten katalog jest celowo tylko notatka/snapshotem. Nie wykonano `git checkout`,
`git revert` ani automatycznego cofania plikow. Aktualny UI zostal zostawiony bez
zmian.

## Pliki zrodlowe

- Aktualny modal: `tentaflow-core/www/js/modules/mesh.js`
- Style modala: `tentaflow-core/www/css/style.css`
- Teksty PL/EN: `tentaflow-core/www/i18n/pl.json`, `tentaflow-core/www/i18n/en.json`

## Wersje

- Aktualna wersja robocza z dnia 2026-05-27:
  - `openPairModal()` w `tentaflow-core/www/js/modules/mesh.js`
  - `wireUpPairTabs()` w `tentaflow-core/www/js/modules/mesh.js`
  - Obecny stan ma zakladke QR z akcja `Odśwież PIN` oraz zakladke ID z polami
    `Node ID` + `PIN`. W kodzie nadal istnieje handler `#pair-scan-btn`, ale
    aktualny HTML modala nie renderuje tego przycisku.

- Wersja sprzed podejrzanego commita:
  - podejrzany commit: `edee1aae202cbf3e6f80d5e230b33ea86bdbaebf`
  - rodzic commita: `c0faa952f0942feec586584b2fba1b5876c17a6e`
  - opis commita: `fix(mesh-pair): drop Host/Port/Relay inputs, dynamic submit label`
  - w tej wersji zakladka ID miala `Node ID`, `PIN`, `Host`, `Port`, `Relay URL`
    oraz przycisk `Zeskanuj kamera`; zakladka QR nie miala dynamicznego labela
    submitu `Odśwież PIN`.

## Co zmienil podejrzany commit

Commit `edee1aae`:

1. Usunal z zakladki `Wpisz ID` pola `Host`, `Port`, `Relay URL`.
2. Usunal z HTML przycisk `pair-scan-btn`, zostawiajac pozniejszy kod obslugi
   skanowania w `wireUpPairTabs()`.
3. Zmienil zachowanie submitu na zakladce QR:
   - przed: przycisk `Paruj` tylko zamykal modal, bo QR byl trybem
     udostepniania danych dla drugiego noda;
   - po: przycisk zmienia label na `Odśwież PIN` i regeneruje invite.
4. Usunal automatyczne wypelnianie `relay` i `host` do widocznych pol, bo te pola
   przestaly istniec w HTML.

## Reczny punkt odtworzenia

Jesli trzeba przywrocic zachowanie sprzed `edee1aae`, nie robic revertu calego
commita. Recznie przeniesc tylko fragmenty z:

```bash
git show c0faa952f0942feec586584b2fba1b5876c17a6e:tentaflow-core/www/js/modules/mesh.js
```

Zakres do porownania:

- `openPairModal()`
- `wireUpPairTabs()`
- `parseManualPairTarget()`
- `buildManualPairAddress()`
- `uniqueStrings()`

Najwazniejsze roznice do recznego przeniesienia:

```diff
Zakladka ID:
- obecnie: Node ID + PIN
- przed edee1aae: Node ID + PIN + Host + Port + Relay URL + pair-scan-btn

Zakladka QR submit:
- obecnie: refresh invite i return false
- przed edee1aae: return true

wireUpPairTabs:
- obecnie: dynamiczny label submitu QR/ID
- przed edee1aae: brak dynamicznego labela

paste tentaflow-pair://:
- obecnie: wypelnia tylko Node ID i PIN
- przed edee1aae: wypelnia Node ID, PIN, Relay URL i Host
```

## Proponowany docelowy kierunek

Nie cofaj wprost calego starego UX. Lepszy docelowy podzial:

1. Zakladka `Udostepnij`:
   - pokazuje QR, Node ID, PIN, timer i kopiowanie;
   - footer: `Odśwież PIN` albo brak glownego przycisku `Paruj`.

2. Zakladka `Polacz`:
   - ma skaner kamery;
   - ma pole na `tentaflow-pair://...` albo Node ID;
   - ma PIN;
   - ewentualnie ukrywane/zaawansowane pola Host/Port/Relay;
   - footer: `Polacz`.

To rozdziela role: pierwszy tab wystawia dane tego noda, drugi tab laczy z
innym nodem.
