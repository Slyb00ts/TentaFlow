# Analityka — unifikacja „Analityka modeli" + „Zużycie tokenów"

Data: 2026-08-19 · Mockupy: `m01`–`m06` · Wcześniejsze wersje: `model-metrics-20260630`

## 1. Diagnoza (audyt kodu)

Dwa ekrany robią w ~70% to samo, a każdy robi to inaczej i brzydko:

| Problem | Dowód |
|---|---|
| Duplikacja danych | `TokenUsageSummaryWire` (8 pól) to ścisły podzbiór `ModelMetricsRowWire`; tabela „Zużycie" = tabele model-metrics minus koszt/percentyle |
| Dwie tabele źródłowe, dwie ścieżki zapisu | `token_usage_daily` (AI gateway) vs `model_metrics_rollup` (runtime executor) — liczby na ekranach MOGĄ się różnić; `bump_model_metric_row` hardkoduje `audio_ms:0, images:0` |
| Gołe UUID | user = UUID, node = 64-hex; backend MA nazwy (`user_accounts.display_name`, `user_groups.name`, `sync_nodes.display_name`) ale ich nie dołącza |
| Fałszywy status | chip „online" przy nodach jest bezwarunkowy (`model-metrics.js:823`); realny liveness leży w `sync_nodes.last_seen_at` |
| Wykres ręczny SVG | `renderTokenChart` z `preserveAspectRatio="none"` — słupki rozciągają się nierównomiernie na każdej szerokości ≠ 960px; `<title>` zamiast tooltipa |
| Komponenty wykresów nieużywane | `TfCartesianChart` (tooltip, legenda, zoom, ResizeObserver) istnieje; model-metrics go nie importuje, token-usage nie włącza legendy/tooltipa |
| Zero animacji | brak keyframes/transition w tf-line/bar-chart i w CSS obu ekranów |
| token-usage bez CSS | klasy `.tu-*` nie istnieją w żadnym arkuszu; layout trzyma się na `.card` i inline style |
| Rozjazd tokenów CSS | model-metrics.css używa `--text-2/--bg-3`, komponenty `--tf-*`; piksele na sztywno (11.5px, 14px…) |
| i18n | tylko pl+en (de/es/fr — zero kluczy), ~20 kluczy zdublowanych między `token_usage.*` i `model_metrics.*` |
| Nieczytelne liczby | `121000000` w tabelach bez skalowania jednostek |

## 2. Decyzje projektowe

**D1. Jeden ekran „Analityka"** zastępuje oba. Zakładki: Przegląd · Użytkownicy i grupy · Modele · Nody i serwisy · Limity · Rozliczenia. Nav: jedna pozycja, `token-usage` i `model-metrics` znikają.

**D2. Jedno źródło danych.** Ekran czyta wyłącznie `model_metrics_rollup` (ma wszystko + percentyle + koszty). `token_usage_daily` zostaje TYLKO dla egzekwowania quot (AI gateway) — przestaje być źródłem UI. Zakładka „Zużycie" umiera (duplikat); „Limity" i „Koordynator" łączą się w jedną zakładkę „Limity".

**D3. Nazwy zamiast UUID — rozwiązywane po stronie Core.** Wire rows dostają opcjonalne `display_name` (append-only, `#[serde(default)]`): user → `display_name||username||email`, group → `name`, node → `sync_nodes.display_name`, model → `display_name` z katalogu. UI pokazuje nazwę jako tytuł, skrócone ID jako drugi wiersz (`tf-table__cell-sub`). Liveness nodów z `last_seen_at` (zielona pulsująca kropka / szara), nie hardkod.

**D4. Adaptacyjne jednostki liczb.** Jedna funkcja `fmtCompact(n)`: `<10 tys` → pełna liczba z separatorem; `<1 mln` → `12,4 tys`; `<1 mld` → `121 mln`; dalej → `3,2 mld`. Pełna wartość zawsze w tooltipie (`title`). Wyjątek: Rozliczenia — kwoty i tokeny DOKŁADNE (separator tysięcy), bo to podstawa faktur. Osie wykresów: `fmtCompact`.

**D5. Wykresy: rozbudowa `TfCartesianChart`, żadnej zewnętrznej biblioteki.** Do komponentu dochodzą: animacja wejścia (słupki `scaleY` 420 ms cubic-bezier ze staggerem 12 ms/słupek, linie `stroke-dashoffset`; jednorazowo, wyłączana `prefers-reduced-motion`), crosshair + tooltip domyślnie ON, stacked bary z zaokrąglonym szczytem, poprawny `viewBox` liczony z `ResizeObserver` (koniec z `preserveAspectRatio="none"`). Model-metrics porzuca ręczny SVG.

