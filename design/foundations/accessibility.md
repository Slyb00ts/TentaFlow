# Dostępność

Dostępność nie jest opcjonalna (zasada 5, [`../README.md`](../README.md)):
widoczny fokus, kontrast AA, klawiatura, `prefers-reduced-motion` w każdym
komponencie z animacją. Repozytorium ma w tym miejscu realną, nietrywialną
implementację — 475 użyć `aria-label`, roving tabindex w listach/drzewach,
`aria-live` w kilku komponentach — silniejszą niż system tokenów. Ten dokument
opisuje kontrakt, cytuje zweryfikowany stan kodu i nazywa wprost miejsca, gdzie
kod dziś nie dotrzymuje reguły.

## Fokus

| | |
|---|---|
| Token | `control.focus_ring`: `width: 2`, `offset: 2`, `color: {border.focus}` (= `accent.primary`, `#6366f1` dark) |
| Selektor | `:focus-visible`, nigdy goły `:focus` dla klawiatury (myszowy klik nie pokazuje ringu) |
| Implementacja | `outline: 2px solid var(--tf-accent-1); outline-offset: 2px` (wzorzec z `controls.css`, np. `.tf-card--clickable:focus-visible`) |

**Zasada twarda:** `outline: none` bez zamiennika jest zabroniony (governance
README, bramka CI docelowa). W kodzie `outline: none` występuje w kilkunastu
miejscach `controls.css`/`style.css` (np. `.tf-input`, `.tf-multiselect-trigger`,
przyciski segmentowe) — **wszystkie zweryfikowane przypadki są poprawne**: albo
komponent ma osobną regułę `:focus-visible` z `box-shadow`/`outline` gdzie
indziej (np. `.tf-multiselect-trigger:focus-within { box-shadow:
var(--tf-glow-accent) }`), albo `outline: none` siedzi na kontenerze, a
faktyczny fokus i ring są na potomku. Przy nowym komponencie: jeśli zerujesz
`outline`, w tym samym miejscu dodaj `:focus-visible` z ringiem — nie później
„jak będzie czas".

## Mapa klawiatury per klasa widgetu

| Klasa | Klawisze | Zweryfikowane w |
|---|---|---|
| Przycisk / link-przycisk | `Enter`, `Space` aktywują; `Tab`/`Shift+Tab` przenoszą fokus | natywne zachowanie `<button>`/`role="button"` |
| Lista / drzewo | Roving tabindex: jeden element `tabindex="0"`, reszta `tabindex="-1"`; `ArrowUp`/`ArrowDown` przenoszą fokus i `tabindex` | `tf-tree.js:166,261,266,333,335` — leniwe dzieci + roving tabindex zaimplementowane |
| Combobox | `ArrowDown`/`ArrowUp` nawigują opcje, `aria-activedescendant` wskazuje aktywną bez przenoszenia realnego fokusu z inputu, `aria-expanded` na inpucie | `tf-combobox.js:103,280,308,318,371,376` |
| Modal / okno | `Esc` zamyka (najbardziej wierzchnie okno w stosie ignoruje niższe) | `tf-modal.js`, `tf-window.js:372-388` |
| Menu | `Esc` zamyka | `tf-menu.js:294` |

**Znana luka — nie ukrywaj jej w PR-ach, tylko napraw przy okazji:**
`tf-modal.js`/`tf-window.js` ustawiają `role="dialog"` + `aria-modal="true"`
i zamykają na `Esc`, ale **nie implementują pułapki fokusu** (`Tab`/`Shift+Tab`
mogą dziś wyjść poza okno modalne do reszty strony pod spodem — grep po
`Tab`-trap w komponentach dialogowych nie znalazł implementacji). Podobnie
`tf-menu.js` zamyka się na `Esc`, ale nie nawiguje opcjami strzałkami (brak
roving tabindex/`ArrowDown` w tym komponencie, w odróżnieniu od `tf-tree.js`).
Nowy modal/menu **musi** to mieć — nie kopiuj istniejący `tf-modal.js` jako
wzorzec kompletności.

## ARIA — role i wymagane atrybuty per klasa komponentu

