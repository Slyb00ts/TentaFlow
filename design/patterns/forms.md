# Formularze

Układ, walidacja, zapis i wzorce wizardów — zgrupowane z realnych ekranów
(`settings.js`, `users.js`, `cluster-wizard.js`). Formularze w TentaFlow są
zawsze **kartami** (`.card` / `.tf-section-card`), nigdy gołym blokiem pól na
stronie.

## Układ

- **Jedna kolumna** do `layout.content.narrow` (800 px, `tokens.json`) —
  domyślny układ pól: `.form-row` / `.form-group` w pionie.
- **Dwie kolumny** od `layout.breakpoints_px.md` (1024 px) w górę, dla par
  pól powiązanych semantycznie — patrz `users.js`
  `.users-form-row` (`display:grid; grid-template-columns: 1fr 1fr`
  odpowiednik) łączące `Nazwa użytkownika` + `Nazwa wyświetlana`, a osobny
  `.users-form-row.full` dla pól rozpiętych na całą szerokość (e-mail).
- **Etykieta nad polem** (top label), nie inline z lewej — tak renderuje
  `<tf-input label="…">` (`tf-input.js`: `.tf-label` jest pierwszym dzieckiem
  `.tf-input-group`, `<input>` pod nim).
- Odstęp między polami w rzędzie: `spacing.lg` (16 px); między rzędami
  podobnie.

```text
┌ .tf-section-card ─────────────────────────────────────────┐
│ h3 Tytuł sekcji                                            │
│ ┌ .form-row (1 kol. < 1024px, 2 kol. ≥ 1024px) ───────────┐│
│ │ [label]                    │ [label]                    ││
│ │ [tf-input.............]    │ [tf-input.............]    ││
│ └────────────────────────────┴────────────────────────────┘│
│ ┌ .form-row.full ──────────────────────────────────────────┐│
│ │ [label]                                                   ││
│ │ [tf-input pełna szerokość..............................] ││
│ └───────────────────────────────────────────────────────────┘│
│                                          [Anuluj]  [Zapisz] │
│ .form-error (hidden dopóki nie ma błędu serwera)             │
└───────────────────────────────────────────────────────────┘
```

## Grupowanie w karty sekcji

Każda tematyczna grupa pól = osobna `.tf-section-card` (lub `.card` w
starszych modułach), z własnym `h3` i, jeśli dotyczy, własnym przyciskiem
Zapisz na dole karty — **nie** jeden globalny „Zapisz" na dole całej strony
dla wielu niezwiązanych sekcji. `users.js` renderuje profil użytkownika w
jednej karcie i rolę/status w drugiej, każda z osobnym `data-action="save-…"`.

## Wymagane / opcjonalne

- Konwencja w kodzie jest **odwrotna do intuicji**: pola wymagane nie mają
  żadnego wizualnego znacznika poza atrybutem HTML `required` (`tf-input`
  ustawia `required` na wewnętrznym `<input>`, `tf-input.js:211`) — nie ma
  gwiazdki ani etykiety „(wymagane)" w markupie.
- Pola **opcjonalne** dostają jawny placeholder z tłumaczonym tekstem, patrz
  `cluster-wizard.js`: `placeholder="${I18n.t('common.optional')}"` na polu
  opisu klastra.
- Walidacja `required` po stronie klienta jest lekka — głównie sprawdzenie
  `value.trim().length > 0` przed wysyłką (`cluster-wizard.js`:
  `submitCluster()`: `if (fName.trim().length === 0) { toast(...); return; }`),
  nie blokada natywnego `<input required>` (formularze nie są w `<form>`,
  submit idzie przez `tf-button` + JS).

## Walidacja: kiedy i jak

