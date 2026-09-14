# Chat composer

| | |
|---|---|
| Tier | 0 (MVP) |
| HTML | `<tf-chat-composer>` — `www/js/components/tf-chat-composer.js`, style `css/style.css` linie 3284-3404; `<tf-mention-input>` — `www/js/components/tf-mention-input.js` (brak wpisu w `controls.css`/`style.css` — patrz odstępstwa) |
| Protokół addonów | `0x0309` `MentionInput` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §5; brak taga dla `ChatComposer` |
| Natywny (TentaEngine) | `tenta_ui_widgets::ChatComposer` / `MentionInput` — status: planowany |
| Status dokumentu | draft |

Pole wpisywania wiadomości czatu: textarea auto-rosnąca, przyciski
załącznika/głosu/wyślij, licznik znaków, podpowiedzi klawiszowe. `MentionInput`
to osobny, niżej-poziomowy komponent (textarea + trigger `@`/`#` +
popover sugestii) używany m.in. wewnątrz kompozytora, gdy host chce
wzmiankowania (`@agent`, `#zasób`).

## Anatomia

```text
┌──────────────────────────────────────────────────┐
│ [📎]  [ textarea auto-grow, rows=1..N     ]  [🎤][➤]│  ← .composer
├──────────────────────────────────────────────────┤
│ Enter wyślij · Shift+Enter nowa linia      0/4096 │  ← .composer-hints
└──────────────────────────────────────────────────┘
```

Wariant `compact` (atrybut boolean) usuwa załącznik, głos i pasek podpowiedzi
— zostaje tylko textarea + przycisk wyślij z widoczną etykietą tekstową
(`tf-button` z treścią zamiast tylko ikony). To jest forma jednowierszowa do
krótkich wymian testowych, nie pełna forma rozmowy.

## Warianty

| Atrybut | Wygląd | Kiedy używać |
|---|---|---|
| *(brak)* | pełna forma: załącznik, textarea, głos, wyślij, hinty, licznik | ekran czatu |
| `compact` | jeden rząd: textarea + wyślij z etykietą | krótkie testy/wbudowane pola czatu (np. panel agenta) |

`MentionInput` nie ma wariantów wizualnych — zachowanie różnicuje się przez
property `.triggers` (domyślnie `['@']`, dowolne znaki 1-znakowe).

## Rozmiary

Brak tokenu `sm`/`md`/`lg`. `max-length` (atrybut, domyślnie 4096) kontroluje
tylko limit znaków licznika, nie wygląd. Textarea rośnie od `rows="1"` w górę
przez `_autogrow()` w `tf-textarea.js` (osobny komponent, atrybut `autogrow`).

## Stany

| Stan | Opis |
|---|---|
| default | placeholder z i18n (`composer.placeholder`), licznik `0 / max` |
| licznik > 90% limitu | `.counter.warn` (`--warning`) |
| `disabled` (atrybut) | textarea i przycisk wyślij dostają `disabled` (propagacja do `tf-textarea`/`tf-button`); `_send()` odrzuca wywołanie jeśli `disabled` jest ustawiony |
| tekst pusty lub tylko whitespace | `_send()` po `trim()` nic nie robi (brak wizualnej blokady przycisku — przycisk **nie** dostaje `disabled` automatycznie przy pustym polu) |
| tekst > `max-length` | `_send()` odrzuca wysyłkę cicho (bez komunikatu błędu w komponencie) |

`MentionInput` popover: `hidden` (zamknięty) / otwarty z listą `role="option"`,
aktywny element oznaczony `.active` + `aria-activedescendant` na textarea.

## Zachowanie

- Interakcja wskaźnikiem: klik `.composer-send` → `_send()`. Klik
  `.composer-voice` → emituje `CustomEvent('voice', { bubbles: true })` (bez
  detail — host decyduje co to znaczy). Klik `.composer-attach` — **przycisk
  istnieje w markupie, ale `tf-chat-composer.js` nie dodaje mu żadnego
  listenera**; obsługa załączników nie jest zaimplementowana w tym
  komponencie (patrz odstępstwa).
- Klawiatura: `Enter` bez `Shift` → `preventDefault()` + `_send()`.
  `Shift+Enter` → nowa linia (zachowanie domyślne textarea, nieprzechwycone).
  Kod **nie sprawdza `e.isComposing`** — podczas kompozycji IME (np. wpisywanie
  japońskiego/chińskiego przez edytor metody wprowadzania) naciśnięcie Enter,
  które powinno zatwierdzić kandydata w IME, wyśle wiadomość przedwcześnie.
  To realny bug do naprawienia w rendererze natywnym (i wskazane do poprawy
  w wersji webowej).
  `MentionInput`: `ArrowUp`/`ArrowDown` (nawigacja sugestii, tylko gdy
  popover otwarty), `Enter` (wybór aktywnej sugestii), `Escape` (zamknięcie
  popovera) — wszystkie tylko gdy `_isOpen`.
- Animacje: brak w `tf-chat-composer.js`; auto-grow textarea to natychmiastowa
  zmiana `style.height`, bez `transition`.
