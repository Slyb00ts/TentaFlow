# Ikonografia

Ikony TentaFlow to wyłącznie kontury SVG (stroke, nie fill) rysowane z jednego
zestawu geometrii per system renderujący. Zero emoji, zero icon fontów — zasada
4.3 w [`../README.md`](../README.md). Ten dokument opisuje, skąd realnie
pochodzi geometria dziś (stan 2026-09-14, zweryfikowany w kodzie), bo — inaczej
niż sugeruje reguła 4.3 README — **nie ma jednego pliku sprite'a**: host-owa
powłoka aplikacji i protokół UI addonów mają dziś dwa niezależne sprite'y,
różne konwencje nazw i różne grubości kreski. Traktuj to jako znany rozjazd do
wyprostowania (patrz `design/MIGRATION.md`, gdy powstanie), nie jako docelowy
stan.

## Dwa sprite'y, dwa systemy

| | Powłoka hosta (`tf-*`, strony admin/user) | Protokół UI addonów |
|---|---|---|
| Plik geometrii | inline `<svg data-role="sprite">` w `www/index.html` (body, linia 60) | `www/img/icons.svg` (osobny plik, ładowany przez `<use>` z sieci) |
| Liczba symboli | 130 (`<symbol id="i-*">`) | 143 (`<symbol id="icon-*">`) |
| Konwencja `id` | `i-<name>` | `icon-<name>` (`_` → `-` z `IconName` enuma) |
| `stroke-width` | 1.75 (`.icon` w `css/style.css:214`) | 1.5 (inline `<style>` w `icons.svg:16-18`) |
| Rozmiary | `.icon` 16px / `.icon-lg` 20px / `.icon-xl` 24px (px, stałe) | `.tf-icon--size-xs…xl` 0.875em…2em (em, względne) |
| Konsument | `tf-*` komponenty (light DOM), CSS `.icon*` po całym `www/css` | `sdk-runtime/icon-renderer.js` → `IconRef.kind === 'named'` |
| Lista dozwolonych nazw | brak whitelisty — dowolny `id` obecny w sprite'cie | `ICON_NAMES` w `icon-renderer.js`, lustro 142 nazw z `tentaflow-sdk-spec/src/protocol/ui/icon_name.rs` |

Oba pliki rysują ten sam wizualny język (Heroicons/Lucide-owy outline 24×24),
ale to **dwie osobne, ręcznie synchronizowane kopie geometrii** pod dwiema
różnymi konwencjami `id`. Ikona `i-key`/`icon-key` może się różnić w
szczegółach ścieżki między plikami — nie zakładaj identyczności bez
porównania. Osobna, potwierdzona usterka: `www/index.html` ma **dwa**
`<symbol id="i-key">` (linie 65 i 107) — duplikat `id` w SVG jest
niezdefiniowany w specyfikacji i zależny od przeglądarki; do naprawienia.

Trzeci, jeszcze inny system rozmiarów istnieje w [`tokens/tokens.json`](../tokens/tokens.json)
→ `control.icon_size` (`xs/sm/md/lg/xl` = 12/16/20/24/32 px) — to jest
**docelowa, kanoniczna skala `IconSize`** współdzielona z `tentaflow-sdk-spec`.
Żadna z dwóch live implementacji nie odwzorowuje jej 1:1 dziś: `.icon`/`.icon-lg`/
`.icon-xl` pokrywa tylko 16/20/24 (brakuje 12 i 32, dorabianych ad-hoc jako
inline `width`/`height` po CSS stron), a `.tf-icon--size-*` używa jednostek
`em` zależnych od `font-size` kontekstu, nie stałych px. Nowy kod powinien
celować w skalę `IconSize` z tokenów, nie w istniejące klasy `.icon-lg`/`.icon-xl`
(ich nazwy nie odpowiadają wartościom tokenów — `.icon-lg` = 20px = token `md`,
`.icon-xl` = 24px = token `lg`).

## Anatomia symbolu

```text
<symbol id="i-<name>" viewBox="0 0 24 24">
  <path d="…"/>       fill: none, stroke: currentColor
</symbol>
```

| Właściwość | Wartość | Token |
|---|---|---|
| `viewBox` | `0 0 24 24` zawsze | — |
| `fill` | `none` | — |
| `stroke` | `currentColor` — dziedziczy kolor tekstu/tone kontenera | `color.themes.dark.tone.*.fg` |
| `stroke-linecap` / `stroke-linejoin` | `round` / `round` | — |
| `stroke-width` domyślny | 1.75 | `control.icon_stroke.default` |
| `stroke-width` przy `≤ 14 px` | 2.0 | `control.icon_stroke.small` |
| `stroke-width` przy `≥ 32 px` | 1.5 | `control.icon_stroke.large` |