| Moment | Mechanizm | Przykład |
|---|---|---|
| Podczas pisania | `tf-input` ma `error` jako atrybut/property — ustaw go z handlera `input`, jeśli walidacja jest tania (format, długość) | brak gotowego przykładu inline-podczas-pisania w audytowanych modułach — wzorzec komponentu istnieje (`tf-input.js` renderuje `.tf-error-text` gdy `error` jest ustawiony), użycie per-pole jest do zaimplementowania per potrzebę |
| Przed wysyłką (client-side gate) | Prosty `if` + `toast(msg, 'warning')`, submit przerwany | `cluster-wizard.js`: `if (selectedIds.size < 2) { toast(I18n.t('clusters.select_min_nodes'), 'warning'); return; }` |
| Po odpowiedzi serwera (błąd zapisu) | `.form-error` div w karcie, `hidden` domyślnie, odsłaniany z treścią `err.message` | `users.js` `wireUserDetail`: `catch (err) { fe.hidden = false; fe.textContent = err.message; }` |
| Sukces zapisu | `toast(msg, 'success')`, **nie** modal | wszędzie — `toast(I18n.t('users.saved'), 'success')` |

Kolor błędu inline: `semantic.critical` z `tokens.json`
(`color.themes.dark.semantic.critical.value = #ef4444`), w implementacji
`var(--tf-danger)` (`controls.css`) / `var(--danger)` (`style.css`) —
`.tf-input.tf-input-error { border-color: var(--tf-danger); }`. Podsumowanie
błędów serwera dla całej sekcji idzie do jednego `.form-error` na dole karty,
nie do toastu — toast jest dla wyniku operacji (sukces/porażka jako fakt),
`.form-error` jest dla treści błędu, którą user musi przeczytać i poprawić.

## Zapis / anuluj / stan „dirty”

- Para przycisków `Anuluj` (`variant="ghost"`) / `Zapisz` (`variant="primary"`)
  na dole każdej karty formularza, `Zapisz` zawsze po prawej.
- **Brak** globalnego mechanizmu „dirty flag → wyszarz Zapisz dopóki nic się
  nie zmieniło" w audytowanych modułach — `Zapisz` jest klikalny od razu.
- Ochrona przed utratą niezapisanej pracy idzie przez **`canUnmount()`** na
  poziomie ekranu (Router odpytuje go przed każdą nawigacją, `router.js`:
  `if (leaving && currentScreen.canUnmount) { allowed = await
  currentScreen.canUnmount(id); if (!allowed) return false; }`), nie przez
  blokadę pojedynczego przycisku:

  ```js
  // tentaquant.js — ekran z otwartym notebookiem odmawia zamknięcia
  canUnmount() {
    return this.confirmLeaveProjectView();
  },
  async confirmLeaveProjectView() {
    const guard = this.projectViewGuard; // ustawiony przez aktywny widok
    return guard ? guard() : true;       // guard() sam pyta usera / TfWindow.confirm
  },
  ```

  Odmowa (`false`) zostawia **stary** ekran zamontowany i nietknięty — Router
  nie czyści `#main`, sidebar zostaje podświetlony na starym widoku.

## Potwierdzenia destrukcyjne

Każda operacja kasująca dane (usunięcie klastra/usera/grupy) idzie przez
`TfWindow.confirm({...})` (`tf-window.js`), **nigdy** przez natywny
`confirm()` przeglądarki (wyjątek: `users.js` ma dwa `confirm()`/`prompt()`
natywne przy resecie hasła i usuwaniu — to jest znany dług, nie wzorzec do
kopiowania w nowym kodzie):

```js
const ok = await TfWindow.confirm({
  title: I18n.t('clusters.delete_title'),
  message: I18n.t('clusters.delete_confirm').replace('{name}', name),
  confirmLabel: I18n.t('common.delete'),
  cancelLabel: I18n.t('common.cancel'),
  danger: true,             // → confirmLabel dostaje variant="danger-solid" + icon="trash"
});
if (!ok) return;
```

