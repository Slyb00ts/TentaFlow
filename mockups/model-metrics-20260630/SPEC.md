# Model Metrics — spec współdzielony mockupów

Statystyki użycia i wydajności modeli AI w sieci mesh TentaFlow. Każdy node widzi
dane CAŁEJ sieci (rollup replikowany jak `token_usage_daily`). Wszystkie mockupy
dzielą `shared/styles.css` (TentaFlow design tokens, Manrope, dark indigo/violet).

## Zasady wizualne (NIE łamać)
- Tylko klasy z `shared/styles.css`: `.kpi/.kpi-grid`, `.tf-table`, `.section-card`,
  `.chip/.status-pill`, `.segmented`, `.filter-chip`, `.btn`, `.tabs-bar/.tab`,
  `.grid-2/.grid-3/.grid-4`, `.mono`, `.breadcrumb`, `.app/.sidebar/.main`.
- Liczby zawsze `font-variant-numeric: tabular-nums` (klasa `.mono` lub inline).
- Wykresy: czysty inline SVG (słupki/linie) albo paski CSS — ZERO bibliotek JS.
- Percentyle pokazuj jako p50 / p90 / p99 (nie średnią) tam gdzie wydajność.
- Polski w UI. Jednostki: tok, tok/s, ms, s, GPU-h, zł.

## Wymiary metryk
user · grupa · org · model · node · serwis · backend · modalność · czas(godz/dzień/mies)

## Metryki (definicje)
- Wolumen: prompt_tokens, completion_tokens, total_tokens, request_count,
  embedding_tokens, audio_ms→sek, images.
- Koszt: tokeny × cennik_per_model (prompt i completion osobno) = zł.
- Sprzęt: compute-time = prefill_secs+decode_secs; GPU-godziny; throughput
  sustained/peak (tok/s); obłożenie busy/idle %.
- Wydajność: TTFT p50/p90/p99 (ms), decode tok/s p50/p90/p99, prefill tok/s,
  latencja end-to-end (ms), queue/admission wait (ms), error_rate %, RPS, współbieżność.
- Quota: limit / wykorzystano / pozostało (token_quota + token_lease).

## Architektura (pokaż w UI jako fakt)
- `model_metrics_rollup` godzinowy, single-writer-per-row, replikowany po mesh →
  każdy node sumuje całą sieć lokalnie. Histogramy addytywne → percentyle mesh-wide.
- Drill-down per-request: lokalny w compliance_ai_events; cross-node = on-demand.
- Banner/nuta w UI: „Dane zagregowane z N nodów mesh • ostatnia synch X s temu".

## Dane demo (UŻYWAJ SPÓJNIE we wszystkich stronach)
Org: `Euvic`. Okres domyślny: czerwiec 2026 (miesiąc), „ten miesiąc".
Nody (4): `rig24` (DGX Spark GB10), `rig25` (DGX Spark GB10), `mac-studio` (M3 Ultra), `phone-piotr` (mobilny).
Serwisy/backendy:
  - rig24: vLLM `Qwen3.6-27B` (TP), vLLM `gpt-oss-120b`
  - rig25: vLLM `Qwen3.6-27B` (TP, drugi twin), parakeet STT
  - mac-studio: MLX `Qwen3.6-27B-Q4`, llama.cpp `gpt-oss-20b`
  - phone-piotr: llama.cpp `Qwen3.6-4B` (lokalny na telefonie)
Modele (z aliasami): `Qwen3.6-27B`, `gpt-oss-120b`, `gpt-oss-20b`, `Qwen3.6-4B`,
  `parakeet-0.6b` (STT), `bge-m3` (embeddings).
Userzy (6): `piotr` (admin, grupa Zarząd), `anna` (grupa Dev), `marek` (grupa Dev),
  `kasia` (grupa Sprzedaż), `tomek` (grupa Sprzedaż), `bot-meeting` (grupa Automaty).
Grupy (4): Zarząd, Dev, Sprzedaż, Automaty.
Skala liczb (realistyczna, miesiąc): total ~84.2M tokenów, ~12 540 zapytań,
  TTFT p50 ~310 ms / p90 ~820 ms / p99 ~2 100 ms, decode p50 ~58 tok/s, error ~0.9%.
Cennik przykładowy (zł/1k): Qwen3.6-27B 0.004/0.012, gpt-oss-120b 0.008/0.024,
  gpt-oss-20b 0.002/0.006, Qwen3.6-4B 0.0008/0.0024.

## Szkielet shellu (każda strona content)
Struktura: `.screen > .screen-header(.num,h2,.desc) > .screen-frame > .app`.
`.app` = sidebar (240px) + `.main`. Sidebar: logo „TentaFlow", sekcje nav z
aktywnym „Statystyki / Analityka modeli". Stopka sidebaru: user-chip „Piotr · Admin".
W `.main`: `.breadcrumb` → tytuł sekcji → pasek filtrów (okres: godzina/dzień/miesiąc
jako `.segmented`; node/model/serwis/grupa jako `.select`/`.filter-chip`) →
mesh-banner (nuta o agregacji) → treść.

Ikony: wklej blok `<svg width=0 height=0>...<symbol id="i-*">` jak w robots
m02-detail.html (home, dashboard, services, settings, cpu, activity, users, audit, list).
Dodaj symbole: i-coins (monety), i-gauge (prędkościomierz), i-server, i-zap, i-trend.

## Strony
- index.html — opis zestawu + 3 karty zasad + grid linków do stron (jak robots/index).
- m01-overview.html — Przegląd globalny (mesh-wide): rząd KPI (total tokenów, zapytania,
  TTFT p50/p90/p99, decode p50, error %, koszt zł), wykres tokenów w czasie (SVG słupki
  prompt vs completion), top modele / top userzy / top nody (3 mini-tabele), pasek nodów online.
- m02-users-groups.html — Rozliczanie userów i grup: zakładki „Użytkownicy" / „Grupy".
  Tabela userów (tokeny prompt/completion/total, zapytania, audio, obrazy, koszt zł, quota bar).
  Tabela grup = rollup. Drill-down panel jednego usera (per-model breakdown + trend).
- m03-models.html — Modele: tabela per-model (total tokeny, zapytania, decode p50/p90,
  TTFT p50/p90, error %, koszt). Sekcja „Porównanie model×node×backend" (ten sam
  Qwen3.6-27B na rig24 vLLM vs rig25 vLLM vs mac MLX — obok siebie KPI + paski wydajności).
- m04-nodes-services.html — Nody i serwisy: karty nodów (produkcja tokenów, GPU-h,
  throughput sustained/peak, obłożenie %), pod każdym tabela serwisów na tym nodzie
  (serwis, backend, model, zapytania, TTFT p50/p99, decode p50, error %, status pill).
- m05-requests.html — Eksplorator zapytań (drill-down request-level): pasek filtrów +
  tabela ostatnich zapytań (czas, user, model, node, serwis, prompt/compl tok, TTFT,
  decode tok/s, e2e ms, status). Wiersz rozwijany = szczegóły jednego zapytania
  (timeline: admission→prefill→1.token→decode→koniec). Nuta: per-request lokalny/on-demand.
- m06-billing.html — Rozliczenia/koszty: przełącznik „Wg userów / Wg grup / Wg sprzętu".
  Tabela kosztów (podmiot, tokeny, GPU-h, koszt zł, udział %), wykres udziału (paski),
  panel cennika modeli (edytowalny), przyciski Eksport CSV / PDF, zakres dat.
