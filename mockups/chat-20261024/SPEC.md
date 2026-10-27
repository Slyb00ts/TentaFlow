# Chat — mockupy (2026-10-24) — kontrakt budowy

Nowy ekran **Chat** z trybami **Chat / Audio** (moduł `tentaflow-core/www/js/modules/chat.js` +
`chat-audio.js`). Kontrakt wizualny jest ten sam co `mockups/tentaquant-20260903/`:
`shared/styles.css` to dosłowna kopia jego tokenów i shella (sidebar, `.btn`, `.chip`, `.segmented`,
`.search-box`, `.screen-header`), a klasy `tf-*`-ekwiwalenty (`.btn`, `.chip`, `.segmented`,
`.search-box`, `.window`) pochodzą stamtąd. Nie było samodzielnego mockupu chatu — czat istniał tylko jako
fragment `projekty-20260723/c01-chat.html`, `crm-v1/c11-ai-chat.html` i sekcji w `component-catalog`;
żaden z nich nie został ruszony.

## Pliki

```
mockups/chat-20261024/
  SPEC.md                    ← ten plik
  index.html                 ← spis ekranów
  shared/styles.css          ← kopia tentaquant-20260903/shared/styles.css (tokeny + shell)
  shared/chat.css            ← style ekranu: pasek górny, metryki, wiadomości, kompozytor, scena audio, transkrypt, breakpointy
  shared/chat.js             ← rozmiar ramki, szuflady, sparkline + metryki sesji, demo streamingu
  shared/audio.js            ← scenariusz Audio: stany słucha / myśli / mówi, transkrypt, sterowanie twarzą
  shared/sprite.js           ← sprite ikon (wstrzykiwany po <body>, zamiast wklejania w każdy plik)
  shared/tf-face.bundle.js   ← prawdziwy komponent tf-face jako skrypt klasyczny (działa z file://)
  shared/build-face-bundle.py← odtwarza bundle z tentaflow-core/www/js (uruchamiać z korzenia repo)
  ch01-pusty.html            ← Chat: pusta rozmowa
  ch02-odpowiedz.html        ← Chat: odpowiedź strumieniowana + metryki sesji
  ch03-audio.html            ← Audio desktop (słucha / myśli / mówi; ?view=phone = CH04)
  ch04-audio-mobile.html     ← Audio telefon (ramka 390 × 844, tylko twarz)
```

## Decyzje

1. **Pasek górny (`.chat-top`)**: `[Decode · ta rozmowa] [TTFT] ⟶ [Chat | Audio]` — dokładnie trzy pola, nic więcej. Pasek nie zawiera eksportu ani usuwania; nie pokazuje nazwy modelu.
   Wzorzec „livebar” z TentaForge (komórki z etykietą nad wartością, hero z gradientem i sparklinem), ale tylko trzy komórki.
2. **Decode jest metryką SESJI**: wartość bieżąca (okno ok. 1 s), średnia sesji, n próbek, sparkline z przerywaną linią średniej.
   Tooltip (hover / focus / dotyk) mówi wprost, że to nie jest przepustowość modelu ani suma ruchu innych użytkowników, i podaje
   teraz / średnia / min–max / liczbę próbek. Pusta rozmowa: „—”.
   Status: `Gotowy` / `Generuje` / `Słucha` / `Myśli` / `Mówi`.
3. **Przełącznik Chat / Audio** to `.segmented` jak w starym chacie (Tekst/Audio); w mockupie linki między plikami, rozmiar ramki
   zachowuje parametr `?view=`. Na wąskich kontenerach zostają same ikony.
4. **Audio**: `tf-face` w trybie `fullscreen` (prawdziwy komponent, pełna animacja: mimika, mruganie, wizemy sterowane
   `setSpeechAmplitude`, śledzenie wskaźnika) wypełnia scenę; host jest celowo większy od sceny (`inset` ujemny), żeby twarz zajmowała
   większość ekranu. Nad nią pigułka stanu, pod nią
   fala i dock (głośnik, mikrofon, pauza, zakończ). Kolory stanów: słucha `--listen`, myśli `--think`, mówi `--speak`.
5. **Transkrypt na żywo** (prawa kolumna 340 px; 300 px poniżej 1024): pozycje Ty / Asystent, bieżąca wypowiedź ramką `now`;
   mowa asystenta podświetla zdanie (`.sent.cur`) i słowo (`.w.cur`), wypowiedź użytkownika to kursywa „na żywo”.
6. **Mobile (kontener < 720 px)**: transkrypt jest ukryty (brak prawego paska, brak overlay — celowo), zostaje twarz + pigułka + fala + dock;
   pasek górny skraca się do liczby tok/s (pełne dane w tooltipie, który na telefonie jest panelem pod paskiem).
7. **Responsywność przez container queries** na `.screen-frame` (nie przez viewport): ten sam markup działa w ramce desktop,
   w ramce telefonu 390 px (przełącznik w nagłówku) i na prawdziwym telefonie. Progi: 1239 (lista rozmów → szuflada), 1023 (nawigacja → szuflada), 719 (transkrypt, wersja telefoniczna).
9. **Kompozytor**: pigułka z załącznikiem, autogrow textarea, przycisk Wyślij zamieniany na Zatrzymaj podczas generowania, licznik
   znaków i skróty (jak `chat.js`).
10. **Bez stubów**: demo CH02 i CH03 to działające, deterministyczne scenariusze (`chat.js`, `audio.js`); `prefers-reduced-motion`
    skraca animacje do zera i przyspiesza scenariusze.

## Dane przykładowe

Sesja Anny: 6 odpowiedzi, średnia 44,8 tok/s, min 43,2, max 47,5,
TTFT 184 ms. Rozmowy: „Przydział VRAM w vLLM”, „Przypomnienie o spotkaniu” (głosowa), …

## Odświeżenie bundla twarzy

```bash
python3 mockups/chat-20261024/shared/build-face-bundle.py
```

Skrypt skleja `data/face-data.js`, `data/face-edges.js` i `components/tf-face.js`, usuwa `import`/`export` i opakowuje w IIFE.

## Kandydaci do aktualizacji (nie ruszane)

- `mockups/projekty-20260723/c01-chat.html` — chat projektu (własny nagłówek `.chat-head`, brak metryk sesji).
- `mockups/crm-v1/c11-ai-chat.html` — chat AI w CRM.
- `mockups/component-catalog/index.html` (sekcje „Chat” i „Audio stage”, ok. linii 2183–2420) — katalog komponentów z poprzednim układem audio (twarz 360 px w kole + `rail`).

11. **Rozłączenie** obsługuje wyłącznie globalny modal platformy (`connection-overlay`); ekran Chat nie ma własnego banera. Przerwana odpowiedź zostaje w wiadomości z akcją „Wznów generowanie”.
12. **Usuwanie rozmowy** jest wyłącznie w menu trzech kropek przy pozycji listy rozmów (Zmień nazwę, Usuń rozmowę), nigdy na górnym pasku. Interfejs nie pokazuje, jaki model odpowiada.
