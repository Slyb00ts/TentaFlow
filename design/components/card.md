# Card

| | |
|---|---|
| Tier | 0 (MVP) |
| HTML | `<tf-section-card>` — `tentaflow-core/www/js/components/tf-section-card.js`, style `controls.css:2929-2978` (+ warianty BEM `__header`/`__titles`/`__body`/`__footer` w `controls.css:5559-5566`). `<tf-choice-card>` + `<tf-choice-group>` — `tf-choice-card.js`, style `controls.css:10390-10505`. Generyczna `.card` (bez custom elementu) — `style.css:520-528`. |
| Protokół addonów | `0x0106` `Card` + `0x0107` `SectionCard` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §1208-1252 |
| Natywny (TentaEngine) | `tenta_ui_widgets::Card` / `tenta_ui_widgets::SectionCard` — status: planowany |
| Status dokumentu | draft |

Kontener grupujący powiązaną treść wizualnie (tło + obramowanie + promień). Trzy realne wcielenia: generyczna `.card` (surowy blok bez nagłówka), `tf-section-card` (karta z wbudowanym nagłówkiem: tytuł/ikona/akcja/stopka) i `tf-choice-card` (karta-radio do wyboru jednej nieodwracalnej opcji, np. w kreatorze). Nie używaj karty jako zamiennika modala (patrz [modal.md](modal.md)) ani jako klikalnego przycisku pełnej szerokości — do tego jest `tf-button` w wariancie block.

## Anatomia

`tf-section-card`:

```text
┌───────────────────────────────────────┐
│ [icon] Tytuł            Akcja →        │  ← tf-section-card-head, padding 14×20
│ ─────────────────────────────────────  │  ← header-divider (opcjonalny)
│                                         │
│  body content (slot domyślny)          │  ← tf-section-card-body, padding 18×20
│                                         │
│  [slot="footer"]                       │  ← poza .tf-section-card-body, bez własnego paddingu
└───────────────────────────────────────┘
  border-radius: --tf-radius (10px) · border 1px --tf-border · tło --tf-bg-card
```

`tf-choice-card` (wewnątrz `tf-choice-group role="radiogroup"`):

```text
┌───────────────────────────────────────┐
│ [icon] Nagłówek              [pill]    │  ← head: ikona 15×15 + heading + pill statusu
│ Opis w jednym zdaniu.                  │  ← __desc
│  • Cecha 1 (tone)                      │  ← __features (lista, per-linia tone)
│  • Cecha 2 (tone)                      │
│ ⚠ Zainstaluj Docker na tym węźle       │  ← __note (widoczna też gdy disabled = "co zrobić")
└───────────────────────────────────────┘
  padding 14px · border-radius --tf-radius-lg (14px) · selected: obwódka accent + glow
```

Generyczna `.card`:

```text
┌───────────────────────────────────────┐
│  dowolna treść (brak nagłówka)         │
└───────────────────────────────────────┘
  padding 16px · border-radius --radius (10px) · border 1px --border
```

## Warianty

| Wariant | Wygląd | Kiedy używać |
|---|---|---|
| `tf-section-card` (domyślny) | nagłówek + treść + opcjonalna stopka | panel ustawień, sekcja szczegółów z akcją |
| `tf-section-card[plain]` | brak wszelkich wrapperów — gołe dziecko na hoście | gdy wywołujący chce sam sterować klasami `.tf-card--*` (rzadkie) |
| `tf-choice-card` | karta-radio z konsekwencjami wyboru | kreator z nieodwracalną decyzją architektoniczną |
| `.card` (generyczna) | blok bez nagłówka | statystyka, miniatura, dowolna treść niepasująca do sekcji |
| Protokół `CardVariant` | `filled` / `outlined` / `elevated` / `ghost` | addon deklaruje wariant deklaratywnie — **żaden z tych czterech nie ma odpowiednika nazwy w HTML** (patrz odstępstwa) |

## Rozmiary

Karta nie ma formalnej skali `sm/md/lg` — rozmiar wyznacza treść i layout rodzica (grid/flex). Jedyne zmienne wymiarowe to padding:

| Element | Padding | Token oczekiwany |
|---|---|---|
| `tf-section-card-head` | `14px 20px` | zbliżone do `spacing.lg` (16) + `spacing.xl` (24), ale nie dokładnie |
| `tf-section-card-body` | `18px 20px` | brak dokładnego odpowiednika w skali `spacing` |
| `tf-choice-card` | `14px` | zbliżone do `spacing.lg` (16) |
| `.card` (generyczna) | `16px` | = `spacing.lg` dokładnie |

Cel dotykowy 44px dotyczy elementów interaktywnych **wewnątrz** karty (przycisk akcji, cały `tf-choice-card` jako radio — wysokość realna zależy od treści, nie jest wymuszona na minimum 44px).

## Stany