| Klasa | Rola/atrybuty | Uwaga |
|---|---|---|
| Modal | `role="dialog"`, `aria-modal="true"`, etykieta (`aria-label` lub `aria-labelledby`) | brak pułapki fokusu — patrz wyżej |
| Combobox | `role` na liście opcji, `aria-expanded`, `aria-activedescendant`, `aria-autocomplete` | wzorzec ARIA 1.2 combobox |
| Drzewo/lista z roving tabindex | `tabindex` roving, `aria-selected`/`aria-current` na elemencie aktywnym | `tf-tree.js` |
| Ikona samodzielna (icon-only button) | `aria-hidden="true"` na `<svg>`, `aria-label` na hoście | patrz `foundations/iconography.md` |
| Pole formularza | `aria-invalid` przy błędzie, `aria-describedby` do komunikatu pomocy/błędu | |
| Toggle/checkbox/radio niestandardowy | `aria-checked`/`aria-pressed` | |
| Kanban / drag-drop | `aria-live="polite"` ogłasza zmiany kolejności | `tf-kanban.js:13,533` |
| Status wiadomości czatu | `role="status"`, `aria-live="polite"` | `tf-chat-bubble.js:72` |

Skala użycia w repo (2026-09-14, grep po `js/`): `aria-label` 475×,
`aria-hidden` 193×, `aria-expanded` 95×, `aria-labelledby` 59×, `aria-selected`
57×, `aria-disabled` 57×, `aria-controls` 48×, `aria-describedby` 41×,
`aria-invalid` 32×, `aria-pressed` 28×, `aria-checked` 27×, `aria-live` 26×,
`aria-activedescendant` 22×, `aria-current` 20×, `aria-autocomplete` 13×,
`tabindex` 114×. To jest siła do zachowania przy migracji na natywny
renderer — nowy komponent musi pokryć te same atrybuty co jego `tf-*`
odpowiednik, nie mniej.

## Live regiony

Status/toast/komunikat asynchroniczny musi trafić do drzewa dostępności bez
przenoszenia fokusu:

- `aria-live="polite"` — domyślne dla toastów, statusów wiadomości, ogłoszeń
  zmian listy (kanban, wyniki wyszukiwania).
- `aria-live="assertive"` — tylko dla błędów wymagających natychmiastowej
  uwagi (rzadkie; nadużycie `assertive` przerywa czytnik ekranu w trakcie
  innej wypowiedzi).
- Region musi istnieć w DOM **przed** zmianą treści — czytniki ekranu nie
  łapią regionu dodanego i wypełnionego w tej samej klatce.

## Kontrast

Z `tokens.json` → `color.themes.dark.text`/`accent` (`contrast_on_bg_base`,
`contrast_on_accent`):

| Token | Wartość | Kontrast na `bg.base` | Zastosowanie |
|---|---|---|---|
| `text.primary` | `#e8ebf5` | **15.4:1** | treść główna |
| `text.secondary` | `#a0a8c8` | **8.2:1** | etykiety, metadane |
| `text.muted` | `#6a7196` | **3.9:1** | wyłącznie dekoracyjne/podpowiedzi — **nigdy** treść główna (poniżej progu AA 4.5:1 dla zwykłego tekstu) |
| `text.on_accent` | `#ffffff` | **4.7:1** (na `accent.primary`) | tekst na wypełnionych przyciskach akcentu |

`text.muted` przy 3.9:1 nie spełnia WCAG AA dla tekstu (próg 4.5:1 dla tekstu
< 18px/14px bold) — token jest świadomie oznaczony w `tokens.json` jako
„decorative only, never body copy". Jeśli komponent renderuje w `text.muted`
coś, co użytkownik musi przeczytać (nie tylko dostrzec), to błąd użycia
tokena, nie błąd tokena.

Motyw `light` w `tokens.json` ma status `draft` — wartości dobrane pod
parytet kontrastu z dark, ale nieprzejrzane wizualnie i nigdzie nieaktywne w
CSS (`[data-theme="light"]` nie istnieje w żadnym pliku `css/*.css`, zero
trafień). Nie projektuj komponentu zakładając, że light theme renderuje się
dziś — zweryfikuj kontrast w `tokens.json`, nie w przeglądarce.

## Cele dotykowe

`control.touch_target_min: 44` px — patrz `foundations/responsive.md`. Dotyczy
też ikon-przycisków: wizualny rozmiar ikony (`IconSize`) może być mniejszy niż
44 px, ale obszar trafień (padding kontenera) musi go dopełniać.

## `prefers-reduced-motion`