**D6. Drill-down.** Każdy wiersz (user/grupa/model/node) klika się w widok szczegółu w tej samej zakładce: breadcrumb, karta encji, sparkline trendu, breakdown per model / per node, ostatnie okresy. Filtry globalne (okres + node + model) są lepkie między zakładkami i **auto-przeładowują** (koniec z „działa dopiero po Odśwież").

**D7. Wydajność.** Zero bibliotek zewnętrznych. Jeden fetch na zakładkę (Przegląd: 5 równoległych jak dziś), cache odpowiedzi per `(tab, filtry)` na czas montażu, rendering SVG < 2 tys elementów, animacje wyłącznie transform/opacity (kompozytor), count-up KPI przez rAF ~500 ms.

**D8. Porządek CSS/i18n.** Jeden arkusz `analytics.css` wyłącznie na tokenach `--tf-*`; klasy `an-*`; skasować `model-metrics.css` (w tym martwe `.mm-grid-2`, `.mm-share-row`…). Jeden namespace `analytics.*` w i18n, komplet 5 locale, formy mnogie przez `{count|...}`; koszt przez `Intl.NumberFormat` z walutą, nie tłumaczony string.

**D9. Pełna responsywność (desktop → tablet → telefon).** Breakpointy ≤1020px (sidebar → sticky
topbar z hamburgerem, gridy 1-kolumnowe) i ≤720px (zakładki przewijane palcem, toolbar
pełną szerokością, KPI 2×2, mini-KPI encji/nodów gridem 2×2 z `white-space: nowrap`,
nagłówki kart zawijane — hint pod tytułem, stopki tabel w kolumnie). Tabele NIGDY nie
rozpychają strony: `min-width: max-content` + scroll poziomy WEWNĄTRZ karty. Wykres
renderuje viewBox z realnej szerokości kontenera (tekst osi zawsze ~10px), na wąskim
ekranie mniej słupków i rzadsze etykiety. Tooltip wykresu wyłączony na dotyku.

**D10. Bez awatarów-inicjałów przy osobach.** Użytkownicy i grupy: sama nazwa + sub
(email / ID / liczba członków). Kafelki literowe zostają TYLKO przy modelach, kropka
liveness tylko przy nodach.

## 3. Mapa mockupów

| Plik | Widok | Kluczowe elementy |
|---|---|---|
| `m01-przeglad.html` | Przegląd | KPI z count-up, stacked wykres prompt/completion z tooltipem i legendą, top modele/userzy/nody Z NAZWAMI, banner mesh |
| `m02-drilldown.html` | Drill-down usera | breadcrumb, karta encji, sparkline, breakdown per model, per node, trend okresów |
| `m03-modele.html` | Modele | tabela z percentylami + porównanie node×serwis, klik → drill |
| `m04-nody.html` | Nody i serwisy | karty nodów z realnym liveness, nazwy, per-serwis tabela |
| `m05-limity.html` | Limity | quoty z paskami zużycie/limit, edytor, panel koordynatora dzierżaw |
| `m06-rozliczenia.html` | Rozliczenia | dokładne kwoty, udział kosztów, edytor cennika |

## 4. Plan implementacji (po akceptacji mockupów)

1. Core: rozszerzyć `ModelMetricsRowWire`/`ModelNodeServiceRowWire` o `display_name` (+ join w dispatch), liveness nodów z `sync_nodes`, przenieść handlery quota/coordinator pod wspólny ekran (payloady zostają — tylko UI).
2. `TfCartesianChart`: animacje, crosshair, stacked-rounded, naprawa viewBox (zyskują wszystkie moduły używające wykresów).
3. Nowy `www/js/modules/analytics.js` + `www/css/analytics.css`; kasacja `token-usage.js`, `model-metrics.js`, `model-metrics.css`; nav i Router — jedna pozycja.
4. i18n: namespace `analytics.*` w 5 locale; usunąć `token_usage.*`/`model_metrics.*`.
5. `fmtCompact` w `utils.js` (wspólne dla innych ekranów, np. Benchmark Studio).

## 5. Poza Analityką — wymagania integracji zewnętrznej z `/v1` (zgłoszone przez klienta API)

Zgłoszenie z projektu integrującego się z bramką OpenAI-compatible (LAN, vLLM za bramką).
Zweryfikowane w naszym kodzie — oba problemy są realne:

**G1. Bramka gubi `response_format.json_schema` (i `guided_json`).**
`ResponseFormat` (`api/openai/types.rs:205`) ma tylko pole `type` — typowana pętla
request→struct→request wycina `json_schema`, więc backend dostaje
`{"type":"json_schema"}` bez schematu i zwraca 400 („the 'json_schema' field must be
provided"). Vendorowe rozszerzenia top-level (`guided_json`) znikają tak samo po cichu.
Zmiany:
- `ResponseFormat` += `json_schema: Option<serde_json::Value>`
  (`#[serde(default, skip_serializing_if = "Option::is_none")]`) — pełny passthrough.
- `ChatCompletionRequest` += `#[serde(flatten)] extra: serde_json::Map<String, Value>`,
  żeby ŻADNE nieznane pole vendorowe nie ginęło w przelocie (koniec klasy błędów, nie
  tylko tego jednego).
- Flaga zdolności `supports_structured_output` przy modelu (obok `supports_embeddings`),
  żeby klient mógł wykryć wsparcie zamiast sondować; obejście przez `tools`+`tool_choice`
  działa i zostaje.

**G2. Certyfikat TLS bez IP SAN + wspólny klucz w repo.**
`certs/cert.pem` jest wkompilowany `include_bytes!` (`unified_server.rs:190`):
CN=localhost, SAN tylko `DNS:localhost`, jeden klucz prywatny dla wszystkich instalacji.
Klient pinujący CA nie zweryfikuje `IP:192.168.11.26`, a `danger_accept_invalid_certs`
to nie jest odpowiedź. Zmiany:
- Generacja certyfikatu per instalacja przy pierwszym starcie (rcgen) do `<data>/tls/`,
  SAN-y: `localhost`, hostname, WSZYSTKIE lokalne adresy IP wykryte przy starcie
  + opcjonalne `[server.tls] extra_sans` w configu (DNS i IP).
- Regeneracja gdy zbiór lokalnych IP przestaje pokrywać SAN-y certyfikatu (log + nowy cert).
- Wkompilowany cert zostaje wyłącznie jako awaryjny fallback; klucz z repo przestaje być
  używany w normalnej pracy.

**G3. Notatka (bez zmian w repo).** Modele rozumujące potrafią zjeść cały `max_tokens`
na `reasoning_content` → `finish_reason: length` bez treści. To budżetuje klient
(tysiące, nie setki tokenów przy structured output); rozważyć wzmiankę w docs/openapi.