| Stan | Tło | Tekst/ikona | Obramowanie | Uwagi |
|---|---|---|---|---|
| default | `--tf-bg-card` | `--tf-text` | 1px `--tf-border` | brak cienia w spoczynku — patrz odstępstwa |
| hover (`tf-section-card`, `.card`) | bez zmiany | bez zmiany | `--tf-border-hover` | + `box-shadow: --tf-shadow, 0 0 20px rgba(99,102,241,.08)` tylko dla `tf-section-card` |
| hover (`tf-choice-card`, nie disabled) | `--tf-bg-card` | bez zmiany | `--tf-border-hover` | |
| selected (`tf-choice-card.is-selected`) | `--tf-bg-card` | bez zmiany | `--tf-accent-1` | + `box-shadow: 0 0 0 1px accent-1, --tf-glow-accent` |
| disabled (`tf-choice-card.is-disabled`) | bez zmiany tła | `opacity: 0.45` na head/desc/features | bez zmiany | `note` (co zainstalować) zostaje w pełnej opacity — jedyny czytelny element |
| focus-visible (`tf-choice-card`) | bez zmiany | bez zmiany | `outline: 2px --tf-accent-1, offset 2px` | brak `focus-visible` na `tf-section-card` (nie jest fokusowalna sama w sobie) |

## Zachowanie

- Interakcja wskaźnikiem: `tf-section-card`/`.card` nie są klikalne same w sobie (chyba że protokół ustawi `clickable: true` na `Card` 0x0106, wtedy emitują `click`). `tf-choice-card` jest zawsze klikalna — klik emituje `choice-select` (bąbelkujące, `cancelable`), ale **nie zaznacza się samo** — to `tf-choice-group` decyduje o stanie `selected`.
- Klawiatura (`tf-choice-group`): `role="radiogroup"`, dzieci `role="radio"`. Strzałki (prawo/dół = następny, lewo/góra = poprzedni, zawijające) oraz `Home`/`End` przesuwają **i od razu zaznaczają** (kontrakt APG radiogroup — inaczej niż w zakładkach, gdzie strzałki tylko przesuwają fokus). `Space`/`Enter` na pojedynczej karcie też zaznacza. Roving tabindex: dokładnie jedna włączona karta ma `tabindex="0"`.
- `tf-choice-card` disabled jest całkowicie poza tabulacją (`tabindex="-1"`) i nie reaguje na klawiaturę — świadomie, bo niedostępna architektura nie może być "wybieralna" tylko dlatego że jest widoczna (komentarz w kodzie, `tf-choice-card.js:233-235`).
- Animacje: przejścia `border-color`/`box-shadow`/`background` 0.15-0.25s `ease`/`--tf-spring-smooth`; brak `@keyframes`, więc nie wymaga osobnej klauzuli `prefers-reduced-motion` poza globalną regułą wildcard.
- Zdarzenia/API: `tf-section-card` — brak zdarzeń własnych, atrybuty `title`/`icon`/`action-text`/`action-href`/`header-divider`. Sloty: domyślny = body, `slot="subtitle"`, `slot="actions"`, `slot="footer"`. `tf-choice-card` — właściwości `value`/`heading`/`description`/`note`/`selected`/`disabled`/`features` (tablica `{icon, tone, lead, text}`), zdarzenie `choice-select`. `tf-choice-group` — właściwość `value`, zdarzenie `change` (`detail.value`).

## Dostępność

- `tf-choice-card` samodzielna (poza grupą) ma `role="button"` + `aria-pressed`; wewnątrz grupy dostaje `role="radio"` + `aria-checked` (grupa nadpisuje rolę synchronicznie przy montowaniu — `tf-choice-card.js:296-301`).
- `tf-choice-card` z ustawionym `note` łączy je przez `aria-describedby` — czytnik ekranu odczyta powód niedostępności razem z kartą.
- `tf-section-card` nie ma wbudowanej roli semantycznej — to zwykły kontener; jeśli reprezentuje sekcję strony, otaczający kod powinien użyć `<section aria-labelledby>` lub podobnego wzorca na wyższym poziomie.
- Kontrast tekstu `--tf-text` na `--tf-bg-card` w motywie dark to 15.4:1 (dziedziczone z `color.themes.dark.text.primary` na `bg.base`, karta jest jaśniejsza więc kontrast jest ≥ tej wartości) — AA spełnione z zapasem.
- `prefers-reduced-motion`: pokryte globalną regułą wildcard w `controls.css:9586`; brak własnej klauzuli w sekcji karty.

## Responsywność i platformy

- Karty nie mają własnych breakpointów — układ (ile kart w rzędzie) ustala rodzic (grid `layout.grid.columns`).
- `tf-choice-group[columns]` (domyślnie 2) kontroluje liczbę kolumn siatki wyboru — nie ma automatycznego przejścia na 1 kolumnę poniżej danego breakpointu w samym komponencie (host musi to zrobić przez CSS strony).
- Na ESP32-P4 (`platform.esp32p4_tab5`, `shadows: flat`) `box-shadow` hover/selected z `tf-section-card`/`tf-choice-card` spłaszcza się do obramowania 1px `border.default` — zgodnie z zasadą platformy z `tokens.json → elevation._comment`.
- Na dotyku (`pointer: coarse`) hover na `.card`/`tf-section-card` nie ma odpowiednika — stan `selected`/`active` musi być czytelny bez hovera (co `tf-choice-card` już spełnia przez trwałe obramowanie `is-selected`).

