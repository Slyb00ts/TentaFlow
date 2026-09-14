# Chat bubble

| | |
|---|---|
| Tier | 0 (MVP) |
| HTML | `<tf-chat-bubble>` — `www/js/components/tf-chat-bubble.js`, style `css/style.css` linie 3048-3260, 4664-4685 (**nie** `controls.css`) |
| Protokół addonów | brak taga — `ChatBubble` nie istnieje w `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` (grep zero trafień dla „Chat”) |
| Natywny (TentaEngine) | `tenta_ui_widgets::ChatBubble` — status: planowany |
| Status dokumentu | draft |

Dymek wiadomości czatu (użytkownik/asystent). Renderuje dokładnie ten sam
DOM co dawna funkcja `renderBubble()` z `chat.js`, teraz opakowany w custom
element — dlatego style żyją w `style.css` (ogólny arkusz aplikacji), nie w
`controls.css` (arkusz komponentów `tf-*`), inaczej niż większość reszty
biblioteki. **Ten komponent nie jest dziś częścią protokołu UI addonów** —
addon nie może wyrenderować czatu przez katalog `0x0…`; czat jest wyłącznie
funkcją hosta (`chat.js`, `chat-audio.js`).

## Anatomia

```text
┌────┐  Model · 14:30                       ← .bubble-meta (.who + czas)
│icon│  ┌──────────────────────────────┐
└────┘  │ treść markdown                │    ← .bubble (assistant: grid col 2)
        └──────────────────────────────┘
        [copy] [regenerate]                  ← .msg-actions, widoczne na hover/focus-within
```

Wiersz użytkownika jest lustrzany (`msg-row.user`, grid `1fr 36px`, avatar w
kolumnie 2, `.bubble-wrap` wyrównany do prawej, `max-width: min(680px,100%)`).
Avatar użytkownika to inicjał (`sender.charAt(0)`); avatar asystenta to ikona
sprite `model`. Opcjonalny wiersz `.bubble-status` (`role="status"
aria-live="polite"`) pokazuje status generowania („narzędzie · search_web”) —
jedyne miejsce w tym komponencie z jawną rolą ARIA.

## Warianty

| `role` (atrybut) | Wygląd | Kiedy używać |
|---|---|---|
| `assistant` (domyślny) | avatar po lewej, dymek `--bg-card`, `border-bottom-left-radius: 4px` | odpowiedź modelu |
| `user` | avatar po prawej, dymek gradient indygo/lilac, `border-bottom-right-radius: 4px`, wyrównanie do prawej | wiadomość operatora |

Akcje różnią się per rola: `user` ma tylko „Copy”; `assistant` ma „Copy” +
„Regenerate”. Nie ma osobnego wariantu wizualnego dla treści systemowej — nie
istnieje `role="system"` w kodzie.

## Rozmiary

Brak tokenu rozmiaru — avatar 36×36px domyślnie, **28×28px poniżej 720px**
(`@media (max-width: 720px)`, `style.css:3406-3409` — wartość spoza skali
`layout.breakpoints_px` z tokens.json, najbliższy token to `xs`=640).
Dymek ma `padding: 12px 16px` (10px/12px na mobile), font 14px.

## Stany

| Stan | Opis |
|---|---|
| default | avatar + meta + treść + akcje ukryte (`opacity: 0`) |
| hover / focus-within na wierszu | `.msg-actions` `opacity: 1` (`.msg-row:hover .msg-actions, .msg-row:focus-within .msg-actions`) |
| `streaming` (atrybut boolean) | dopisuje `<span class="streaming-caret">` po treści, tylko dla `assistant` |
| `status` (atrybut, tekst) | pokazuje `.bubble-status` nad treścią, znika przy usunięciu atrybutu |
| `<think>...</think>` w markdownie | renderowany jako `<details class="thinking">` zwijalny, z `.thinking-head`/`.thinking-body` — natywna funkcja `<details>`, nie JS |

## Zachowanie

- Interakcja wskaźnikiem: kliknięcie `.msg-act` (delegacja eventów na
  `.msg-actions`) emituje `CustomEvent('action', { bubbles: true, detail:
  { type: 'copy' | 'regenerate' } })` — sam komponent **nie kopiuje do
  schowka ani nie wywołuje regeneracji**; to musi zrobić słuchacz zewnętrzny
  (`chat.js`).
- Klawiatura: przyciski akcji są `<button>`, w naturalnej kolejności Tab;
  `.thinking` (`<details>`) obsługuje Enter/Space natywnie na `<summary>`
  odpowiedniku (`.thinking-head`).