Grubszy stroke na małych rozmiarach kompensuje utratę czytelności przy
skalowaniu w dół; cieńszy na dużych zapobiega efektowi "grubej kreski" na
ikonach 32 px+. Reguła jest zdefiniowana w `tokens.json`, ale nie jest jeszcze
zautomatyzowana w CSS — dziś większość reguł `.icon` w `css/*.css` ustawia
`stroke-width` ręcznie per selektor (patrz np. `.nav-section .heading .icon`
→ `stroke-width: 2` przy 13px, `.icon-think` itp.) zamiast liczyć z rozmiaru.

## `IconSize` — skala kanoniczna

| Token | px | Typowe użycie |
|---|---|---|
| `xs` | 12 | ikony w metadanych, chipach, `caption` |
| `sm` | 16 | domyślny rozmiar w przyciskach/inputach (`control.height.sm/md`) |
| `md` | 20 | nagłówki sekcji, `tf-detail-header` |
| `lg` | 24 | ikony akcji, avatary ikon, `tf-empty-state` |
| `xl` | 32 | hero/empty-state duże ikony |

## Użycie w HTML

```html
<svg class="icon" aria-hidden="true">
  <use href="#i-settings"/>
</svg>
```

- Ikona dekoracyjna obok tekstu → `aria-hidden="true"` na `<svg>`, etykieta
  nosi tekst obok.
- Ikona samodzielna (icon-only button) → `aria-hidden="true"` na `<svg>` +
  `aria-label` na przycisku-hoście, nigdy odwrotnie.
- W protokole addonów `IconRef` renderuje się przez `renderIcon()`
  (`sdk-runtime/icon-renderer.js`), które samo nadaje `aria-hidden="true"` i
  `focusable="false"`; etykieta zawsze przychodzi z komponentu-rodzica
  (Button, MenuItem, …), nigdy z `IconRef` samego.

## Pułapka Shadow DOM

`<use href="#i-…">` wewnątrz shadow roota **nie widzi** symboli zdefiniowanych
w light DOM (ograniczenie specyfikacji/przeglądarek — potwierdzone w
komentarzu `shared-styles.js:38-41`). Komponenty renderujące się w shadow DOM
muszą sklonować sprite lokalnie:

```js
import { injectSpriteIntoShadow } from './shared-styles.js';
// w constructor()/connectedCallback() komponentu:
injectSpriteIntoShadow(this.shadowRoot);
```

`injectSpriteIntoShadow()` szuka `svg[data-role="sprite"]` (albo
`body > svg[aria-hidden="true"]`) w dokumencie, klonuje go raz (cache w module
`_cachedSprite`), zeruje `width`/`height` i chowa (`position: absolute`,
`overflow: hidden`). Klon jest tworzony leniwie przy pierwszym komponencie
shadow-DOM, który go zażąda — nie przy starcie aplikacji.

## Dodawanie nowej ikony

### Do powłoki hosta (`i-*`, strony admin/user)

1. Zacznij od źródła w stylu Lucide/Heroicons (ISC/MIT) — outline 24×24,
   jedna rodzina wizualna z resztą sprite'a.
2. Zoptymalizuj SVG (usuń `id`/`class` ze ścieżek, zaokrąglij współrzędne,
   usuń zbędne grupy) — sprite ma pozostać lekki, bo ładuje się inline przy
   każdym starcie strony.
3. Dodaj `<symbol id="i-<name>" viewBox="0 0 24 24">…</symbol>` do
   `www/index.html` w bloku `<svg data-role="sprite">`. Sprawdź, że `id` jest
   unikalny (patrz usterka `i-key` wyżej — nie powielaj).
4. Nie ma osobnego pliku katalogu ikon host-owych do zaktualizowania — jedynym
   miejscem prawdy jest sam sprite w `index.html`. Jeśli dodajesz ikonę do
   nawigacji/komponentu, użyj `<svg class="icon"><use href="#i-<name>"/></svg>`.

### Do protokołu UI addonów (`icon-*`, `IconRef`)

1. To samo źródło wizualne (Lucide/Heroicons), ale dodawane w **dwóch**
   miejscach naraz — inaczej addon dostanie `TypeError: unknown icon`:
   - `www/img/icons.svg`: `<symbol id="icon-<name>" viewBox="0 0 24 24">…</symbol>`
     (konwencja `id`: nazwa z `_` zamienionym na `-`).
   - `tentaflow-sdk-spec/src/protocol/ui/icon_name.rs`: nowy wariant enuma
     `IconName` (właściciel nazwy — decoder protokołu odrzuca nieznane nazwy).
   - `www/js/sdk-runtime/icon-renderer.js`: dopisz `snake_case` nazwę do
     `ICON_NAMES` (lustro whitelisty z `icon_name.rs`; musi być identyczne).
2. Zaktualizuj wpis w `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` (katalog ikon
   dostępnych addonom), jeśli katalog ją wymienia.
3. Addon używa jej przez `IconRef`, nie bezpośrednio przez `<use>`:
   `{ kind: 'named', name: 'my_icon', size: 'md', tone: 'primary' }`.