`tokens.json` → `motion.reduced_motion`: „all durations → 0ms, springs → snap
to target; mandatory on every platform". W praktyce (grep 2026-09-14):
**tylko 7 z ~40 plików `css/*.css`** implementuje zapytanie
`@media (prefers-reduced-motion: reduce)`, mimo 124 odrębnych bloków
`@keyframes` w całym CSS. To jest udokumentowana, otwarta luka — nowy kod z
animacją **musi** dodać wariant reduced-motion (patrz `motion.md`, gdy
powstanie), a przegląd istniejącej strony pod kątem tej reguły jest
uzasadnionym, samodzielnym zadaniem, nie tylko punktem checklisty przy innej
zmianie. Wyjątek zaimplementowany poprawnie: `tf-line-chart.js` sprawdza
`matchMedia('(prefers-reduced-motion: reduce)')` przed animacją wejścia i
przed przejściem przewijania strumienia (`_motionAllowed()`).

## `lang`

`document.documentElement.lang` jest ustawiane przez `I18n` przy starcie i przy
każdej zmianie języka (`www/js/i18n.js:55`, `loadTranslations()`). Wspierane
języki: `pl`, `en`, `fr`, `es`, `de` — wszystkie LTR; `dir="rtl"` nie występuje
nigdzie w kodzie i nie jest dziś planowane (brak wymogu, nie przeoczenie —
udokumentuj jako świadome ograniczenie, jeśli dodajesz komponent zależny od
kierunku pisma).

## Zaznaczanie tekstu

`tokens.json` → `typography.rules.text_selection`: dozwolone wszędzie poza
przyciskami, ikonami i uchwytami przeciągania (`user-select: none` tylko tam).
Treść (tabele, karty, kod, czat) musi pozostać zaznaczalna — nie blokuj
zaznaczania „dla estetyki" na kontenerach z tekstem.

## Co musi wystawiać natywny renderer

W TentaFlow nie ma dziś kodu natywnego renderera UI (`tentaflow-ui-native` nie
istnieje jako crate w repo; `docs/TENTAENGINE_INTEGRATION_PLAN.md`, cytowany
przez `design/README.md`, też jeszcze nie istnieje) — poniższe to wymaganie
na przyszłość, nie opis zaimplementowanego stanu:

- Każdy węzeł drzewa UI musi nieść rolę i etykietę semantyczną (odpowiednik
  `role`/`aria-label` z HTML) niezależnie od tego, czy cokolwiek je dziś
  konsumuje — desktop/mobile mają docelowo użyć **AccessKit** (biblioteka
  Rust; dziś obecna w `Cargo.lock` TentaFlow tylko jako zależność
  tranzytywna, **nie** jest jeszcze wpięta), web ma dostać most DOM/ARIA
  generowany z tego samego drzewa.
- ESP32-P4 nie ma czytnika ekranu (brak headsetu/driverów na urządzeniu), ale
  dane roli/etykiety są niesione mimo to — panel dotykowy koduje encoder
  danych identycznie na każdej platformie; różni się tylko konsument.
- Fokus, `focus_ring` i kolejność `Tab` muszą być odtwarzalne z tego samego
  drzewa co na webie — jeden model klawiatury/fokusu, nie osobny per
  platforma.

## Checklist przeglądu per komponent

- [ ] Rola ARIA i wymagane atrybuty ustawione (patrz tabela wyżej dla klasy
      komponentu).
- [ ] `:focus-visible` z ringiem `control.focus_ring` na każdym elemencie
      fokusowalnym; jeśli `outline: none`, zamiennik jest w tym samym
      commicie.
- [ ] Cała funkcjonalność dostępna z klawiatury — brak akcji „tylko na
      hover"/„tylko na klik myszy".
- [ ] Modal/dialog: `Esc` zamyka, fokus wraca do elementu, który go otworzył,
      **i** jest pułapka fokusu (nie kopiuj `tf-modal.js` bez tego).
- [ ] Lista/drzewo z wieloma elementami: roving tabindex + strzałki, nie sam
      `Tab` po każdym elemencie.
- [ ] Treść dynamiczna (toast, status, wynik) ma `aria-live` już obecny w DOM
      przed wypełnieniem.
- [ ] Żaden tekst czytany przez użytkownika nie jest w `text.muted`
      (3.9:1 — dekoracyjne only).
- [ ] Animacja ma wariant `prefers-reduced-motion: reduce` w tym samym pliku
      CSS/JS, nie „do zrobienia później".
- [ ] Cele dotykowe ≥ 44 px na `pointer: coarse`.
