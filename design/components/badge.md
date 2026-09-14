# Badge / Status pill

| | |
|---|---|
| Tier | 0 (MVP) — `Badge`; 1 — `StatusPill` |
| HTML | `<tf-badge>` — `tentaflow-core/www/js/components/tf-badge.js`, style `controls.css` linie 3145–3206; `<tf-status-pill>` — `tf-status-pill.js`, style linie 2703–2740 (przybliżone) |
| Protokół addonów | `0x020A` `Badge` — `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §4, linie 1548–1561; **brak** tagu dla `StatusPill` (tylko HTML, patrz odstępstwa) |
| Natywny (TentaEngine) | `tenta_ui_widgets::Badge` — status: planowany |
| Status dokumentu | draft |

`Badge` — mały znacznik liczby/statusu przyklejony do elementu (licznik
powiadomień, etykieta stanu w tabeli). `StatusPill` — pełniejsza pigułka z
pulsującą kropką i tekstem, do statusu procesu/usługi w nagłówkach. Nie używaj
`Badge` do akcji klikalnej (to `Chip`, patrz `chip.md`) ani do statycznej
etykiety bez znaczenia semantycznego (to `Tag`, `0x020C`, poza tym specem).

## Anatomia

```text
┌ tf-badge (liczbowy) ┐   ┌ tf-status-pill ──────────┐
│  12  │  ●  │         │   │ ● Running                 │
└──────┴─────┘           └───────────────────────────┘
  min 20×20, radius.pill    kropka 7px + gap 7px
```

Części *badge*: pojedynczy `<span class="tf-badge">` — tło+tekst w jednym
elemencie, brak osobnej „kropki” poza wariantami `dot`/`pulse` (patrz niżej).
Części *status-pill*: `<span class="tf-status-pill">` z pseudo-elementem
`::before` jako kropka i tekstem statusu.

## Warianty

sdk-spec `BadgeVariant` (`solid`/`soft`/`outline`/`pulse`/`dot`) **nie
odpowiada** rzeczywistej implementacji — `tf-badge.js` nie ma pojęcia
„wariantu” w ogóle, tylko `tone` (kolor). Tabela poniżej łączy oba spojrzenia:

| Wariant (sdk-spec) | Stan w HTML | Uwagi |
|---|---|---|
| `soft` | **domyślny wygląd** każdego `tone` (tło = kolor @ 12–18% alpha) | jedyny realnie istniejący wariant kolorystyczny |
| `solid` | **tylko `tone="hot"`** ma pełne wypełnienie (`background: var(--tf-warning)`, tekst ciemny) | nie da się dostać `solid` dla innych tonów |
| `outline` | **niezaimplementowany** dla `tf-badge` (istnieje za to na `tf-chip`, patrz `chip.md`) | — |
| `dot` | `tf-badge[variant="dot"]` — działa przez selektor atrybutu CSS, mimo że `variant` nie jest w `observedAttributes` JS | 8×8px kropka bez tekstu |
| `pulse` | `tf-badge[variant="pulse"]` — działa analogicznie, dodaje `::before` z animacją `tf-badge-pulse` | pulsujący halo (`box-shadow` 0→6px→0) |

## Rozmiary

Brak skali rozmiarów w sdk-spec dla `Badge` (nie ma `BadgeSize`) — jeden
rozmiar (`min-width: 20px; height: 20px; font-size: 10px`, `typography.scale.overline`
najbliższy pod względem rozmiaru, ale bez `uppercase`/`tracking`). `StatusPill`
podobnie — jeden rozmiar.

## Stany

| Stan | Tło | Tekst/ikona | Obramowanie | Uwagi |
|---|---|---|---|---|
| `tone="accent"` | `accent.glow` | `accent.primary_hover` (`--tf-accent-2`) | brak | domyślny ton |
| `tone="danger"` | `semantic.critical` @ 18% | `semantic.critical` | brak | |
| `tone="success"` | `semantic.success` @ 18% | `semantic.success` | brak | |
| `tone="warning"` | `semantic.warning` @ 18% | `semantic.warning` | brak | |
| `tone="info"` | `semantic.info` @ 18% | `semantic.info` | brak | |
| `tone="neutral"` | `bg.elevated` | `text.secondary` | brak | stan wygaszony (np. trening anulowany) |
| `tone="hot"` | pełny `semantic.warning` | ciemny (`#1a1200`) | brak | jedyny solid; animacja pop-in 0.34s przy pojawieniu |
| hover | `scale(1.12)` | — | — | tylko warianty tekstowe (nie `dot`) |
| status-pill `ok`/`err`/`warn` | kolor @ 12% | `semantic.success/critical/warning` | — | kropka `::before` pulsuje (sprawdzić `@keyframes` w pełnym zakresie CSS) |

## Zachowanie

- Wskaźnik: brak interaktywności — `tf-badge`/`tf-status-pill` to elementy
  informacyjne, nie klikalne (hover `scale(1.12)` to czysto dekoracyjna reakcja
  na wskazanie, nie sygnał klikalności).