## Pełna lista ikon hosta (`i-*`, 130 symboli, 2026-09-14)

Pogrupowane tematycznie dla przeglądu — grupy są redakcyjne (sam plik
`index.html` nie dzieli ich komentarzami sekcji, w przeciwieństwie do
`icons.svg`), alfabetycznie w obrębie grupy.

| Domena | Ikony |
|---|---|
| Nawigacja / powłoka | apps, bus, catalog, chat, cluster, core, dashboard, desktop, flow, globe, globe-grid, grid-2x2, home, home-simple, host, list, management, meeting, models, network, network-svg, os, pi, prompt, puzzle, registry, rules, services, settings, users |
| Akcje | check, check-circle, close, collapse, copy, download, edit, external-link, filter, max, min, more, pause, play, plus, record, record-dot, refresh, rotate, save, search, send, share, stop, trash, unlock, x |
| Status / uwaga | alert, ban, bell, crown, info, question, shield, star, target |
| Komunikacja / media | image, mail, message, mic, paperclip, pin, speaker, speaker-alt, volume |
| Kierunek | arrow, arrow-left, arrow-out, chevron-down, chevron-left, chevron-right |
| Infrastruktura / sprzęt | atom, bolt, brain, branch, chip, cloud, cpu, cylinder, database, docker, git, gpu, grid-rows, iface-lan, iface-loop, iface-tb, iface-virt, iface-vpn, iface-wifi, ram, rag-db, transform |
| Dane / wykresy | bar-chart, chart-line, clock, clock-glance, history, layers, line-chart, trend |
| Pliki / kod | bot, code, file, file-text, flask, folder, terminal |
| Bezpieczeństwo | key, lock, shield, unlock |
| Inne | calendar, sparkle, zap |

Uwaga: host nie ma `i-warning`/`i-danger` (protokół addonów ma `icon-warning`/
`icon-danger`) — stany ostrzeżenia/błędu w powłoce hosta idą przez `i-alert`
+ klasę `tone`, nie przez osobną ikonę. Nie zakładaj symetrii nazw między
oboma sprite'ami przy migracji komponentu z jednego systemu na drugi.

## Jak ikony rysuje natywny renderer (planowane)

W repozytorium TentaFlow nie ma dziś kodu natywnego renderera ikon — dokument
`docs/TENTAENGINE_INTEGRATION_PLAN.md`, na który wskazuje `design/README.md`,
jeszcze nie istnieje. Poniższe jest planem opartym na zweryfikowanym prior
art z silnika TentaEngine (`crates/tenta-render-gpu/shaders/shader_text.wgsl`),
nie na zaimplementowanym kodzie ikon:

- TentaEngine renderuje dziś tekst przez **atlas pokrycia** (coverage atlas):
  `atlas_tex` to tekstura alpha, `fs_main` próbkuje `s.a` i mnoży przez
  `u.color` — `return vec4(u.color.rgb, u.color.a * s.a)`. Kolor glifu = kolor
  UI, nie kolor zapieczony w atlasie.
- Plan dla ikon: ta sama technika, jeden atlas pokrycia **per `IconSize`**
  (12/16/20/24/32 px), wypiekany offline z tego samego źródła wektorowego co
  sprite web (Lucide-style outline), tintowany `currentColor`/`tone.fg` w
  fragment shaderze — identycznie jak glify tekstu.
- Wektorowe rysowanie ścieżek ikon (bez wypiekania do atlasu, dla dowolnej
  skali) jest odłożone na później — koszt tesselacji per klatka nie jest
  uzasadniony dla stałego, małego zestawu ikon UI.
- Renderer natywny konsumowałby jeden zestaw nazw ikon (docelowo `IconName`
  z `tentaflow-sdk-spec`, bo to jedyna strona, która ma formalną, wersjonowaną
  whitelistę) — nie dwa sprite'y jak dziś w HTML.

## Checklist dla autora strony

- [ ] Ikona pochodzi z istniejącego `id` w sprite'cie (`i-*` dla stron hosta,
      `icon-*`/`IconName` dla UI addonów) — nie wklejasz inline SVG per strona.
- [ ] `viewBox="0 0 24 24"`, brak `fill` poza `none`, `stroke="currentColor"`.
- [ ] Rozmiar z `IconSize` (`control.icon_size`), nie dowolny px.
- [ ] Ikona dekoracyjna → `aria-hidden="true"`; ikona samodzielna →
      `aria-label` na hoście (przycisku/linku), nie na `<svg>`.
- [ ] Komponent renderuje się w shadow DOM → wywołany `injectSpriteIntoShadow()`
      przed pierwszym `<use>`.
- [ ] Nowa ikona dodana do obu miejsc, jeśli ma być dostępna addonom
      (`icons.svg` + `icon_name.rs` + `ICON_NAMES`), nie tylko do jednego.
- [ ] Brak nowego duplikatu `id` w `index.html` (sprawdź `grep 'id="i-<nazwa>"'`).