## Tokeny użyte

- `color.themes.dark.bg.card`, `color.themes.dark.bg.card_hover` — tło karty i stanu hover.
- `color.themes.dark.border.default`, `color.themes.dark.border.hover` — obramowanie.
- `color.themes.dark.accent.primary`, `elevation.accent_glow` (`--tf-glow-accent`) — stan selected `tf-choice-card`.
- `radius.md` (10px, `--tf-radius`) — `tf-section-card`/`.card` (protokół oczekuje `lg`, patrz odstępstwa).
- `radius.lg` (14px, `--tf-radius-lg`) — `tf-choice-card` (zgodne z protokołem).
- `elevation.subtle` — oczekiwany cień spoczynkowy wg protokołu (`shadow: "subtle"` domyślnie dla `SectionCard`); brak w żywym CSS, patrz odstępstwa.
- `spacing.lg` (16) — padding `.card`.
- `motion.easing.standard`, `motion.duration_ms.normal/slow` — przejścia hover.

## Znane odstępstwa w kodzie (2026-09-14)

1. **`tf-section-card` i `.card` używają promienia `md` (10px), nie `lg` (14px).** Protokół (`Card`/`SectionCard`, pola `radius`) deklaruje domyślny `RadiusToken::Lg`, a `tokens.json → radius.usage.lg` wprost mówi „cards, modals”. `tf-choice-card` jest jedyną z trzech implementacji zgodną z tym domyślnym (`--tf-radius-lg`, `controls.css:10395`).
2. **Brak cienia w spoczynku.** `radius.usage`/protokół oczekują `elevation.subtle` na karcie w spoczynku (rola „cards at rest”). Żywy kod nie rysuje żadnego `box-shadow` dopóki karta nie dostanie hovera (`tf-section-card`) — `.card` generyczna nie ma cienia w ogóle, nawet na hover.
3. **Nazwy wariantów `CardVariant` (`filled`/`outlined`/`elevated`/`ghost`) z protokołu nie mają odpowiedników w HTML.** `tf-section-card`/`.card`/`tf-choice-card` renderują zawsze ten sam, pojedynczy wizualny wariant (odpowiadający mniej więcej `outlined`) — `filled`/`elevated`/`ghost` istnieją tylko po stronie protokołu addonów, host HTML ich nie implementuje.
4. **Padding karty nie jest tokenizowany 1:1.** `14×20` (nagłówek) i `18×20` (treść) w `tf-section-card` nie odpowiadają żadnej parze wartości ze skali `spacing` (4/8/12/16/24/32) — to osobne, niezależne literały pikselowe.
5. **Druga rodzina klas BEM istnieje równolegle** (`.tf-section-card__header`/`__titles`/`__body`/`__footer`, `controls.css:5559-5566`) obok oryginalnych (`tf-section-card-head`/`-body`, bez podwójnego podkreślnika) używanych przez `tf-section-card.js`. Obie rodziny są aktywne w tym samym pliku CSS — druga wygląda jak przygotowanie pod addon-renderowany `SectionCard` (0x0107), które jeszcze nie zastąpiło HTML-owej implementacji.

## Przykłady

```html
<tf-section-card title="Ustawienia sieci" icon="wifi" action-text="Edytuj" action-href="#network" header-divider>
  <p>Treść sekcji…</p>
  <div slot="footer">
    <tf-button variant="secondary">Anuluj</tf-button>
    <tf-button variant="primary">Zapisz</tf-button>
  </div>
</tf-section-card>

<tf-choice-group value="native" aria-label="Tryb wykonania">
  <tf-choice-card value="native" icon="zap" heading="Natywny"
    pill="domyślne" pill-tone="warn" description="Uruchamia się bezpośrednio na hoście."></tf-choice-card>
  <tf-choice-card value="container" icon="shield" heading="Kontener"
    disabled note="Zainstaluj Docker na tym węźle"></tf-choice-card>
</tf-choice-group>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
SectionCard::new("Ustawienia sieci")
    .icon(Icon::named("wifi"))
    .action("Edytuj", Msg::EditNetwork)
    .header_divider(true)
    .body(vec![text("Treść sekcji…")])
    .footer(vec![
        Button::new("Anuluj").variant(ButtonVariant::Secondary),
        Button::new("Zapisz").variant(ButtonVariant::Primary),
    ]);

ChoiceGroup::new("native")
    .option(ChoiceCard::new("native", "Natywny").icon("zap").pill("domyślne", Tone::Warning))
    .option(ChoiceCard::new("container", "Kontener").icon("shield").disabled_with_note("Zainstaluj Docker na tym węźle"))
    .on_change(Msg::SelectExecutionMode);
```