- Klawiatura: nie dotyczy (brak `tabindex`, nie jest fokusowalny).
- Animacje: `pulse`/`dot[pulse]` — `@keyframes tf-badge-pulse`, 2s,
  `motion.easing.standard` (kod: `--tf-spring-smooth`), nieskończona pętla; `hot`
  — pop-in 0.34s `motion.easing.overshoot` (kod: `--tf-spring-snappy`) przy
  montażu. **Brak `prefers-reduced-motion`** dla animacji nieskończonych
  (`pulse`) — realne ryzyko a11y (migające elementy bez możliwości wyłączenia).
- Zdarzenia/API: `tf-badge` — atrybuty `tone`, `value`; treść ze slotu jako
  fallback gdy brak `value`. `tf-status-pill` — atrybuty `status`, `label`. Żaden
  z dwóch nie emituje eventów (czysto prezentacyjne).
- Zaznaczanie tekstu: dozwolone (to zwykły tekst).

## Dostępność

Brak jawnej roli ARIA w obu komponentach — renderowane jako zwykły `<span>` z
tekstem, co jest poprawne dla czysto informacyjnego badge'a **pod warunkiem**,
że tekst niesie pełne znaczenie (np. „12” obok „Powiadomienia” w kontekście, nie
samo „12” bez etykiety nadrzędnej). Animacja `pulse` bez `prefers-reduced-motion`
to odstępstwo od reguły globalnej w `design/README.md` punkt 5. Kontrast:
`tone="warning"`/`"hot"` tekst ciemny na jasnym tle — zamierzone i poprawne;
pozostałe tony (tekst kolorowy na przezroczystym tle @ 12–18%) wymagają
sprawdzenia rzeczywistego kontrastu na tle karty/tabeli, nie tylko na `bg.base`.

## Responsywność i platformy

Brak logiki responsywnej — stały rozmiar niezależny od breakpointu. Na
ESP32-P4: animacja `pulse` (box-shadow blur) może być kosztowna na CPU
rendererze — do zweryfikowania przy implementacji natywnej (`platforms/esp32p4.md`
mówi o płaskich cieniach; pulsujący `box-shadow` to inny przypadek niż statyczna
elewacja).

## Tokeny użyte

`color.themes.dark.accent.glow`, `color.themes.dark.accent.primary_hover`,
`color.themes.dark.semantic.success/warning/critical/info`,
`color.themes.dark.bg.elevated`, `color.themes.dark.text.secondary`,
`typography.scale.overline`, `radius.pill`, `motion.duration_ms.slow` (pulse ~2s
przekracza `deliberate` 450ms — własna wartość, patrz odstępstwa),
`motion.easing.standard`, `motion.easing.overshoot`.

## Znane odstępstwa w kodzie (2026-09-14)

- **`BadgeVariant` (solid/soft/outline/pulse/dot) nie jest realizowany jako
  jeden spójny atrybut `variant`.** `tf-badge.js` `observedAttributes`
  (linia 15) zna tylko `tone`/`value` — `dot`/`pulse` działają wyłącznie dzięki
  selektorom CSS na atrybucie hosta (`tf-badge[variant="dot"]`,
  `controls.css:3179`), które zadziałają, bo CSS nie potrzebuje
  `observedAttributes`, ale **JS-owa logika `_update()` nigdy nie czyta ani nie
  reaguje na `variant`** — czysty przypadek, że to działa. `outline` nie ma
  odpowiednika wcale, `solid` istnieje tylko dla `tone="hot"`.
- **Nazwy tonów nie pokrywają się z `Tone`.** Kod: `accent/danger/success/
  warning/info/neutral/hot` (`tf-badge.js:11`). Spec: `neutral/primary/success/
  warning/critical/info/muted`. `accent`≈`primary`, `danger`≈`critical`, `hot`
  nie ma odpowiednika w `Tone` w ogóle — to osobna, custom kategoria
  („czekam na Ciebie”), udokumentowana w komentarzu kodu jako świadoma decyzja.
- **`StatusPill` nie ma tagu w katalogu protokołu** (`docs/ADDON_UI_COMPONENT_CATALOG_v1.md`
  nie zawiera frazy „StatusPill”) — istnieje wyłącznie jako komponent HTML;
  addony realizują ten sam efekt przez `Badge` z `dot`/`pulse`. Do zdecydowania,
  czy `StatusPill` dostaje własny tag, czy zostaje wyłącznie warstwą HTML.
- **Animacje `pulse`/`hot` bez `prefers-reduced-motion`** — brak guardu w
  zakresie CSS odczytanym dla tego komponentu.

## Przykłady

```html
<tf-badge tone="danger" value="3"></tf-badge>
<tf-badge tone="hot" value="1"></tf-badge>
<tf-badge variant="dot" tone="success"></tf-badge>

<tf-status-pill status="ok" label="Running"></tf-status-pill>
<tf-status-pill status="err" label="Down"></tf-status-pill>
```

```rust
// natywnie (tenta-ui-widgets, API docelowe)
Badge::new("3").tone(Tone::Critical)
Badge::dot().tone(Tone::Success).pulse(true)

StatusPill::new("Running").tone(Tone::Success)
```