`danger: true` zmienia wariant przycisku potwierdzenia na czerwony z ikoną
kosza — sam kolor/ikona komunikuje nieodwracalność, tekst przycisku to wciąż
akcja („Usuń"), nie „Tak"/„OK”.

## Wzorzec wizard (2-krokowy) — `cluster-wizard.js`

```text
┌ <tf-window modal draggable width=720> ────────────────────┐
│ [●]──[○]   Krok 1: Podstawy                                │
│ Krok 1: nazwa, opis, strategia, failover, interwały         │
│                                                              │
│                             [Anuluj]           [Dalej →]    │
└──────────────────────────────────────────────────────────┘
                    │ Dalej (włączony tylko gdy fName.trim())
                    ▼
┌ <tf-window> — Krok 2 ───────────────────────────────────────┐
│ [✓]──[●]   Krok 2: Wybór nodów                              │
│ tabela nodów z checkboxami + „Uruchom test" (probe SSE)      │
│ macierz przepustowości między wybranymi nodami               │
│                                                              │
│                  [Anuluj]  [← Wstecz]  [Utwórz klaster]     │
└──────────────────────────────────────────────────────────┘
```

- Wskaźnik kroków: `.wizard-step-indicator` z kropkami `active`/`done`,
  renderowany na nowo przy każdej zmianie `currentStep`.
- Przycisk główny (`primaryLabel`) zmienia etykietę i zachowanie: „Dalej" na
  kroku 1, „Utwórz klaster"/„Zapisz" na ostatnim.
- Walidacja bramkująca przejście: `canPrimary` wyliczany na nowo przy każdym
  renderze (`fName.trim().length > 0` na kroku 1, `selectedIds.size >= 2` na
  kroku 2) — przycisk ma `disabled`, nie ukrywa się.
- Stan wizardu żyje w module-scoped zmiennych (nie w komponencie) i jest
  resetowany explicite w `open()` — wizard nie jest reużywalnym komponentem
  `tf-*`, jest funkcją modułu budującą `<tf-window>` programowo.
- Zamknięcie: `close()` czyści subskrypcję probe (`probeUnsub()`), usuwa
  `winEl`/`backdropEl` z DOM. Klik na backdrop = zamknięcie (`backdropEl
  .addEventListener('click', () => close())`).

## Submit klawiaturą

- Pola tekstowe pojedyncze (`tf-input`) w formularzach-kartach nie mają w
  audytowanym kodzie jawnego handlera `Enter → submit` — zapis idzie przez
  klik `tf-button`. Jeśli dodajesz submit na Enter, wiąż go na poziomie
  formularza (`keydown` na kontenerze, sprawdź `e.key === 'Enter' &&
  !e.shiftKey` dla pól jednowierszowych), nie na pojedynczym polu.
- `tf-window` samo obsługuje `Esc` (zamyka najwyższe okno) — nie duplikuj
  tego w module.

## Autosave

Nie znaleziono wzorca autosave w audytowanych modułach (`settings.js`,
`users.js`, `cluster-wizard.js`, `chat.js`) — każdy zapis jest jawną akcją
usera (klik `Zapisz`). Wyjątek częściowy: `chat.js` zapisuje konwersacje do
`localStorage` po każdej zmianie (`saveConversations()`), ale to jest
persystencja stanu UI, nie autosave formularza z polami do walidacji. Jeśli
strona potrzebuje autosave, udokumentuj to jako świadome odejście od wzorca
i dodaj wizualny wskaźnik stanu zapisu (nieznaleziony w kodzie — do
zaprojektowania).

## Checklist

- [ ] Pola w `.tf-section-card`/`.card`, pogrupowane tematycznie, nie jeden
      wielki formularz bez podziału.
- [ ] Jedna kolumna < 1024 px, dwie kolumny ≥ 1024 px dla par pól.
- [ ] Etykieta nad polem (`<tf-input label="…">`), nie inline z boku.
- [ ] Pola opcjonalne mają placeholder „opcjonalne" (`I18n.t('common.optional')`);
      pola wymagane nie dostają żadnego dodatkowego znacznika.
- [ ] Walidacja przed wysyłką: krótki `if` + `toast('…', 'warning')`.
- [ ] Błąd zapisu serwera: `.form-error` w karcie, nie tylko toast.
- [ ] Operacja destrukcyjna: `TfWindow.confirm({ danger: true, ... })`.
- [ ] Formularz z niezapisanymi zmianami: `canUnmount()` na ekranie, nie
      blokada pojedynczego przycisku.
- [ ] Wizard wielokrokowy: wskaźnik kroków, przycisk główny zmienia
      etykietę/zachowanie, walidacja bramkuje przejście przez `disabled`.
