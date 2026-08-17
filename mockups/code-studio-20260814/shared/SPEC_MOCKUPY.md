# SPEC MOCKUPÓW — Code Studio

Kontrakt: `BUILD_CONTRACT.md` w tym katalogu. Dane przykładowe: workspace `tentaflow-core` na węźle
`dev-ryzen`, sesja #4 „Dodaj endpoint /v1/embeddings do API", gałąź `cs/piotr/9f2a1c4b`, tryb
`trusted_native`, autonomia `normalny`. Użytkownik: Piotr · Dev.

---

## Układ przyjęty (obowiązuje wszystkie ekrany)

**Szeroki ekran** — dwie kolumny w `.cs-body`:

```
┌─────────────────────────────────────────────┬──────────────────┐
│ pasek projektów (poziomy, status per kafel) │                  │
├─────────────────────────────────────────────┼──────────────────┤
│ SCENA                                       │ DOK              │
│  [Konsola][…otwarte pozycje kategorii…]     │  kategorie       │
│  treść: rozmowa / plik / diff / terminal /  │  spis            │
│         wnętrze subagenta / commit          │                  │
└─────────────────────────────────────────────┴──────────────────┘
```

- **Dok jest nawigatorem** — same spisy. Kliknięcie otwiera rzecz **w scenie**; 372 px to za mało na
  kod, diff czy terminal.
- **Pasek zakładek sceny jest spisem otwartych pozycji AKTUALNEJ kategorii doku.** Na Subagentach
  widać subagentów, na Plikach pliki, na Zmianach patch set, na Git commity, na Terminalu sesje.
- **Przycisk „Konsola" stoi na początku paska** — rozmowa jest zawsze o jedno kliknięcie stąd.
- Poniżej 1350 px dok chowa się do szuflady z prawej (przycisk „Spis" po prawej stronie paska).

**Telefon (< 900 px)** — jedna kolumna:

- górny pasek: przycisk workspace (kropka + nazwa + licznik pytań) otwierający **arkusz z góry**,
  obok tytuł sesji;
- dolny pasek przypięty: Konsola · Agenci · Zmiany · Pliki · Git · Term — przełącza widok **nad
  sobą**, nigdy nie rozwija panelu pod sobą;
- widoki treściowe (konsola, pliki, zmiany, terminal) pokazują scenę, spisowe (agenci, git) dok;
  przy treści dok wraca jako szuflada z prawej;
- pływający skrót „← Agent główny" nad dolnym paskiem, bursztynowy i dopominający się, gdy agent
  czeka na odpowiedź.

**Pytanie agenta** przejmuje kompozytor: pole wejściowe robi się bursztynowe, dostaje nagłówek,
treść pytania i opcje. Na telefonie opcje to jeden przewijalny rząd chipów — zawsze widoczne.
W strumieniu zostaje jednolinijkowa kotwica.

---

## GOTOWE

### K01 — k01-konsola.html — Konsola sesji
Źródło prawdy dla układu i języka wizualnego; obsługuje oba układy z jednego pliku. Zawiera:
pasek projektów z czterema workspace'ami w czterech stanach, strumień z wiadomością użytkownika,
rozumowaniem, wierszami narzędzi (w tym jednym w trakcie), rozwinięciem wyniku, kartą subagenta,
kotwicą pytania i kartą zmian; kompozytor w trybie pytania; pięć paneli treści; pięć spisów w doku;
arkusz workspace'ów; szufladę nawigatora.

### M01 — m01-mobile.html — Telefon, poglądowo
Cztery stany obok siebie w ramkach telefonu, do oglądania na dużym ekranie. Wersja żywa to K01.

---

## DO ZROBIENIA

### W01 — w01-lista.html — Lista workspace'ów
Pełna strona. Tabela/karty: nazwa, węzeł, repozytorium, gałąź, tryb wykonania (chip `trusted_native`
ostrzegawczy / `container`), aktywne sesje, zużycie kwoty, ostatnia aktywność. Toolbar: searchbox,
filtr węzła, filtr stanu, `btn-primary` „Nowy workspace" (otwiera W02). Stopka podsumowania. Menu ⋯
per wiersz: Otwórz / Ustawienia / Archiwizuj / Usuń. Wariant pustego stanu z wyjaśnieniem grantu
tworzenia.

### W02 — w02-kreator.html — OKNO: Nowy workspace
Bazowy = W01. `.window.wizard`, `.stepper` (1 Podstawy · 2 Wykonanie · 3 Kod). Trzy sekcje `.screen`:
KROK 1 nazwa, węzeł (select z listy mesh); KROK 2 **dwie karty trybu** — `trusted_native`
preselekcjonowany z etykietą „Native — kod ma dostęp do hosta", `container` z listą tego, co zyskuje
(izolacja, wymuszone ro/cow, kontrolowany egress), nieaktywny na węźle bez runtime'u; poniżej tryb
autonomii i polityka egress z **ukrytymi** `autonomous` i `local_only` przy natywnym; KROK 3 źródło
(puste / git URL), poświadczenia (token / klucz SSH z `.hint` o szyfrowaniu), gałąź docelowa,
indeks semantyczny. Komunikat, gdzie system założy katalog — ścieżki się nie podaje.

### K02 — k02-zmiany.html — Przegląd zmian
Pełna strona, scena na `zmiany`. Lista plików patch setu, diff z akceptacją **per hunk**
(przyciski przy każdym hunku), stan `conflicted` na jednym pliku z wyjaśnieniem CAS, stopka
z „Akceptuj zaznaczone / Odrzuć / Poproś o poprawkę". Druga sekcja `.screen`: częściowa akceptacja —
widok rekonstrukcji pliku z zaakceptowanych hunków.

### K03 — k03-git.html — Git
Pełna strona. Gałęzie, historia, worktree sesji, **merge przez worktree integracyjny**: kroki
scalenie → testy → review → `update-ref`, z wariantem konfliktu (worktree w stanie `held`, run
rewizji). Obowiązkowe pytanie przy push i merge pokazane jako karta w kompozytorze.

### K04 — k04-flow.html — Code Harness we Flow Builderze
Graf z §16.2 planu narysowany w konwencji Flow Buildera: bloki, region pętli, `ask_user`, `spawn`,
`patch_review`, `git_op`. Pokazuje, że nic nie dzieje się poza grafem.

### G01 — g01-rozlaczenie.html — Węzeł nieosiągalny
Wariant overlaya połączenia dla pracy zdalnej: sesja żyje dalej na owner node, UI pokazuje stan
i ponowienie. Baza: `connection-overlay.css`.