- Zdarzenia/API `tf-chat-composer`:
  - Atrybuty: `placeholder`, `max-length`, `disabled`, `compact`.
  - Property `.value` (get/set), `.maxLength` (get, z atrybutu).
  - Metoda `.focus()`.
  - Event `send` (`detail: { text }`, bubbles) — jedyny sposób odczytania
    wysłanej wiadomości; komponent czyści `.value` po wysłaniu.
  - Event `voice` (bez detail).
  - **Brak eventu dla załączników.**
  Zdarzenia/API `tf-mention-input`:
  - Property `.triggers` (`Array<string>`, filtr do 1-znakowych), `.suggestions`
    (`Array<{id, label}>`, host wypycha po evencie `search`), `.disabled`.
  - Event `search` (`detail: { trigger, query }`) — host odpowiada
    ustawiając `.suggestions`.
  - Event `mention` (`detail: { id, label, trigger }`) po wyborze sugestii.
  - Event `change` (`detail: { value }`) przy każdym wejściu.
  - Trigger jest rozpoznawany tylko na początku linii lub po białym znaku
    (`_detectTrigger()`) — `email@domena` nie otworzy popovera.
- Zaznaczanie tekstu: dozwolone w textarea; przyciski ikon mają
  `user-select` domyślny (nieblokowane).

## Dostępność

Przyciski akcji (`attach`/`voice`/`send`) mają `aria-label` z i18n —
poprawnie oznaczone dla czytników ekranu mimo bycia icon-only.
`MentionInput`: textarea ma `aria-autocomplete="list"`, `aria-expanded`
zsynchronizowane z otwarciem popovera, `aria-activedescendant` wskazujące
aktywną opcję, popover ma `role="listbox"`, każda opcja `role="option"` +
`id` stabilne (`${uid}-opt-${i}`) — to kompletny wzorzec combobox/listbox.
`tf-chat-composer` sam nie ma `aria-label`/`role` na kontenerze — to zwykły
`<div class="composer-wrap">`; landmark `role="form"`/`aria-label` na całym
kompozytorze nie jest ustawiany.

## Responsywność i platformy

`@media (max-width: 720px)` (`style.css:3402-3404`, ta sama reguła co
`chat-bubble.md`): padding `.composer-wrap` z `env(safe-area-inset-bottom)`
(obszar bezpieczny iOS), `.composer` zaokrąglenie 14px, drugi `<kbd>` w
podpowiedziach (`Shift+Enter`) ukryty — zostaje tylko podpowiedź `Enter`.
Voice-mode (stany `listen`/`think`/`speak`, `color.themes.dark.voice` w
`tokens.json`) **nie żyje w tym komponencie** — patrz odstępstwa.

## Tokeny użyte

- `color.themes.dark.text.muted` (`--text-3`) — licznik, podpowiedzi
- `color.themes.dark.semantic.warning` (`--warning`) — licznik `.warn`
- `spacing.*` — nieużywane wprost (paddingi w `style.css` to literały px, patrz odstępstwa)
- `color.themes.dark.voice.listen/think/speak` — zdefiniowane w tokenach, ale konsumowane przez inny moduł (patrz odstępstwa)

## Znane odstępstwa w kodzie (2026-09-14)

- **Ekran czatu głównego nie używa `<tf-chat-composer>`.** `chat.js`
  renderuje własny markup kompozytora ręcznie (linia 1340: `id="chat-attach"`)
  z własnym, **działającym** handlerem załącznika (`chat.js:1460`).
  `<tf-chat-composer>` jest realnie używany tylko w `agents.js`,
  `project-studio.js` i `tf-chat-panel.js` — tam, gdzie jest używany,
  przycisk załącznika **nie robi nic** (brak listenera w komponencie).
  Dokumentacja komponentu musi więc jasno rozróżniać: pełna funkcjonalność
  załączników istnieje w kodzie aplikacji, ale nie w tym reużywalnym
  komponencie.
- **Brak obsługi `isComposing` (IME).** `Enter` podczas kompozycji IME wysyła
  wiadomość zamiast zatwierdzić znak — do naprawienia w natywnym rendererze
  jako wymaganie projektowe, nie tylko w wersji webowej.
- **Stany głosowe `listen`/`think`/`speak` żyją w osobnym module.**
  `color.themes.dark.voice` (`tokens.json`) jest konsumowany przez
  `chat-audio.css`/`chat-audio.js` przez selektor `.audio-stage[data-state="listen|think|speak|idle"]`
  — pełnoekranowy widok trybu głosowego, całkowicie odrębny od
  `tf-chat-composer`. Przycisk `.composer-voice` tylko **emituje event
  `voice`**; to host (`chat.js`) decyduje, że oznacza to przejście do
  `chat-audio.js`. Ten dokument nie powinien sugerować, że kompozytor sam
  zna/renderuje stan `listen`/`think`/`speak`.
- **`tf-mention-input` bez wpisu w arkuszach CSS repo** (`controls.css`,
  `style.css`) — klasy `.tf-mention-input`, `.tf-mention-input-area`,
  `.tf-mention-input-popover`, `.tf-mention-input-option` nie mają żadnej
  reguły stylu w żadnym z dwóch głównych arkuszy przeszukanych dla tego
  dokumentu; jeśli jest stylowany, to przez plik strony spoza zakresu tego
  przeglądu — do zweryfikowania przed użyciem w nowym miejscu.
- **Paddingi/rozmiary jako literały px**, nie tokeny `spacing.*` — cały blok
  `.composer*` w `style.css:3284-3404` używa wartości typu `10px 12px`,
  `14px`, nie `var(--tf-space-*)`.

## Przykłady

```html
<tf-chat-composer placeholder="Zapytaj o cokolwiek..." max-length="4096"></tf-chat-composer>
<tf-mention-input placeholder="Wiadomość... @wzmianka"></tf-mention-input>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
ChatComposer::new()
    .placeholder("Zapytaj o cokolwiek...")
    .max_length(4096)
    .on_send(Msg::SendMessage)
    .on_voice(Msg::EnterVoiceMode)

MentionInput::new()
    .trigger('@', Msg::SearchMentions)
    .on_mention(Msg::InsertMention)
```