- Animacje: `streaming-caret` (blinking cursor, patrz CSS `@keyframes` w
  `style.css` w tej samej sekcji), obrót chevronu `.thinking-head .chev` przy
  `[open]`. `.bubble-status-dot` ma animację wygaszaną przez
  `prefers-reduced-motion` (`style.css:4684-4685` — jedna z tylko 7 reguł w
  całym repo, które faktycznie to honorują).
- Zdarzenia/API: atrybuty obserwowane: `role`, `sender`, `time`, `model`,
  `streaming`, `status`. Treść dymka **nie jest atrybutem** — ustawiana
  przez `innerHTML` w momencie tworzenia elementu (przechwycona jako
  `_slotContent` przy pierwszym `connectedCallback`) albo metodą
  `setContent(html)` (re-renderuje cały element, w tym markdown-już-HTML
  przekazany przez wywołującego — **komponent sam nie renderuje markdownu**,
  oczekuje gotowego HTML).
- Zaznaczanie tekstu: dozwolone w treści dymka; przyciski akcji mają
  `user-select` domyślny przeglądarki (nieblokowane jawnie).

## Dostępność

Jedyna jawna rola ARIA to `.bubble-status` (`role="status" aria-live="polite"`)
— reszta dymka (treść, meta, akcje) nie ma landmarków ani `aria-label`.
Przyciski akcji mają `title` (tooltip natywny), ale nie `aria-label` —
działa dla myszy/tooltipa, słabiej dla czytników ekranu bez natywnego
mapowania `title→accessible name` w każdej przeglądarce (jest wspierane, ale
mniej niezawodne niż `aria-label`). Kod komponentu sam nie escapuje treści
markdown wstrzykiwanej przez `setContent`/konstruktor (`innerHTML`
bezpośrednio) — bezpieczeństwo (sanityzacja HTML) leży całkowicie po stronie
wywołującego (`chat.js`), nie tego komponentu.

## Responsywność i platformy

`@media (max-width: 720px)` zmniejsza avatar do 28px, gap do 8px, padding
dymka do 10px/12px — jedyna reguła responsywna. 720px nie jest tokenem
`layout.breakpoints_px` (najbliższy: `xs`=640). Na ESP32-P4 (bez cieni,
`scale_factor: 1.25`) dymek traci ewentualny box-shadow (brak w kodzie
zresztą — dymek dziś nie ma cienia, tylko border).

## Tokeny użyte

- `color.themes.dark.bg.card` (`--bg-card`) — dymek assistant
- `color.themes.dark.accent.primary`/`secondary` — gradient dymka user (0.25/0.18 alfa, nie czysty `accent.soft`)
- `color.themes.dark.text.secondary`/`muted` — meta, timestamp
- `color.themes.dark.accent.secondary` (`--accent-2`) — avatar assistant border/color, etykieta thinking
- `typography.scale.caption` (11px) — `.bubble-meta`, zgodne z `caption.size=11`
- `radius.lg` (dymek 14px, zgodne z `radius.lg=14`) — z asymetrycznym narożnikiem 4px po stronie avatara

## Znane odstępstwa w kodzie (2026-09-14)

- **Poza katalogiem protokołu addonów.** `ChatBubble`/`ChatComposer` nie
  mają taga `0x0…` — addon nie może dziś renderować czatu przez CBOR UI;
  czat jest funkcją wyłącznie hosta. Każdy plan udostępnienia czatu addonom
  wymaga najpierw wpisu w `ADDON_UI_COMPONENT_CATALOG_v1.md`.
- **Komponent nie sanityzuje HTML.** `innerHTML`/`setContent(html)`
  wstrzykuje zaufany-z-założenia HTML (markdown już wyrenderowany przez
  `chat.js`) bez własnej walidacji — ryzyko XSS jeśli treść modelu trafi tu
  bez sanityzacji po stronie wywołującego.
- **Breakpoint 720px** zamiast najbliższego tokenu `xs` (640) z `tokens.json`.
- **`title` zamiast `aria-label`** na przyciskach akcji.
- **Styl żyje w `style.css`, nie `controls.css`** — niespójne z resztą
  biblioteki `tf-*`, utrudnia migrację do generowanego `tokens.css`.

## Przykłady

```html
<tf-chat-bubble role="assistant" sender="GPT" model="gpt-x" time="14:30">
  <p>Cześć! W czym mogę pomóc?</p>
</tf-chat-bubble>
<tf-chat-bubble role="user" sender="Piotr" time="14:31">Zrestartuj node 3.</tf-chat-bubble>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
ChatBubble::assistant("GPT")
    .timestamp("14:30")
    .markdown("Cześć! W czym mogę pomóc?")
    .streaming(false)
    .on_action(|act| match act { ChatAction::Copy => Msg::CopyBubble, ChatAction::Regenerate => Msg::Regenerate })
```
